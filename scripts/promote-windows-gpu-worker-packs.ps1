[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$HandoffDirectory,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedRepository,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedSourceRef,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedSourceRevision,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedWorkflowRef,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedRunId,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedRunAttempt,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedArtifactId,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedArtifactDigest,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedHandoffSha256,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedReleaseSetDigest,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedToolchainManifestSha256,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedPackVersion,
    [Parameter(Mandatory = $true)]
    [uint64]$MinimumSecurityEpoch,
    [Parameter(Mandatory = $true)]
    [string]$AuthoringToolPath,
    [switch]$FixtureSigning
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $FixtureSigning) {
    throw 'Production promotion is available only through the independently installed protected signer; this repository script never receives production signing authority.'
}

function Get-NormalizedFullPath([string]$Path) {
    $full = [System.IO.Path]::GetFullPath($Path)
    $root = [System.IO.Path]::GetPathRoot($full)
    if ([string]::Equals($full, $root, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $root
    }
    return $full.TrimEnd([char[]]@('\', '/'))
}

function Assert-ExactProperties([psobject]$Value, [string[]]$Names, [string]$Label) {
    if ($null -eq $Value) { throw "$Label is missing." }
    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $expected = @($Names | Sort-Object)
    if ($actual.Count -ne $expected.Count -or
        (Compare-Object -ReferenceObject $expected -DifferenceObject $actual -CaseSensitive)) {
        throw "$Label has unknown or missing fields."
    }
}

function Assert-CanonicalSha256([string]$Value, [string]$Label) {
    if ($Value -cnotmatch '^[0-9a-f]{64}$') { throw "$Label is not a canonical SHA-256 value." }
}

function Assert-RegularNonReparseFile([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "$Label is missing: $Path" }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label must be a regular non-reparse file: $Path"
    }
}

function Assert-PhysicalTree([string]$Root, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Root -PathType Container)) { throw "$Label is missing: $Root" }
    $pending = [System.Collections.Generic.Queue[string]]::new()
    $pending.Enqueue((Get-NormalizedFullPath $Root))
    while ($pending.Count -gt 0) {
        $directory = $pending.Dequeue()
        $directoryItem = Get-Item -LiteralPath $directory -Force
        if (-not $directoryItem.PSIsContainer -or
            ($directoryItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "$Label contains a link or reparse point: $directory"
        }
        foreach ($entry in @(Get-ChildItem -LiteralPath $directory -Force)) {
            if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "$Label contains a link or reparse point: $($entry.FullName)"
            }
            if ($entry.PSIsContainer) { $pending.Enqueue($entry.FullName) }
            elseif (-not (Test-Path -LiteralPath $entry.FullName -PathType Leaf)) {
                throw "$Label contains a nonregular entry: $($entry.FullName)"
            }
        }
    }
}

function Invoke-AuthoringTool([string[]]$Arguments, [string]$FailureMessage) {
    $output = @(& $script:authoringTool @Arguments 2>&1)
    if ($LASTEXITCODE -ne 0) { throw "$FailureMessage $($output -join [Environment]::NewLine)" }
    if ($output.Count -ne 1) { throw "$FailureMessage The tool returned an unexpected output shape." }
    try { return ([string]$output[0] | ConvertFrom-Json) }
    catch { throw "$FailureMessage The tool returned invalid JSON." }
}

function Get-CanonicalReleaseMaterialJson($Handoff) {
    $packs = @($Handoff.packs | ForEach-Object {
        [ordered]@{
            backend = [string]$_.backend
            pack_root = [string]$_.pack_root
            pack_id = [string]$_.pack_id
            pack_version = [string]$_.pack_version
            pack_digest = [string]$_.pack_digest
            security_epoch = [uint64]$_.security_epoch
            provider = [string]$_.provider
            manifest_sha256 = [string]$_.manifest_sha256
        }
    })
    return ([ordered]@{
        schema_version = [int]$Handoff.schema_version
        source_repository = [string]$Handoff.source_repository
        source_ref = [string]$Handoff.source_ref
        source_revision = [string]$Handoff.source_revision
        workflow_ref = [string]$Handoff.workflow_ref
        run_id = [string]$Handoff.run_id
        run_attempt = [string]$Handoff.run_attempt
        pack_version = [string]$Handoff.pack_version
        toolchain_manifest_sha256 = [string]$Handoff.toolchain_manifest_sha256
        packs = $packs
    } | ConvertTo-Json -Depth 8 -Compress)
}

$handoffRoot = Get-NormalizedFullPath $HandoffDirectory
$outputRoot = Get-NormalizedFullPath $OutputDirectory
$authoringTool = Get-NormalizedFullPath $AuthoringToolPath
Assert-PhysicalTree $handoffRoot 'Unsigned GPU pack handoff'
Assert-RegularNonReparseFile $authoringTool 'Fixture authoring tool'
foreach ($digest in @($ExpectedArtifactDigest, $ExpectedHandoffSha256, $ExpectedReleaseSetDigest, $ExpectedToolchainManifestSha256)) {
    Assert-CanonicalSha256 $digest 'Expected promotion digest'
}
if ($ExpectedSourceRevision -cnotmatch '^[0-9a-f]{40}$' -or
    $ExpectedRunId -cnotmatch '^[1-9][0-9]{0,19}$' -or
    $ExpectedRunAttempt -cnotmatch '^[1-9][0-9]{0,9}$' -or
    $ExpectedArtifactId -cnotmatch '^[1-9][0-9]{0,19}$') {
    throw 'Expected source revision, run identity, or artifact ID is noncanonical.'
}
if ($ExpectedPackVersion -cnotmatch '^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$' -or $MinimumSecurityEpoch -lt 1) {
    throw 'Expected pack version or minimum security epoch is noncanonical.'
}
if (Test-Path -LiteralPath $outputRoot) { throw "Promotion output already exists: $outputRoot" }

$handoffPath = Join-Path $handoffRoot 'windows-gpu-pack-handoff.json'
Assert-RegularNonReparseFile $handoffPath 'GPU pack handoff metadata'
$actualHandoffHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $handoffPath).Hash.ToLowerInvariant()
if ($actualHandoffHash -cne $ExpectedHandoffSha256) { throw 'GPU pack handoff metadata digest does not match the approved digest.' }
$handoffText = [System.IO.File]::ReadAllText($handoffPath, [System.Text.UTF8Encoding]::new($false))
try { $handoff = $handoffText | ConvertFrom-Json }
catch { throw 'GPU pack handoff metadata is not valid JSON.' }
Assert-ExactProperties $handoff @(
    'schema_version', 'source_repository', 'source_ref', 'source_revision', 'workflow_ref',
    'run_id', 'run_attempt', 'pack_version', 'toolchain_manifest_sha256', 'packs', 'release_set_digest'
) 'GPU pack handoff metadata'
if ($handoff.schema_version -ne 1 -or
    $handoff.source_repository -cne $ExpectedRepository -or
    $handoff.source_ref -cne $ExpectedSourceRef -or
    $handoff.source_revision -cne $ExpectedSourceRevision -or
    $handoff.workflow_ref -cne $ExpectedWorkflowRef -or
    [string]$handoff.run_id -cne $ExpectedRunId -or
    [string]$handoff.run_attempt -cne $ExpectedRunAttempt -or
    $handoff.pack_version -cne $ExpectedPackVersion -or
    $handoff.toolchain_manifest_sha256 -cne $ExpectedToolchainManifestSha256 -or
    $handoff.release_set_digest -cne $ExpectedReleaseSetDigest) {
    throw 'GPU pack handoff provenance does not match the protected promotion request.'
}
Assert-CanonicalSha256 ([string]$handoff.toolchain_manifest_sha256) 'Toolchain manifest digest'

$releaseMaterialJson = Get-CanonicalReleaseMaterialJson $handoff
$releaseMaterialBytes = [System.Text.Encoding]::UTF8.GetBytes("scribe-windows-gpu-release-set-v1`0$releaseMaterialJson")
$computedReleaseSetDigest = ([System.BitConverter]::ToString(
    [System.Security.Cryptography.SHA256]::Create().ComputeHash($releaseMaterialBytes)
)).Replace('-', '').ToLowerInvariant()
if ($computedReleaseSetDigest -cne $ExpectedReleaseSetDigest) { throw 'GPU pack release-set digest is not canonical for the handoff metadata.' }
$canonicalHandoff = ([ordered]@{
    schema_version = [int]$handoff.schema_version
    source_repository = [string]$handoff.source_repository
    source_ref = [string]$handoff.source_ref
    source_revision = [string]$handoff.source_revision
    workflow_ref = [string]$handoff.workflow_ref
    run_id = [string]$handoff.run_id
    run_attempt = [string]$handoff.run_attempt
    pack_version = [string]$handoff.pack_version
    toolchain_manifest_sha256 = [string]$handoff.toolchain_manifest_sha256
    packs = @($handoff.packs | ForEach-Object { [ordered]@{
        backend = [string]$_.backend; pack_root = [string]$_.pack_root
        pack_id = [string]$_.pack_id; pack_version = [string]$_.pack_version
        pack_digest = [string]$_.pack_digest; security_epoch = [uint64]$_.security_epoch
        provider = [string]$_.provider; manifest_sha256 = [string]$_.manifest_sha256
    } })
    release_set_digest = [string]$handoff.release_set_digest
} | ConvertTo-Json -Depth 8 -Compress)
if ($handoffText -cne $canonicalHandoff) { throw 'GPU pack handoff metadata is not canonical JSON.' }

$rootEntries = @(Get-ChildItem -LiteralPath $handoffRoot -Force | ForEach-Object Name | Sort-Object)
if ($rootEntries.Count -ne 3 -or
    (Compare-Object -ReferenceObject @('cuda', 'vulkan', 'windows-gpu-pack-handoff.json') -DifferenceObject $rootEntries -CaseSensitive)) {
    throw 'Unsigned GPU pack handoff has an unexpected top-level inventory.'
}
$packs = @($handoff.packs)
if ($packs.Count -ne 2 -or $packs[0].backend -cne 'cuda' -or $packs[1].backend -cne 'vulkan') {
    throw 'GPU pack handoff must contain exactly one CUDA pack followed by one Vulkan pack.'
}

$descriptors = @()
foreach ($pack in $packs) {
    Assert-ExactProperties $pack @(
        'backend', 'pack_root', 'pack_id', 'pack_version', 'pack_digest', 'security_epoch',
        'provider', 'manifest_sha256'
    ) 'GPU pack handoff entry'
    $backend = [string]$pack.backend
    if ($pack.pack_root -cne $backend -or $backend -notin @('cuda', 'vulkan')) {
        throw 'GPU pack handoff contains a noncanonical pack root.'
    }
    $packRoot = Join-Path $handoffRoot $backend
    $descriptor = Invoke-AuthoringTool @('inspect-prepared-pack', '--pack-root', $packRoot) 'Prepared GPU pack validation failed.'
    if ($descriptor.schema_version -ne 1 -or
        $descriptor.backend -cne $backend -or
        $descriptor.pack_id -cne $pack.pack_id -or
        $descriptor.pack_version -cne $handoff.pack_version -or
        $descriptor.pack_version -cne $pack.pack_version -or
        $descriptor.pack_digest -cne $pack.pack_digest -or
        [uint64]$descriptor.security_epoch -ne [uint64]$pack.security_epoch -or
        [uint64]$descriptor.security_epoch -lt $MinimumSecurityEpoch -or
        $descriptor.provider -cne $pack.provider -or
        $descriptor.target_os -cne 'windows' -or
        $descriptor.target_arch -cne 'x86_64' -or
        $descriptor.manifest_sha256 -cne $pack.manifest_sha256) {
        throw 'Prepared GPU pack does not match its canonical handoff entry.'
    }
    $descriptors += $descriptor
}

$outputParent = Split-Path -Parent $outputRoot
if (-not (Test-Path -LiteralPath $outputParent -PathType Container)) {
    New-Item -ItemType Directory -Path $outputParent -Force | Out-Null
}
$stagingRoot = "$outputRoot.staging-$([guid]::NewGuid().ToString('N'))"
$stagingCreated = $false
try {
    New-Item -ItemType Directory -Path $stagingRoot | Out-Null
    $stagingCreated = $true
    for ($index = 0; $index -lt $packs.Count; $index++) {
        $pack = $packs[$index]
        $source = Join-Path $handoffRoot ([string]$pack.pack_root)
        $destination = Join-Path $stagingRoot ([string]$pack.pack_root)
        Copy-Item -LiteralPath $source -Destination $destination -Recurse
        $copied = Invoke-AuthoringTool @('inspect-prepared-pack', '--pack-root', $destination) 'Copied prepared GPU pack validation failed.'
        if ($copied.manifest_sha256 -cne $pack.manifest_sha256 -or $copied.pack_digest -cne $pack.pack_digest) {
            throw 'Copied prepared GPU pack changed before fixture signing.'
        }
        $signed = Invoke-AuthoringTool @(
            'sign-prepared-pack', '--pack-root', $destination,
            '--expected-manifest-sha256', ([string]$pack.manifest_sha256),
            '--expected-pack-digest', ([string]$pack.pack_digest), '--fixture-signing'
        ) 'Fixture prepared-pack signing failed.'
        if ($signed.pack_digest -cne $pack.pack_digest) { throw 'Fixture signer returned a mismatched pack digest.' }
        $null = Invoke-AuthoringTool @('verify-fixture', '--pack-root', $destination) 'Promoted fixture pack verification failed.'
    }
    $receipt = [ordered]@{
        schema_version = 1
        authority = 'fixture-only'
        source_repository = $ExpectedRepository
        source_ref = $ExpectedSourceRef
        source_revision = $ExpectedSourceRevision
        workflow_ref = $ExpectedWorkflowRef
        run_id = $ExpectedRunId
        run_attempt = $ExpectedRunAttempt
        artifact_id = $ExpectedArtifactId
        artifact_digest = $ExpectedArtifactDigest
        handoff_sha256 = $ExpectedHandoffSha256
        release_set_digest = $ExpectedReleaseSetDigest
        packs = @($packs | ForEach-Object { [ordered]@{ backend = $_.backend; pack_digest = $_.pack_digest } })
    } | ConvertTo-Json -Depth 6 -Compress
    [System.IO.File]::WriteAllText(
        (Join-Path $stagingRoot 'windows-gpu-pack-promotion.json'),
        $receipt,
        [System.Text.UTF8Encoding]::new($false)
    )
    Move-Item -LiteralPath $stagingRoot -Destination $outputRoot
    $stagingCreated = $false
    [pscustomobject]@{
        SchemaVersion = 1
        Authority = 'fixture-only'
        OutputDirectory = $outputRoot
        ReleaseSetDigest = $ExpectedReleaseSetDigest
        PackRoots = @((Join-Path $outputRoot 'cuda'), (Join-Path $outputRoot 'vulkan'))
    }
}
finally {
    if ($stagingCreated -and (Test-Path -LiteralPath $stagingRoot)) {
        $observedParent = Get-NormalizedFullPath (Split-Path -Parent $stagingRoot)
        if ($observedParent -cne (Get-NormalizedFullPath $outputParent) -or
            -not (Split-Path -Leaf $stagingRoot).StartsWith("$(Split-Path -Leaf $outputRoot).staging-", [System.StringComparison]::Ordinal)) {
            throw 'Refusing to clean a staging path outside the exact promotion output parent.'
        }
        Remove-Item -LiteralPath $stagingRoot -Recurse -Force
    }
}
