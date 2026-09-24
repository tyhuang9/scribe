[CmdletBinding()]
param(
    [string]$BaseRevision,
    [switch]$SelfTestOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-PolicyTransition($Previous, $Current) {
    if ($null -eq $Current -or $Current.schema_version -ne 1) {
        throw 'The reviewed signing policy must exist and have schema version 1.'
    }
    $expectedIds = @('scribe-cuda-windows-x64', 'scribe-vulkan-windows-x64')
    foreach ($policy in @($Previous, $Current)) {
        if ($null -eq $policy) { continue }
        if ($policy.schema_version -ne 1 -or $policy.minimum_security_epoch -isnot [long] -and $policy.minimum_security_epoch -isnot [int]) {
            throw 'Signing policy schema or security floor is invalid.'
        }
        if ($policy.minimum_security_epoch -lt 1 -or @($policy.packs).Count -ne 2) {
            throw 'Signing policy must retain its positive security floor and complete pack pair.'
        }
        for ($index = 0; $index -lt 2; $index++) {
            $pack = $policy.packs[$index]
            if ($pack.pack_id -cne $expectedIds[$index] -or
                ($pack.security_epoch -isnot [long] -and $pack.security_epoch -isnot [int]) -or
                $pack.security_epoch -lt $policy.minimum_security_epoch) {
                throw 'Signing policy pack identity or security epoch is invalid.'
            }
        }
    }
    if ($null -eq $Previous) { return }
    if ($Current.minimum_security_epoch -lt $Previous.minimum_security_epoch) {
        throw 'Signing policy minimum security epoch must never decrease.'
    }
    for ($index = 0; $index -lt 2; $index++) {
        if ($Current.packs[$index].security_epoch -lt $Previous.packs[$index].security_epoch) {
            throw 'Signing policy pack security epochs must never decrease.'
        }
    }
}

function New-TestPolicy([long]$Floor, [long]$Cuda, [long]$Vulkan) {
    return [pscustomobject]@{
        schema_version = 1
        minimum_security_epoch = $Floor
        packs = @(
            [pscustomobject]@{ pack_id = 'scribe-cuda-windows-x64'; security_epoch = $Cuda },
            [pscustomobject]@{ pack_id = 'scribe-vulkan-windows-x64'; security_epoch = $Vulkan }
        )
    }
}

# These checks are deliberately offline and independent of GitHub credentials.
$initial = New-TestPolicy 1 1 1
Assert-PolicyTransition $null $initial
Assert-PolicyTransition $initial (New-TestPolicy 1 1 1)
Assert-PolicyTransition $initial (New-TestPolicy 1 2 1)
Assert-PolicyTransition $initial (New-TestPolicy 2 2 2)
$negativeCases = @(
    @((New-TestPolicy 2 2 2), (New-TestPolicy 1 2 2)),
    @((New-TestPolicy 1 2 2), (New-TestPolicy 1 1 2)),
    @((New-TestPolicy 1 2 2), (New-TestPolicy 1 2 1)),
    @($initial, (New-TestPolicy 2 1 2)),
    @($initial, $null)
)
$wrongId = New-TestPolicy 1 1 1
$wrongId.packs[1].pack_id = 'replacement-pack'
$negativeCases += ,@($initial, $wrongId)
$reversed = New-TestPolicy 1 1 1
$reversed.packs = @($reversed.packs[1], $reversed.packs[0])
$negativeCases += ,@($initial, $reversed)
$stringEpoch = New-TestPolicy 1 1 1
$stringEpoch.packs[0].security_epoch = '2'
$negativeCases += ,@($initial, $stringEpoch)
foreach ($case in $negativeCases) {
    $rejected = $false
    try { Assert-PolicyTransition $case[0] $case[1] } catch { $rejected = $true }
    if (-not $rejected) { throw 'Security-epoch policy regression was accepted.' }
}

if (-not $SelfTestOnly) {
    if ($BaseRevision -cnotmatch '^[0-9a-f]{40}$' -or $BaseRevision -ceq ('0' * 40)) {
        throw 'Policy verification requires the exact immutable base commit.'
    }
    $repositoryRoot = Split-Path -Parent $PSScriptRoot
    $relativePath = 'runtime-manifests/windows-gpu-signing-policy.json'
    git -C $repositoryRoot merge-base --is-ancestor $BaseRevision HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Policy base commit must be available and an ancestor of the checkout.' }
    $current = Get-Content -LiteralPath (Join-Path $repositoryRoot $relativePath) -Raw | ConvertFrom-Json
    $previous = $null
    $entry = @(git -C $repositoryRoot ls-tree $BaseRevision -- $relativePath)
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect the immutable base policy tree.' }
    if ($entry.Count -ne 0) {
        $baseJson = @(git -C $repositoryRoot show "${BaseRevision}:$relativePath")
        if ($LASTEXITCODE -ne 0) { throw 'Could not read the immutable base signing policy.' }
        $previous = ($baseJson -join "`n") | ConvertFrom-Json
    }
    Assert-PolicyTransition $previous $current
}
Write-Output 'Windows GPU signing policy transition tests passed (12 cases).'
