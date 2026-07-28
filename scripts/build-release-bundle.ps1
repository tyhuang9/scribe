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
    [hashtable]$PortableRuntimes = @{},
    [string]$WhisperGpuRuntimeDir
)

$ErrorActionPreference = 'Stop'
$PortableRuntimes = if ($null -eq $PortableRuntimes) { @{} } else { $PortableRuntimes }
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$scribeDir = Split-Path -Parent $scriptDir
$releaseDir = Join-Path $scribeDir 'target\release'
$runtimeDir = Join-Path $releaseDir 'runtimes\whisper_cpp'
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
    if ($catalog.schema_version -ne 1 -or -not $catalog.catalog_version -or @($catalog.artifacts).Count -eq 0) {
        throw 'Release runtime catalog must use schema 1 and contain at least one real artifact.'
    }
    $env:SCRIBE_RUNTIME_ARTIFACT_CATALOG = $resolvedCatalog
}
elseif ($AllowEmptyCatalog) {
    Remove-Item Env:SCRIBE_RUNTIME_ARTIFACT_CATALOG -ErrorAction SilentlyContinue
}
elseif (-not $AllowEmptyCatalog) {
    throw 'Provide -CatalogPath from package-runtime-artifact.py, or explicitly use -AllowEmptyCatalog for a CPU-only release.'
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

function Copy-PortableRuntime([string]$RuntimeId, [string]$Source, [string]$Entrypoint) {
    Assert-PortableRuntime $Source $RuntimeId 'cpu' $Entrypoint
    $destination = Join-Path $releaseDir "runtimes\$RuntimeId"
    if (Test-Path -LiteralPath $destination) {
        Remove-Item -LiteralPath $destination -Recurse -Force
    }
    New-Item -ItemType Directory -Path $destination | Out-Null
    Get-ChildItem -LiteralPath $Source -Force | Copy-Item -Destination $destination -Recurse -Force
}

cargo build --release --manifest-path (Join-Path $scribeDir 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
if ($Mode -ne 'OfflineCpu') {
    foreach ($runtimeId in @('faster_whisper', 'vosk', 'sherpa_onnx', 'moonshine', 'parakeet')) {
        $oldRuntime = Join-Path $releaseDir "runtimes\$runtimeId"
        if (Test-Path -LiteralPath $oldRuntime) {
            Remove-Item -LiteralPath $oldRuntime -Recurse -Force
        }
    }
}

if (Test-Path -LiteralPath $runtimeDir) {
    Remove-Item -LiteralPath $runtimeDir -Recurse -Force
}
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

Write-Host "Release bundle ready ($Mode): $releaseDir"
if ($Mode -eq 'Standard') {
    Write-Host 'Standard contains only bundled CPU whisper.cpp; optional runtimes need an embedded trusted catalog.'
}
