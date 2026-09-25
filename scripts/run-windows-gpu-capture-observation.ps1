[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$CollectorPath,
    [Parameter(Mandatory)][ValidatePattern('\A[0-9a-f]{64}\z')][string]$CollectorSha256,
    [Parameter(Mandatory)][string]$ModelPath,
    [Parameter(Mandatory)][ValidatePattern('\A[0-9a-f]{64}\z')][string]$ModelSha256,
    [Parameter(Mandatory)][string]$WavPath,
    [Parameter(Mandatory)][ValidatePattern('\A[0-9a-f]{64}\z')][string]$WavSha256,
    [Parameter(Mandatory)][string]$GpuPackId,
    [Parameter(Mandatory)][ValidateSet('cuda', 'vulkan')][string]$GpuBackend,
    [Parameter(Mandatory)][string]$GpuDevice,
    [Parameter(Mandatory)][string]$OutputPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $IsWindows -or -not [Environment]::Is64BitProcess) {
    throw 'Capture observation requires 64-bit PowerShell on Windows.'
}
foreach ($path in @($CollectorPath, $ModelPath, $WavPath, $OutputPath)) {
    if (-not [IO.Path]::IsPathFullyQualified($path)) {
        throw 'Capture observation paths must be absolute.'
    }
}
foreach ($digest in @($CollectorSha256, $ModelSha256, $WavSha256)) {
    if ($digest -cnotmatch '\A[0-9a-f]{64}\z') {
        throw 'Capture observation digests must be lowercase SHA-256.'
    }
}
if ($GpuBackend -cnotin @('cuda', 'vulkan')) {
    throw 'Capture observation backend must be lowercase cuda or vulkan.'
}

# This wrapper runs an operator-selected, trusted collector build. Its digest
# check is not a substitute for source provenance or the collector's separate
# CPU anchor and signed GPU-pack verification. It never supplies a worker path.
$collector = Get-Item -LiteralPath $CollectorPath -Force
if ($collector.PSIsContainer -or $collector.Extension -ine '.exe' -or
    ($collector.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
    $collector.Length -le 0 -or $collector.Length -gt 512MB) {
    throw 'Collector must be a bounded regular executable in a trusted directory.'
}
$collectorPathFull = $collector.FullName
$collectorLock = [IO.File]::Open($collectorPathFull, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
try {
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $actual = [Convert]::ToHexString($sha.ComputeHash($collectorLock)).ToLowerInvariant()
    }
    finally { $sha.Dispose() }
    if ($actual -cne $CollectorSha256) { throw 'Collector digest does not match the expected build.' }

    # Pass individual arguments, never a composed shell command. Rust owns the
    # actual input/output checks, observation lifetime and no-replace publication.
    $arguments = @(
        '--scribe-windows-gpu-capture-observation',
        '--model', $ModelPath, '--model-sha256', $ModelSha256,
        '--wav', $WavPath, '--wav-sha256', $WavSha256,
        '--gpu-pack-id', $GpuPackId, '--gpu-backend', $GpuBackend,
        '--gpu-device', $GpuDevice, '--output', $OutputPath
    )
    & $collectorPathFull @arguments
    if ($LASTEXITCODE -ne 0) {
        throw 'Capture observation failed; no qualification or release approval was produced.'
    }
}
finally { $collectorLock.Dispose() }
