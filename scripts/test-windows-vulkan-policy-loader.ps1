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
# Exercise the actual pure/input helpers without executing the native builder.
$loaderPatchDefinition = $null
foreach ($name in @('Assert-LoaderProperties', 'Open-LoaderInput', 'Expand-LoaderSource', 'Assert-NoLoaderBuildOverrides',
    'Invoke-LoaderNative', 'Invoke-LoaderPatch')) {
    $definitions = @($ast.FindAll({ param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq $name
    }, $false))
    if ($definitions.Count -ne 1) { throw "Expected exactly one tested helper: $name" }
    if ($name -ceq 'Invoke-LoaderPatch') { $loaderPatchDefinition = $definitions[0] }
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
    $patchCalls = @($loaderPatchDefinition.Body.FindAll({ param($node)
        $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -ceq 'Invoke-LoaderNative'
    }, $true))
    $normalizedPatchCalls = @($patchCalls | ForEach-Object { ($_.Extent.Text -replace '\s+', ' ').Trim() })
    Assert-Policy ($normalizedPatchCalls.Count -eq 2) 'Expected checked and applied Vulkan policy patch calls.'
    Assert-Policy ($normalizedPatchCalls[0] -ceq "Invoke-LoaderNative `$Git @('-C', `$Loader, '-c', 'core.autocrlf=true', 'apply', '--check', `$PatchPath)") 'Vulkan policy patch check must pin CRLF checkout conversion.'
    Assert-Policy ($normalizedPatchCalls[1] -ceq "Invoke-LoaderNative `$Git @('-C', `$Loader, '-c', 'core.autocrlf=true', 'apply', `$PatchPath)") 'Vulkan policy patch application must pin CRLF checkout conversion.'
    $originalLoaderNative = ${function:Invoke-LoaderNative}
    $script:loaderPatchSpyCalls = [Collections.Generic.List[object]]::new()
    $script:loaderPatchSpyRejectCheck = $false
    function Invoke-LoaderNative([string]$Executable, [string[]]$Arguments) {
        $script:loaderPatchSpyCalls.Add([pscustomobject]@{
            Executable = $Executable
            Arguments = @($Arguments)
        }) | Out-Null
        if ($script:loaderPatchSpyRejectCheck -and $Arguments -contains '--check') {
            throw 'Synthetic Vulkan policy patch check failure.'
        }
    }
    try {
        Invoke-LoaderPatch 'git.exe' 'loader-root' 'policy.patch'
        Assert-Policy ($script:loaderPatchSpyCalls.Count -eq 2) 'Vulkan policy patch helper did not make both native calls.'
        Assert-Policy (($script:loaderPatchSpyCalls[0].Arguments -join '|') -ceq '-C|loader-root|-c|core.autocrlf=true|apply|--check|policy.patch') 'Vulkan policy patch check arguments drifted.'
        Assert-Policy (($script:loaderPatchSpyCalls[1].Arguments -join '|') -ceq '-C|loader-root|-c|core.autocrlf=true|apply|policy.patch') 'Vulkan policy patch application arguments drifted.'
        $script:loaderPatchSpyCalls.Clear()
        $script:loaderPatchSpyRejectCheck = $true
        Assert-PolicyRejected { Invoke-LoaderPatch 'git.exe' 'loader-root' 'policy.patch' } 'Vulkan policy patch helper ignored a failed check.'
        Assert-Policy ($script:loaderPatchSpyCalls.Count -eq 1) 'Vulkan policy patch helper applied after a failed check.'
    } finally {
        Set-Item -LiteralPath Function:Invoke-LoaderNative -Value $originalLoaderNative
        Remove-Variable -Name loaderPatchSpyCalls -Scope Script
        Remove-Variable -Name loaderPatchSpyRejectCheck -Scope Script
    }

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

    $git = (Get-Command git.exe -CommandType Application | Select-Object -First 1).Source
    $expectedPatchedBytes = [Text.Encoding]::ASCII.GetBytes("patched`r`n")
    $expectedPatchedHex = [Convert]::ToHexString($expectedPatchedBytes)
    $expectedPatchedHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($expectedPatchedBytes)).ToLowerInvariant()
    $gitConfigEnvironment = @{}
    foreach ($name in @('GIT_CONFIG_NOSYSTEM', 'GIT_CONFIG_GLOBAL', 'GIT_CONFIG_COUNT', 'GIT_CONFIG_KEY_0', 'GIT_CONFIG_VALUE_0')) {
        $gitConfigEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
    }
    try {
        foreach ($autocrlf in @('false', 'input', 'true')) {
            $caseRoot = Join-Path $scratch "patch-line-endings-$autocrlf"
            [IO.Directory]::CreateDirectory($caseRoot) | Out-Null
            $sourcePath = Join-Path $caseRoot 'line-endings.c'
            $patchPath = Join-Path $caseRoot 'line-endings.patch'
            [IO.File]::WriteAllBytes($sourcePath, [Text.Encoding]::ASCII.GetBytes("original`r`n"))
            [IO.File]::WriteAllText($patchPath, (@(
                'diff --git a/line-endings.c b/line-endings.c'
                '--- a/line-endings.c'
                '+++ b/line-endings.c'
                '@@ -1 +1 @@'
                '-original'
                '+patched'
                ''
            ) -join "`n"), [Text.UTF8Encoding]::new($false))
            # These environment entries emulate conflicting caller command-scope
            # configuration without reading or modifying personal Git settings.
            [Environment]::SetEnvironmentVariable('GIT_CONFIG_NOSYSTEM', '1', 'Process')
            [Environment]::SetEnvironmentVariable('GIT_CONFIG_GLOBAL', (Join-Path $scratch 'absent-global.gitconfig'), 'Process')
            [Environment]::SetEnvironmentVariable('GIT_CONFIG_COUNT', '1', 'Process')
            [Environment]::SetEnvironmentVariable('GIT_CONFIG_KEY_0', 'core.autocrlf', 'Process')
            [Environment]::SetEnvironmentVariable('GIT_CONFIG_VALUE_0', $autocrlf, 'Process')
            Invoke-LoaderPatch $git $caseRoot $patchPath
            Assert-Policy ([Environment]::GetEnvironmentVariable('GIT_CONFIG_NOSYSTEM', 'Process') -ceq '1') "Pinned patch helper changed GIT_CONFIG_NOSYSTEM for core.autocrlf=$autocrlf."
            Assert-Policy ([Environment]::GetEnvironmentVariable('GIT_CONFIG_GLOBAL', 'Process') -ceq (Join-Path $scratch 'absent-global.gitconfig')) "Pinned patch helper changed GIT_CONFIG_GLOBAL for core.autocrlf=$autocrlf."
            Assert-Policy ([Environment]::GetEnvironmentVariable('GIT_CONFIG_COUNT', 'Process') -ceq '1') "Pinned patch helper changed GIT_CONFIG_COUNT for core.autocrlf=$autocrlf."
            Assert-Policy ([Environment]::GetEnvironmentVariable('GIT_CONFIG_KEY_0', 'Process') -ceq 'core.autocrlf') "Pinned patch helper changed GIT_CONFIG_KEY_0 for core.autocrlf=$autocrlf."
            Assert-Policy ([Environment]::GetEnvironmentVariable('GIT_CONFIG_VALUE_0', 'Process') -ceq $autocrlf) "Pinned patch helper changed GIT_CONFIG_VALUE_0 for core.autocrlf=$autocrlf."
            $actualPatchedBytes = [IO.File]::ReadAllBytes($sourcePath)
            $actualPatchedHash = (Get-FileHash -LiteralPath $sourcePath -Algorithm SHA256).Hash.ToLowerInvariant()
            Assert-Policy ([Convert]::ToHexString($actualPatchedBytes) -ceq $expectedPatchedHex) "Pinned patch conversion did not produce CRLF bytes for core.autocrlf=$autocrlf."
            Assert-Policy ($actualPatchedHash -ceq $expectedPatchedHash) "Pinned patch conversion did not produce the expected CRLF hash for core.autocrlf=$autocrlf."
        }

        $rejectedRoot = Join-Path $scratch 'patch-check-rejected'
        [IO.Directory]::CreateDirectory($rejectedRoot) | Out-Null
        $rejectedSource = Join-Path $rejectedRoot 'line-endings.c'
        $rejectedPatch = Join-Path $rejectedRoot 'line-endings.patch'
        $unchangedBytes = [Text.Encoding]::ASCII.GetBytes("original`r`n")
        [IO.File]::WriteAllBytes($rejectedSource, $unchangedBytes)
        $unchangedHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($unchangedBytes)).ToLowerInvariant()
        [IO.File]::WriteAllText($rejectedPatch, (@(
            'diff --git a/line-endings.c b/line-endings.c'
            '--- a/line-endings.c'
            '+++ b/line-endings.c'
            '@@ -1 +1 @@'
            '-missing'
            '+patched'
            ''
        ) -join "`n"), [Text.UTF8Encoding]::new($false))
        Assert-PolicyRejected { Invoke-LoaderPatch $git $rejectedRoot $rejectedPatch } 'A failed Vulkan policy patch check applied the patch.'
        Assert-Policy ((Get-FileHash -LiteralPath $rejectedSource -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $unchangedHash) 'A failed Vulkan policy patch check changed the source hash.'
        Assert-Policy ([Convert]::ToHexString([IO.File]::ReadAllBytes($rejectedSource)) -ceq [Convert]::ToHexString($unchangedBytes)) 'A failed Vulkan policy patch check changed the source bytes.'
    } finally {
        foreach ($entry in $gitConfigEnvironment.GetEnumerator()) {
            if ($null -eq $entry.Value) {
                Remove-Item -LiteralPath "Env:$($entry.Key)" -Force -ErrorAction SilentlyContinue
            } else {
                [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
            }
        }
    }
    foreach ($entry in $gitConfigEnvironment.GetEnumerator()) {
        Assert-Policy ([Environment]::GetEnvironmentVariable($entry.Key, 'Process') -ceq $entry.Value) "Synthetic Git patch test changed caller environment $($entry.Key)."
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
    Assert-Policy ($script:checks -ge 141) 'Expected policy tests were not executed.'
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
