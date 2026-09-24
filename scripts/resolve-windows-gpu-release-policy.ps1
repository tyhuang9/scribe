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
    [string]$Repository = '',
    [string]$SigningRunId = '',
    [string]$SigningRunAttempt = '',
    [string]$SignedArtifactId = '',
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
$includeGpuPacks = $false
$signedInputValues = @($SigningRunId, $SigningRunAttempt, $SignedArtifactId)
$suppliedSignedInputs = @($signedInputValues | Where-Object { -not [string]::IsNullOrEmpty($_) })
$requestSignedPacks = $suppliedSignedInputs.Count -gt 0
$resolvedPolicy = if ([string]::IsNullOrEmpty($Policy)) { 'unconfigured' } else { $Policy }

if ($RequestedGpuPacks.IsPresent) {
    throw 'This candidate-ref workflow never receives GPU pack signing authority. Production GPU packs require a separately protected trusted signing workflow over fixed verified unsigned artifacts.'
}

if ($requestSignedPacks) {
    if ($suppliedSignedInputs.Count -ne 3 -or
        @($signedInputValues | Where-Object { $_ -cnotmatch '\A[1-9][0-9]{0,19}\z' }).Count -ne 0) {
        throw 'Signed GPU inputs require all three canonical signing run, attempt, and artifact IDs.'
    }
    if ($EventName -cne 'workflow_dispatch' -or $Ref -cne 'refs/heads/main' -or
        $Repository -cne 'tyhuang9/scribe' -or $Policy -cne 'gpu_packs_required') {
        throw 'Signed GPU inputs require the fixed repository default-branch manual workflow and gpu_packs_required policy.'
    }
}

if ($isOfficialRelease) {
    switch ($Policy) {
        'temporary_cpu_only_stage4' {
            $includeGpuPacks = $false
        }
        'gpu_packs_required' {
            if (-not $requestSignedPacks) {
                throw 'The gpu_packs_required policy is not provisioned in this candidate-ref workflow without exact signed inputs. Use the fixed default-branch manual workflow with a completed protected signing artifact.'
            }
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
    signed_gpu_inputs_requested = $requestSignedPacks
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
        "signed_gpu_inputs_requested=$($requestSignedPacks.ToString().ToLowerInvariant())"
    )
}

if ($isOfficialRelease -and $Policy -ceq 'temporary_cpu_only_stage4') {
    Write-Warning 'Official release is using the explicit temporary Stage 4 CPU-only policy. Provision a separately protected trusted signing workflow before changing the repository policy to gpu_packs_required.'
}

[pscustomobject]$result
