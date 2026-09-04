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
    Assert-True ($brokerNativeProduction.Contains('require_user_sid(token.raw(), authorized_client_sid)')) 'Broker does not require exact client TokenUser equality.'
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
    Assert-True ($provisioner.Contains('ProvisioningState')) 'Client policy provisioner lacks an incomplete-policy marker.'
    Assert-True ($provisioner.Contains('RegistryView]::Registry64')) 'Client policy provisioner does not pin the 64-bit registry view.'
    Assert-True (-not $provisioner.Contains('[string]$AccountName')) 'Client policy provisioner accepts an account name.'
    $bornProtected = $provisioner.LastIndexOf('Assert-PolicySecurity -Key $key', [StringComparison]::Ordinal)
    $firstValueWrite = $provisioner.IndexOf('$key.SetValue($provisioningValue', [StringComparison]::Ordinal)
    Assert-True ($bornProtected -ge 0 -and $bornProtected -lt $firstValueWrite) 'Client policy provisioner writes values before verifying create-time protection.'
    $ancestorValidation = $provisioner.IndexOf('foreach ($ancestorPath in $policyAncestors)', [StringComparison]::Ordinal)
    $leafCreation = $provisioner.IndexOf('$status = [Scribe.GpuBroker.RegistryNative]::CreateProtectedKey(', [StringComparison]::Ordinal)
    Assert-True ($ancestorValidation -ge 0 -and $ancestorValidation -lt $leafCreation) 'Client policy provisioner creates the leaf before validating and protecting its ancestor chain.'
    $transportHarness = Get-Content -LiteralPath (Join-Path $repositoryRoot 'scripts\test-windows-gpu-broker-transport.ps1') -Raw
    Assert-True ($transportHarness.Contains('Assert-OwnedPolicyState -State $state')) 'Broker harness cleanup does not revalidate exact ownership state.'
    Assert-True ($transportHarness.Contains('SecurityFingerprint')) 'Broker harness cleanup does not pin the policy security descriptor.'
    Assert-True ($transportHarness.Contains('CleanupTamper')) 'Broker harness lacks an adversarial same-name cleanup test.'
    Assert-True ($transportHarness.Contains('if ($result.ExitCode -eq 0)')) 'Broker harness claims policy ownership without successful provisioning.'
    Assert-True (-not $transportHarness.Contains('if (Test-Path -LiteralPath $policyRegistryPath) { $script:createdPolicy = $true }')) 'Broker harness derives destructive ownership from path existence.'
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
