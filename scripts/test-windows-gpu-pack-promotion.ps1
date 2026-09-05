[CmdletBinding()]
param(
    [string]$CargoTargetDirectory,
    [string]$InteropFixtureDirectory,
    [string]$InteropPublicationDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Get-NormalizedSourceRegionSha256(
    [string]$Source,
    [string]$StartMarker,
    [string]$EndMarker
) {
    $normalized = $Source.Replace("`r`n", "`n").Replace("`r", "`n")
    Assert-True ([regex]::Matches($normalized, [regex]::Escape($StartMarker)).Count -eq 1) 'Pinned source-region start marker is not unique.'
    $start = $normalized.IndexOf($StartMarker, [StringComparison]::Ordinal)
    $end = $normalized.IndexOf($EndMarker, $start + $StartMarker.Length, [StringComparison]::Ordinal)
    Assert-True ($start -ge 0 -and $end -gt $start) 'Pinned source-region boundaries are missing or reversed.'
    $region = $normalized.Substring($start, $end - $start).Trim()
    $bytes = [Text.Encoding]::UTF8.GetBytes($region)
    return ([BitConverter]::ToString([Security.Cryptography.SHA256]::HashData($bytes))).Replace('-', '').ToLowerInvariant()
}

function Invoke-Tool([string]$Tool, [string[]]$Arguments, [switch]$AllowFailure) {
    $output = @(& $Tool @Arguments 2>&1)
    $exitCode = $LASTEXITCODE
    if (-not $AllowFailure -and $exitCode -ne 0) {
        throw "Worker-pack tool failed: $($output -join [Environment]::NewLine)"
    }
    return [pscustomobject]@{ ExitCode = $exitCode; Output = ($output -join [Environment]::NewLine) }
}

function New-PreparedPack([string]$Tool, [string]$Root, [string]$Backend) {
    New-Item -ItemType Directory -Path (Join-Path $Root 'bin') -Force | Out-Null
    [IO.File]::WriteAllBytes((Join-Path $Root 'bin\scribe-inference-worker.exe'), [byte[]](1, 2, 3, 4))
    $backendName = $Backend.ToLowerInvariant()
    $provider = "transcribe-cpp-ggml-$backendName"
    $result = Invoke-Tool $Tool @(
        'prepare-pack', '--backend', $backendName,
        '--pack-id', "scribe-$backendName-windows-x64",
        '--pack-root', $Root, '--pack-version', '0.1.0-promotion-fixture',
        '--provider', $provider, '--security-epoch', '1',
        '--worker-path', 'bin/scribe-inference-worker.exe'
    )
    return ($result.Output | ConvertFrom-Json)
}

function Write-Handoff([string]$Root, $Cuda, $Vulkan, [string]$Revision, [string]$ToolchainDigest) {
    $packs = @(
        [ordered]@{ backend = 'cuda'; pack_root = 'cuda'; pack_id = $Cuda.pack_id; pack_version = $Cuda.pack_version; pack_digest = $Cuda.pack_digest; security_epoch = [uint64]$Cuda.security_epoch; provider = $Cuda.provider; manifest_sha256 = $Cuda.manifest_sha256 },
        [ordered]@{ backend = 'vulkan'; pack_root = 'vulkan'; pack_id = $Vulkan.pack_id; pack_version = $Vulkan.pack_version; pack_digest = $Vulkan.pack_digest; security_epoch = [uint64]$Vulkan.security_epoch; provider = $Vulkan.provider; manifest_sha256 = $Vulkan.manifest_sha256 }
    )
    $material = [ordered]@{
        schema_version = 1
        source_repository = 'tyhuang9/scribe'
        source_ref = 'refs/heads/main'
        source_revision = $Revision
        workflow_ref = 'tyhuang9/scribe/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main'
        run_id = '1001'
        run_attempt = '1'
        pack_version = '0.1.0-promotion-fixture'
        toolchain_manifest_sha256 = $ToolchainDigest
        packs = $packs
    }
    $materialJson = $material | ConvertTo-Json -Depth 8 -Compress
    $materialBytes = [Text.Encoding]::UTF8.GetBytes("scribe-windows-gpu-release-set-v1`0$materialJson")
    $releaseSetDigest = ([BitConverter]::ToString(
        [Security.Cryptography.SHA256]::Create().ComputeHash($materialBytes)
    )).Replace('-', '').ToLowerInvariant()
    $handoff = [ordered]@{} + $material
    $handoff.release_set_digest = $releaseSetDigest
    $path = Join-Path $Root 'windows-gpu-pack-handoff.json'
    [IO.File]::WriteAllText($path, ($handoff | ConvertTo-Json -Depth 8 -Compress), [Text.UTF8Encoding]::new($false))
    return [pscustomobject]@{
        Sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
        ReleaseSetDigest = $releaseSetDigest
    }
}

$repositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$revision = (git -C $repositoryRoot rev-parse --verify HEAD).Trim()
Assert-True ($LASTEXITCODE -eq 0 -and $revision -cmatch '^[0-9a-f]{40}$') 'Could not resolve the exact test source revision.'
$target = if ([string]::IsNullOrWhiteSpace($CargoTargetDirectory)) {
    Join-Path ([IO.Path]::GetTempPath()) "scribe-pack-promotion-cargo-$([guid]::NewGuid().ToString('N'))"
} else { [IO.Path]::GetFullPath($CargoTargetDirectory) }
$testRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-pack-promotion-$([guid]::NewGuid().ToString('N'))"
$previousTarget = $env:CARGO_TARGET_DIR
$previousRevision = $env:SCRIBE_BUILD_REVISION
try {
    if ([string]::IsNullOrWhiteSpace($InteropFixtureDirectory) -ne [string]::IsNullOrWhiteSpace($InteropPublicationDirectory)) {
        throw 'Interoperability fixture and publication directories must be supplied together.'
    }
    $env:CARGO_TARGET_DIR = $target
    $env:SCRIBE_BUILD_REVISION = $revision
    cargo build --locked --offline --manifest-path (Join-Path $repositoryRoot 'tools\worker-pack-author\Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw 'Could not build the fixture worker-pack tool.' }
    $tool = Join-Path $target 'debug\scribe-worker-pack-tool.exe'
    Assert-True (Test-Path -LiteralPath $tool -PathType Leaf) 'Fixture worker-pack tool was not built.'
    New-Item -ItemType Directory -Path $testRoot | Out-Null
    $handoffRoot = Join-Path $testRoot 'handoff'
    New-Item -ItemType Directory -Path $handoffRoot | Out-Null
    $cuda = New-PreparedPack $tool (Join-Path $handoffRoot 'cuda') 'Cuda'
    $vulkan = New-PreparedPack $tool (Join-Path $handoffRoot 'vulkan') 'Vulkan'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $handoffRoot 'cuda\pack-manifest.sig'))) 'Prepared CUDA pack was prematurely signed.'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $handoffRoot 'vulkan\pack-manifest.sig'))) 'Prepared Vulkan pack was prematurely signed.'
    $toolchainDigest = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $repositoryRoot 'runtime-manifests\gpu-worker-toolchain-windows-x64.json')).Hash.ToLowerInvariant()
    $handoff = Write-Handoff $handoffRoot $cuda $vulkan $revision $toolchainDigest
    $artifactDigest = 'a' * 64
    $common = @{
        HandoffDirectory = $handoffRoot
        ExpectedRepository = 'tyhuang9/scribe'
        ExpectedSourceRef = 'refs/heads/main'
        ExpectedSourceRevision = $revision
        ExpectedWorkflowRef = 'tyhuang9/scribe/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main'
        ExpectedRunId = '1001'
        ExpectedRunAttempt = '1'
        ExpectedArtifactId = '2002'
        ExpectedArtifactDigest = $artifactDigest
        ExpectedHandoffSha256 = $handoff.Sha256
        ExpectedReleaseSetDigest = $handoff.ReleaseSetDigest
        ExpectedToolchainManifestSha256 = $toolchainDigest
        ExpectedPackVersion = '0.1.0-promotion-fixture'
        MinimumSecurityEpoch = [uint64]1
        AuthoringToolPath = $tool
    }
    $output = Join-Path $testRoot 'promoted'
    $result = & (Join-Path $PSScriptRoot 'promote-windows-gpu-worker-packs.ps1') @common -OutputDirectory $output -FixtureSigning
    Assert-True ($result.Authority -ceq 'fixture-only') 'Fixture promotion did not identify its non-production authority.'
    foreach ($backend in @('cuda', 'vulkan')) {
        Assert-True (Test-Path -LiteralPath (Join-Path $output "$backend\pack-manifest.sig") -PathType Leaf) "Promoted $backend pack is unsigned."
        $verified = Invoke-Tool $tool @('verify-fixture', '--pack-root', (Join-Path $output $backend))
        $expectedPackDigest = if ($backend -ceq 'cuda') { $cuda.pack_digest } else { $vulkan.pack_digest }
        Assert-True (($verified.Output | ConvertFrom-Json).pack_digest -ceq $expectedPackDigest) "Promoted $backend pack did not verify."
    }

    $wrongOutput = Join-Path $testRoot 'wrong-provenance'
    $wrongRejected = $false
    $wrongCommon = @{} + $common
    $wrongCommon.ExpectedRunAttempt = '2'
    try {
        & (Join-Path $PSScriptRoot 'promote-windows-gpu-worker-packs.ps1') @wrongCommon -OutputDirectory $wrongOutput -FixtureSigning | Out-Null
    } catch { $wrongRejected = $_.Exception.Message.Contains('provenance') }
    Assert-True $wrongRejected 'Mismatched run attempt was not rejected.'
    Assert-True (-not (Test-Path -LiteralPath $wrongOutput)) 'Rejected provenance created a promotion output.'

    foreach ($negative in @(
        [pscustomobject]@{ Name = 'handoff-digest'; Property = 'ExpectedHandoffSha256'; Value = ('0' * 64); Message = 'metadata digest' },
        [pscustomobject]@{ Name = 'release-set'; Property = 'ExpectedReleaseSetDigest'; Value = ('0' * 64); Message = 'provenance' },
        [pscustomobject]@{ Name = 'toolchain'; Property = 'ExpectedToolchainManifestSha256'; Value = ('0' * 64); Message = 'provenance' },
        [pscustomobject]@{ Name = 'version'; Property = 'ExpectedPackVersion'; Value = '0.1.1-promotion-fixture'; Message = 'provenance' },
        [pscustomobject]@{ Name = 'epoch'; Property = 'MinimumSecurityEpoch'; Value = [uint64]2; Message = 'canonical handoff entry' }
    )) {
        $negativeCommon = @{} + $common
        $negativeCommon[$negative.Property] = $negative.Value
        $negativeOutput = Join-Path $testRoot $negative.Name
        $rejected = $false
        try {
            & (Join-Path $PSScriptRoot 'promote-windows-gpu-worker-packs.ps1') @negativeCommon -OutputDirectory $negativeOutput -FixtureSigning | Out-Null
        } catch { $rejected = $_.Exception.Message.Contains($negative.Message) }
        Assert-True $rejected "Promotion did not reject mismatched $($negative.Name) authority."
        Assert-True (-not (Test-Path -LiteralPath $negativeOutput)) "Rejected $($negative.Name) authority created output."
    }

    $collisionRejected = $false
    try {
        & (Join-Path $PSScriptRoot 'promote-windows-gpu-worker-packs.ps1') @common -OutputDirectory $output -FixtureSigning | Out-Null
    } catch { $collisionRejected = $_.Exception.Message.Contains('already exists') }
    Assert-True $collisionRejected 'Promotion accepted an existing output directory.'

    $vulkanRoot = Join-Path $handoffRoot 'vulkan'
    $heldVulkanRoot = Join-Path $testRoot 'held-vulkan'
    Move-Item -LiteralPath $vulkanRoot -Destination $heldVulkanRoot
    try {
        $incompleteOutput = Join-Path $testRoot 'incomplete'
        $incompleteRejected = $false
        try {
            & (Join-Path $PSScriptRoot 'promote-windows-gpu-worker-packs.ps1') @common -OutputDirectory $incompleteOutput -FixtureSigning | Out-Null
        } catch { $incompleteRejected = $_.Exception.Message.Contains('top-level inventory') }
        Assert-True $incompleteRejected 'Incomplete CUDA/Vulkan pair was not rejected.'
        Assert-True (-not (Test-Path -LiteralPath $incompleteOutput)) 'Incomplete pair created promotion output.'
    } finally { Move-Item -LiteralPath $heldVulkanRoot -Destination $vulkanRoot }

    $unexpectedPath = Join-Path $handoffRoot 'unexpected.txt'
    [IO.File]::WriteAllText($unexpectedPath, 'unexpected', [Text.UTF8Encoding]::new($false))
    try {
        $unexpectedOutput = Join-Path $testRoot 'unexpected'
        $unexpectedRejected = $false
        try {
            & (Join-Path $PSScriptRoot 'promote-windows-gpu-worker-packs.ps1') @common -OutputDirectory $unexpectedOutput -FixtureSigning | Out-Null
        } catch { $unexpectedRejected = $_.Exception.Message.Contains('top-level inventory') }
        Assert-True $unexpectedRejected 'Unexpected handoff file was not rejected.'
        Assert-True (-not (Test-Path -LiteralPath $unexpectedOutput)) 'Unexpected handoff file created promotion output.'
    } finally { Remove-Item -LiteralPath $unexpectedPath -Force }

    [IO.File]::AppendAllText((Join-Path $handoffRoot 'vulkan\bin\scribe-inference-worker.exe'), 'tamper', [Text.UTF8Encoding]::new($false))
    $tamperedOutput = Join-Path $testRoot 'tampered'
    $tamperRejected = $false
    try {
        & (Join-Path $PSScriptRoot 'promote-windows-gpu-worker-packs.ps1') @common -OutputDirectory $tamperedOutput -FixtureSigning | Out-Null
    } catch { $tamperRejected = $_.Exception.Message.Contains('validation failed') }
    Assert-True $tamperRejected 'Tampered second prepared payload was not rejected.'
    Assert-True (-not (Test-Path -LiteralPath $tamperedOutput)) 'Rejected second payload left partial promotion output.'

    $productionOutput = Join-Path $testRoot 'production'
    $productionClosed = $false
    try {
        & (Join-Path $PSScriptRoot 'promote-windows-gpu-worker-packs.ps1') @common -OutputDirectory $productionOutput | Out-Null
    } catch { $productionClosed = $_.Exception.Message.Contains('independently installed protected signer') }
    Assert-True $productionClosed 'Repository promotion script accepted production authority.'
    Assert-True (-not (Test-Path -LiteralPath $productionOutput)) 'Production fail-closed gate created output.'

    $workflow = Get-Content -LiteralPath (Join-Path $repositoryRoot '.github\workflows\windows-gpu-pack-promotion.yml') -Raw
    $protected = $workflow.Split('  protected-promote:', 2)[1]
    Assert-True ($workflow.Contains('environment: windows-gpu-pack-signing')) 'Protected environment gate is missing.'
    Assert-True ($workflow.Contains('steps.upload.outputs.artifact-digest')) 'Unsigned artifact digest is not bound across jobs.'
    Assert-True ($workflow.Contains('steps.upload.outputs.artifact-id')) 'Unsigned artifact ID is not bound across jobs.'
    Assert-True ($workflow.Contains('cargo fetch --locked --manifest-path tools/worker-pack-author/Cargo.toml')) 'Clean hosted runners do not fetch the locked worker-pack tool dependencies before offline testing.'
    Assert-True ($workflow.Contains('cargo fetch --locked --manifest-path tools/windows-gpu-promotion-broker/Cargo.toml')) 'Clean hosted runners do not fetch the independently locked broker-contract dependencies.'
    Assert-True ($workflow.Contains('cargo test --locked --offline --manifest-path tools/windows-gpu-promotion-broker/Cargo.toml')) 'Hosted contract validation does not exercise the locked offline broker state-machine proof.'
    Assert-True ($workflow.Contains('test-windows-gpu-broker-transport.ps1 -RequireScmIntegration')) 'Hosted contract validation does not exercise the exact restricted-service transport.'
    Assert-True ($workflow.Contains("- 'scripts/provision-windows-gpu-broker-client-policy.ps1'")) 'Client policy provisioner changes do not trigger hosted broker verification.'
    Assert-True ($workflow.Contains('github.event.repository.default_branch')) 'Production dispatch is not restricted to the default branch.'
    Assert-True ($protected.Contains('SCRIBE_WINDOWS_GPU_TRUSTED_CLIENT_SHA256')) 'Protected broker-client digest is not independently configured.'
    Assert-True ($protected.Contains('SCRIBE_WINDOWS_GPU_PRODUCTION_BROKER_PROVISIONED')) 'Separately privileged broker provisioning gate is missing.'
    Assert-True ($protected.Contains('SCRIBE_WINDOWS_GPU_AUTHORIZED_CLIENT_SID')) 'Protected workflow client SID variable is missing.'
    Assert-True ($protected.Contains('[Security.Principal.WindowsIdentity]::GetCurrent().User.Value')) 'Protected runner does not inspect its exact TokenUser SID.'
    Assert-True ($protected.Contains('$currentClientSid -cne $configuredClientSid.Value')) 'Protected runner does not compare exact configured and current client SIDs.'
    $identityCheck = $protected.IndexOf('$currentClientSid -cne $configuredClientSid.Value', [StringComparison]::Ordinal)
    $brokerGate = $protected.IndexOf('$env:PRODUCTION_BROKER_PROVISIONED -cne', [StringComparison]::Ordinal)
    Assert-True ($identityCheck -ge 0 -and $identityCheck -lt $brokerGate) 'Protected runner identity preflight does not precede the closed broker gate.'
    Assert-True ($protected.Contains('actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c')) 'Protected artifact download is not pinned to the reviewed v8.0.1 action.'
    Assert-True ($protected.Contains('digest-mismatch: error')) 'Protected artifact download does not fail closed on a digest mismatch.'
    Assert-True ($protected.Contains('--require-unused-release-set')) 'Trusted signer interface does not require replay rejection.'
    Assert-True ($protected.Contains('no filesystem, ledger, or signing authority was accessed')) 'Unprovisioned production path does not fail before client invocation.'
    Assert-True ($protected.Contains('[IO.FileShare]::Read')) 'Protected workflow does not retain a no-write/delete client handle.'
    Assert-True ($protected.Contains('provide no-follow open semantics or pin path ancestors')) 'Protected workflow overstates its leaf handle authority.'
    Assert-True ($protected.Contains('$processInfo.ArgumentList.Add')) 'Protected workflow does not use the structured child-process argument API.'
    Assert-True ($protected.Contains('scribe-gpu-pack-signer-ephemeral')) 'Protected signer runner is not required to be ephemeral.'
    Assert-True (-not $protected.Contains('actions/checkout@')) 'Protected signing job checks out candidate source.'
    Assert-True (-not $protected.Contains('cargo ')) 'Protected signing job compiles candidate source.'
    Assert-True (-not $protected.Contains('promote-windows-gpu-worker-packs.ps1')) 'Protected job runs a repository-owned promotion script.'
    Assert-True (-not $workflow.Contains('secrets.')) 'Promotion workflow exposes raw private-key secrets to repository jobs.'
    Assert-True (-not $protected.Contains('--private-key')) 'Protected broker client accepts a raw key path.'
    Assert-True (-not $protected.Contains('--ledger-root')) 'Ephemeral runner configures durable broker state.'
    Assert-True (-not $protected.Contains('--broker-endpoint')) 'Ephemeral runner can redirect broker authority.'
    $brokerContract = Get-Content -LiteralPath (Join-Path $repositoryRoot 'tools\windows-gpu-promotion-broker\src\lib.rs') -Raw
    Assert-True ($brokerContract.Contains('pub struct PromotionIntent')) 'Broker contract lost its path-free promotion intent.'
    Assert-True ($brokerContract.Contains('pub struct ClientInvocation')) 'Broker contract lost its process-local invocation wrapper.'
    Assert-True ($brokerContract.Contains('self.workflow_source_sha != self.source_revision')) 'Broker intent does not bind workflow source to default-branch pack source.'
    Assert-True ($brokerContract.Contains('scribe-windows-gpu-promotion-intent-v1')) 'Broker intent digest is not domain separated.'
    $brokerNative = Get-Content -LiteralPath (Join-Path $repositoryRoot 'tools\windows-gpu-promotion-broker\src\windows_native.rs') -Raw
    $brokerNativeProduction = $brokerNative.Split('#[cfg(test)]', 2)[0]
    Assert-True ($brokerNativeProduction.Contains('SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization')) 'Broker lost its fixed machine-wide client policy path.'
    Assert-True ($brokerNativeProduction.Contains('KEY_READ | KEY_WOW64_64KEY')) 'Broker no longer opens only the fixed 64-bit policy view for read.'
    Assert-True ($brokerNativeProduction.Contains('RegEnumValueW')) 'Broker does not enumerate exact policy value names before case-insensitive lookup.'
    Assert-True ($brokerNativeProduction.Contains('actual_names.as_slice() != expected_names.as_slice()')) 'Broker does not compare policy value-name spelling ordinally.'
    Assert-True ($brokerNativeProduction.Contains('require_user_sid(token.raw(), authorized_client_sid)')) 'Broker does not require exact client TokenUser equality.'
    foreach ($requiredKernelAccessBoundary in @('LookupAccountSidW', 'SidTypeUser', 'GetKernelObjectSecurity', 'SetKernelObjectSecurity', 'TokenSessionId', 'PROCESS_CLIENT_QUERY_ACCESS', 'TOKEN_CLIENT_QUERY_ACCESS', 'require_exact_query_acl_delta')) {
        Assert-True ($brokerNativeProduction.Contains($requiredKernelAccessBoundary)) "Broker lost kernel-object query grant boundary $requiredKernelAccessBoundary."
    }
    foreach ($forbiddenKernelAccessMechanism in @('ProcessIdToSessionId', 'SetTokenInformation', 'TokenDefaultDacl', 'AdjustTokenPrivileges')) {
        Assert-True (-not $brokerNativeProduction.Contains($forbiddenKernelAccessMechanism)) "Broker regained forbidden kernel-object access mechanism $forbiddenKernelAccessMechanism."
    }
    Assert-True (-not $brokerNativeProduction.Contains(';;;AU)')) 'Broker pipe regained Authenticated Users admission.'
    $provisioner = Get-Content -LiteralPath (Join-Path $repositoryRoot 'scripts\provision-windows-gpu-broker-client-policy.ps1') -Raw
    Assert-True ($provisioner.Contains('RegCreateKeyExW')) 'Client policy provisioner is not create-new.'
    $createMethodStart = $provisioner.IndexOf('public static int CreateProtectedKey(', [StringComparison]::Ordinal)
    $createMethodEnd = $provisioner.IndexOf('public static int OpenExistingKeyNoFollow(', $createMethodStart, [StringComparison]::Ordinal)
    Assert-True ($createMethodStart -ge 0 -and $createMethodEnd -gt $createMethodStart) 'Client policy provisioner lost its bounded native create helper.'
    $createMethod = $provisioner.Substring($createMethodStart, $createMethodEnd - $createMethodStart)
    Assert-True ($createMethod.Contains('ref securityAttributes')) 'Client policy provisioner does not pass non-null creation security attributes.'
    Assert-True ($provisioner.Contains('O:SYD:P(A;;KA;;;SY)(A;;KA;;;BA)(A;;KR;;;')) 'Client policy provisioner does not create the final SYSTEM-owned protected DACL atomically.'
    Assert-True ($provisioner.Contains('OpenExistingKeyNoFollow')) 'Client policy provisioner follows registry links in the fixed ancestor chain.'
    Assert-True ($provisioner.Contains('REG_OPTION_OPEN_LINK')) 'Client policy provisioner does not inspect registry links themselves.'
    Assert-True ($provisioner.Contains('SymbolicLinkValue')) 'Client policy provisioner does not identify registry link keys.'
    Assert-True ($provisioner.Contains('$mutationMask = [uint32]0x500d0026')) 'Client policy provisioner no longer rejects ancestor mutation authority.'
    $creatorOwnerPredicateName = 'Test-StandardSoftwareCreatorOwnerInheritanceTemplate'
    Assert-True (($provisioner.Split($creatorOwnerPredicateName, [StringSplitOptions]::None).Count - 1) -eq 2) 'Client policy provisioner must define and call its CREATOR OWNER predicate exactly once.'
    $creatorOwnerPredicateStart = $provisioner.IndexOf("function $creatorOwnerPredicateName(", [StringComparison]::Ordinal)
    $rawAclClassifierName = 'Test-SafePolicyAncestorAcl'
    Assert-True (($provisioner.Split($rawAclClassifierName, [StringSplitOptions]::None).Count - 1) -eq 2) 'Client policy provisioner must define and call its raw ancestor-DACL classifier exactly once.'
    $rawAclClassifierStart = $provisioner.IndexOf("function $rawAclClassifierName(", $creatorOwnerPredicateStart, [StringComparison]::Ordinal)
    Assert-True ($creatorOwnerPredicateStart -ge 0 -and $rawAclClassifierStart -gt $creatorOwnerPredicateStart) 'Client policy provisioner lost its bounded raw CREATOR OWNER predicate.'
    $creatorOwnerPredicate = $provisioner.Substring($creatorOwnerPredicateStart, $rawAclClassifierStart - $creatorOwnerPredicateStart)
    foreach ($requiredPredicateComparison in @(
        "`$Path -ceq 'SOFTWARE'",
        '$Ace -is [Security.AccessControl.CommonAce]',
        '-not $Ace.IsCallback',
        '$Ace.AceType -eq [Security.AccessControl.AceType]::AccessAllowed',
        '$Ace.AceQualifier -eq [Security.AccessControl.AceQualifier]::AccessAllowed',
        '$Ace.AceFlags -eq [Security.AccessControl.AceFlags]::ContainerInherit',
        '[uint32]$Ace.AccessMask -eq [uint32]0x000f003f',
        "`$Ace.SecurityIdentifier.Value -ceq 'S-1-3-0'",
        '$Ace.OpaqueLength -eq 0'
    )) {
        Assert-True ($creatorOwnerPredicate.Contains($requiredPredicateComparison)) "Client policy CREATOR OWNER predicate lost exact comparison $requiredPredicateComparison."
    }
    $ancestorValidationStart = $provisioner.IndexOf('function Assert-SafePolicyAncestor(', $rawAclClassifierStart, [StringComparison]::Ordinal)
    Assert-True ($ancestorValidationStart -gt $rawAclClassifierStart) 'Client policy provisioner lost its bounded raw ancestor-DACL classifier.'
    $rawAclClassifier = $provisioner.Substring($rawAclClassifierStart, $ancestorValidationStart - $rawAclClassifierStart)
    foreach ($requiredRawClassifierGuard in @(
        '$null -eq $Acl',
        '$creatorOwnerTemplateCount -gt 1',
        'Test-StandardSoftwareCreatorOwnerInheritanceTemplate -Ace $ace -Path $Path',
        '$ace -isnot [Security.AccessControl.QualifiedAce]',
        '$ace.AceQualifier -eq [Security.AccessControl.AceQualifier]::AccessDenied',
        '$ace.AceQualifier -ne [Security.AccessControl.AceQualifier]::AccessAllowed',
        '([uint32]$ace.AccessMask -band $MutationMask) -eq 0',
        '$TrustedOwners -cnotcontains $ace.SecurityIdentifier.Value'
    )) {
        Assert-True ($rawAclClassifier.Contains($requiredRawClassifierGuard)) "Client policy raw ancestor-DACL classifier lost guard $requiredRawClassifierGuard."
    }
    foreach ($forbiddenPredicateMutation in @(
        'SetAccessControl',
        'SetAccessRule',
        'AddAccessRule',
        'RemoveAccessRule',
        'SetValue',
        'DeleteValue',
        'CreateSubKey',
        'RegCreateKey',
        'RegDeleteKey',
        'RegSetValue',
        'NtDeleteKey'
    )) {
        Assert-True (-not $creatorOwnerPredicate.Contains($forbiddenPredicateMutation)) "Client policy CREATOR OWNER predicate gained forbidden mutation API $forbiddenPredicateMutation."
        Assert-True (-not $rawAclClassifier.Contains($forbiddenPredicateMutation)) "Client policy raw ancestor-DACL classifier gained forbidden mutation API $forbiddenPredicateMutation."
    }
    $ancestorValidationEnd = $provisioner.IndexOf('function Assert-Policy(', $ancestorValidationStart, [StringComparison]::Ordinal)
    Assert-True ($ancestorValidationEnd -gt $ancestorValidationStart) 'Client policy provisioner lost its bounded ancestor validator.'
    $ancestorValidationBody = $provisioner.Substring($ancestorValidationStart, $ancestorValidationEnd - $ancestorValidationStart)
    Assert-True ($ancestorValidationBody.Contains('[Security.AccessControl.RawSecurityDescriptor]::new($security.GetSecurityDescriptorBinaryForm(), 0)')) 'Ancestor validation no longer obtains the complete raw DACL.'
    Assert-True ($ancestorValidationBody.Contains('Test-SafePolicyAncestorAcl -Acl $raw.DiscretionaryAcl -Path $Path -TrustedOwners $trustedOwners -MutationMask $mutationMask')) 'Ancestor validation no longer classifies the complete raw DACL at its sole trust decision.'
    Assert-True (-not $ancestorValidationBody.Contains('GetAccessRules(')) 'Ancestor validation regressed to projected RegistryAccessRules that can omit raw ACE types.'
    Assert-True ($provisioner.Contains('ProvisioningState')) 'Client policy provisioner lacks an incomplete-policy marker.'
    Assert-True ($provisioner.Contains('[string]$InvocationNonce')) 'Client policy provisioner does not bind success to a caller nonce.'
    Assert-True ($provisioner.Contains('REG_CREATED_NEW_KEY')) 'Client policy provisioner does not distinguish exact create-new dispositions.'
    Assert-True ($provisioner.Contains('created_ancestors = @($createdAncestorPaths)')) 'Client policy provisioner does not report its exact newly created ancestors.'
    Assert-True ($provisioner.Contains('scribe-windows-gpu-broker-client-policy-provisioning-success-v1')) 'Client policy provisioner lacks a versioned success-record kind.'
    Assert-True ($provisioner.Contains('RestorePrivilegeScope : IDisposable')) 'Client policy provisioner does not scope SeRestorePrivilege lifetime.'
    Assert-True ($provisioner.Contains('AdjustTokenPrivilegesAndCapturePrevious')) 'Client policy provisioner does not capture the prior TOKEN_PRIVILEGES state.'
    Assert-True ($provisioner.Contains('RestoreTokenPrivileges')) 'Client policy provisioner does not restore the captured TOKEN_PRIVILEGES state.'
    Assert-True ($provisioner.Contains('private bool restorationComplete;')) 'Client policy provisioner does not distinguish restored state from released token ownership.'
    Assert-True ($provisioner.Contains('public static void RestoreOrFailFast(RestorePrivilegeScope scope)')) 'Client policy provisioner lacks its mandatory outer restoration boundary.'
    Assert-True ($provisioner.Contains('Environment.FailFast(')) 'Client policy provisioner can return after persistent privilege restoration failure.'
    Assert-True (-not $provisioner.Contains('PrivilegeRestoreFailureEvidencePath')) 'Client policy provisioner exposes the test-only privilege failure path.'
    Assert-True (-not $provisioner.Contains('restoreFailuresRemaining')) 'Client policy provisioner exposes deterministic test failure injection.'
    Assert-True ($provisioner.Contains('RegistryView]::Registry64')) 'Client policy provisioner does not pin the 64-bit registry view.'
    Assert-True (-not $provisioner.Contains('[string]$AccountName')) 'Client policy provisioner accepts an account name.'
    $bornProtected = $provisioner.LastIndexOf('Assert-PolicySecurity -Key $key', [StringComparison]::Ordinal)
    $firstValueWrite = $provisioner.IndexOf('$key.SetValue($provisioningValue', [StringComparison]::Ordinal)
    Assert-True ($bornProtected -ge 0 -and $bornProtected -lt $firstValueWrite) 'Client policy provisioner writes values before verifying create-time protection.'
    $ancestorValidation = $provisioner.IndexOf('foreach ($ancestorPath in $policyAncestors)', [StringComparison]::Ordinal)
    $leafCreation = $provisioner.IndexOf('$status = [Scribe.GpuBroker.RegistryNative]::CreateProtectedKey(', [StringComparison]::Ordinal)
    Assert-True ($ancestorValidation -ge 0 -and $ancestorValidation -lt $leafCreation) 'Client policy provisioner creates the leaf before validating and protecting its ancestor chain.'
    $privilegeEnable = $provisioner.IndexOf('$restorePrivilegeScope = [Scribe.GpuBroker.RegistryNative]::EnableRestorePrivilege()', [StringComparison]::Ordinal)
    $privilegeRestoreBeforeCommit = $provisioner.IndexOf('$restorePrivilegeScope.Dispose()', $privilegeEnable, [StringComparison]::Ordinal)
    $policyCommit = $provisioner.IndexOf('$key.DeleteValue($provisioningValue', [StringComparison]::Ordinal)
    $privilegeRestore = $provisioner.LastIndexOf('[Scribe.GpuBroker.RegistryNative]::RestoreOrFailFast($restorePrivilegeScope)', [StringComparison]::Ordinal)
    $successEmission = $provisioner.LastIndexOf('$successRecord | ConvertTo-Json', [StringComparison]::Ordinal)
    Assert-True ($privilegeEnable -ge 0 -and $privilegeEnable -lt $ancestorValidation -and $ancestorValidation -lt $privilegeRestoreBeforeCommit -and $privilegeRestoreBeforeCommit -lt $policyCommit) 'Client policy provisioner can commit policy before restoring its exact privilege scope.'
    Assert-True ($privilegeRestore -gt $policyCommit -and $privilegeRestore -lt $successEmission) 'Client policy provisioner lacks retry-or-terminate restoration before success output.'
    $restoreScopeStart = $provisioner.IndexOf('public sealed class RestorePrivilegeScope : IDisposable', [StringComparison]::Ordinal)
    $restoreScopeEnd = $provisioner.IndexOf('public static void RestoreOrFailFast(RestorePrivilegeScope scope)', $restoreScopeStart, [StringComparison]::Ordinal)
    Assert-True ($restoreScopeStart -ge 0 -and $restoreScopeEnd -gt $restoreScopeStart) 'Client policy provisioner lost its bounded restoration scope implementation.'
    $restoreScope = $provisioner.Substring($restoreScopeStart, $restoreScopeEnd - $restoreScopeStart)
    $nativeRestore = $restoreScope.IndexOf('if (!RestoreTokenPrivileges(', [StringComparison]::Ordinal)
    $restoredState = $restoreScope.IndexOf('restorationComplete = true;', [StringComparison]::Ordinal)
    $closeToken = $restoreScope.IndexOf('if (!CloseHandle(token))', [StringComparison]::Ordinal)
    $releaseToken = $restoreScope.IndexOf('token = IntPtr.Zero;', [StringComparison]::Ordinal)
    $releasePrevious = $restoreScope.IndexOf('previousState = default(TokenPrivileges);', [StringComparison]::Ordinal)
    Assert-True ($nativeRestore -ge 0 -and $nativeRestore -lt $restoredState -and $restoredState -lt $closeToken -and $closeToken -lt $releaseToken -and $releaseToken -lt $releasePrevious) 'Client policy provisioner can discard token or prior-state ownership before exact restore and successful close.'
    $transportHarness = Get-Content -LiteralPath (Join-Path $repositoryRoot 'scripts\test-windows-gpu-broker-transport.ps1') -Raw
    $authenticatedCallAssignment = '$roundTrip = Invoke-EphemeralProcess -FilePath $clientForCredential -Arguments $arguments -TimeoutSeconds 20 -AllowFailure'
    $sanitizationHelpersDigest = Get-NormalizedSourceRegionSha256 `
        -Source $transportHarness `
        -StartMarker 'function Test-ServerAccessProbeRecord' `
        -EndMarker 'function Test-SanitizedClientDiagnosticCategoryContract'
    Assert-True ($sanitizationHelpersDigest -ceq 'f2e1655725a4834ba9465397c63a78b59ef96ca70e279c06a8505c0accd71c93') 'Sanitized diagnostic helpers changed; review their exact output behavior before updating the pinned digest.'
    $authenticatedRegionDigest = Get-NormalizedSourceRegionSha256 `
        -Source $transportHarness `
        -StartMarker $authenticatedCallAssignment `
        -EndMarker '$stop = Invoke-Sc -Arguments @(''stop'', $serviceName) -AllowFailure'
    Assert-True ($authenticatedRegionDigest -ceq 'ccd48fcd90b0e5ea291e047f18cb2b614077254371141bb31a8aad171a67cb7c') 'Authenticated real-client region changed; review every captured-output use before updating the pinned digest.'
    $preBlockAliasMutation = $transportHarness.Replace(
        $authenticatedCallAssignment,
        $authenticatedCallAssignment + "`r`n    `$roundTripAlias = `$roundTrip`r`n    throw `$roundTripAlias.PSObject.Properties['Stderr'].Value"
    )
    Assert-True ((Get-NormalizedSourceRegionSha256 `
        -Source $preBlockAliasMutation `
        -StartMarker $authenticatedCallAssignment `
        -EndMarker '$stop = Invoke-Sc -Arguments @(''stop'', $serviceName) -AllowFailure') -cne $authenticatedRegionDigest) 'Pinned authenticated region did not detect a pre-block indirect Stderr leak.'
    Assert-True ($transportHarness.Contains('Assert-OwnedPolicyState -State $state')) 'Broker harness cleanup does not revalidate exact ownership state.'
    Assert-True ($transportHarness.Contains('SecurityFingerprint')) 'Broker harness cleanup does not pin the policy security descriptor.'
    Assert-True ($transportHarness.Contains('CleanupTamper')) 'Broker harness lacks an adversarial same-name cleanup test.'
    Assert-True ($transportHarness.Contains('if ($result.ExitCode -eq 0)')) 'Broker harness claims policy ownership without successful provisioning.'
    Assert-True ($transportHarness.Contains('New-ProvisioningInvocationNonce')) 'Broker harness does not generate a fresh provisioning correlation nonce.'
    Assert-True ($transportHarness.Contains('Read-ProvisioningSuccessRecord')) 'Broker harness does not validate the provisioner success record.'
    Assert-True ($transportHarness.Contains('$script:ownedPolicyAncestors = @($ownedPolicyAncestors) + @($createdByInvocation)')) 'Broker harness does not derive ancestor ownership solely from the validated success record.'
    Assert-True (-not $transportHarness.Contains('initiallyMissingPolicyAncestors')) 'Broker harness infers ancestor ownership from an initial missing-path snapshot.'
    Assert-True ($transportHarness.Contains('NtDeleteKey(SafeRegistryHandle keyHandle)')) 'Broker harness cleanup is not bound to an exact live registry handle.'
    Assert-True ($transportHarness.Contains('RegRenameKey(')) 'Broker harness lacks an exact registry-object boundary-swap test.'
    Assert-True ($transportHarness.Contains('KEY_WRITE | DELETE | KEY_QUERY_VALUE | KEY_WOW64_64KEY')) 'Broker harness boundary rename lacks retained parent/leaf mutation rights.'
    Assert-True ($transportHarness.Contains('DELETE | READ_CONTROL | KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS | KEY_WOW64_64KEY')) 'Broker harness cleanup does not open the exact no-follow key with delete and validation rights.'
    $constructorNormalizationStart = $transportHarness.IndexOf('function Test-FileSystemAccessRuleConstructorNormalization', [StringComparison]::Ordinal)
    $constructorNormalizationCall = $transportHarness.LastIndexOf('Test-FileSystemAccessRuleConstructorNormalization', [StringComparison]::Ordinal)
    $constructorNormalizationNonElevatedReturn = $transportHarness.IndexOf("if (-not `$isElevated)", [StringComparison]::Ordinal)
    Assert-True ($constructorNormalizationStart -ge 0 -and $constructorNormalizationStart -lt $constructorNormalizationCall -and $constructorNormalizationCall -lt $constructorNormalizationNonElevatedReturn) 'Broker harness does not run its pure FileSystemAccessRule constructor-normalization test before the non-elevated return.'
    foreach ($requiredConstructorNormalization in @(
        '0x000200a9',
        '0x001200a9',
        '$persistedRights',
        'FileSystemAccessRule constructor did not normalize',
        '[Security.AccessControl.FileSystemRights]::ReadAndExecute',
        '[Security.AccessControl.FileSystemRights]::Synchronize',
        '[Security.AccessControl.FileSystemRights]::Write',
        '[Security.AccessControl.FileSystemRights]::Delete',
        '[Security.AccessControl.FileSystemRights]::ChangePermissions',
        '[Security.AccessControl.FileSystemRights]::TakeOwnership',
        '$persistedReadAndExecuteRights'
    )) {
        Assert-True ($transportHarness.Contains($requiredConstructorNormalization)) "Broker harness lost FileSystemAccessRule constructor-normalization contract $requiredConstructorNormalization."
    }
    $commandLineContractStart = $transportHarness.IndexOf('function Test-CredentialCommandLineContract', [StringComparison]::Ordinal)
    $commandLineContractCall = $transportHarness.LastIndexOf('Test-CredentialCommandLineContract', [StringComparison]::Ordinal)
    $commandLineContractNonElevatedReturn = $transportHarness.IndexOf("if (-not `$isElevated)", [StringComparison]::Ordinal)
    Assert-True ($commandLineContractStart -ge 0 -and $commandLineContractStart -lt $commandLineContractCall -and $commandLineContractCall -lt $commandLineContractNonElevatedReturn) 'Broker harness does not run its pure credentialed command-line contract before the non-elevated return.'
    foreach ($requiredCommandLineContract in @(
        'public static class CredentialCommandLine',
        'CreateProcessWithLogonMaximumUtf16Units = 1024',
        'ReservedUtf16Units = 64',
        'MaximumUtf16UnitsIncludingNull = 960',
        'Credentialed command-line bound lost its fixed 64-unit reserve below the native 1024-unit ceiling.',
        'Render(fileName, arguments).Length + 1',
        'Char.IsWhiteSpace(value)',
        "commandLine.Append('\\', checked(backslashes * 2 + 1))",
        "commandLine.Append('\\', checked(backslashes * 2))",
        'Executable path is not canonical for credentialed launch.',
        'Credentialed launch arguments cannot contain null.',
        'Credentialed launch arguments cannot contain NUL.',
        'surrogate pair',
        'non-ASCII whitespace',
        'backslashes before quote',
        'quoted trailing backslash',
        'requires 961 UTF-16 units including NUL; limit is 960.',
        '$acceptedLength -eq 960',
        '$maximumShapeLength -le 960',
        '$maximumArguments = New-ValidClientArguments',
        '$null -eq $script:activeCredentialProcess',
        'Credentialed command-line preflight failure started or adopted a process.'
    )) {
        Assert-True ($transportHarness.Contains($requiredCommandLineContract)) "Broker harness lost credentialed command-line contract $requiredCommandLineContract."
    }
    $credentialStartFunctionStart = $transportHarness.IndexOf('function Start-EphemeralProcess', [StringComparison]::Ordinal)
    $credentialStartFunctionEnd = $transportHarness.IndexOf('function Test-CredentialCommandLineContract', $credentialStartFunctionStart, [StringComparison]::Ordinal)
    Assert-True ($credentialStartFunctionStart -ge 0 -and $credentialStartFunctionEnd -gt $credentialStartFunctionStart) 'Broker harness lost its bounded credentialed process start helper.'
    $credentialStartFunction = $transportHarness.Substring($credentialStartFunctionStart, $credentialStartFunctionEnd - $credentialStartFunctionStart)
    $immutableArgumentCopy = $credentialStartFunction.IndexOf('[string[]]$Arguments.Clone()', [StringComparison]::Ordinal)
    $credentialLengthPreflight = $credentialStartFunction.IndexOf('CredentialCommandLine]::ValidateLength($FilePath, $immutableArguments)', [StringComparison]::Ordinal)
    $credentialStructuredArguments = $credentialStartFunction.IndexOf('New-EphemeralProcessStartInfo -FilePath $FilePath -Arguments $immutableArguments', [StringComparison]::Ordinal)
    $credentialProcessStart = $credentialStartFunction.IndexOf('[Diagnostics.Process]::Start($start)', [StringComparison]::Ordinal)
    $credentialProcessOwnership = $credentialStartFunction.IndexOf('$script:activeCredentialProcess = $process', [StringComparison]::Ordinal)
    Assert-True ($immutableArgumentCopy -ge 0 -and $immutableArgumentCopy -lt $credentialLengthPreflight -and $credentialLengthPreflight -lt $credentialStructuredArguments -and $credentialStructuredArguments -lt $credentialProcessStart -and $credentialProcessStart -lt $credentialProcessOwnership) 'Credentialed launch does not clone, preflight, populate ArgumentList, start, and then establish exact process ownership in order.'
    $credentialStartInfoStart = $transportHarness.IndexOf('function New-EphemeralProcessStartInfo', [StringComparison]::Ordinal)
    $credentialStartInfoEnd = $transportHarness.IndexOf('function Start-EphemeralProcess', $credentialStartInfoStart, [StringComparison]::Ordinal)
    Assert-True ($credentialStartInfoStart -ge 0 -and $credentialStartInfoEnd -gt $credentialStartInfoStart) 'Broker harness lost its bounded credentialed ProcessStartInfo builder.'
    $credentialStartInfo = $transportHarness.Substring($credentialStartInfoStart, $credentialStartInfoEnd - $credentialStartInfoStart)
    Assert-True ($credentialStartInfo.Contains('foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }')) 'Credentialed ProcessStartInfo does not populate structured ArgumentList from the preflighted immutable copy.'
    Assert-True ([regex]::Matches($transportHarness, [regex]::Escape('$candidateToken = New-CryptographicHex -ByteCount 16')).Count -eq 1) 'Broker harness must generate one 128-bit token per attempted three-path fixture set.'
    $noTouchBuilderStart = $transportHarness.IndexOf('function Get-ValidatedNoTouchPath', [StringComparison]::Ordinal)
    $noTouchBuilderEnd = $transportHarness.IndexOf('function Assert-NoTouchPathsRemainAbsent', $noTouchBuilderStart, [StringComparison]::Ordinal)
    Assert-True ($noTouchBuilderStart -ge 0 -and $noTouchBuilderEnd -gt $noTouchBuilderStart) 'Broker harness lost its bounded no-touch path validator.'
    $noTouchBuilder = $transportHarness.Substring($noTouchBuilderStart, $noTouchBuilderEnd - $noTouchBuilderStart)
    Assert-True (-not $noTouchBuilder.Contains('New-Item') -and -not $noTouchBuilder.Contains('Remove-Item') -and -not $noTouchBuilder.Contains('Set-Acl')) 'No-touch path validation creates, reserves, adopts, deletes, or changes ACLs on a drive-root path.'
    Assert-True (-not $transportHarness.Contains('New-Item -ItemType Directory -Path $handoff') -and -not $transportHarness.Contains('New-Item -ItemType Directory -Path $output')) 'Broker harness reserves a no-touch client path.'
    Assert-True (-not $transportHarness.Contains('Set-Acl -LiteralPath $handoff') -and -not $transportHarness.Contains('Set-Acl -LiteralPath $output') -and -not $transportHarness.Contains('Set-Acl -LiteralPath $systemVolumeRoot')) 'Broker harness mutates a no-touch path or drive-root ACL.'
    $pathSetSelectorStart = $transportHarness.IndexOf('function Select-AvailableFixturePathSet', [StringComparison]::Ordinal)
    $pathSetSelectorEnd = $transportHarness.IndexOf('function Test-FixturePathSetAvailabilityContract', $pathSetSelectorStart, [StringComparison]::Ordinal)
    Assert-True ($pathSetSelectorStart -ge 0 -and $pathSetSelectorEnd -gt $pathSetSelectorStart) 'Broker harness lost its bounded three-path collision selector.'
    $pathSetSelector = $transportHarness.Substring($pathSetSelectorStart, $pathSetSelectorEnd - $pathSetSelectorStart)
    foreach ($requiredSelectorContract in @(
        '$maximumAttempts = 8',
        '$attempt -lt $maximumAttempts',
        '$candidateToken = New-CryptographicHex -ByteCount 16',
        '-Token $candidateToken',
        'Test-Path -LiteralPath $candidateMachineTarget',
        'Test-Path -LiteralPath $candidateHandoff',
        'Test-Path -LiteralPath $candidateOutput',
        '$candidateSetAvailable = Test-FixturePathSetAvailable',
        'if (-not $candidateSetAvailable) { continue }',
        'Token = $candidateToken',
        'MachineTarget = $candidateMachineTarget',
        'Handoff = $candidateHandoff',
        'Output = $candidateOutput',
        'after $maximumAttempts attempts.'
    )) {
        Assert-True ($pathSetSelector.Contains($requiredSelectorContract)) "Broker harness lost bounded collision-selection contract $requiredSelectorContract."
    }
    Assert-True ([regex]::Matches($pathSetSelector, [regex]::Escape('-Token $candidateToken')).Count -eq 3) 'Every attempted staging/handoff/output path must derive from the same single candidate token.'
    Assert-True (-not $pathSetSelector.Contains('New-Item') -and -not $pathSetSelector.Contains('Remove-Item') -and -not $pathSetSelector.Contains('Set-Acl')) 'Three-path collision selection mutates, reserves, adopts, or deletes a candidate path.'
    $availabilityTestStart = $transportHarness.IndexOf('function Test-FixturePathSetAvailabilityContract', [StringComparison]::Ordinal)
    $availabilityTestCall = $transportHarness.LastIndexOf('Test-FixturePathSetAvailabilityContract', [StringComparison]::Ordinal)
    $availabilityTestNonElevatedReturn = $transportHarness.IndexOf("if (-not `$isElevated)", [StringComparison]::Ordinal)
    Assert-True ($availabilityTestStart -ge 0 -and $availabilityTestStart -lt $availabilityTestCall -and $availabilityTestCall -lt $availabilityTestNonElevatedReturn) 'Broker harness does not run deterministic three-path collision coverage before its non-elevated return.'
    $serverAccessNative = $transportHarness.IndexOf('public static class ServerAccessProbeNative', [StringComparison]::Ordinal)
    $serverAccessDispatch = $transportHarness.IndexOf('if ($RunEphemeralServerAccessProbe)', [StringComparison]::Ordinal)
    $mainNativeTypes = $transportHarness.IndexOf("if (-not ('Scribe.GpuBroker.RegistryCleanupNative' -as [type]))", [StringComparison]::Ordinal)
    Assert-True ($serverAccessNative -ge 0 -and $serverAccessNative -lt $serverAccessDispatch -and $serverAccessDispatch -lt $mainNativeTypes) 'Server-access native probe is not available inside the early credential-child dispatcher.'
    foreach ($requiredServerAccessProbe in @('PROCESS_QUERY_LIMITED_INFORMATION = 0x1000', 'TOKEN_QUERY = 0x0008', 'ProcessIdToSessionId', 'OpenProcess(', 'OpenProcessToken(', 'GetTokenInformation(', 'ForbiddenProcessRights', 'ForbiddenTokenRights', 'VerifyMinimalRights', 'CloseHandle(', 'ExpectedBrokerProcessId', 'Test-ServerAccessProbeRecord')) {
        Assert-True ($transportHarness.Contains($requiredServerAccessProbe)) "Broker harness lost server-access probe contract $requiredServerAccessProbe."
    }
    $serverAccessTestStart = $transportHarness.IndexOf('function Test-ServerAccessProbeContract', [StringComparison]::Ordinal)
    $serverAccessTestCall = $transportHarness.LastIndexOf('Test-ServerAccessProbeContract', [StringComparison]::Ordinal)
    Assert-True ($serverAccessTestStart -ge 0 -and $serverAccessTestStart -lt $serverAccessTestCall -and $serverAccessTestCall -lt $availabilityTestNonElevatedReturn) 'Broker harness does not compile and exercise its native server-access probe before the non-elevated return.'
    $diagnosticContractStart = $transportHarness.IndexOf('function Test-SanitizedClientDiagnosticCategoryContract', [StringComparison]::Ordinal)
    $diagnosticContractCall = $transportHarness.LastIndexOf('Test-SanitizedClientDiagnosticCategoryContract', [StringComparison]::Ordinal)
    Assert-True ($diagnosticContractStart -ge 0 -and $diagnosticContractStart -lt $diagnosticContractCall -and $diagnosticContractCall -lt $availabilityTestNonElevatedReturn) 'Broker harness does not run sanitized client-diagnostic mapping tests before its non-elevated return.'
    Assert-True ($transportHarness.Contains('Sanitized failure record retained untrusted diagnostic content.')) 'Broker harness lost its behavioral raw-diagnostic non-disclosure test.'
    $directCredentialCall = 'Invoke-EphemeralProcess -FilePath $clientForCredential'
    Assert-True ([regex]::Matches($transportHarness, [regex]::Escape($directCredentialCall)).Count -eq 2) 'Broker harness must retain exactly two direct authenticated real-client calls.'
    $firstCredentialCall = $transportHarness.IndexOf($directCredentialCall, [StringComparison]::Ordinal)
    $secondCredentialCall = $transportHarness.IndexOf($directCredentialCall, $firstCredentialCall + $directCredentialCall.Length, [StringComparison]::Ordinal)
    $noTouchAssertion = 'Assert-NoTouchPathsRemainAbsent -HandoffRoot $handoff -OutputRoot $output'
    $firstCredentialStatus = $transportHarness.IndexOf('Broker service did not remain running after the authenticated round trip.', $firstCredentialCall, [StringComparison]::Ordinal)
    $firstCredentialAbsent = $transportHarness.IndexOf($noTouchAssertion, $firstCredentialStatus, [StringComparison]::Ordinal)
    $firstCredentialStop = $transportHarness.IndexOf('[void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)', $firstCredentialAbsent, [StringComparison]::Ordinal)
    Assert-True ($firstCredentialCall -ge 0 -and $firstCredentialCall -lt $firstCredentialStatus -and $firstCredentialStatus -lt $firstCredentialAbsent -and $firstCredentialAbsent -lt $firstCredentialStop -and $firstCredentialStop -lt $secondCredentialCall) 'First authenticated real-client call does not assert no-touch paths after result/status postconditions and before service stop work.'
    Assert-True ([regex]::Matches($transportHarness.Substring($firstCredentialCall, $secondCredentialCall - $firstCredentialCall), [regex]::Escape($noTouchAssertion)).Count -eq 1) 'First authenticated real-client region must contain exactly one post-call no-touch assertion.'
    $secondCredentialStatus = $transportHarness.IndexOf('Rejected unmapped-policy restart exposed a broker pipe.', $secondCredentialCall, [StringComparison]::Ordinal)
    $secondCredentialAbsent = $transportHarness.IndexOf($noTouchAssertion, $secondCredentialStatus, [StringComparison]::Ordinal)
    $secondCredentialStop = $transportHarness.IndexOf('[void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)', $secondCredentialAbsent, [StringComparison]::Ordinal)
    Assert-True ($secondCredentialCall -lt $secondCredentialStatus -and $secondCredentialStatus -lt $secondCredentialAbsent -and $secondCredentialAbsent -lt $secondCredentialStop) 'Second authenticated real-client call does not assert no-touch paths after result/status postconditions and before service stop work.'
    Assert-True ([regex]::Matches($transportHarness.Substring($secondCredentialCall, $secondCredentialStop - $secondCredentialCall), [regex]::Escape($noTouchAssertion)).Count -eq 1) 'Second authenticated real-client region must contain exactly one post-call no-touch assertion.'
    Assert-True ([regex]::Matches($transportHarness, [regex]::Escape($noTouchAssertion)).Count -eq 4) 'Broker harness must retain initial, two authenticated post-call, and final cleanup no-touch assertions.'
    Assert-True (-not $transportHarness.Contains('.DeleteSubKey(')) 'Broker harness cleanup regained path-based registry deletion.'
    Assert-True ($transportHarness.Contains('PrivilegeRestoreRetryModel')) 'Broker harness lacks deterministic test-only privilege restoration injection.'
    Assert-True ($transportHarness.Contains('marker=incomplete;restore_attempts=3;token_owned=true;previous_state=37')) 'Broker harness does not prove persistent restoration failure retains fail-closed state until process termination.'
    Assert-True ($transportHarness.Contains('Object.ReferenceEquals(provisioningOriginal, provisioningPropagated)')) 'Broker harness does not prove a successful outer retry preserves the original provisioning exception.'
    Assert-True (-not $transportHarness.Contains('PrivilegeRestoreFailureEvidencePath')) 'Broker harness persistent-failure fixture exposes an arbitrary evidence path.'
    Assert-True ($transportHarness.Contains('Console.Out.Flush();')) 'Broker harness does not flush in-band persistent-failure evidence before FailFast.'
    Assert-True ($transportHarness.Contains('-MaximumCapturedOutputCharacters 16384')) 'Broker harness retains unbounded child FailFast diagnostics.'
    $boundCleanupStart = $transportHarness.IndexOf('function Remove-RegistryKeyByValidatedHandle', [StringComparison]::Ordinal)
    $boundCleanupEnd = $transportHarness.IndexOf('function Remove-OwnedPolicy', $boundCleanupStart, [StringComparison]::Ordinal)
    Assert-True ($boundCleanupStart -ge 0 -and $boundCleanupEnd -gt $boundCleanupStart) 'Broker harness lost its bounded exact-handle cleanup helper.'
    $boundCleanup = $transportHarness.Substring($boundCleanupStart, $boundCleanupEnd - $boundCleanupStart)
    $boundValidation = $boundCleanup.IndexOf('& $Validate $key', [StringComparison]::Ordinal)
    $boundaryHook = $boundCleanup.IndexOf('& $BeforeDelete', [StringComparison]::Ordinal)
    $boundDeletion = $boundCleanup.IndexOf('DeleteExactKey($handle)', [StringComparison]::Ordinal)
    Assert-True ($boundValidation -ge 0 -and $boundValidation -lt $boundaryHook -and $boundaryHook -lt $boundDeletion) 'Broker harness does not validate, exercise the boundary hook, and then delete through the same still-live handle.'
    $removePolicyStart = $transportHarness.IndexOf('function Remove-OwnedPolicy(', [StringComparison]::Ordinal)
    $removePolicyEnd = $transportHarness.IndexOf('function Remove-OwnedPolicyAncestors', $removePolicyStart, [StringComparison]::Ordinal)
    Assert-True ($removePolicyStart -ge 0 -and $removePolicyEnd -gt $removePolicyStart) 'Broker harness lost its bounded policy cleanup function.'
    $removePolicy = $transportHarness.Substring($removePolicyStart, $removePolicyEnd - $removePolicyStart)
    $exactDeleteCall = $removePolicy.IndexOf('Remove-RegistryKeyByValidatedHandle', [StringComparison]::Ordinal)
    $dropOwnership = $removePolicy.IndexOf('$script:ownedPolicyState = $null', [StringComparison]::Ordinal)
    $postDeleteObservation = $removePolicy.IndexOf('Test-Path -LiteralPath $policyRegistryPath', [StringComparison]::Ordinal)
    Assert-True ($exactDeleteCall -ge 0 -and $exactDeleteCall -lt $dropOwnership -and $dropOwnership -lt $postDeleteObservation) 'Broker harness retains policy ownership across a post-delete path observation.'
    Assert-True ($transportHarness.Contains('$script:ownedPolicyAncestors = @($ownedPolicyAncestors | Where-Object { $_ -cne $path })')) 'Broker harness retains a successfully deleted ancestor until later cleanup completes.'
    Assert-True ($transportHarness.Contains('boundary-swap policy')) 'Broker harness lacks deterministic post-delete replacement coverage.'
    Assert-True ($transportHarness.Contains("Assert-True (`$null -eq `$ownedPolicyState) 'Policy cleanup retained authority after its exact NtDeleteKey succeeded.'")) 'Broker harness does not prove ownership is dropped at the delete boundary.'
    Assert-True (-not $transportHarness.Contains('if (Test-Path -LiteralPath $policyRegistryPath) { $script:createdPolicy = $true }')) 'Broker harness derives destructive ownership from path existence.'
    Assert-True (-not $transportHarness.Contains('-Sid $identity.User.Value')) 'Broker harness reuses the reserved elevated runner as a valid client SID.'
    Assert-True (-not $transportHarness.Contains('New-ProtectedPolicy -Sid $runnerSid')) 'Broker harness provisions a valid policy for the elevated runner SID.'
    foreach ($required in @(
        '$ephemeralAccount = New-EphemeralStandardAccount',
        "-Sid `$ephemeralSid",
        "@(`$ephemeralSid, [Security.AccessControl.FileSystemRights]::ReadAndExecute)",
        '$clientForCredential = Join-Path $machineTarget',
        '$harnessForCredential = Join-Path $machineTarget',
        'Get-FileHash -Algorithm SHA256 -LiteralPath $clientForCredential',
        'Get-FileHash -Algorithm SHA256 -LiteralPath $harnessForCredential',
        'Invoke-EphemeralProcess -FilePath $clientForCredential',
        '$start.UserName = $script:ownedEphemeralAccount.Name',
        '$start.Domain = $env:COMPUTERNAME',
        '$start.Password = $script:ephemeralPassword',
        '$start.LoadUserProfile = $false',
        '$start.WorkingDirectory = [IO.Path]::GetFullPath($machineTarget)',
        '$start.Environment.Clear()',
        '$candidateToken = New-CryptographicHex -ByteCount 16',
        '$pathToken = $fixturePaths.Token',
        '$machineTarget = $fixturePaths.MachineTarget',
        '$handoff = $fixturePaths.Handoff',
        '$output = $fixturePaths.Output',
        "-cmatch '^[0-9a-f]{32}$'",
        "-cmatch '^s[0-9a-f]{32}$'",
        '$leaf -cmatch "^$Prefix[0-9a-f]{32}$"',
        "`$serviceForScm = Join-Path `$machineTarget 's.exe'",
        "`$clientForCredential = Join-Path `$machineTarget 'c.exe'",
        "`$harnessForCredential = Join-Path `$machineTarget 'p.ps1'",
        'Get-ValidatedNoTouchPath -DriveRoot $DriveRoot -Prefix ''h'' -Token $candidateToken',
        'Get-ValidatedNoTouchPath -DriveRoot $DriveRoot -Prefix ''o'' -Token $candidateToken',
        'Assert-NoTouchPathsRemainAbsent -HandoffRoot $handoff -OutputRoot $output',
        '$DriveRoot -cmatch ''^[A-Z]:\\$''',
        '[IO.Path]::IsPathFullyQualified($candidate)',
        '[IO.Path]::GetFullPath($candidate) -ceq $candidate',
        '[IO.Path]::GetPathRoot($candidate) -ceq $DriveRoot',
        '[IO.Path]::GetDirectoryName($candidate) -ceq $DriveRoot',
        '[IO.Path]::GetFileName($candidate) -ceq $leaf',
        '[IO.Path]::IsPathFullyQualified($env:SystemRoot)',
        'Windows system directory is noncanonical.',
        '[IO.Path]::IsPathFullyQualified($CommonAppData)',
        'Machine-wide application-data root is noncanonical.',
        '$HandoffRoot -cne $OutputRoot',
        'Could not select an absent three-path credentialed fixture set after $maximumAttempts attempts.',
        'The no-touch handoff path appeared and will be left untouched.',
        'The no-touch output path appeared and will be left untouched.',
        '$safeToRemoveMachineTarget = $null -ne $ownedMachineTarget',
        'Refusing protected staging cleanup while a credentialed process may still be active.',
        'Refusing protected staging cleanup while the exact broker service still exists.',
        'Refusing protected staging cleanup outside its exact CommonApplicationData parent.',
        'Refusing protected staging cleanup through a reparse point.',
        'Refusing protected staging cleanup containing an unexpected entry.',
        '-RunEphemeralIdentityProbe',
        '-RunEphemeralFullControlProbe',
        '-RunEphemeralStalledProbe',
        '$current.User.Value -ceq $ExpectedSid',
        '$expectedRid -ge 1000',
        'Add-LocalGroupMember -SID $standardUsersSid -Member $verified',
        'ephemeral-stalled-ready',
        'Assert-True (-not $stalledProcess.HasExited)',
        'Disable-LocalUser -SID $state.Sid',
        'Remove-LocalUser -SID $state.Sid',
        '$script:ownedEphemeralAccount = $null',
        'Assert-NoEphemeralProfileRegistration',
        'foreign-cleanup-marker',
        'expected-post-create-pre-enable-failure',
        "`$marker = 'ScribeGpu:' + (New-CryptographicHex -ByteCount 16)",
        'Assert-True ($marker.Length -le 48)',
        'Test-EphemeralProcessOwnershipBoundary',
        'Refusing to release credentialed process ownership before exit is positively confirmed.',
        'Failed credential-process release discarded exact ownership.',
        'Credentialed process termination remained uncertain after kill.'
        'Refusing account cleanup while its exact credentialed process may still be active.'
    )) {
        Assert-True ($transportHarness.Contains($required)) "Broker harness lost ephemeral-client identity control: $required"
    }
    Assert-True (-not $transportHarness.Contains('Remove-LocalUser -Name')) 'Broker harness deletes an ephemeral account by mutable name.'
    Assert-True ([regex]::Matches($transportHarness, [regex]::Escape("@(`$users | Where-Object { `$_.SID.Value -ceq `$sid.Value }).Count")).Count -eq 2) 'Broker harness membership counts are not both StrictMode-safe exact-SID arrays.'
    foreach ($forbidden in @(
        'ConvertFrom-SecureString',
        'SecureStringToBSTR',
        'PasswordInClearText',
        'RunImpersonated',
        'ArgumentList.Add($script:ephemeralPassword',
        "Environment['PASSWORD']",
        'Write-Output $script:ephemeralPassword',
        'WriteAllText($script:ephemeralPassword',
        '.Arguments =',
        'UseShellExecute = $true',
        'Start-Process',
        'CreateProcessAsUser',
        'CreateProcessWithToken',
        ' -EncodedCommand',
        'cmd.exe /c',
        "Environment['ARGUMENTS']",
        "Environment['ARGS']",
        "Environment['COMMAND_LINE']",
        'ConvertTo-ResponseFile',
        'Write-ResponseFile'
    )) {
        Assert-True (-not $transportHarness.Contains($forbidden)) "Broker harness exposes or weakens ephemeral credential handling: $forbidden"
    }
    Assert-True (-not $transportHarness.Contains('Remove-Item -LiteralPath $handoff')) 'Broker harness may delete its no-touch handoff path.'
    Assert-True (-not $transportHarness.Contains('Remove-Item -LiteralPath $output')) 'Broker harness may delete its no-touch output path.'
    $nonElevatedReturn = $transportHarness.IndexOf("if (-not `$isElevated)", [StringComparison]::Ordinal)
    $accountCreation = $transportHarness.IndexOf('$ephemeralAccount = New-EphemeralStandardAccount', [StringComparison]::Ordinal)
    Assert-True ($nonElevatedReturn -ge 0 -and $nonElevatedReturn -lt $accountCreation) 'Broker harness creates local credential state before preserving its non-elevated return path.'
    $probeDispatch = $transportHarness.IndexOf('if ($ephemeralProbeCount -eq 1)', [StringComparison]::Ordinal)
    $firstAddType = $transportHarness.IndexOf("if (-not ('Scribe.GpuBroker.RegistryCleanupNative' -as [type]))", [StringComparison]::Ordinal)
    Assert-True ($probeDispatch -ge 0 -and $probeDispatch -lt $firstAddType) 'Fixed credential probes are not dispatched before the elevated main harness and embedded helpers.'
    $newAccountStart = $transportHarness.IndexOf('function New-EphemeralStandardAccount', [StringComparison]::Ordinal)
    $newAccountEnd = $transportHarness.IndexOf('function Remove-OwnedEphemeralAccount', $newAccountStart, [StringComparison]::Ordinal)
    Assert-True ($newAccountStart -ge 0 -and $newAccountEnd -gt $newAccountStart) 'Broker harness lost its bounded ephemeral-account creation helper.'
    $newAccount = $transportHarness.Substring($newAccountStart, $newAccountEnd - $newAccountStart)
    $accountCreated = $newAccount.IndexOf('$created = New-LocalUser', [StringComparison]::Ordinal)
    $accountOwnership = $newAccount.IndexOf('$script:ownedEphemeralAccount = [pscustomobject]@{', [StringComparison]::Ordinal)
    $accountSidValidation = $newAccount.IndexOf('$sid = Assert-CanonicalEphemeralSid', [StringComparison]::Ordinal)
    Assert-True ($accountCreated -ge 0 -and $accountCreated -lt $accountOwnership -and $accountOwnership -lt $accountSidValidation) 'Broker harness does not bind raw SID ownership immediately after account creation and before canonical validation.'
    $removeAccountStart = $transportHarness.IndexOf('function Remove-OwnedEphemeralAccount', [StringComparison]::Ordinal)
    $removeAccountEnd = $transportHarness.IndexOf('function New-EphemeralProcessStartInfo', $removeAccountStart, [StringComparison]::Ordinal)
    Assert-True ($removeAccountStart -ge 0 -and $removeAccountEnd -gt $removeAccountStart) 'Broker harness lost its bounded SID-owned account cleanup helper.'
    $removeAccount = $transportHarness.Substring($removeAccountStart, $removeAccountEnd - $removeAccountStart)
    $exactAccountValidation = $removeAccount.IndexOf('Assert-OwnedEphemeralAccount -State $state', [StringComparison]::Ordinal)
    $disableAccount = $removeAccount.IndexOf('Disable-LocalUser -SID $state.Sid', [StringComparison]::Ordinal)
    $deleteAccount = $removeAccount.IndexOf('Remove-LocalUser -SID $state.Sid', [StringComparison]::Ordinal)
    $dropAccountOwnership = $removeAccount.IndexOf('$script:ownedEphemeralAccount = $null', [StringComparison]::Ordinal)
    $verifySidAbsent = $removeAccount.IndexOf('Get-ExactLocalUserBySid -Sid $deletedSid', [StringComparison]::Ordinal)
    Assert-True ($exactAccountValidation -ge 0 -and $exactAccountValidation -lt $disableAccount -and $disableAccount -lt $deleteAccount -and $deleteAccount -lt $dropAccountOwnership -and $dropAccountOwnership -lt $verifySidAbsent) 'Broker harness does not validate, disable, delete by exact SID, drop ownership, then verify absence in order.'
    $completeProcessStart = $transportHarness.IndexOf('function Complete-EphemeralProcess(', [StringComparison]::Ordinal)
    $completeProcessEnd = $transportHarness.IndexOf('function Invoke-EphemeralProcess', $completeProcessStart, [StringComparison]::Ordinal)
    Assert-True ($completeProcessStart -ge 0 -and $completeProcessEnd -gt $completeProcessStart) 'Broker harness lost its bounded credential-process completion helper.'
    $completeProcess = $transportHarness.Substring($completeProcessStart, $completeProcessEnd - $completeProcessStart)
    $completionExitProof = $completeProcess.IndexOf('$Process.HasExited', [StringComparison]::Ordinal)
    $completionRelease = $completeProcess.IndexOf('Release-ExitedEphemeralProcess -Process $Process', [StringComparison]::Ordinal)
    Assert-True ($completionExitProof -ge 0 -and $completionExitProof -lt $completionRelease) 'Credential-process completion can relinquish ownership without a positive exit observation.'
    Assert-True (-not $completeProcess.Contains('$script:activeCredentialProcess = $null')) 'Credential-process completion directly discards ownership across uncertain termination.'
    $outerCleanupStart = $transportHarness.LastIndexOf("finally {", [StringComparison]::Ordinal)
    $outerCleanup = $transportHarness.Substring($outerCleanupStart)
    $killCredentialChild = $outerCleanup.IndexOf('$activeCredentialProcess.Kill($true)', [StringComparison]::Ordinal)
    $confirmCredentialChildExit = $outerCleanup.IndexOf('$activeCredentialProcess.WaitForExit(10000)', [StringComparison]::Ordinal)
    $releaseCredentialChild = $outerCleanup.IndexOf('Release-ExitedEphemeralProcess -Process $activeCredentialProcess', [StringComparison]::Ordinal)
    $removeCredentialAccount = $outerCleanup.IndexOf('Remove-OwnedEphemeralAccount', [StringComparison]::Ordinal)
    $disposeCredential = $outerCleanup.IndexOf('$ephemeralPassword.Dispose()', [StringComparison]::Ordinal)
    $removeCredentialPolicy = $outerCleanup.IndexOf('if ($null -ne $ownedPolicyState) { Remove-OwnedPolicy }', [StringComparison]::Ordinal)
    Assert-True ($killCredentialChild -ge 0 -and $killCredentialChild -lt $confirmCredentialChildExit -and $confirmCredentialChildExit -lt $releaseCredentialChild -and $releaseCredentialChild -lt $removeCredentialAccount -and $removeCredentialAccount -lt $disposeCredential -and $disposeCredential -lt $removeCredentialPolicy) 'Broker harness does not confirm credentialed child exit before release, remove its exact account, dispose credential material, then clean orphaned policy state.'
    Assert-True ((Get-Content -LiteralPath (Join-Path $repositoryRoot 'tools\windows-gpu-promotion-broker\src\fixture.rs') -Raw).Contains('consumes_canonical_handoff_generated_by_powershell_and_worker_pack_author')) 'Broker proof does not consume the PowerShell/worker-pack-author interoperability fixture.'

    if (-not [string]::IsNullOrWhiteSpace($InteropFixtureDirectory)) {
        $interopFixture = [IO.Path]::GetFullPath($InteropFixtureDirectory)
        $interopPublication = [IO.Path]::GetFullPath($InteropPublicationDirectory)
        if (Test-Path -LiteralPath $interopFixture) { throw 'Interoperability fixture directory must be fresh.' }
        if (Test-Path -LiteralPath $interopPublication) { throw 'Interoperability publication directory must be fresh.' }
        if (-not (Test-Path -LiteralPath (Split-Path -Parent $interopFixture) -PathType Container)) { throw 'Interoperability fixture parent is missing.' }
        New-Item -ItemType Directory -Path $interopFixture | Out-Null
        New-Item -ItemType Directory -Path $interopPublication | Out-Null
        $interopHandoffRoot = Join-Path $interopFixture 'handoff'
        New-Item -ItemType Directory -Path $interopHandoffRoot | Out-Null
        $interopCuda = New-PreparedPack $tool (Join-Path $interopHandoffRoot 'cuda') 'Cuda'
        $interopVulkan = New-PreparedPack $tool (Join-Path $interopHandoffRoot 'vulkan') 'Vulkan'
        $interopHandoff = Write-Handoff $interopHandoffRoot $interopCuda $interopVulkan $revision $toolchainDigest
        $interopIntent = [ordered]@{
            schema_version = 1
            policy_namespace = 'scribe-windows-gpu-production-v1'
            source_repository = 'tyhuang9/scribe'
            source_ref = 'refs/heads/main'
            source_revision = $revision
            workflow_ref = 'tyhuang9/scribe/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main'
            workflow_source_sha = $revision
            run_id = '1001'
            run_attempt = '1'
            artifact_id = '2002'
            artifact_digest = $artifactDigest
            handoff_sha256 = $interopHandoff.Sha256
            release_set_digest = $interopHandoff.ReleaseSetDigest
            toolchain_manifest_sha256 = $toolchainDigest
            pack_version = '0.1.0-promotion-fixture'
            minimum_security_epoch = [uint64]1
            require_unused_release_set = $true
        }
        [IO.File]::WriteAllText(
            (Join-Path $interopFixture 'promotion-intent.json'),
            ($interopIntent | ConvertTo-Json -Depth 8 -Compress),
            [Text.UTF8Encoding]::new($false)
        )
    }
}
finally {
    $env:CARGO_TARGET_DIR = $previousTarget
    $env:SCRIBE_BUILD_REVISION = $previousRevision
    foreach ($path in @($testRoot, $(if ([string]::IsNullOrWhiteSpace($CargoTargetDirectory)) { $target }))) {
        if ([string]::IsNullOrWhiteSpace($path) -or -not (Test-Path -LiteralPath $path)) { continue }
        $canonical = [IO.Path]::GetFullPath($path)
        $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
        if (-not $canonical.StartsWith($temp, [StringComparison]::OrdinalIgnoreCase)) { throw 'Refusing to clean a test path outside the temp directory.' }
        Remove-Item -LiteralPath $canonical -Recurse -Force
    }
}

Write-Output 'Windows GPU pack promotion contract tests passed.'
