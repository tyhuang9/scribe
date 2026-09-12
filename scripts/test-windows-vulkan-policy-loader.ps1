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
foreach ($name in @('Assert-LoaderProperties', 'Open-LoaderInput', 'Expand-LoaderSource', 'Assert-NoLoaderBuildOverrides')) {
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
    Assert-Policy ($script:checks -ge 104) 'Expected policy tests were not executed.'
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
