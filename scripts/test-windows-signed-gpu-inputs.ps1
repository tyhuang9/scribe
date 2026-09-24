$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'resolve-windows-signed-gpu-inputs.ps1') -FunctionsOnly

$script:ProductionVerifierPathResolver = ${function:Resolve-TrustedWindowsGpuVerifierPath}
$script:SignedInputTestCount = 0
$script:NativeCalls = [Collections.Generic.List[object]]::new()
$script:GitHubCalls = [Collections.Generic.List[string]]::new()
$script:TestRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-signed-inputs-$([guid]::NewGuid().ToString('N'))"
$script:SavedEnvironment = @{}
$script:EnvironmentNames = @(
    'GITHUB_REPOSITORY', 'GITHUB_EVENT_NAME', 'GITHUB_REF', 'GITHUB_SHA',
    'SCRIBE_GPU_SIGNER_SOURCE_SHA', 'SCRIBE_GPU_SIGNER_RUN_ID',
    'SCRIBE_GPU_SIGNER_RUN_ATTEMPT', 'SCRIBE_GPU_SIGNER_ARTIFACT_ID',
    'SCRIBE_GPU_SIGNER_ARTIFACT_SHA256', 'SCRIBE_GPU_SIGNER_BINARY_SHA256',
    'SCRIBE_GPU_SIGNER_WRAPPER_SHA256'
)
$lastExitVariable = Get-Variable -Name LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue
$script:HadSavedLastExitCode = $null -ne $lastExitVariable
$script:SavedLastExitCode = if ($script:HadSavedLastExitCode) { [int]$lastExitVariable.Value } else { $null }

function Assert-Test([bool]$Condition, [string]$Message) {
    $script:SignedInputTestCount++
    if (-not $Condition) { throw "TEST FAILED: $Message" }
}

function Assert-Rejected([string]$Name, [scriptblock]$Action) {
    $script:SignedInputTestCount++
    try {
        & $Action
        throw "TEST FAILED: $Name was accepted."
    }
    catch {
        if ($_.Exception.Message.StartsWith('TEST FAILED:', [StringComparison]::Ordinal)) { throw }
    }
}

function Copy-TestJson($Value) {
    return ($Value | ConvertTo-Json -Depth 32 -Compress | ConvertFrom-Json -AsHashtable -Depth 32)
}

function Write-TestBytes([string]$Path, [byte[]]$Bytes) {
    [IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
    [IO.File]::WriteAllBytes($Path, $Bytes)
}

function Write-TestJson([string]$Path, $Value) {
    Write-TestBytes $Path $script:Utf8.GetBytes(($Value | ConvertTo-Json -Depth 32 -Compress))
}

function New-TestRun([string]$RunId, [string]$Attempt, [string]$Revision) {
    return [ordered]@{
        id = [int64]$RunId
        run_attempt = [int64]$Attempt
        repository = [ordered]@{ full_name = 'tyhuang9/scribe' }
        head_repository = [ordered]@{ full_name = 'tyhuang9/scribe' }
        path = '.github/workflows/windows-gpu-pack-promotion.yml'
        event = 'workflow_dispatch'
        head_branch = 'main'
        status = 'completed'
        conclusion = 'success'
        head_sha = $Revision
    }
}

function New-TestArtifact(
    [string]$ArtifactId,
    [string]$RunId,
    [string]$Revision,
    [string]$Name,
    [string]$Digest
) {
    return [ordered]@{
        id = [int64]$ArtifactId
        workflow_run = [ordered]@{ id = [int64]$RunId; head_sha = $Revision; head_branch = 'main' }
        name = $Name
        expired = $false
        digest = "sha256:$Digest"
        size_in_bytes = [int64]1048576
    }
}

function Reset-TestFixture {
    $source = 'a' * 40
    $signingRunId = '9001'
    $signingAttempt = '2'
    $signedArtifactId = '9101'
    $producerRunId = '8001'
    $producerAttempt = '1'
    $producerArtifactId = '8101'
    $signedDigest = 'b' * 64
    $unsignedDigest = 'c' * 64
    $policy = [ordered]@{
        schema_version = 1
        policy_id = 'scribe-windows-gpu-github-signing-v1'
        key_id = 'scribe-production-key-1'
        public_key_sha256 = 'd' * 64
        source_repository = 'tyhuang9/scribe'
        source_ref = 'refs/heads/main'
        app_version = '0.1.0'
        protocol_version = 5
        worker_abi_version = 1
        minimum_security_epoch = 7
        packs = @(
            [ordered]@{
                backend = 'cuda'; pack_id = 'scribe-cuda-windows-x64'
                provider = 'transcribe-cpp-ggml-cuda'
                worker_path = 'bin/scribe-inference-worker.exe'; security_epoch = [int64]7
            },
            [ordered]@{
                backend = 'vulkan'; pack_id = 'scribe-vulkan-windows-x64'
                provider = 'transcribe-cpp-ggml-vulkan'
                worker_path = 'bin/scribe-inference-worker.exe'; security_epoch = [int64]7
            }
        )
    }
    $policyBytes = $script:Utf8.GetBytes(($policy | ConvertTo-Json -Depth 16 -Compress))
    $toolchainBytes = $script:Utf8.GetBytes('{"schema_version":1,"fixture":"toolchain"}')
    $policyPath = Join-Path $script:TestRoot 'checkout/policy.json'
    $toolchainPath = Join-Path $script:TestRoot 'checkout/toolchain.json'
    $signedRoot = Join-Path $script:TestRoot 'signed'
    $catalogPath = Join-Path $script:TestRoot 'catalog.json'
    [IO.Directory]::CreateDirectory((Join-Path $signedRoot 'cuda')) | Out-Null
    [IO.Directory]::CreateDirectory((Join-Path $signedRoot 'vulkan')) | Out-Null
    Write-TestBytes $policyPath $policyBytes
    Write-TestBytes $toolchainPath $toolchainBytes
    $policyDigest = Get-SigningHash $policyBytes
    $toolchainDigest = Get-SigningHash $toolchainBytes
    $pins = [ordered]@{
        SOURCE_SHA = 'e' * 40
        RUN_ID = '7001'
        RUN_ATTEMPT = '1'
        ARTIFACT_ID = '7101'
        ARTIFACT_SHA256 = 'f' * 64
        BINARY_SHA256 = '1' * 64
        WRAPPER_SHA256 = '2' * 64
    }
    foreach ($name in $pins.Keys) {
        [Environment]::SetEnvironmentVariable("SCRIBE_GPU_SIGNER_$name", $pins[$name])
    }
    $pinsDigest = Get-SigningPinsHash $pins
    $receipt = [ordered]@{
        schema_version = 1
        policy_id = $policy.policy_id
        policy_sha256 = $policyDigest
        key_id = $policy.key_id
        signer_source_revision = $pins.SOURCE_SHA
        signer_sha256 = $pins.BINARY_SHA256
        source_repository = 'tyhuang9/scribe'
        source_ref = 'refs/heads/main'
        source_revision = $source
        workflow_ref = 'tyhuang9/scribe/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main'
        run_id = $producerRunId
        run_attempt = $producerAttempt
        artifact_id = $producerArtifactId
        artifact_digest = $unsignedDigest
        handoff_sha256 = '3' * 64
        release_set_digest = '4' * 64
        toolchain_manifest_sha256 = $toolchainDigest
        pack_version = '0.1.0-release.1'
        packs = @(
            [ordered]@{
                backend = 'cuda'; pack_root = 'cuda'; pack_id = 'scribe-cuda-windows-x64'
                pack_version = '0.1.0-release.1'; pack_digest = '5' * 64
                manifest_sha256 = '6' * 64; security_epoch = [int64]7
                provider = 'transcribe-cpp-ggml-cuda'; payload_files = [int64]4
                installed_payload_bytes = [int64]1000
            },
            [ordered]@{
                backend = 'vulkan'; pack_root = 'vulkan'; pack_id = 'scribe-vulkan-windows-x64'
                pack_version = '0.1.0-release.1'; pack_digest = '7' * 64
                manifest_sha256 = '8' * 64; security_epoch = [int64]7
                provider = 'transcribe-cpp-ggml-vulkan'; payload_files = [int64]5
                installed_payload_bytes = [int64]2000
            }
        )
    }
    $catalogPacks = @()
    foreach ($pack in $receipt.packs) {
        $root = "workers/packs/$($pack.pack_id)/$($pack.pack_version)/$($pack.pack_digest)"
        $catalogPacks += [ordered]@{
            pack_id = $pack.pack_id; pack_version = $pack.pack_version
            pack_digest = $pack.pack_digest; security_epoch = [int64]$pack.security_epoch
            runtime_abi_version = [int64]1; backend = $pack.backend; provider = $pack.provider
            target_os = 'windows'; target_arch = 'x86_64'
            worker_relative_path = 'bin/scribe-inference-worker.exe'; root = $root
            installed_size_bytes = [int64]$pack.installed_payload_bytes
            compressed_size_bytes = [int64]500
            files = @("$root/pack-manifest.json", "$root/pack-manifest.sig", "$root/bin/scribe-inference-worker.exe")
        }
    }
    $catalog = [ordered]@{ schema_version = 1; packs = $catalogPacks }
    Write-TestJson $catalogPath $catalog
    $script:Fixture = [ordered]@{
        Source = $source
        SigningRunId = $signingRunId
        SigningAttempt = $signingAttempt
        SignedArtifactId = $signedArtifactId
        ProducerRunId = $producerRunId
        ProducerAttempt = $producerAttempt
        ProducerArtifactId = $producerArtifactId
        SignedDigest = $signedDigest
        UnsignedDigest = $unsignedDigest
        SignRun = New-TestRun $signingRunId $signingAttempt $source
        SignLatest = New-TestRun $signingRunId $signingAttempt $source
        SignArtifact = New-TestArtifact $signedArtifactId $signingRunId $source "windows-gpu-signed-$signingRunId-$signingAttempt" $signedDigest
        ProducerRun = New-TestRun $producerRunId $producerAttempt $source
        ProducerLatest = New-TestRun $producerRunId $producerAttempt $source
        ProducerArtifact = New-TestArtifact $producerArtifactId $producerRunId $source "windows-gpu-unsigned-$producerRunId" $unsignedDigest
        GitHead = $source
        MainHead = $source
        Policy = $policy
        PolicyBytes = $policyBytes
        RemotePolicyBytes = $policyBytes
        RemotePolicyBytesAfterFirst = $null
        PolicyContentCalls = 0
        PolicyPath = $policyPath
        PolicyDigest = $policyDigest
        ToolchainPath = $toolchainPath
        ToolchainDigest = $toolchainDigest
        SignedRoot = $signedRoot
        CatalogPath = $catalogPath
        Catalog = $catalog
        Pins = $pins
        PinsDigest = $pinsDigest
        Receipt = $receipt
        NativeOutput = $null
        NativeFailure = $false
    }
    $script:NativeCalls.Clear()
    $script:GitHubCalls.Clear()
    $env:GITHUB_REPOSITORY = 'tyhuang9/scribe'
    $env:GITHUB_EVENT_NAME = 'workflow_dispatch'
    $env:GITHUB_REF = 'refs/heads/main'
    $env:GITHUB_SHA = $source
}

function Invoke-SigningGitHubGet([string]$Path) {
    $script:GitHubCalls.Add($Path)
    $prefix = '/repos/tyhuang9/scribe'
    switch -CaseSensitive ($Path) {
        "$prefix/actions/runs/$($script:Fixture.SigningRunId)/attempts/$($script:Fixture.SigningAttempt)" { return $script:Fixture.SignRun }
        "$prefix/actions/runs/$($script:Fixture.SigningRunId)" { return $script:Fixture.SignLatest }
        "$prefix/actions/artifacts/$($script:Fixture.SignedArtifactId)" { return $script:Fixture.SignArtifact }
        "$prefix/actions/runs/$($script:Fixture.ProducerRunId)/attempts/$($script:Fixture.ProducerAttempt)" { return $script:Fixture.ProducerRun }
        "$prefix/actions/runs/$($script:Fixture.ProducerRunId)" { return $script:Fixture.ProducerLatest }
        "$prefix/actions/artifacts/$($script:Fixture.ProducerArtifactId)" { return $script:Fixture.ProducerArtifact }
        "$prefix/git/ref/heads/main" { return [ordered]@{ object = [ordered]@{ sha = $script:Fixture.MainHead } } }
        "$prefix/compare/$($script:Fixture.Source)...$($script:Fixture.MainHead)" {
            return [ordered]@{ merge_base_commit = [ordered]@{ sha = $script:Fixture.Source }; status = 'identical' }
        }
        "$prefix/contents/runtime-manifests/windows-gpu-signing-policy.json?ref=$($script:Fixture.MainHead)" {
            $script:Fixture.PolicyContentCalls++
            $bytes = if ($script:Fixture.PolicyContentCalls -gt 1 -and $null -ne $script:Fixture.RemotePolicyBytesAfterFirst) {
                $script:Fixture.RemotePolicyBytesAfterFirst
            } else { $script:Fixture.RemotePolicyBytes }
            return [ordered]@{
                type = 'file'; path = 'runtime-manifests/windows-gpu-signing-policy.json'
                encoding = 'base64'; size = $bytes.Length; content = [Convert]::ToBase64String($bytes)
            }
        }
        default { throw "Unexpected offline GitHub request: $Path" }
    }
}

function Get-WindowsGpuInstallerGitHead { return $script:Fixture.GitHead }

function Resolve-TrustedWindowsGpuVerifierPath([string]$Path, [string]$ArtifactRoot) {
    Assert-WindowsGpuInputCondition (-not (Test-WindowsGpuPathWithin $Path $ArtifactRoot)) 'Test verifier unexpectedly came from the artifact.'
    return [IO.Path]::GetFullPath((Join-Path $script:TestRoot 'trusted/scribe-worker-pack-tool.exe'))
}

function Invoke-WindowsGpuSignedSetVerifier(
    [string]$Executable,
    [string]$ArtifactRoot,
    [string]$LocalPolicyPath,
    [string]$LocalToolchainPath
) {
    $script:NativeCalls.Add([pscustomobject]@{
        Executable = $Executable
        ArtifactRoot = $ArtifactRoot
        PolicyPath = $LocalPolicyPath
        ToolchainPath = $LocalToolchainPath
    })
    if ($script:Fixture.NativeFailure) { throw 'Deterministic native rejection.' }
    if ($null -ne $script:Fixture.NativeOutput) { return $script:Fixture.NativeOutput }
    return ($script:Fixture.Receipt | ConvertTo-Json -Depth 32 -Compress)
}

function Invoke-TestResolver([string]$Mode, [bool]$Expected = $true) {
    $arguments = @{
        Mode = $Mode
        SourceRevision = $script:Fixture.Source
        SigningRunId = $script:Fixture.SigningRunId
        SigningRunAttempt = $script:Fixture.SigningAttempt
        SignedArtifactId = $script:Fixture.SignedArtifactId
        PolicyPath = $script:Fixture.PolicyPath
    }
    if ($Expected) {
        $arguments.ExpectedArtifactSha256 = $script:Fixture.SignedDigest
        $arguments.ExpectedPolicySha256 = $script:Fixture.PolicyDigest
        $arguments.ExpectedSignerPinsSha256 = $script:Fixture.PinsDigest
    }
    if ($Mode -cne 'Preflight') {
        $arguments.SignedRoot = $script:Fixture.SignedRoot
        $arguments.VerifierExecutable = Join-Path $script:TestRoot 'trusted/scribe-worker-pack-tool.exe'
        $arguments.ToolchainManifestPath = $script:Fixture.ToolchainPath
    }
    if ($Mode -ceq 'VerifyCatalog') { $arguments.CatalogPath = $script:Fixture.CatalogPath }
    return Invoke-WindowsGpuSignedInputs @arguments
}

function Assert-PreflightRejectedWithoutNative([string]$Name, [scriptblock]$Mutation) {
    Reset-TestFixture
    & $Mutation
    Assert-Rejected $Name { Invoke-TestResolver 'Preflight' $true }
    Assert-Test ($script:NativeCalls.Count -eq 0) "$Name reached the native verifier."
}

function Assert-ResolveRejected([string]$Name, [scriptblock]$Mutation) {
    Reset-TestFixture
    & $Mutation
    Assert-Rejected $Name { Invoke-TestResolver 'Resolve' $true }
}

function Assert-CatalogRejected([string]$Name, [scriptblock]$Mutation) {
    Reset-TestFixture
    & $Mutation
    Write-TestJson $script:Fixture.CatalogPath $script:Fixture.Catalog
    Assert-Rejected $Name { Invoke-TestResolver 'VerifyCatalog' $true }
}

foreach ($name in $script:EnvironmentNames) {
    $script:SavedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
}

try {
    [IO.Directory]::CreateDirectory($script:TestRoot) | Out-Null

    Reset-TestFixture
    $preflight = Invoke-TestResolver 'Preflight' $false
    Assert-Test ($preflight.ArtifactSha256 -ceq $script:Fixture.SignedDigest) 'Preflight did not return the authenticated signed artifact digest.'
    Assert-Test ($preflight.PolicySha256 -ceq $script:Fixture.PolicyDigest) 'Preflight did not return the current byte-identical policy digest.'
    Assert-Test ($preflight.SignerPinsSha256 -ceq $script:Fixture.PinsDigest) 'Preflight did not return the complete reviewed signer-pin digest.'
    Assert-Test ($script:NativeCalls.Count -eq 0) 'Preflight invoked the native verifier.'

    Reset-TestFixture
    $fresh = Invoke-TestResolver 'Preflight' $true
    Assert-Test ($fresh.ArtifactSha256 -ceq $script:Fixture.SignedDigest -and $script:NativeCalls.Count -eq 0) 'Hash-pinned final preflight did not remain metadata-only.'

    Reset-TestFixture
    $resolved = Invoke-TestResolver 'Resolve' $true
    $expectedRoots = @(
        [IO.Path]::GetFullPath((Join-Path $script:Fixture.SignedRoot 'cuda')),
        [IO.Path]::GetFullPath((Join-Path $script:Fixture.SignedRoot 'vulkan'))
    )
    Assert-Test (($resolved.PackRoots -join '|') -ceq ($expectedRoots -join '|')) 'Resolve did not forward only the ordered absolute CUDA/Vulkan roots.'
    Assert-Test ($resolved.Receipt.run_id -ceq $script:Fixture.ProducerRunId -and $resolved.Receipt.run_id -cne $script:Fixture.SigningRunId) 'Resolve confused the original producer identity with the signing run.'
    Assert-Test ($script:NativeCalls.Count -eq 1 -and $script:NativeCalls[0].ArtifactRoot -ceq (Get-WindowsGpuNormalizedPath $script:Fixture.SignedRoot)) 'Resolve did not invoke the trusted verifier exactly once over the signed root.'
    Assert-Test ($script:NativeCalls[0].PolicyPath -ceq (Get-WindowsGpuNormalizedPath $script:Fixture.PolicyPath) -and $script:NativeCalls[0].ToolchainPath -ceq (Get-WindowsGpuNormalizedPath $script:Fixture.ToolchainPath)) 'Resolve did not forward the exact policy/toolchain checkout paths.'

    Reset-TestFixture
    $verified = Invoke-TestResolver 'VerifyCatalog' $true
    Assert-Test ($verified.Included -is [bool] -and $verified.Included) 'VerifyCatalog did not report inclusion only after catalog validation.'
    Assert-Test (($verified.PackRoots -join '|') -ceq ($expectedRoots -join '|')) 'VerifyCatalog changed the trusted root ordering.'

    Reset-TestFixture
    Assert-Rejected 'partial expected hash set' {
        Invoke-WindowsGpuSignedInputs -Mode Preflight -SourceRevision $script:Fixture.Source `
            -SigningRunId $script:Fixture.SigningRunId -SigningRunAttempt $script:Fixture.SigningAttempt `
            -SignedArtifactId $script:Fixture.SignedArtifactId -PolicyPath $script:Fixture.PolicyPath `
            -ExpectedArtifactSha256 $script:Fixture.SignedDigest
    }
    Assert-Test ($script:GitHubCalls.Count -eq 0) 'Partial expected hashes reached GitHub.'
    Reset-TestFixture
    Assert-Rejected 'Resolve without preflight hashes' { Invoke-TestResolver 'Resolve' $false }
    Assert-Test ($script:NativeCalls.Count -eq 0) 'Resolve without preflight hashes reached native verification.'
    Assert-Rejected 'artifact-provided verifier path' {
        & $script:ProductionVerifierPathResolver (Join-Path $script:Fixture.SignedRoot 'candidate-tool.exe') $script:Fixture.SignedRoot
    }

    foreach ($case in @(
        @{ Name = 'noncanonical source'; Apply = { $script:Fixture.Source = 'a' * 39 } },
        @{ Name = 'missing signing run'; Apply = { $script:Fixture.SigningRunId = '' } },
        @{ Name = 'noncanonical signing attempt'; Apply = { $script:Fixture.SigningAttempt = '01' } },
        @{ Name = 'noncanonical signed artifact ID'; Apply = { $script:Fixture.SignedArtifactId = '-1' } },
        @{ Name = 'wrong workflow repository'; Apply = { $env:GITHUB_REPOSITORY = 'attacker/fork' } },
        @{ Name = 'wrong workflow event'; Apply = { $env:GITHUB_EVENT_NAME = 'push' } },
        @{ Name = 'wrong workflow ref'; Apply = { $env:GITHUB_REF = 'refs/tags/v0.1.0' } },
        @{ Name = 'wrong workflow source'; Apply = { $env:GITHUB_SHA = '9' * 40 } },
        @{ Name = 'wrong physical checkout source'; Apply = { $script:Fixture.GitHead = '9' * 40 } }
    )) { Assert-PreflightRejectedWithoutNative $case.Name $case.Apply }

    foreach ($case in @(
        @{ Name = 'signing run repository mismatch'; Apply = { $script:Fixture.SignRun.repository.full_name = 'attacker/fork' } },
        @{ Name = 'signing workflow mismatch'; Apply = { $script:Fixture.SignRun.path = '.github/workflows/release.yml' } },
        @{ Name = 'signing run event mismatch'; Apply = { $script:Fixture.SignRun.event = 'push' } },
        @{ Name = 'signing run ref mismatch'; Apply = { $script:Fixture.SignRun.head_branch = 'feature' } },
        @{ Name = 'signing run source mismatch'; Apply = { $script:Fixture.SignRun.head_sha = '9' * 40 } },
        @{ Name = 'signing run attempt mismatch'; Apply = { $script:Fixture.SignRun.run_attempt = [int64]3 } },
        @{ Name = 'signing run incomplete'; Apply = { $script:Fixture.SignRun.status = 'in_progress' } },
        @{ Name = 'signing run failure'; Apply = { $script:Fixture.SignRun.conclusion = 'failure' } },
        @{ Name = 'stale signing attempt'; Apply = { $script:Fixture.SignLatest.run_attempt = [int64]3 } },
        @{ Name = 'signed artifact ID mismatch'; Apply = { $script:Fixture.SignArtifact.id = [int64]9999 } },
        @{ Name = 'signed artifact association mismatch'; Apply = { $script:Fixture.SignArtifact.workflow_run.id = [int64]9999 } },
        @{ Name = 'signed artifact source mismatch'; Apply = { $script:Fixture.SignArtifact.workflow_run.head_sha = '9' * 40 } },
        @{ Name = 'signed artifact name mismatch'; Apply = { $script:Fixture.SignArtifact.name = 'windows-gpu-signed-wrong' } },
        @{ Name = 'expired signed artifact'; Apply = { $script:Fixture.SignArtifact.expired = $true } },
        @{ Name = 'malformed signed artifact digest'; Apply = { $script:Fixture.SignArtifact.digest = 'sha256:ABC' } },
        @{ Name = 'empty signed artifact'; Apply = { $script:Fixture.SignArtifact.size_in_bytes = [int64]0 } }
    )) { Assert-PreflightRejectedWithoutNative $case.Name $case.Apply }

    Assert-PreflightRejectedWithoutNative 'signed artifact digest changed after approval' { $script:Fixture.SignedDigest = '9' * 64 }
    Assert-PreflightRejectedWithoutNative 'current policy differs from checkout' {
        $changed = Copy-TestJson $script:Fixture.Policy
        $changed.protocol_version = [int64]6
        $script:Fixture.RemotePolicyBytes = $script:Utf8.GetBytes(($changed | ConvertTo-Json -Depth 16 -Compress))
    }
    Assert-PreflightRejectedWithoutNative 'signer pins changed after approval' {
        [Environment]::SetEnvironmentVariable('SCRIBE_GPU_SIGNER_BINARY_SHA256', '9' * 64)
    }

    Reset-TestFixture
    $script:Fixture.SignRun.status = 'in_progress'
    $script:Fixture.SignArtifact.name = 'malicious-later-boundary'
    Assert-Rejected 'compound preflight failure boundary' { Invoke-TestResolver 'Preflight' $true }
    Assert-Test (@($script:GitHubCalls | Where-Object { $_ -like '*/actions/artifacts/*' }).Count -eq 0) 'Failed run metadata did not stop before artifact lookup.'
    Assert-Test ($script:NativeCalls.Count -eq 0) 'Compound preflight failure reached native verification.'

    foreach ($case in @(
        @{ Name = 'native rejection'; Apply = { $script:Fixture.NativeFailure = $true } },
        @{ Name = 'native invalid JSON'; Apply = { $script:Fixture.NativeOutput = '{not-json' } },
        @{ Name = 'native receipt extra field'; Apply = {
            $value = Copy-TestJson $script:Fixture.Receipt; $value.extra = 'no'; $script:Fixture.NativeOutput = $value | ConvertTo-Json -Depth 32 -Compress
        } },
        @{ Name = 'receipt policy digest mismatch'; Apply = { $script:Fixture.Receipt.policy_sha256 = '9' * 64 } },
        @{ Name = 'receipt key mismatch'; Apply = { $script:Fixture.Receipt.key_id = 'other-key' } },
        @{ Name = 'receipt signer source mismatch'; Apply = { $script:Fixture.Receipt.signer_source_revision = '9' * 40 } },
        @{ Name = 'receipt signer binary mismatch'; Apply = { $script:Fixture.Receipt.signer_sha256 = '9' * 64 } },
        @{ Name = 'receipt installer source mismatch'; Apply = { $script:Fixture.Receipt.source_revision = '9' * 40 } },
        @{ Name = 'receipt workflow mismatch'; Apply = { $script:Fixture.Receipt.workflow_ref = 'tyhuang9/scribe/.github/workflows/release.yml@refs/heads/main' } },
        @{ Name = 'receipt toolchain mismatch'; Apply = { $script:Fixture.Receipt.toolchain_manifest_sha256 = '9' * 64 } },
        @{ Name = 'receipt reuses signing run'; Apply = { $script:Fixture.Receipt.run_id = $script:Fixture.SigningRunId } },
        @{ Name = 'receipt reuses signed artifact'; Apply = { $script:Fixture.Receipt.artifact_id = $script:Fixture.SignedArtifactId } },
        @{ Name = 'original producer failure'; Apply = { $script:Fixture.ProducerRun.conclusion = 'failure' } },
        @{ Name = 'original producer source mismatch'; Apply = { $script:Fixture.ProducerRun.head_sha = '9' * 40 } },
        @{ Name = 'original artifact digest mismatch'; Apply = { $script:Fixture.ProducerArtifact.digest = 'sha256:' + ('9' * 64) } },
        @{ Name = 'receipt pack root mismatch'; Apply = { $script:Fixture.Receipt.packs[0].pack_root = 'vulkan' } },
        @{ Name = 'receipt pack provider mismatch'; Apply = { $script:Fixture.Receipt.packs[1].provider = 'attacker-provider' } },
        @{ Name = 'receipt pack epoch mismatch'; Apply = { $script:Fixture.Receipt.packs[0].security_epoch = [int64]8 } }
    )) { Assert-ResolveRejected $case.Name $case.Apply }

    Reset-TestFixture
    $changed = Copy-TestJson $script:Fixture.Policy
    $changed.protocol_version = [int64]6
    $script:Fixture.RemotePolicyBytesAfterFirst = $script:Utf8.GetBytes(($changed | ConvertTo-Json -Depth 16 -Compress))
    Assert-Rejected 'policy changed during native verification' { Invoke-TestResolver 'Resolve' $true }
    Assert-Test ($script:NativeCalls.Count -eq 1) 'Policy freshness test did not cross the intended post-native boundary.'

    foreach ($case in @(
        @{ Name = 'catalog extra pack'; Apply = { $script:Fixture.Catalog.packs += Copy-TestJson $script:Fixture.Catalog.packs[0] } },
        @{ Name = 'catalog extra field'; Apply = { $script:Fixture.Catalog.packs[0].extra = 'no' } },
        @{ Name = 'catalog pack ID mismatch'; Apply = { $script:Fixture.Catalog.packs[0].pack_id = 'other-pack' } },
        @{ Name = 'catalog provider mismatch'; Apply = { $script:Fixture.Catalog.packs[0].provider = 'other-provider' } },
        @{ Name = 'catalog backend order mismatch'; Apply = { $script:Fixture.Catalog.packs[0].backend = 'vulkan' } },
        @{ Name = 'catalog version mismatch'; Apply = { $script:Fixture.Catalog.packs[0].pack_version = '0.1.0-other' } },
        @{ Name = 'catalog digest mismatch'; Apply = { $script:Fixture.Catalog.packs[0].pack_digest = '9' * 64 } },
        @{ Name = 'catalog epoch mismatch'; Apply = { $script:Fixture.Catalog.packs[0].security_epoch = [int64]8 } },
        @{ Name = 'catalog ABI mismatch'; Apply = { $script:Fixture.Catalog.packs[0].runtime_abi_version = [int64]2 } },
        @{ Name = 'catalog OS mismatch'; Apply = { $script:Fixture.Catalog.packs[0].target_os = 'linux' } },
        @{ Name = 'catalog architecture mismatch'; Apply = { $script:Fixture.Catalog.packs[0].target_arch = 'aarch64' } },
        @{ Name = 'catalog worker path mismatch'; Apply = { $script:Fixture.Catalog.packs[0].worker_relative_path = 'worker.exe' } },
        @{ Name = 'catalog immutable root mismatch'; Apply = { $script:Fixture.Catalog.packs[0].root = 'workers/packs/elsewhere' } },
        @{ Name = 'catalog file escapes immutable root'; Apply = { $script:Fixture.Catalog.packs[0].files[0] = 'workers/packs/elsewhere/file.dll' } },
        @{ Name = 'catalog duplicate file'; Apply = { $script:Fixture.Catalog.packs[0].files[1] = $script:Fixture.Catalog.packs[0].files[0] } }
    )) { Assert-CatalogRejected $case.Name $case.Apply }

    Reset-TestFixture
    Assert-Rejected 'missing VerifyCatalog path' {
        Invoke-WindowsGpuSignedInputs -Mode VerifyCatalog -SourceRevision $script:Fixture.Source `
            -SigningRunId $script:Fixture.SigningRunId -SigningRunAttempt $script:Fixture.SigningAttempt `
            -SignedArtifactId $script:Fixture.SignedArtifactId -PolicyPath $script:Fixture.PolicyPath `
            -SignedRoot $script:Fixture.SignedRoot -VerifierExecutable 'trusted.exe' `
            -ToolchainManifestPath $script:Fixture.ToolchainPath `
            -ExpectedArtifactSha256 $script:Fixture.SignedDigest -ExpectedPolicySha256 $script:Fixture.PolicyDigest `
            -ExpectedSignerPinsSha256 $script:Fixture.PinsDigest
    }

    # Exercise the actual script entrypoint as a separate process. These cases
    # all fail before any network or native process can be reached.
    $entrypoint = Join-Path $PSScriptRoot 'resolve-windows-signed-gpu-inputs.ps1'
    $badIdOutput = @(& pwsh -NoProfile -File $entrypoint -Mode Preflight `
        -SourceRevision $script:Fixture.Source -SigningRunId 0 -SigningRunAttempt 1 `
        -SignedArtifactId 1 2>&1)
    $badIdExit = [int]$LASTEXITCODE
    Assert-Test ($badIdExit -ne 0 -and ($badIdOutput -join "`n").Contains('Signing run ID is not a canonical GitHub ID.')) 'Actual entrypoint did not reject a bad signing ID before provenance access.'

    $savedRepository = [string]$env:GITHUB_REPOSITORY
    try {
        $env:GITHUB_REPOSITORY = 'attacker/fork'
        $preflightOutput = @(& pwsh -NoProfile -File $entrypoint -Mode Preflight `
            -SourceRevision $script:Fixture.Source -SigningRunId $script:Fixture.SigningRunId `
            -SigningRunAttempt $script:Fixture.SigningAttempt -SignedArtifactId $script:Fixture.SignedArtifactId 2>&1)
        $preflightExit = [int]$LASTEXITCODE
    }
    finally { $env:GITHUB_REPOSITORY = $savedRepository }
    Assert-Test ($preflightExit -ne 0 -and ($preflightOutput -join "`n").Contains('fixed repository default-branch workflow dispatch')) 'Actual Preflight entrypoint was clobbered while helper functions were imported.'

    $resolveOutput = @(& pwsh -NoProfile -File $entrypoint -Mode Resolve `
        -SourceRevision $script:Fixture.Source -SigningRunId $script:Fixture.SigningRunId `
        -SigningRunAttempt $script:Fixture.SigningAttempt -SignedArtifactId $script:Fixture.SignedArtifactId 2>&1)
    $resolveExit = [int]$LASTEXITCODE
    Assert-Test ($resolveExit -ne 0 -and ($resolveOutput -join "`n").Contains('require all three preflight hashes')) 'Actual Resolve entrypoint was clobbered while helper functions were imported.'

    $source = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'resolve-windows-signed-gpu-inputs.ps1') -Raw
    Assert-Test ($source.Contains(".Environment.Remove('GH_TOKEN')") -and $source.Contains(".Environment.Remove('GITHUB_TOKEN')") -and $source.Contains(".Environment.Remove('SCRIBE_GPU_PACK_PRIVATE_KEY_BASE64')")) 'Production verifier subprocess does not strip GitHub/signing secrets.'
    Assert-Test ($source.Contains('WaitForExit(120000)') -and $source.Contains('$errorText.Length -le 65536')) 'Production verifier subprocess is missing time/output bounds.'
    Assert-Test (-not $source.Contains('Write-Error $errorText') -and -not $source.Contains('Write-Output $errorText')) 'Production verifier relays child stderr.'
    Assert-Test ($script:SignedInputTestCount -ge 100) 'Expected signed-input boundary cases were not discovered.'
    Write-Output "Windows signed GPU installer input tests passed ($script:SignedInputTestCount cases; offline, no keys, workers, GPU execution, or network)."
}
finally {
    foreach ($name in $script:EnvironmentNames) {
        [Environment]::SetEnvironmentVariable($name, $script:SavedEnvironment[$name])
    }
    if ($script:HadSavedLastExitCode) { $global:LASTEXITCODE = $script:SavedLastExitCode }
    else { Remove-Variable -Name LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue }
    if (Test-Path -LiteralPath $script:TestRoot) {
        $resolvedTestRoot = Get-WindowsGpuNormalizedPath $script:TestRoot
        $resolvedTemp = Get-WindowsGpuNormalizedPath ([IO.Path]::GetTempPath())
        if (-not (Test-WindowsGpuPathWithin $resolvedTestRoot $resolvedTemp) -or
            -not (Split-Path -Leaf $resolvedTestRoot).StartsWith('scribe-signed-inputs-', [StringComparison]::Ordinal)) {
            throw 'Refusing to clean an unexpected signed-input test path.'
        }
        Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force
    }
}
