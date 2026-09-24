[CmdletBinding()]
param(
    [ValidateSet('Preflight', 'Resolve', 'VerifyCatalog')]
    [string]$Mode,
    [string]$SourceRevision,
    [string]$SigningRunId,
    [string]$SigningRunAttempt,
    [string]$SignedArtifactId,
    [string]$SignedRoot,
    [string]$VerifierExecutable,
    [string]$PolicyPath,
    [string]$ToolchainManifestPath,
    [string]$CatalogPath,
    [string]$ExpectedArtifactSha256,
    [string]$ExpectedPolicySha256,
    [string]$ExpectedSignerPinsSha256,
    [switch]$FunctionsOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$signedGpuEntryArguments = @{
    Mode = $Mode
    SourceRevision = $SourceRevision
    SigningRunId = $SigningRunId
    SigningRunAttempt = $SigningRunAttempt
    SignedArtifactId = $SignedArtifactId
    SignedRoot = $SignedRoot
    VerifierExecutable = $VerifierExecutable
    PolicyPath = $PolicyPath
    ToolchainManifestPath = $ToolchainManifestPath
    CatalogPath = $CatalogPath
    ExpectedArtifactSha256 = $ExpectedArtifactSha256
    ExpectedPolicySha256 = $ExpectedPolicySha256
    ExpectedSignerPinsSha256 = $ExpectedSignerPinsSha256
}
$signedGpuFunctionsOnly = $FunctionsOnly.IsPresent
# The reused script has its own top-level parameter block. Preserve this
# script's public invocation before dot-sourcing those helper functions.
. (Join-Path $PSScriptRoot 'invoke-windows-gpu-approved-signing.ps1') -FunctionsOnly

$script:SignedGpuRepositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$script:SignedGpuDefaultPolicyPath = Join-Path $script:SignedGpuRepositoryRoot 'runtime-manifests/windows-gpu-signing-policy.json'
$script:SignedGpuDefaultToolchainPath = Join-Path $script:SignedGpuRepositoryRoot 'runtime-manifests/gpu-worker-toolchain-windows-x64.json'
$script:SignedGpuDefaultVerifierPath = Join-Path $script:SignedGpuRepositoryRoot 'target/gpu-installer-author/release/scribe-worker-pack-tool.exe'
$script:SignedGpuReceiptFields = @(
    'schema_version', 'policy_id', 'policy_sha256', 'key_id',
    'signer_source_revision', 'signer_sha256', 'source_repository', 'source_ref',
    'source_revision', 'workflow_ref', 'run_id', 'run_attempt', 'artifact_id',
    'artifact_digest', 'handoff_sha256', 'release_set_digest',
    'toolchain_manifest_sha256', 'pack_version', 'packs'
)
$script:SignedGpuReceiptPackFields = @(
    'backend', 'pack_root', 'pack_id', 'pack_version', 'pack_digest',
    'manifest_sha256', 'security_epoch', 'provider', 'payload_files',
    'installed_payload_bytes'
)
$script:SignedGpuCatalogPackFields = @(
    'pack_id', 'pack_version', 'pack_digest', 'security_epoch',
    'runtime_abi_version', 'backend', 'provider', 'target_os', 'target_arch',
    'worker_relative_path', 'root', 'installed_size_bytes',
    'compressed_size_bytes', 'files'
)
$script:SignedGpuExpectedPacks = @(
    [ordered]@{
        backend = 'cuda'
        pack_id = 'scribe-cuda-windows-x64'
        provider = 'transcribe-cpp-ggml-cuda'
    },
    [ordered]@{
        backend = 'vulkan'
        pack_id = 'scribe-vulkan-windows-x64'
        provider = 'transcribe-cpp-ggml-vulkan'
    }
)
$script:SignedGpuWorkerPath = 'bin/scribe-inference-worker.exe'

function Assert-WindowsGpuInputCondition([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-WindowsGpuExactKeys($Value, [string[]]$Names, [string]$Label) {
    Assert-WindowsGpuInputCondition ($null -ne $Value) "$Label is missing."
    Assert-WindowsGpuInputCondition ($Value -is [Collections.IDictionary]) "$Label is not a JSON object."
    $actual = @($Value.Keys | ForEach-Object { [string]$_ } | Sort-Object -CaseSensitive)
    $expected = @($Names | Sort-Object -CaseSensitive)
    Assert-WindowsGpuInputCondition (
        $actual.Count -eq $expected.Count -and
        -not (Compare-Object -ReferenceObject $expected -DifferenceObject $actual -CaseSensitive)
    ) "$Label has unknown or missing fields."
}

function Assert-WindowsGpuJsonInteger($Value, [uint64]$Minimum, [uint64]$Maximum, [string]$Label) {
    Assert-WindowsGpuInputCondition (
        $Value -is [byte] -or $Value -is [uint16] -or $Value -is [uint32] -or
        $Value -is [uint64] -or $Value -is [int16] -or $Value -is [int32] -or
        $Value -is [int64]
    ) "$Label is not an integer."
    try { $number = [uint64]$Value }
    catch { throw "$Label is outside its unsigned integer range." }
    Assert-WindowsGpuInputCondition ($number -ge $Minimum -and $number -le $Maximum) "$Label is outside its permitted range."
    return $number
}

function Test-WindowsGpuByteEquality([byte[]]$Left, [byte[]]$Right) {
    if ($Left.Length -ne $Right.Length) { return $false }
    for ($index = 0; $index -lt $Left.Length; $index++) {
        if ($Left[$index] -ne $Right[$index]) { return $false }
    }
    return $true
}

function Get-WindowsGpuNormalizedPath([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    $root = [IO.Path]::GetPathRoot($full)
    if ([string]::Equals($full, $root, [StringComparison]::OrdinalIgnoreCase)) { return $root }
    return $full.TrimEnd([char[]]@('\', '/'))
}

function Test-WindowsGpuPathWithin([string]$Candidate, [string]$Parent) {
    $candidateFull = Get-WindowsGpuNormalizedPath $Candidate
    $parentFull = Get-WindowsGpuNormalizedPath $Parent
    if ([string]::Equals($candidateFull, $parentFull, [StringComparison]::OrdinalIgnoreCase)) { return $true }
    $prefix = $parentFull + [IO.Path]::DirectorySeparatorChar
    return $candidateFull.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)
}

function Assert-WindowsGpuPhysicalDirectory([string]$Path, [string]$Label) {
    $full = Get-WindowsGpuNormalizedPath $Path
    Assert-WindowsGpuInputCondition (Test-Path -LiteralPath $full -PathType Container) "$Label is missing."
    $cursor = $full
    while ($cursor) {
        $item = Get-Item -LiteralPath $cursor -Force
        Assert-WindowsGpuInputCondition (
            $item.PSIsContainer -and
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0
        ) "$Label cannot traverse a link or reparse point."
        $parent = Split-Path -Parent $cursor
        if ([string]::IsNullOrEmpty($parent) -or $parent -ceq $cursor) { break }
        $cursor = $parent
    }
    return $full
}

function Get-WindowsGpuInstallerGitHead {
    $output = @(& git -C $script:SignedGpuRepositoryRoot rev-parse HEAD 2>$null)
    Assert-WindowsGpuInputCondition ($LASTEXITCODE -eq 0 -and $output.Count -eq 1) 'Could not determine the installer checkout revision.'
    $head = ([string]$output[0]).Trim()
    Assert-WindowsGpuInputCondition ($head -cmatch '\A[0-9a-f]{40}\z') 'Installer checkout revision is not canonical.'
    return $head
}

function Assert-WindowsGpuProductionContext([string]$Revision) {
    Assert-WindowsGpuInputCondition (
        $env:GITHUB_REPOSITORY -ceq $script:SigningRepository -and
        $env:GITHUB_EVENT_NAME -ceq 'workflow_dispatch' -and
        $env:GITHUB_REF -ceq $script:SigningRef
    ) 'Signed GPU installer inputs require the fixed repository default-branch workflow dispatch.'
    Assert-WindowsGpuInputCondition ($env:GITHUB_SHA -ceq $Revision) 'Workflow checkout revision does not match the requested installer source.'
    Assert-WindowsGpuInputCondition ((Get-WindowsGpuInstallerGitHead) -ceq $Revision) 'Actual installer checkout does not match the requested source revision.'
}

function Assert-WindowsGpuExpectedHashSet(
    [string]$ArtifactSha256,
    [string]$PolicySha256,
    [string]$SignerPinsSha256,
    [bool]$Required
) {
    $values = @($ArtifactSha256, $PolicySha256, $SignerPinsSha256)
    $present = @($values | Where-Object { -not [string]::IsNullOrEmpty($_) }).Count
    Assert-WindowsGpuInputCondition ($present -eq 0 -or $present -eq 3) 'Expected signed-input hashes must be omitted together or supplied together.'
    Assert-WindowsGpuInputCondition (-not $Required -or $present -eq 3) 'Resolve and VerifyCatalog require all three preflight hashes.'
    if ($present -eq 3) {
        Assert-SigningHash $ArtifactSha256 'Expected signed artifact digest'
        Assert-SigningHash $PolicySha256 'Expected signing policy digest'
        Assert-SigningHash $SignerPinsSha256 'Expected signer pins digest'
    }
}

function Assert-WindowsGpuPolicy($Policy) {
    Assert-WindowsGpuExactKeys $Policy @(
        'schema_version', 'policy_id', 'key_id', 'public_key_sha256',
        'source_repository', 'source_ref', 'app_version', 'protocol_version',
        'worker_abi_version', 'minimum_security_epoch', 'packs'
    ) 'Windows GPU signing policy'
    Assert-WindowsGpuInputCondition ((Assert-WindowsGpuJsonInteger $Policy.schema_version 1 1 'Signing policy schema version') -eq 1) 'Signing policy schema is unsupported.'
    Assert-WindowsGpuInputCondition (
        $Policy.source_repository -ceq $script:SigningRepository -and
        $Policy.source_ref -ceq $script:SigningRef
    ) 'Signing policy repository binding does not match.'
    Assert-WindowsGpuInputCondition (
        $Policy.policy_id -ceq 'scribe-windows-gpu-github-signing-v1' -and
        $Policy.key_id -cmatch '\A[A-Za-z0-9._:-]{1,96}\z' -and
        $Policy.public_key_sha256 -cmatch '\A[0-9a-f]{64}\z'
    ) 'Signing policy identity or public-key pin is invalid.'
    Assert-WindowsGpuInputCondition ((Assert-WindowsGpuJsonInteger $Policy.worker_abi_version 1 1 'Signing policy worker ABI') -eq 1) 'Signing policy worker ABI is unsupported.'
    $null = Assert-WindowsGpuJsonInteger $Policy.protocol_version 1 65535 'Signing policy protocol version'
    $minimumEpoch = Assert-WindowsGpuJsonInteger $Policy.minimum_security_epoch 1 ([uint64]::MaxValue) 'Signing policy minimum security epoch'
    Assert-WindowsGpuInputCondition ($Policy.app_version -cmatch '\A[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?\z') 'Signing policy application version is invalid.'
    $packs = @($Policy.packs)
    Assert-WindowsGpuInputCondition ($packs.Count -eq 2) 'Signing policy must contain exactly the Windows CUDA/Vulkan pair.'
    for ($index = 0; $index -lt 2; $index++) {
        $pack = $packs[$index]
        $expected = $script:SignedGpuExpectedPacks[$index]
        Assert-WindowsGpuExactKeys $pack @('backend', 'pack_id', 'provider', 'worker_path', 'security_epoch') "Signing policy pack $index"
        $epoch = Assert-WindowsGpuJsonInteger $pack.security_epoch $minimumEpoch ([uint64]::MaxValue) "Signing policy pack $index security epoch"
        Assert-WindowsGpuInputCondition (
            $pack.backend -ceq $expected.backend -and
            $pack.pack_id -ceq $expected.pack_id -and
            $pack.provider -ceq $expected.provider -and
            $pack.worker_path -ceq $script:SignedGpuWorkerPath -and
            $epoch -ge $minimumEpoch
        ) "Signing policy pack $index does not match the fixed installer contract."
    }
}

function Get-WindowsGpuAuthenticatedSigningArtifact(
    [string]$RunId,
    [string]$Attempt,
    [string]$ArtifactId,
    [string]$Revision
) {
    Assert-SigningId $RunId 'Signing run ID'
    Assert-SigningId $Attempt 'Signing run attempt'
    Assert-SigningId $ArtifactId 'Signed artifact ID'
    $run = Invoke-SigningGitHubGet "/repos/$script:SigningRepository/actions/runs/$RunId/attempts/$Attempt"
    Assert-SigningRunMetadata $run $RunId $Attempt $script:ProducerWorkflow $Revision
    $latest = Invoke-SigningGitHubGet "/repos/$script:SigningRepository/actions/runs/$RunId"
    Assert-SigningRunMetadata $latest $RunId $Attempt $script:ProducerWorkflow $Revision
    $artifact = Invoke-SigningGitHubGet "/repos/$script:SigningRepository/actions/artifacts/$ArtifactId"
    Assert-SigningArtifactMetadata $artifact $ArtifactId $RunId $Revision "windows-gpu-signed-$RunId-$Attempt"
    return [pscustomobject]@{ Run = $run; Artifact = $artifact }
}

function Get-WindowsGpuLocalAndCurrentPolicy([string]$Revision, [string]$Path, [string]$ExpectedSha256 = '') {
    $localPath = Get-WindowsGpuNormalizedPath $Path
    $localBytes = Read-SigningFile $localPath 65536
    $current = Get-CurrentSigningPolicy $Revision $ExpectedSha256
    Assert-WindowsGpuInputCondition (Test-WindowsGpuByteEquality $localBytes $current.Bytes) 'Checkout signing policy differs byte-for-byte from current protected main.'
    Assert-WindowsGpuPolicy $current.Document
    return [pscustomobject]@{
        Path = $localPath
        Bytes = $localBytes
        Digest = $current.Digest
        Document = $current.Document
        Revision = $current.Revision
    }
}

function Invoke-WindowsGpuSignedInputPreflight(
    [string]$Revision,
    [string]$RunId,
    [string]$Attempt,
    [string]$ArtifactId,
    [string]$LocalPolicyPath,
    [string]$ExpectedArtifactSha256 = '',
    [string]$ExpectedPolicySha256 = '',
    [string]$ExpectedSignerPinsSha256 = ''
) {
    $signing = Get-WindowsGpuAuthenticatedSigningArtifact $RunId $Attempt $ArtifactId $Revision
    $artifactDigest = ([string]$signing.Artifact.digest).Substring(7)
    if ($ExpectedArtifactSha256) {
        Assert-WindowsGpuInputCondition ($artifactDigest -ceq $ExpectedArtifactSha256) 'Signed artifact digest changed after preflight.'
    }
    $policy = Get-WindowsGpuLocalAndCurrentPolicy $Revision $LocalPolicyPath $ExpectedPolicySha256
    $pins = Get-ApprovedSignerPins
    $pinsDigest = Get-SigningPinsHash $pins
    if ($ExpectedSignerPinsSha256) { Assert-SigningPinsUnchanged $pins $ExpectedSignerPinsSha256 }
    return [pscustomobject]@{
        ArtifactSha256 = $artifactDigest
        PolicySha256 = $policy.Digest
        SignerPinsSha256 = $pinsDigest
        Policy = $policy
        Pins = $pins
    }
}

function Resolve-TrustedWindowsGpuVerifierPath([string]$Path, [string]$ArtifactRoot) {
    $verifier = Get-WindowsGpuNormalizedPath $Path
    $expected = Get-WindowsGpuNormalizedPath $script:SignedGpuDefaultVerifierPath
    Assert-WindowsGpuInputCondition ([string]::Equals($verifier, $expected, [StringComparison]::OrdinalIgnoreCase)) 'Signed-set verifier must be the fixed executable built from the installer checkout.'
    Assert-WindowsGpuInputCondition (-not (Test-WindowsGpuPathWithin $verifier $ArtifactRoot)) 'Signed-set verifier cannot come from the downloaded artifact.'
    $bytes = Read-SigningFile $verifier 134217728
    Assert-WindowsGpuInputCondition ($bytes.Length -gt 0) 'Signed-set verifier is empty.'
    return $verifier
}

function Invoke-WindowsGpuSignedSetVerifier(
    [string]$Executable,
    [string]$ArtifactRoot,
    [string]$LocalPolicyPath,
    [string]$LocalToolchainPath
) {
    $lock = [IO.FileStream]::new($Executable, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $process = $null
    try {
        $info = [Diagnostics.ProcessStartInfo]::new()
        $info.FileName = $Executable
        $info.WorkingDirectory = $script:SignedGpuRepositoryRoot
        $info.UseShellExecute = $false
        $info.CreateNoWindow = $true
        $info.RedirectStandardInput = $true
        $info.RedirectStandardOutput = $true
        $info.RedirectStandardError = $true
        $info.Environment.Remove('GH_TOKEN') | Out-Null
        $info.Environment.Remove('GITHUB_TOKEN') | Out-Null
        $info.Environment.Remove('SCRIBE_GPU_PACK_PRIVATE_KEY_BASE64') | Out-Null
        foreach ($argument in @(
            'verify-signed-windows-set', '--signed-root', $ArtifactRoot,
            '--policy', $LocalPolicyPath, '--toolchain-manifest', $LocalToolchainPath
        )) { $info.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($info)
        Assert-WindowsGpuInputCondition ($null -ne $process) 'Could not start the trusted signed-set verifier.'
        $process.StandardInput.Close()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(120000)) {
            $process.Kill($true)
            $process.WaitForExit()
            throw 'Trusted signed-set verifier exceeded its two-minute execution bound.'
        }
        $output = $stdout.GetAwaiter().GetResult()
        $errorText = $stderr.GetAwaiter().GetResult()
        Assert-WindowsGpuInputCondition (
            $process.ExitCode -eq 0 -and
            $output.Length -gt 0 -and $output.Length -le 262144 -and
            $errorText.Length -le 65536
        ) 'Trusted signed-set verifier rejected the artifact or exceeded its output bound.'
        return $output
    }
    finally {
        if ($null -ne $process) { $process.Dispose() }
        $lock.Dispose()
    }
}

function Assert-WindowsGpuStoreComponent([string]$Value, [string]$Label) {
    Assert-WindowsGpuInputCondition (
        $Value -cmatch '\A[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?\z'
    ) "$Label is not a canonical immutable-store component."
}

function Assert-WindowsGpuReceipt(
    $Receipt,
    $Policy,
    $Pins,
    [string]$Revision,
    [string]$SigningRunId,
    [string]$SignedArtifactId,
    [string]$ToolchainSha256
) {
    Assert-WindowsGpuExactKeys $Receipt $script:SignedGpuReceiptFields 'Verified Windows pack signing receipt'
    Assert-WindowsGpuInputCondition ((Assert-WindowsGpuJsonInteger $Receipt.schema_version 1 1 'Signing receipt schema version') -eq 1) 'Signing receipt schema is unsupported.'
    foreach ($field in @('policy_sha256', 'signer_sha256', 'artifact_digest', 'handoff_sha256', 'release_set_digest', 'toolchain_manifest_sha256')) {
        Assert-SigningHash ([string]$Receipt[$field]) "Signing receipt $field"
    }
    Assert-WindowsGpuInputCondition (
        $Receipt.policy_id -ceq $Policy.Document.policy_id -and
        $Receipt.policy_sha256 -ceq $Policy.Digest -and
        $Receipt.key_id -ceq $Policy.Document.key_id
    ) 'Signing receipt policy binding does not match the current reviewed policy.'
    Assert-WindowsGpuInputCondition (
        $Receipt.signer_source_revision -ceq $Pins.SOURCE_SHA -and
        $Receipt.signer_sha256 -ceq $Pins.BINARY_SHA256
    ) 'Signing receipt does not match the current reviewed signer pins.'
    Assert-WindowsGpuInputCondition (
        $Receipt.source_repository -ceq $script:SigningRepository -and
        $Receipt.source_ref -ceq $script:SigningRef -and
        $Receipt.source_revision -ceq $Revision -and
        $Receipt.workflow_ref -ceq "$script:SigningRepository/$script:ProducerWorkflow@$script:SigningRef"
    ) 'Signing receipt source binding does not match the installer checkout.'
    Assert-WindowsGpuInputCondition ($Receipt.toolchain_manifest_sha256 -ceq $ToolchainSha256) 'Signing receipt toolchain digest does not match the checkout bytes.'
    Assert-WindowsGpuStoreComponent ([string]$Receipt.pack_version) 'Signing receipt pack version'
    Assert-WindowsGpuInputCondition (
        [string]$Receipt.run_id -cne $SigningRunId -and
        [string]$Receipt.artifact_id -cne $SignedArtifactId
    ) 'Signing receipt must identify the distinct original unsigned producer artifact.'

    $producer = Get-SigningProducer ([string]$Receipt.run_id) ([string]$Receipt.run_attempt) ([string]$Receipt.artifact_id) ([string]$Receipt.artifact_digest)
    Assert-WindowsGpuInputCondition ($producer.Run.head_sha -ceq $Revision) 'Original unsigned producer source differs from the installer checkout.'

    $receiptPacks = @($Receipt.packs)
    $policyPacks = @($Policy.Document.packs)
    Assert-WindowsGpuInputCondition ($receiptPacks.Count -eq 2) 'Signing receipt must contain exactly the CUDA/Vulkan pair.'
    for ($index = 0; $index -lt 2; $index++) {
        $pack = $receiptPacks[$index]
        $expected = $script:SignedGpuExpectedPacks[$index]
        $policyPack = $policyPacks[$index]
        Assert-WindowsGpuExactKeys $pack $script:SignedGpuReceiptPackFields "Signing receipt pack $index"
        Assert-SigningHash ([string]$pack.pack_digest) "Signing receipt pack $index digest"
        Assert-SigningHash ([string]$pack.manifest_sha256) "Signing receipt pack $index manifest digest"
        $epoch = Assert-WindowsGpuJsonInteger $pack.security_epoch 1 ([uint64]::MaxValue) "Signing receipt pack $index security epoch"
        $null = Assert-WindowsGpuJsonInteger $pack.payload_files 1 1024 "Signing receipt pack $index payload-file count"
        $null = Assert-WindowsGpuJsonInteger $pack.installed_payload_bytes 1 ([uint64]::MaxValue) "Signing receipt pack $index installed size"
        Assert-WindowsGpuInputCondition (
            $pack.backend -ceq $expected.backend -and
            $pack.pack_root -ceq $expected.backend -and
            $pack.pack_id -ceq $expected.pack_id -and
            $pack.provider -ceq $expected.provider -and
            $pack.pack_version -ceq $Receipt.pack_version -and
            $epoch -eq [uint64]$policyPack.security_epoch
        ) "Signing receipt pack $index does not match the reviewed CUDA/Vulkan contract."
    }
}

function Assert-WindowsGpuCatalog($Catalog, $Receipt, $Policy) {
    Assert-WindowsGpuExactKeys $Catalog @('schema_version', 'packs') 'Installer worker-pack catalog'
    Assert-WindowsGpuInputCondition ((Assert-WindowsGpuJsonInteger $Catalog.schema_version 1 1 'Installer catalog schema version') -eq 1) 'Installer catalog schema is unsupported.'
    $catalogPacks = @($Catalog.packs)
    $receiptPacks = @($Receipt.packs)
    $policyPacks = @($Policy.packs)
    Assert-WindowsGpuInputCondition ($catalogPacks.Count -eq 2) 'Installer catalog must contain exactly the CUDA/Vulkan pair.'
    for ($index = 0; $index -lt 2; $index++) {
        $catalogPack = $catalogPacks[$index]
        $receiptPack = $receiptPacks[$index]
        $policyPack = $policyPacks[$index]
        $expected = $script:SignedGpuExpectedPacks[$index]
        Assert-WindowsGpuExactKeys $catalogPack $script:SignedGpuCatalogPackFields "Installer catalog pack $index"
        $epoch = Assert-WindowsGpuJsonInteger $catalogPack.security_epoch 1 ([uint64]::MaxValue) "Installer catalog pack $index security epoch"
        $abi = Assert-WindowsGpuJsonInteger $catalogPack.runtime_abi_version 1 1 "Installer catalog pack $index runtime ABI"
        $null = Assert-WindowsGpuJsonInteger $catalogPack.installed_size_bytes 1 ([uint64]::MaxValue) "Installer catalog pack $index installed size"
        $null = Assert-WindowsGpuJsonInteger $catalogPack.compressed_size_bytes 1 ([uint64]::MaxValue) "Installer catalog pack $index compressed size"
        $immutableRoot = "workers/packs/$($receiptPack.pack_id)/$($receiptPack.pack_version)/$($receiptPack.pack_digest)"
        Assert-WindowsGpuInputCondition (
            $catalogPack.backend -ceq $expected.backend -and
            $catalogPack.pack_id -ceq $expected.pack_id -and
            $catalogPack.provider -ceq $expected.provider -and
            $catalogPack.pack_version -ceq $receiptPack.pack_version -and
            $catalogPack.pack_digest -ceq $receiptPack.pack_digest -and
            $epoch -eq [uint64]$receiptPack.security_epoch -and
            $epoch -eq [uint64]$policyPack.security_epoch -and
            $abi -eq 1 -and
            $catalogPack.target_os -ceq 'windows' -and
            $catalogPack.target_arch -ceq 'x86_64' -and
            $catalogPack.worker_relative_path -ceq $script:SignedGpuWorkerPath -and
            $catalogPack.root -ceq $immutableRoot
        ) "Installer catalog pack $index does not match the verified immutable pack."
        $files = @($catalogPack.files)
        Assert-WindowsGpuInputCondition ($files.Count -gt 0 -and $files.Count -le 1024) "Installer catalog pack $index has an invalid file inventory."
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($file in $files) {
            Assert-WindowsGpuInputCondition (
                $file -is [string] -and
                $file.StartsWith("$immutableRoot/", [StringComparison]::Ordinal) -and
                $file -cnotmatch '\\|:|//|/\z|\A/|(?:\A|/)\.\.?(/|\z)' -and
                $seen.Add($file)
            ) "Installer catalog pack $index contains a noncanonical immutable file path."
        }
    }
}

function Invoke-WindowsGpuSignedInputs {
    [CmdletBinding()]
    param(
        [ValidateSet('Preflight', 'Resolve', 'VerifyCatalog')][string]$Mode,
        [string]$SourceRevision,
        [string]$SigningRunId,
        [string]$SigningRunAttempt,
        [string]$SignedArtifactId,
        [string]$SignedRoot,
        [string]$VerifierExecutable,
        [string]$PolicyPath,
        [string]$ToolchainManifestPath,
        [string]$CatalogPath,
        [string]$ExpectedArtifactSha256,
        [string]$ExpectedPolicySha256,
        [string]$ExpectedSignerPinsSha256
    )
    Assert-WindowsGpuInputCondition ($Mode -cin @('Preflight', 'Resolve', 'VerifyCatalog')) 'Signed GPU input mode is missing.'
    Assert-WindowsGpuInputCondition ($SourceRevision -cmatch '\A[0-9a-f]{40}\z') 'Installer source revision is not canonical.'
    Assert-SigningId $SigningRunId 'Signing run ID'
    Assert-SigningId $SigningRunAttempt 'Signing run attempt'
    Assert-SigningId $SignedArtifactId 'Signed artifact ID'
    Assert-WindowsGpuExpectedHashSet $ExpectedArtifactSha256 $ExpectedPolicySha256 $ExpectedSignerPinsSha256 ($Mode -cne 'Preflight')
    Assert-WindowsGpuProductionContext $SourceRevision

    $resolvedPolicyPath = if ([string]::IsNullOrEmpty($PolicyPath)) { $script:SignedGpuDefaultPolicyPath } else { $PolicyPath }
    $preflight = Invoke-WindowsGpuSignedInputPreflight `
        $SourceRevision $SigningRunId $SigningRunAttempt $SignedArtifactId `
        $resolvedPolicyPath $ExpectedArtifactSha256 $ExpectedPolicySha256 $ExpectedSignerPinsSha256
    if ($Mode -ceq 'Preflight') {
        return [pscustomobject]@{
            ArtifactSha256 = $preflight.ArtifactSha256
            PolicySha256 = $preflight.PolicySha256
            SignerPinsSha256 = $preflight.SignerPinsSha256
        }
    }

    Assert-WindowsGpuInputCondition (-not [string]::IsNullOrWhiteSpace($SignedRoot)) 'Resolve requires the downloaded signed artifact root.'
    $artifactRoot = Assert-WindowsGpuPhysicalDirectory $SignedRoot 'Downloaded signed artifact root'
    $resolvedVerifierPath = if ([string]::IsNullOrEmpty($VerifierExecutable)) { $script:SignedGpuDefaultVerifierPath } else { $VerifierExecutable }
    $trustedVerifier = Resolve-TrustedWindowsGpuVerifierPath $resolvedVerifierPath $artifactRoot
    $resolvedToolchainPath = if ([string]::IsNullOrEmpty($ToolchainManifestPath)) { $script:SignedGpuDefaultToolchainPath } else { $ToolchainManifestPath }
    $toolchainPath = Get-WindowsGpuNormalizedPath $resolvedToolchainPath
    $toolchainBytes = Read-SigningFile $toolchainPath 16777216
    $toolchainSha256 = Get-SigningHash $toolchainBytes

    $nativeOutput = Invoke-WindowsGpuSignedSetVerifier $trustedVerifier $artifactRoot $preflight.Policy.Path $toolchainPath
    Assert-WindowsGpuInputCondition ($nativeOutput -is [string] -and $nativeOutput.Length -gt 0 -and $nativeOutput.Length -le 262144) 'Trusted signed-set verifier returned an invalid output shape.'
    $receipt = ConvertFrom-SigningJson ($script:Utf8.GetBytes($nativeOutput))
    Assert-WindowsGpuReceipt $receipt $preflight.Policy $preflight.Pins $SourceRevision $SigningRunId $SignedArtifactId $toolchainSha256

    # Re-read protected main immediately before returning consumable roots. A
    # policy change during native verification invalidates this invocation.
    $finalPolicy = Get-WindowsGpuLocalAndCurrentPolicy $SourceRevision $preflight.Policy.Path $preflight.PolicySha256
    $packRoots = @(
        [IO.Path]::GetFullPath((Join-Path $artifactRoot 'cuda')),
        [IO.Path]::GetFullPath((Join-Path $artifactRoot 'vulkan'))
    )
    foreach ($packRoot in $packRoots) {
        Assert-WindowsGpuInputCondition (Test-Path -LiteralPath $packRoot -PathType Container) 'Trusted verifier did not leave the expected signed pack roots available.'
    }

    if ($Mode -ceq 'Resolve') {
        return [pscustomobject]@{
            PackRoots = $packRoots
            Receipt = $receipt
            ArtifactSha256 = $preflight.ArtifactSha256
            PolicySha256 = $preflight.PolicySha256
            SignerPinsSha256 = $preflight.SignerPinsSha256
        }
    }

    Assert-WindowsGpuInputCondition (-not [string]::IsNullOrWhiteSpace($CatalogPath)) 'VerifyCatalog requires the staged installer catalog.'
    $catalogBytes = Read-SigningFile (Get-WindowsGpuNormalizedPath $CatalogPath) 4194304
    $catalog = ConvertFrom-SigningJson $catalogBytes
    Assert-WindowsGpuCatalog $catalog $receipt $finalPolicy.Document
    $null = Get-WindowsGpuLocalAndCurrentPolicy $SourceRevision $preflight.Policy.Path $preflight.PolicySha256
    return [pscustomobject]@{
        Included = $true
        PackRoots = $packRoots
        Receipt = $receipt
        ArtifactSha256 = $preflight.ArtifactSha256
        PolicySha256 = $preflight.PolicySha256
        SignerPinsSha256 = $preflight.SignerPinsSha256
    }
}

if (-not $signedGpuFunctionsOnly) {
    Invoke-WindowsGpuSignedInputs @signedGpuEntryArguments
}
