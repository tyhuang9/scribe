param(
    [Parameter(Mandatory = $true)]
    [string]$ModelSource,
    [Parameter(Mandatory = $true)]
    [string]$RuntimeSource
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$releaseRoot = Join-Path $repositoryRoot "target\release"

if (-not [Environment]::Is64BitOperatingSystem -or
    [Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "The release bundle is qualified only for Windows x64."
}

& cargo build --release --all-features --manifest-path (Join-Path $repositoryRoot "Cargo.toml")
if ($LASTEXITCODE -ne 0) {
    throw "The Windows release build failed."
}

& (Join-Path $PSScriptRoot "bundle-whisper-runtime.ps1") `
    -Profile release `
    -Source $RuntimeSource `
    -Destination (Join-Path $releaseRoot "runtimes\whisper_cpp")
& (Join-Path $PSScriptRoot "bundle-base-model.ps1") `
    -Profile release `
    -Source $ModelSource `
    -Destination $releaseRoot `
    -Executable (Join-Path $releaseRoot "local-transcriber.exe")

Write-Output "Windows x64 release bundle ready at $releaseRoot"
