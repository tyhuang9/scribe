[CmdletBinding()]
param(
    [string]$TargetDirectory = (Join-Path ([System.IO.Path]::GetTempPath()) 'scribe-vulkan-worker-dev')
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($env:OS -ne 'Windows_NT') {
    throw 'The Vulkan developer worker build is currently supported only on Windows.'
}
if ([string]::IsNullOrWhiteSpace($env:VULKAN_SDK) -or
    -not (Test-Path -LiteralPath (Join-Path $env:VULKAN_SDK 'Lib\vulkan-1.lib') -PathType Leaf)) {
    throw 'VULKAN_SDK must name an installed Khronos Vulkan SDK with Lib\vulkan-1.lib.'
}

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $repositoryRoot 'Cargo.toml'
$targetRoot = [System.IO.Path]::GetFullPath($TargetDirectory)
$previousTargetDirectory = $env:CARGO_TARGET_DIR
$previousWorkerDigest = $env:SCRIBE_BUNDLED_WORKER_SHA256
$previousBuildingWorker = $env:SCRIBE_BUILDING_WORKER

try {
    $env:CARGO_TARGET_DIR = $targetRoot
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $null
    $env:SCRIBE_BUILDING_WORKER = '1'
    & cargo build --locked --offline --bin scribe-inference-worker --features vulkan-acceleration --manifest-path $manifestPath
    if ($LASTEXITCODE -ne 0) {
        throw "Vulkan inference worker build failed with exit code $LASTEXITCODE."
    }

    $env:SCRIBE_BUILDING_WORKER = $null
    & cargo build --locked --offline --bin local-transcriber --features ui-harness,vulkan-acceleration --manifest-path $manifestPath
    if ($LASTEXITCODE -ne 0) {
        throw "Vulkan desktop build failed with exit code $LASTEXITCODE."
    }

    $cargoDesktop = Join-Path $targetRoot 'debug\local-transcriber.exe'
    $cargoWorker = Join-Path $targetRoot 'debug\scribe-inference-worker.exe'
    if (-not (Test-Path -LiteralPath $cargoDesktop -PathType Leaf) -or
        -not (Test-Path -LiteralPath $cargoWorker -PathType Leaf)) {
        throw 'The adjacent Vulkan developer desktop and worker outputs were not produced.'
    }

    # Cargo may hardlink a binary from debug\deps into debug\. Production
    # launch rejects hardlinks, so materialize a fresh two-file developer
    # bundle just as release staging materializes its immutable payload.
    $developerBundle = Join-Path $targetRoot ("vulkan-dev-bundle-{0}" -f [guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Path $developerBundle
    $desktop = Join-Path $developerBundle 'local-transcriber.exe'
    $worker = Join-Path $developerBundle 'scribe-inference-worker.exe'
    Copy-Item -LiteralPath $cargoDesktop -Destination $desktop
    Copy-Item -LiteralPath $cargoWorker -Destination $worker
    if (-not (Test-Path -LiteralPath $desktop -PathType Leaf) -or
        -not (Test-Path -LiteralPath $worker -PathType Leaf)) {
        throw 'The fresh adjacent Vulkan developer bundle was not produced.'
    }
    Write-Output "Vulkan developer desktop: $desktop"
    Write-Output "Vulkan developer worker:  $worker"
}
finally {
    $env:CARGO_TARGET_DIR = $previousTargetDirectory
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $previousWorkerDigest
    $env:SCRIBE_BUILDING_WORKER = $previousBuildingWorker
}
