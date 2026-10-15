#requires -Version 7.0
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'Windows Vulkan loader tests require Windows.' }
. (Join-Path $PSScriptRoot 'windows-gpu-worker-cmake-bootstrap.ps1')

$builderPath = Join-Path $PSScriptRoot 'build-windows-vulkan-policy-loader.ps1'
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($builderPath, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'Vulkan policy builder did not parse.' }
# Exercise the actual pure/input helpers and Git patch policy without executing
# the native builder.
foreach ($name in @('Assert-LoaderProperties', 'Open-LoaderInput', 'Expand-LoaderSource', 'Assert-NoLoaderBuildOverrides', 'Get-LoaderModulePathFunction', 'Invoke-LoaderNative', 'Invoke-LoaderPatch')) {
    $definitions = @($ast.FindAll({ param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq $name
    }, $false))
    if ($definitions.Count -ne 1) { throw "Expected exactly one tested helper: $name" }
    . ([scriptblock]::Create($definitions[0].Extent.Text))
}

$script:checks = 0
function Assert-Policy([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:checks++
}
function Assert-PolicyRejected([scriptblock]$Action, [string]$Message) {
    $rejected = $false
    try { & $Action | Out-Null } catch { $rejected = $true }
    Assert-Policy $rejected $Message
}
function Assert-PolicyBytes([byte[]]$Actual, [byte[]]$Expected, [string]$Message) {
    $matches = $Actual.Length -eq $Expected.Length
    if ($matches) {
        for ($index = 0; $index -lt $Actual.Length; $index++) {
            if ($Actual[$index] -ne $Expected[$index]) {
                $matches = $false
                break
            }
        }
    }
    Assert-Policy $matches $Message
}
function Get-PolicyGitEnvironmentNames([hashtable]$Values) {
    $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($name in @('GIT_CONFIG_NOSYSTEM', 'GIT_CONFIG_GLOBAL', 'GIT_CONFIG_PARAMETERS',
        'GIT_CONFIG_COUNT', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR', 'GIT_INDEX_FILE',
        'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES')) {
        $null = $names.Add($name)
    }
    foreach ($key in [Environment]::GetEnvironmentVariables('Process').Keys) {
        $name = [string]$key
        if ($name -match '^GIT_CONFIG_(?:KEY|VALUE)_\d+$') { $null = $names.Add($name) }
    }
    foreach ($key in $Values.Keys) { $null = $names.Add([string]$key) }
    return @($names)
}
function Get-PolicyEnvironmentSnapshot([string[]]$Names) {
    $snapshot = @{}
    foreach ($name in $Names) {
        $path = "Env:$name"
        if (Test-Path -LiteralPath $path) {
            $snapshot[$name] = [pscustomobject]@{
                Present = $true
                Value = (Get-Item -LiteralPath $path).Value
            }
        } else {
            $snapshot[$name] = [pscustomobject]@{ Present = $false; Value = $null }
        }
    }
    return $snapshot
}
function Restore-PolicyEnvironmentSnapshot([hashtable]$Snapshot) {
    foreach ($entry in $Snapshot.GetEnumerator()) {
        $path = "Env:$($entry.Key)"
        if ($entry.Value.Present) {
            [Environment]::SetEnvironmentVariable($entry.Key, [string]$entry.Value.Value, 'Process')
        } elseif (Test-Path -LiteralPath $path) {
            # Passing null through PowerShell's .NET string binding can leave an
            # empty variable behind, which is not equivalent to absence.
            Remove-Item -LiteralPath $path -ErrorAction Stop
        }
    }
}
function Assert-PolicyEnvironmentSnapshot([hashtable]$Expected, [string]$Message) {
    $actual = Get-PolicyEnvironmentSnapshot @($Expected.Keys)
    $matches = $true
    foreach ($name in $Expected.Keys) {
        if ($actual[$name].Present -ne $Expected[$name].Present -or
            ($actual[$name].Present -and $actual[$name].Value -cne $Expected[$name].Value)) {
            $matches = $false
            break
        }
    }
    Assert-Policy $matches $Message
}
function Invoke-PolicyWithGitEnvironment([hashtable]$Values, [scriptblock]$Action) {
    $names = @(Get-PolicyGitEnvironmentNames $Values)
    $saved = Get-PolicyEnvironmentSnapshot $names
    try {
        foreach ($name in $names) {
            $path = "Env:$name"
            if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -ErrorAction Stop }
        }
        foreach ($entry in $Values.GetEnumerator()) {
            [Environment]::SetEnvironmentVariable($entry.Key, [string]$entry.Value, 'Process')
        }
        & $Action
    } finally {
        Restore-PolicyEnvironmentSnapshot $saved
    }
}
function Assert-PolicyFixtureOutsideGitRepository([string]$Source) {
    $current = [IO.DirectoryInfo]::new([IO.Path]::GetFullPath($Source))
    while ($null -ne $current) {
        Assert-Policy (-not (Test-Path -LiteralPath (Join-Path $current.FullName '.git'))) "Fixture is inside a Git repository: $Source"
        $parent = $current.Parent
        if ($null -eq $parent -or $parent.FullName -ceq $current.FullName) { break }
        $current = $parent
    }
}
function Invoke-PolicyWithHostileGitOverrides([string]$Root, [scriptblock]$Action) {
    $overrides = @{
        GIT_CONFIG_PARAMETERS = "'core.autocrlf=false'"
        GIT_CONFIG_GLOBAL = ''
        GIT_CONFIG_COUNT = '1'
        GIT_CONFIG_KEY_0 = 'core.eol'
        GIT_CONFIG_VALUE_0 = 'lf'
        GIT_CONFIG_KEY_9 = 'core.attributesFile'
        GIT_CONFIG_VALUE_9 = ''
        GIT_DIR = (Join-Path $Root 'hostile-git-dir')
        GIT_WORK_TREE = (Join-Path $Root 'hostile-work-tree')
        GIT_COMMON_DIR = (Join-Path $Root 'hostile-common-dir')
        GIT_INDEX_FILE = (Join-Path $Root 'hostile-index')
        GIT_OBJECT_DIRECTORY = (Join-Path $Root 'hostile-objects')
        GIT_ALTERNATE_OBJECT_DIRECTORIES = (Join-Path $Root 'hostile-alternate-objects')
    }
    $saved = Get-PolicyEnvironmentSnapshot @($overrides.Keys)
    foreach ($entry in $overrides.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, [string]$entry.Value, 'Process')
    }
    try {
        & $Action
    } finally {
        Restore-PolicyEnvironmentSnapshot $saved
        Assert-PolicyEnvironmentSnapshot $saved 'Hostile Git override restoration changed the caller environment.'
    }
}
function Invoke-PolicyGitEnvironmentFailureCase {
    $values = @{
        GIT_CONFIG_NOSYSTEM = '1'
        GIT_CONFIG_GLOBAL = 'NUL'
        GIT_CONFIG_COUNT = '1'
        GIT_CONFIG_KEY_0 = 'core.autocrlf'
        GIT_CONFIG_VALUE_0 = 'true'
    }
    $callerEnvironment = Get-PolicyEnvironmentSnapshot @(Get-PolicyGitEnvironmentNames $values)
    $failure = $null
    try {
        Invoke-PolicyWithGitEnvironment $values { throw 'controlled Git environment action failure' }
    } catch {
        $failure = $_.Exception
    }
    Assert-Policy ($null -ne $failure -and $failure.Message -ceq 'controlled Git environment action failure') 'Git environment action failure was not propagated exactly.'
    Assert-PolicyEnvironmentSnapshot $callerEnvironment 'Git environment action failure changed the caller environment.'
    return 'action-failure-restores-caller-git-environment'
}
function Invoke-PolicyLoaderPatchCase([string]$Name, [string]$Git, [string]$Root,
    [hashtable]$Config, [bool]$InvalidPatch) {
    $source = Join-Path $Root "$Name-source"
    [IO.Directory]::CreateDirectory($source) | Out-Null
    $sourcePath = Join-Path $source 'fixture.txt'
    $patchPath = Join-Path $Root "$Name.patch"
    $attributesPath = Join-Path $Root "$Name.attributes"
    $original = [Text.Encoding]::UTF8.GetBytes("first`nsecond`n")
    $expected = [Text.Encoding]::UTF8.GetBytes("first`r`nsecond patched`r`nthird`r`n")
    [IO.File]::WriteAllBytes($sourcePath, $original)
    [IO.File]::WriteAllBytes($attributesPath, [byte[]]::new(0))
    $removedLine = if ($InvalidPatch) { '-missing' } else { '-second' }
    $patch = [string]::Join("`n", @(
        '--- a/fixture.txt', '+++ b/fixture.txt', '@@ -1,2 +1,3 @@', ' first',
        $removedLine, '+second patched', '+third')) + "`n"
    [IO.File]::WriteAllText($patchPath, $patch, [Text.UTF8Encoding]::new($false))
    $effectiveConfig = @{ 'core.attributesFile' = $attributesPath }
    foreach ($entry in $Config.GetEnumerator()) {
        if ($effectiveConfig.ContainsKey($entry.Key)) { throw "Duplicate Git fixture configuration: $($entry.Key)" }
        $effectiveConfig[[string]$entry.Key] = [string]$entry.Value
    }
    $environment = @{ GIT_CONFIG_NOSYSTEM = '1'; GIT_CONFIG_GLOBAL = 'NUL'; GIT_CONFIG_COUNT = [string]$effectiveConfig.Count }
    $index = 0
    foreach ($entry in $effectiveConfig.GetEnumerator() | Sort-Object Key) {
        $environment["GIT_CONFIG_KEY_$index"] = [string]$entry.Key
        $environment["GIT_CONFIG_VALUE_$index"] = [string]$entry.Value
        $index++
    }
    $callerEnvironment = Get-PolicyEnvironmentSnapshot @(Get-PolicyGitEnvironmentNames $environment)
    Invoke-PolicyWithGitEnvironment $environment {
        Assert-PolicyFixtureOutsideGitRepository $source
        if ($InvalidPatch) {
            Assert-PolicyRejected { Invoke-LoaderPatch $Git $source $patchPath } "$Name invalid patch was accepted."
        } else {
            Invoke-LoaderPatch $Git $source $patchPath
        }
    }
    Assert-PolicyEnvironmentSnapshot $callerEnvironment "$Name changed caller Git environment."
    if ($InvalidPatch) {
        Assert-PolicyBytes ([IO.File]::ReadAllBytes($sourcePath)) $original "$Name invalid check changed source bytes."
    } else {
        Assert-PolicyBytes ([IO.File]::ReadAllBytes($sourcePath)) $expected "$Name patch output was not exact CRLF bytes."
    }
    return $Name
}
function New-PolicyZip([object[]]$Entries) {
    $stream = [IO.MemoryStream]::new()
    $zip = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create, $true)
    try {
        foreach ($spec in $Entries) {
            $entry = $zip.CreateEntry($spec.Name)
            if ($spec.ContainsKey('Attributes')) { $entry.ExternalAttributes = $spec.Attributes }
            if (-not $spec.Name.EndsWith('/')) {
                $content = [Text.Encoding]::UTF8.GetBytes('pinned source fixture')
                $output = $entry.Open()
                try { $output.Write($content, 0, $content.Length) } finally { $output.Dispose() }
            }
        }
    } finally { $zip.Dispose() }
    $stream.Position = 0
    return $stream
}

$tempParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\', '/')
$scratch = Join-Path $tempParent ('scribe-vulkan-policy-tests-' + [Guid]::NewGuid().ToString('N'))
Assert-ScribeGpuWorkerNoReparse $scratch
if (Test-Path -LiteralPath $scratch) { throw 'Expected fresh test directory.' }
[IO.Directory]::CreateDirectory($scratch) | Out-Null
try {
    $functionFixture = "VkResult get_library_path_of_dl_handle(void) {`n    return 0;`n}"
    Assert-Policy ((Get-LoaderModulePathFunction $functionFixture) -ceq $functionFixture) 'Module-path extraction changed the implementation.'
    Assert-PolicyRejected { Get-LoaderModulePathFunction '' } 'Missing module-path implementation was accepted.'
    Assert-PolicyRejected { Get-LoaderModulePathFunction ($functionFixture + "`n" + $functionFixture) } 'Ambiguous module-path implementations were accepted.'
    Assert-PolicyRejected { Get-LoaderModulePathFunction $functionFixture.TrimEnd('}') } 'Unterminated module-path implementation was accepted.'
    Assert-PolicyRejected { Get-LoaderModulePathFunction ('x' * 33554433) } 'Oversized module-path source was accepted.'
    Assert-PolicyRejected { Get-LoaderModulePathFunction $functionFixture.Replace('return 0;', ('x' * 16384)) } 'Oversized module-path function was accepted.'
    $inputPath = Join-Path $scratch 'input.txt'
    [IO.File]::WriteAllText($inputPath, 'authenticated build input', [Text.UTF8Encoding]::new($false))
    $hash = (Get-FileHash -LiteralPath $inputPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $size = (Get-Item -LiteralPath $inputPath).Length
    $inputLease = Open-LoaderInput $inputPath $hash $size
    try {
        Assert-Policy ($inputLease.Position -eq 0 -and $inputLease.Length -eq $size) 'Validated input must be rewound.'
        Assert-PolicyRejected { [IO.File]::WriteAllText($inputPath, 'changed') } 'Pinned input allowed a concurrent write.'
        Assert-PolicyRejected { [IO.File]::Delete($inputPath) } 'Pinned input allowed deletion.'
    } finally { $inputLease.Dispose() }
    Assert-PolicyRejected { Open-LoaderInput $inputPath ('0' * 64) $size } 'Wrong input hash was accepted.'
    Assert-PolicyRejected { Open-LoaderInput $inputPath $hash ($size + 1) } 'Wrong input size was accepted.'
    Assert-PolicyRejected { Open-LoaderInput $inputPath $hash.ToUpperInvariant() $size } 'Noncanonical digest was accepted.'
    Assert-PolicyRejected { Open-LoaderInput $scratch $hash } 'Directory input was accepted.'
    Assert-PolicyRejected { Open-LoaderInput (Join-Path $scratch 'missing') $hash } 'Missing input was accepted.'

    $accepted = New-PolicyZip @(@{Name='source/'}, @{Name='source/loader/'}, @{Name='source/loader/source.c'})
    $acceptedRoot = Join-Path $scratch 'accepted'
    [IO.Directory]::CreateDirectory($acceptedRoot) | Out-Null
    try { Expand-LoaderSource $accepted 'source' $acceptedRoot } finally { $accepted.Dispose() }
    Assert-Policy ((Get-Content -LiteralPath (Join-Path $acceptedRoot 'source\loader\source.c') -Raw) -ceq 'pinned source fixture') 'Valid nested source did not extract.'

    $rejections = @(
        @(@{Name='other/source.c'}),
        @(@{Name='source/../escaped.c'}),
        @(@{Name='source//'}),
        @(@{Name='source/a//'}),
        @(@{Name='source/source.c:stream'}),
        @(@{Name='source\source.c'}),
        @(@{Name='source/file.'}),
        @(@{Name='source/file '}),
        @(@{Name='source/NUL.txt'}),
        @(@{Name='source/CONIN$'}),
        @(@{Name='source/CONOUT$'}),
        @(@{Name='source/COM¹.txt'}),
        @(@{Name='source/LPT³'}),
        @(@{Name='source/a?c'}),
        @(@{Name='source/a*c'}),
        @(@{Name='source/a|c'}),
        @(@{Name='source/a<c'}),
        @(@{Name='source/a>c'}),
        @(@{Name='source/a"c'}),
        @(@{Name='source/A.c'}, @{Name='source/a.c'}),
        @(@{Name='source/a.c'}, @{Name='source/a.c'}),
        @(@{Name='source/a'}, @{Name='source/a/b.c'}),
        @(@{Name='source/a/b.c'}, @{Name='source/a'}),
        @(@{Name='source/A/b.c'}, @{Name='source/a/c.c'}),
        @(@{Name='source/a/b.c'}, @{Name='source/A/'}),
        @(@{Name='source/a/'; Attributes=-2147483648}),
        @(@{Name='source/a'; Attributes=1073741824}),
        @(@{Name='source/link'; Attributes=-1610612736}),
        @(@{Name='source/socket'; Attributes=-1073741824})
    )
    $case = 0
    foreach ($entries in $rejections) {
        $case++
        $zip = New-PolicyZip $entries
        $destination = Join-Path $scratch "rejected-$case"
        [IO.Directory]::CreateDirectory($destination) | Out-Null
        try {
            Assert-PolicyRejected { Expand-LoaderSource $zip 'source' $destination } "Unsafe source archive $case was accepted."
            Assert-Policy (@(Get-ChildItem -LiteralPath $destination -Force).Count -eq 0) 'Rejected archive produced partial output.'
        } finally { $zip.Dispose() }
    }

    $policyRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'native\vulkan-policy-loader'
    $manifest = Get-Content -LiteralPath (Join-Path $policyRoot 'source-manifest.json') -Raw | ConvertFrom-Json
    $patch = Join-Path $policyRoot 'no-layers-or-settings.patch'
    Assert-Policy ((Get-FileHash -LiteralPath $patch -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $manifest.patch.sha256) 'Checked-in policy patch digest drifted.'
    Assert-Policy (@($manifest.patch.patched_files).Count -eq 3) 'Expected all three policy source bindings.'
    Assert-PolicyRejected { Assert-LoaderProperties ([pscustomobject]@{known=1;unknown=2}) @('known') } 'Unknown manifest field was accepted.'
    Assert-PolicyRejected { Assert-LoaderProperties ([pscustomobject]@{}) @('required') } 'Missing manifest field was accepted.'
    $cleanEnvironment = @{Path='trusted tool path'; SystemRoot='trusted system path'; CMAKE_TOOLCHAIN_FILE=''}
    Assert-NoLoaderBuildOverrides $cleanEnvironment
    Assert-Policy ($cleanEnvironment.Count -eq 3) 'Environment validation mutated accepted input.'
    foreach ($name in @('CMAKE_TOOLCHAIN_FILE', 'CMAKE_C_COMPILER_LAUNCHER', 'CMAKE_CXX_COMPILER_LAUNCHER',
        'CMAKE_C_LINKER_LAUNCHER', 'CMAKE_CXX_LINKER_LAUNCHER', 'CMAKE_PROJECT_TOP_LEVEL_INCLUDES',
        'cmake_prefix_path', 'CTEST_LAUNCH_COMMAND', 'ASMFLAGS', 'ASM_MASMFLAGS', 'RCFLAGS',
        'LDFLAGS', '_LINK_', 'MAKEFLAGS', 'MFLAGS', 'NMAKEFLAGS')) {
        $hostileEnvironment = @{$name='untrusted value'; Path='trusted tool path'}
        Assert-PolicyRejected { Assert-NoLoaderBuildOverrides $hostileEnvironment } "Ambient override $name was accepted."
        Assert-Policy ($hostileEnvironment.Count -eq 2 -and $hostileEnvironment[$name] -ceq 'untrusted value') 'Environment rejection mutated caller input.'
    }
    $git = (Get-Command git.exe -CommandType Application | Select-Object -First 1).Source
    $patchCases = @(
        (Invoke-PolicyGitEnvironmentFailureCase),
        (Invoke-PolicyLoaderPatchCase 'absent-autocrlf-global-system-disabled' $git $scratch @{} $false),
        (Invoke-PolicyLoaderPatchCase 'inherited-false-core-eol-lf' $git $scratch @{ 'core.autocrlf' = 'false'; 'core.eol' = 'lf' } $false),
        (Invoke-PolicyWithHostileGitOverrides $scratch { Invoke-PolicyLoaderPatchCase 'inherited-true' $git $scratch @{ 'core.autocrlf' = 'true' } $false }),
        (Invoke-PolicyLoaderPatchCase 'invalid-patch-preserves-original-bytes' $git $scratch @{ 'core.autocrlf' = 'true' } $true)
    )
    foreach ($name in @('action-failure-restores-caller-git-environment',
        'absent-autocrlf-global-system-disabled', 'inherited-false-core-eol-lf', 'inherited-true',
        'invalid-patch-preserves-original-bytes')) {
        Assert-Policy ($patchCases -ccontains $name) "Loader patch case $name did not execute."
    }
    Assert-Policy ($patchCases.Count -eq 5) 'Unexpected loader patch case inventory.'
    Assert-Policy ($script:checks -ge 119) 'Expected policy tests were not executed.'
    Write-Output "Windows Vulkan policy loader input tests passed ($script:checks checks)."
} finally {
    # Delete only this invocation's exact physical scratch tree. Do not follow
    # any reparse entry or clean another test/build's files.
    if (Test-Path -LiteralPath $scratch) {
        $current = (Get-ScribeGpuWorkerPhysicalDirectory $scratch 'Owned Vulkan test scratch').FullName
        if ($current -cne $scratch -or (Split-Path -Parent $current) -cne $tempParent) {
            throw 'Test cleanup target changed.'
        }
        Assert-ScribeGpuWorkerNoReparseDescendants $current
        Remove-Item -LiteralPath $current -Recurse
    }
}
