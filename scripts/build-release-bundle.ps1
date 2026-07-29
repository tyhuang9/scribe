[CmdletBinding()]
param(
    [ValidateSet('Standard', 'OfflineCpu', 'Gpu')]
    [string]$Mode = 'Standard',
    [Parameter(Mandatory)]
    [string]$WhisperBuildDir,
    [Parameter(Mandatory)]
    [string]$WhisperVersion,
    [Parameter(Mandatory)]
    [string]$WhisperSourceCommit,
    [string]$CatalogPath,
    [switch]$AllowEmptyCatalog,
    [switch]$VoiceAi,
    [hashtable]$PortableRuntimes = @{},
    [string]$WhisperGpuRuntimeDir
)

$ErrorActionPreference = 'Stop'
$PortableRuntimes = if ($null -eq $PortableRuntimes) { @{} } else { $PortableRuntimes }
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$scribeDir = Split-Path -Parent $scriptDir
$releaseDir = Join-Path $scribeDir 'target\release'
$runtimesDir = Join-Path $releaseDir 'runtimes'
$runtimeDir = Join-Path $runtimesDir 'whisper_cpp'
$runtimeBin = Join-Path $runtimeDir 'bin'
$platformArchitecture = switch ([string][System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) {
    'X64' { 'x86_64' }
    'Arm64' { 'aarch64' }
    default { throw 'Unsupported or unavailable Windows release architecture.' }
}

if ($WhisperSourceCommit -notmatch '^[0-9a-f]{40,64}$') {
    throw 'WhisperSourceCommit must be a lowercase 40-64 character commit digest.'
}
if ($WhisperVersion -notmatch '^[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$') {
    throw 'WhisperVersion is not a safe immutable version identifier.'
}
if ($CatalogPath) {
    $resolvedCatalog = (Resolve-Path -LiteralPath $CatalogPath).Path
    $catalog = Get-Content -LiteralPath $resolvedCatalog -Raw | ConvertFrom-Json
    if (@(1, 2) -notcontains $catalog.schema_version -or -not $catalog.catalog_version -or @($catalog.artifacts).Count -eq 0) {
        throw 'Release runtime catalog must use schema 1 or 2 and contain at least one real artifact.'
    }
    $env:SCRIBE_RUNTIME_ARTIFACT_CATALOG = $resolvedCatalog
}
elseif ($AllowEmptyCatalog) {
    Remove-Item Env:SCRIBE_RUNTIME_ARTIFACT_CATALOG -ErrorAction SilentlyContinue
}
elseif (-not $AllowEmptyCatalog) {
    throw 'Provide -CatalogPath from package-runtime-artifact.py, or explicitly use -AllowEmptyCatalog for a CPU-only release.'
}

if ($VoiceAi) {
    if (-not $CatalogPath) {
        throw 'VoiceAi releases require a schema-2 catalog with the exact official pinned llama runtime and both official revision-pinned Qwen tiers.'
    }
    $env:SCRIBE_REQUIRE_VOICE_INTENT_ARTIFACTS = '1'
}
else {
    Remove-Item Env:SCRIBE_REQUIRE_VOICE_INTENT_ARTIFACTS -ErrorAction SilentlyContinue
}

$whisperBin = Join-Path $WhisperBuildDir 'bin'
$whisperCli = Join-Path $whisperBin 'whisper-cli.exe'
if (-not (Test-Path -LiteralPath $whisperCli -PathType Leaf)) {
    throw "Provided whisper build does not contain bin\whisper-cli.exe: $WhisperBuildDir"
}
if ($Mode -eq 'OfflineCpu') {
    foreach ($runtimeId in @('faster_whisper', 'vosk', 'sherpa_onnx', 'moonshine', 'parakeet')) {
        if (-not $PortableRuntimes.ContainsKey($runtimeId)) {
            throw "OfflineCpu releases require a platform-CI portable runtime for $runtimeId in -PortableRuntimes."
        }
    }
}

function Assert-PortableRuntime([string]$Path, [string]$RuntimeId, [string]$Device, [string]$Entrypoint) {
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "Portable runtime input does not exist: $Path"
    }
    $unsafe = Get-ChildItem -LiteralPath $Path -Recurse -Force | Where-Object {
        ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $_.Name -ieq 'pyvenv.cfg'
    } | Select-Object -First 1
    if ($unsafe) {
        throw "Portable runtime input contains a link/reparse point or raw Python venv file: $($unsafe.FullName)"
    }
    $manifestPath = Join-Path $Path 'runtime-manifest.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "Portable runtime is missing runtime-manifest.json: $Path"
    }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($manifest.manifest_version -ne 1 -or $manifest.runtime_id -ne $RuntimeId -or
        $manifest.platform -ne "windows-$platformArchitecture" -or $manifest.device -ne $Device -or
        $manifest.entrypoint -ne $Entrypoint -or $manifest.portable -ne $true -or
        -not $manifest.version) {
        throw "Portable runtime manifest does not match $RuntimeId/windows-$platformArchitecture/$Device/$Entrypoint."
    }
    if (-not (Test-Path -LiteralPath (Join-Path $Path $Entrypoint) -PathType Leaf)) {
        throw "Portable runtime entrypoint is missing: $Entrypoint"
    }
}

function Assert-ExactRuntimeSet([string[]]$ExpectedRuntimeIds) {
    if (-not (Test-Path -LiteralPath $runtimesDir -PathType Container)) {
        throw "Release runtime directory is missing: $runtimesDir"
    }
    $entries = @(Get-ChildItem -LiteralPath $runtimesDir -Force)
    $unexpected = @($entries | Where-Object {
        -not $_.PSIsContainer -or
        ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        $ExpectedRuntimeIds -cnotcontains $_.Name
    })
    $missing = @($ExpectedRuntimeIds | Where-Object {
        $expectedPath = Join-Path $runtimesDir $_
        -not (Test-Path -LiteralPath $expectedPath -PathType Container) -or
        ((Get-Item -LiteralPath $expectedPath -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)
    })
    if ($unexpected.Count -gt 0 -or $missing.Count -gt 0) {
        $unexpectedNames = ($unexpected | ForEach-Object Name) -join ', '
        $missingNames = $missing -join ', '
        throw "Release runtime set is not exact. Unexpected: [$unexpectedNames]. Missing: [$missingNames]."
    }
}

function Copy-PortableRuntime([string]$RuntimeId, [string]$Source, [string]$Entrypoint) {
    Assert-PortableRuntime $Source $RuntimeId 'cpu' $Entrypoint
    $destination = Join-Path $releaseDir "runtimes\$RuntimeId"
    if (Test-Path -LiteralPath $destination) {
        Remove-Item -LiteralPath $destination -Recurse -Force
    }
    New-Item -ItemType Directory -Path $destination | Out-Null
    Get-ChildItem -LiteralPath $Source -Force | Copy-Item -Destination $destination -Recurse -Force
}

$releaseDirFull = [IO.Path]::GetFullPath($releaseDir).TrimEnd([IO.Path]::DirectorySeparatorChar)
$runtimesDirFull = [IO.Path]::GetFullPath($runtimesDir).TrimEnd([IO.Path]::DirectorySeparatorChar)
if ([IO.Path]::GetDirectoryName($runtimesDirFull) -cne $releaseDirFull -or
    [IO.Path]::GetFileName($runtimesDirFull) -cne 'runtimes') {
    throw "Refusing to reset unexpected release runtime path: $runtimesDirFull"
}
foreach ($ancestor in @((Join-Path $scribeDir 'target'), $releaseDirFull)) {
    if (Test-Path -LiteralPath $ancestor) {
        $ancestorItem = Get-Item -LiteralPath $ancestor -Force
        if (-not $ancestorItem.PSIsContainer -or
            ($ancestorItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Refusing to reset release runtimes through unsafe ancestor: $ancestor"
        }
    }
}
if (Test-Path -LiteralPath $runtimesDirFull) {
    $staleRuntimes = Get-Item -LiteralPath $runtimesDirFull -Force
    if ($staleRuntimes.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        Remove-Item -LiteralPath $runtimesDirFull -Force
    }
    elseif ($staleRuntimes.PSIsContainer) {
        Remove-Item -LiteralPath $runtimesDirFull -Recurse -Force
    }
    else {
        Remove-Item -LiteralPath $runtimesDirFull -Force
    }
}
cargo build --release --manifest-path (Join-Path $scribeDir 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
New-Item -ItemType Directory -Path $runtimesDirFull | Out-Null

New-Item -ItemType Directory -Path $runtimeDir | Out-Null
New-Item -ItemType Directory -Path $runtimeBin | Out-Null
Copy-Item -LiteralPath $whisperCli -Destination $runtimeBin
Get-ChildItem -LiteralPath $whisperBin -File -Filter '*.dll' | Copy-Item -Destination $runtimeBin

$device = 'cpu'
if ($Mode -eq 'Gpu') {
    if (-not $WhisperGpuRuntimeDir) {
        throw 'GPU releases require -WhisperGpuRuntimeDir with release-supplied CUDA DLLs.'
    }
    if (-not (Test-Path -LiteralPath $WhisperGpuRuntimeDir -PathType Container)) {
        throw "GPU runtime input does not exist: $WhisperGpuRuntimeDir"
    }
    $gpuLink = Get-ChildItem -LiteralPath $WhisperGpuRuntimeDir -Recurse -Force | Where-Object {
        $_.Attributes -band [IO.FileAttributes]::ReparsePoint
    } | Select-Object -First 1
    if ($gpuLink) { throw "GPU runtime input contains a reparse point: $($gpuLink.FullName)" }
    $gpuDir = Join-Path $runtimeDir 'cuda'
    New-Item -ItemType Directory -Path $gpuDir | Out-Null
    Get-ChildItem -LiteralPath $WhisperGpuRuntimeDir -File -Filter '*.dll' | Copy-Item -Destination $gpuDir
    if (-not (Get-ChildItem -LiteralPath $gpuDir -File -Filter '*.dll')) {
        throw 'GPU runtime input did not contain any DLLs.'
    }
    $device = 'gpu'
}

$manifestPath = Join-Path $runtimeDir 'runtime-manifest.json'
$manifestJson = @{
    manifest_version = 1
    runtime_id = 'whisper_cpp'
    version = $WhisperVersion
    source_commit = $WhisperSourceCommit
    whisper_cli = 'bin/whisper-cli.exe'
    platform = "windows-$platformArchitecture"
    device = $device
    entrypoint = 'bin/whisper-cli.exe'
    cuda_bundled = ($device -eq 'gpu')
    portable = $true
} | ConvertTo-Json
[IO.File]::WriteAllText($manifestPath, $manifestJson, [Text.UTF8Encoding]::new($false))
$null = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json

if ($Mode -eq 'OfflineCpu') {
    foreach ($runtimeId in @('faster_whisper', 'vosk', 'sherpa_onnx', 'moonshine', 'parakeet')) {
        $entrypoint = switch ($runtimeId) {
            'faster_whisper' { 'bin/scribe-faster-whisper.exe' }
            'vosk' { 'bin/scribe-vosk.exe' }
            'sherpa_onnx' { 'bin/scribe-sherpa-onnx.exe' }
            'moonshine' { 'bin/scribe-moonshine.exe' }
            'parakeet' { 'bin/scribe-parakeet.exe' }
        }
        Copy-PortableRuntime $runtimeId $PortableRuntimes[$runtimeId] $entrypoint
    }
}
elseif ($Mode -eq 'Gpu' -and $PortableRuntimes.ContainsKey('faster_whisper')) {
    throw 'Package faster-whisper GPU as a separate verified artifact; do not mix it into the whisper GPU product.'
}

$expectedRuntimeIds = if ($Mode -eq 'OfflineCpu') {
    @('whisper_cpp', 'faster_whisper', 'vosk', 'sherpa_onnx', 'moonshine', 'parakeet')
}
else {
    @('whisper_cpp')
}
Assert-ExactRuntimeSet $expectedRuntimeIds

Write-Host "Release bundle ready ($Mode): $releaseDir"
if ($Mode -eq 'Standard') {
    Write-Host 'Standard contains only bundled CPU whisper.cpp; optional runtimes need an embedded trusted catalog.'
}
if ($VoiceAi) {
    Write-Host 'Voice AI catalog verified: exact official CPU llama runtime and both official revision-pinned Qwen sources are embedded; the standard bundle still contains only whisper.cpp.'
}
