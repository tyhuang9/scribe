[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PackVersion,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [Parameter(Mandatory = $true)]
    [string]$ProductionPrivateKeyPath,
    [Parameter(Mandatory = $true)]
    [string]$ProductionKeyId,
    [string]$NativeArchiveDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($env:OS -ne 'Windows_NT' -or
    -not [System.Environment]::Is64BitOperatingSystem -or
    -not [System.Environment]::Is64BitProcess) {
    throw 'Production GPU worker-pack preparation requires Windows x64.'
}
if ([string]::IsNullOrWhiteSpace($ProductionPrivateKeyPath) -or
    [string]::IsNullOrWhiteSpace($ProductionKeyId)) {
    throw 'GPU worker-pack publication requires an external private key and reviewed key ID.'
}

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory).TrimEnd([char[]]@('\', '/'))
if (Test-Path -LiteralPath $outputRoot) {
    throw "GPU worker-pack publication output already exists: $outputRoot"
}
$outputParent = Split-Path -Parent $outputRoot
if (-not (Test-Path -LiteralPath $outputParent -PathType Container)) {
    New-Item -ItemType Directory -Path $outputParent -Force | Out-Null
}
New-Item -ItemType Directory -Path $outputRoot | Out-Null

$buildScript = Join-Path $PSScriptRoot 'build-windows-gpu-worker-pack.ps1'
$shared = @{
    PackVersion = $PackVersion
    SigningMode = 'Production'
    ProductionPrivateKeyPath = $ProductionPrivateKeyPath
    ProductionKeyId = $ProductionKeyId
}
if (-not [string]::IsNullOrWhiteSpace($NativeArchiveDirectory)) {
    $shared.NativeArchiveDirectory = $NativeArchiveDirectory
}

$vulkanRoot = Join-Path $outputRoot 'vulkan'
$cudaRoot = Join-Path $outputRoot 'cuda'
$vulkan = & $buildScript @shared `
    -Backend Vulkan `
    -OutputDirectory $vulkanRoot `
    -CargoTargetDirectory (Join-Path $repositoryRoot 'target-gpu-pack-release-vulkan')
$cuda = & $buildScript @shared `
    -Backend Cuda `
    -OutputDirectory $cudaRoot `
    -CargoTargetDirectory (Join-Path $repositoryRoot 'target-gpu-pack-release-cuda')

if ($vulkan.PackRoot -cne $vulkanRoot -or $cuda.PackRoot -cne $cudaRoot) {
    throw 'GPU worker-pack builders returned an unexpected output root.'
}
[pscustomobject]@{
    SchemaVersion = 1
    PackVersion = $PackVersion
    PackRoots = @($cudaRoot, $vulkanRoot)
    Packs = @($cuda, $vulkan)
}
