[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('pull_request', 'push', 'workflow_dispatch')]
    [string]$EventName,
    [Parameter(Mandatory = $true)]
    [string]$Ref,
    [Parameter(Mandatory = $true)]
    [AllowEmptyString()]
    [string]$Policy,
    [switch]$RequestedGpuPacks,
    [switch]$PublishRelease,
    [string]$GitHubOutputPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($Ref.Length -gt 1024 -or -not $Ref.StartsWith('refs/', [StringComparison]::Ordinal)) {
    throw 'GitHub release-policy ref must be a bounded canonical refs/ value.'
}

$isTagRelease = $EventName -ceq 'push' -and $Ref.StartsWith('refs/tags/', [StringComparison]::Ordinal)
$isManualRelease = $EventName -ceq 'workflow_dispatch' -and $PublishRelease.IsPresent
$isOfficialRelease = $isTagRelease -or $isManualRelease
$includeGpuPacks = $RequestedGpuPacks.IsPresent
$resolvedPolicy = if ([string]::IsNullOrEmpty($Policy)) { 'unconfigured' } else { $Policy }

if ($isOfficialRelease) {
    switch ($Policy) {
        'temporary_cpu_only_stage4' {
            if ($RequestedGpuPacks.IsPresent) {
                throw 'The temporary_cpu_only_stage4 official-release policy forbids GPU pack inclusion; change the reviewed repository policy to gpu_packs_required after production trust is provisioned.'
            }
            $includeGpuPacks = $false
        }
        'gpu_packs_required' {
            $includeGpuPacks = $true
        }
        default {
            throw 'Official Windows releases require SCRIBE_GPU_PACK_RELEASE_POLICY to be exactly temporary_cpu_only_stage4 or gpu_packs_required.'
        }
    }
} elseif ($Policy -notin @('', 'temporary_cpu_only_stage4', 'gpu_packs_required')) {
    throw 'SCRIBE_GPU_PACK_RELEASE_POLICY has an unsupported value.'
}

$result = [ordered]@{
    official_release = $isOfficialRelease
    release_policy = $resolvedPolicy
    include_gpu_worker_packs = $includeGpuPacks
}

if (-not [string]::IsNullOrWhiteSpace($GitHubOutputPath)) {
    $outputPath = [System.IO.Path]::GetFullPath($GitHubOutputPath)
    $outputItem = Get-Item -LiteralPath $outputPath -Force
    if ($outputItem.PSIsContainer -or
        ($outputItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $outputItem.Length -gt 1MB) {
        throw 'GitHub output file must be a bounded regular non-reparse file.'
    }
    Add-Content -LiteralPath $outputPath -Encoding utf8NoBOM -Value @(
        "official_release=$($isOfficialRelease.ToString().ToLowerInvariant())",
        "release_policy=$resolvedPolicy",
        "include_gpu_worker_packs=$($includeGpuPacks.ToString().ToLowerInvariant())"
    )
}

if ($isOfficialRelease -and $Policy -ceq 'temporary_cpu_only_stage4') {
    Write-Warning 'Official release is using the explicit temporary Stage 4 CPU-only policy. Provision reviewed production trust before changing the repository policy to gpu_packs_required.'
}

[pscustomobject]$result
