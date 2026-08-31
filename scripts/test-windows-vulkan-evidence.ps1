$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$preflightPath = (Resolve-Path (Join-Path $PSScriptRoot 'windows-vulkan-evidence-preflight.ps1')).Path
$previousIncompatibleTypeTestPath = $env:SCRIBE_EVIDENCE_INCOMPATIBLE_TYPE_TEST_PATH
$incompatibleTypeHarness = @'
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @"
namespace ScribeEvidenceNative
{
    public sealed class BoundPendingFile { }
}
"@
$failure = $null
try {
    . $env:SCRIBE_EVIDENCE_INCOMPATIBLE_TYPE_TEST_PATH
}
catch {
    $failure = $_.Exception.GetBaseException().Message
}
if ($failure -cne 'Restart PowerShell/session: incompatible native evidence type is already loaded.') {
    throw "Incompatible native evidence type did not fail clearly at load time: $failure"
}
Write-Output 'incompatible native evidence type rejected'
'@
try {
    $env:SCRIBE_EVIDENCE_INCOMPATIBLE_TYPE_TEST_PATH = $preflightPath
    $encodedHarness = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($incompatibleTypeHarness))
    $incompatibleTypeOutput = @(& (Join-Path $PSHOME 'pwsh.exe') -NoProfile -EncodedCommand $encodedHarness)
    if ($LASTEXITCODE -ne 0 -or
        $incompatibleTypeOutput.Count -ne 1 -or
        $incompatibleTypeOutput[0] -cne 'incompatible native evidence type rejected') {
        throw 'Fresh child PowerShell did not reject an incompatible native evidence type deterministically.'
    }
}
finally {
    $env:SCRIBE_EVIDENCE_INCOMPATIBLE_TYPE_TEST_PATH = $previousIncompatibleTypeTestPath
}
. (Join-Path $PSScriptRoot 'windows-vulkan-evidence-preflight.ps1')

if ((ConvertTo-ScribeVulkanEvidencePci 'native:0000:01:00.0') -cne '0000:01:00.0') { throw 'Native PCI parsing regressed.' }
if ((ConvertTo-ScribeVulkanEvidencePci '00000000:01:00.0') -cne '0000:01:00.0') { throw 'nvidia-smi PCI parsing regressed.' }
foreach ($value in @('native:0000:01:00.8', 'native:0000:01:00.0 ', 'uuid:secret')) {
    $accepted = $false
    try { $null = ConvertTo-ScribeVulkanEvidencePci $value; $accepted = $true } catch {}
    if ($accepted) { throw "Malformed PCI identity was accepted: $value" }
}
$fixturePackVersion = New-ScribeEvidenceFixturePackVersion ('a' * 40 -join '') ('b' * 12 -join '')
$fixtureCargoLeaf = "vulkan-$fixturePackVersion-cargo"
if ($fixturePackVersion -cne 'fixture-aaaaaaaaaaaa-bbbbbbbbbbbb' -or
    $fixtureCargoLeaf.Length -ne 46 -or
    $fixtureCargoLeaf -cnotmatch '^[a-z0-9](?:[a-z0-9._-]{0,46}[a-z0-9])?$') {
    throw 'Fixture pack version does not compose into the builder Cargo target bound.'
}
foreach ($invalidPackVersionInput in @(
    @('A' * 40 -join '', 'b' * 12 -join ''),
    @('a' * 40 -join '', 'b' * 11 -join '')
)) {
    $accepted = $false
    try { $null = New-ScribeEvidenceFixturePackVersion $invalidPackVersionInput[0] $invalidPackVersionInput[1]; $accepted = $true } catch {}
    if ($accepted) { throw 'Noncanonical fixture pack version input was accepted.' }
}
$knownCmakeFailure = @(
    'error: failed to run custom build command for `transcribe-cpp-sys v0.1.3`',
    '  Error: failed to execute command: cmake -S C:\\safe\\source',
    '  The directory name is invalid. (os error 267)'
)
if (-not (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure $knownCmakeFailure)) { throw 'Known bounded CMake failure was not classified.' }
foreach ($malformedCmakeFailure in @(
    @('transcribe-cpp-sys v0.1.3', 'failed to execute command:', 'The directory name is invalid. (os error 267)'),
    @('error: failed to run custom build command for `transcribe-cpp-sys v0.1.4`', '  Error: failed to execute command: cmake', '  The directory name is invalid. (os error 267)'),
    @('error: failed to run custom build command for `transcribe-cpp-sys v0.1.3`', '  Error: failed to execute command: cmake', '  access denied'),
    @('  The directory name is invalid. (os error 267)', '  Error: failed to execute command: cmake', 'error: failed to run custom build command for `transcribe-cpp-sys v0.1.3`'),
    @('error: failed to run custom build command for `transcribe-cpp-sys v0.1.3', '  Error: failed to execute command: cmake', '  The directory name is invalid. (os error 267)')
)) {
    if (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure $malformedCmakeFailure) { throw 'Malformed CMake failure was classified.' }
}
$sanitizedClassifierResult = Test-ScribeGpuWorkerKnownCmakeBootstrapFailure @('secret-token', 'unrelated failure')
if ($sanitizedClassifierResult -isnot [bool] -or $sanitizedClassifierResult) { throw 'CMake classifier exposed or accepted unrelated output.' }
$overlongCmakeFailure = [System.Collections.Generic.List[object]]::new()
foreach ($unused in 1..2048) { $overlongCmakeFailure.Add('noise') }
foreach ($line in $knownCmakeFailure) { $overlongCmakeFailure.Add($line) }
if (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure -Output $overlongCmakeFailure.ToArray()) { throw 'Unbounded CMake output was classified outside the bounded window.' }
$previousTestWorkerDigest = $env:SCRIBE_BUNDLED_WORKER_SHA256
$previousTestBuildingWorker = $env:SCRIBE_BUILDING_WORKER
try {
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = 'a' * 64 -join ''
    $env:SCRIBE_BUILDING_WORKER = 'ambient'
    Set-ScribeEvidenceWorkerBuildMode $true
    if ($env:SCRIBE_BUILDING_WORKER -cne '1' -or $null -ne $env:SCRIBE_BUNDLED_WORKER_SHA256) { throw 'Worker build mode did not clear the desktop digest.' }
    Set-ScribeEvidenceWorkerBuildMode $false
    if ($null -ne $env:SCRIBE_BUILDING_WORKER -or $null -ne $env:SCRIBE_BUNDLED_WORKER_SHA256) { throw 'Harness mode retained worker-build state.' }
}
finally {
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $previousTestWorkerDigest
    $env:SCRIBE_BUILDING_WORKER = $previousTestBuildingWorker
}
$previousToolchainTestValue = $env:SCRIBE_EVIDENCE_TOOLCHAIN_TEST
try {
    $env:SCRIBE_EVIDENCE_TOOLCHAIN_TEST = 'ambient'
    $toolchainRestoreState = Set-ScribeEvidenceProcessEnvironment ([ordered]@{ SCRIBE_EVIDENCE_TOOLCHAIN_TEST = 'pinned' })
    if ($env:SCRIBE_EVIDENCE_TOOLCHAIN_TEST -cne 'pinned') { throw 'Pinned toolchain environment was not applied.' }
    Restore-ScribeEvidenceProcessEnvironment $toolchainRestoreState
    if ($env:SCRIBE_EVIDENCE_TOOLCHAIN_TEST -cne 'ambient') { throw 'Pinned toolchain environment was not restored.' }
}
finally {
    $env:SCRIBE_EVIDENCE_TOOLCHAIN_TEST = $previousToolchainTestValue
}
$topologyRoot = Join-Path ([IO.Path]::GetTempPath()) ("scribe-evidence-topology-$([guid]::NewGuid().ToString('N'))")
try {
    $buildDirectory = Join-Path $topologyRoot 'cargo\\debug\\build\\transcribe-cpp-sys-0123456789abcdef\\out\\build'
    $outsideDirectory = Join-Path $topologyRoot 'outside'
    New-Item -ItemType Directory -Path $buildDirectory -Force | Out-Null
    New-Item -ItemType Directory -Path $outsideDirectory -Force | Out-Null
    $sentinel = Join-Path $outsideDirectory 'must-not-delete.txt'
    [IO.File]::WriteAllText($sentinel, 'sentinel')
    New-Item -ItemType Junction -Path (Join-Path $buildDirectory 'escaped') -Target $outsideDirectory | Out-Null
    $rejected = $false
    try { Assert-ScribeEvidenceNoReparseDescendants $buildDirectory } catch { $rejected = $true }
    if (-not $rejected) { throw 'CMake deletion topology accepted a descendant junction.' }
    if (-not (Test-Path -LiteralPath $sentinel -PathType Leaf)) { throw 'CMake topology validation deleted outside-scope data.' }
    $regularFileRejected = $false
    try { $null = Get-ScribeEvidencePhysicalDirectory $sentinel 'test file' } catch { $regularFileRejected = $true }
    if (-not $regularFileRejected) { throw 'CMake topology validation accepted a regular file as a directory.' }
}
finally {
    if (Test-Path -LiteralPath $topologyRoot) { Remove-Item -LiteralPath $topologyRoot -Recurse -Force }
}
$publicationRoot = Join-Path ([IO.Path]::GetTempPath()) ("scribe-evidence-publication-$([guid]::NewGuid().ToString('N'))")
try {
    New-Item -ItemType Directory -Path $publicationRoot | Out-Null
    $finalLeaf = 'windows-vulkan-fixture-evidence.json'
    $finalPath = Join-Path $publicationRoot $finalLeaf
    $partialLeaf = 'windows-vulkan-fixture-evidence.pending-partial.json'
    $partialPath = Join-Path $publicationRoot $partialLeaf
    [IO.File]::WriteAllText($partialPath, '{')
    $writeFailure = [InvalidOperationException]::new('forced evidence write failure')
    try {
        $null = Complete-ScribeEvidencePendingReport $partialPath $finalPath $publicationRoot $partialLeaf $finalLeaf $writeFailure @()
        throw 'Forced evidence write failure unexpectedly published.'
    }
    catch {
        if ($_.Exception.Message -cne 'forced evidence write failure') { throw }
    }
    if ((Test-Path -LiteralPath $partialPath) -or (Test-Path -LiteralPath $finalPath)) {
        throw 'Forced evidence write failure left a pending or final artifact.'
    }

    $invalidLeaf = 'windows-vulkan-fixture-evidence.pending-invalid.json'
    $invalidPath = Join-Path $publicationRoot $invalidLeaf
    [IO.File]::WriteAllText($invalidPath, '{')
    $invalidFailure = $null
    try {
        $null = Complete-ScribeEvidencePendingReport $invalidPath $finalPath $publicationRoot $invalidLeaf $finalLeaf $null @()
    }
    catch {
        $invalidFailure = $_.Exception
    }
    if ($null -eq $invalidFailure -or
        (Test-Path -LiteralPath $invalidPath) -or
        (Test-Path -LiteralPath $finalPath)) {
        throw 'Invalid partial evidence was published or retained after validation failure.'
    }

    $topologyTarget = Join-Path $publicationRoot 'handle-topology-target'
    New-Item -ItemType Directory -Path $topologyTarget | Out-Null
    $topologyLeaf = 'windows-vulkan-fixture-evidence.pending-topology.json'
    [IO.File]::WriteAllText((Join-Path $topologyTarget $topologyLeaf), '{}')
    $topologyJunction = Join-Path (Split-Path -Parent $publicationRoot) ("scribe-evidence-publication-junction-$([guid]::NewGuid().ToString('N'))")
    try {
        New-Item -ItemType Junction -Path $topologyJunction -Target $topologyTarget | Out-Null
        $topologyRejected = $false
        try {
            $topologyBinding = [ScribeEvidenceNative.BoundPendingFile]::Open(
                $topologyJunction,
                (Join-Path $topologyJunction $topologyLeaf),
                $topologyLeaf,
                1MB,
                $false,
                $false
            )
            $topologyBinding.Dispose()
        }
        catch {
            $topologyRejected = $true
        }
        if (-not $topologyRejected) { throw 'Handle publication accepted a reparse evidence root.' }
    }
    finally {
        Remove-Item -LiteralPath $topologyJunction -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $topologyTarget -Recurse -Force
    }

    $guardLeaf = 'windows-vulkan-fixture-evidence.pending-guard.json'
    $guardPath = Join-Path $publicationRoot $guardLeaf
    [IO.File]::WriteAllText($guardPath, '{}')
    $guardFailure = [InvalidOperationException]::new('forced final Auto guard failure')
    try {
        $null = Complete-ScribeEvidencePendingReport $guardPath $finalPath $publicationRoot $guardLeaf $finalLeaf $null @($guardFailure)
        throw 'Forced final guard failure unexpectedly published.'
    }
    catch {
        if ($_.Exception.Message -cne 'forced final Auto guard failure') { throw }
    }
    if ((Test-Path -LiteralPath $guardPath) -or (Test-Path -LiteralPath $finalPath)) {
        throw 'Forced final guard failure left a pending or final artifact.'
    }

    $cleanupLeaf = 'windows-vulkan-fixture-evidence.pending-cleanup.json'
    $cleanupPath = Join-Path $publicationRoot $cleanupLeaf
    New-Item -ItemType Directory -Path $cleanupPath | Out-Null
    $primaryFailure = [InvalidOperationException]::new('forced primary harness failure')
    $observedPrimary = $null
    try {
        $null = Complete-ScribeEvidencePendingReport $cleanupPath $finalPath $publicationRoot $cleanupLeaf $finalLeaf $primaryFailure @()
    }
    catch {
        $observedPrimary = $_.Exception
    }
    if ($null -eq $observedPrimary -or
        $observedPrimary.Message -cne 'forced primary harness failure' -or
        $observedPrimary.Data.Count -eq 0 -or
        (Test-Path -LiteralPath $finalPath)) {
        throw 'Pending cleanup failure masked the primary failure or published final evidence.'
    }
    Remove-Item -LiteralPath $cleanupPath -Force

    $hardlinkLeaf = 'windows-vulkan-fixture-evidence.pending-hardlink.json'
    $hardlinkPath = Join-Path $publicationRoot $hardlinkLeaf
    $hardlinkAlias = Join-Path $publicationRoot 'windows-vulkan-fixture-evidence.pending-hardlink-alias.json'
    [IO.File]::WriteAllText($hardlinkPath, '{}')
    New-Item -ItemType HardLink -Path $hardlinkAlias -Target $hardlinkPath | Out-Null
    $hardlinkPrimary = [InvalidOperationException]::new('forced hardlink cleanup primary failure')
    $observedHardlinkPrimary = $null
    try {
        $null = Complete-ScribeEvidencePendingReport $hardlinkPath $finalPath $publicationRoot $hardlinkLeaf $finalLeaf $hardlinkPrimary @()
    }
    catch {
        $observedHardlinkPrimary = $_.Exception
    }
    if ($null -eq $observedHardlinkPrimary -or
        $observedHardlinkPrimary.Message -cne 'forced hardlink cleanup primary failure' -or
        $observedHardlinkPrimary.Data.Count -eq 0 -or
        -not (Test-Path -LiteralPath $hardlinkPath -PathType Leaf) -or
        -not (Test-Path -LiteralPath $hardlinkAlias -PathType Leaf) -or
        (Test-Path -LiteralPath $finalPath)) {
        throw 'Hardlinked pending cleanup did not fail closed while preserving the primary failure.'
    }
    Remove-Item -LiteralPath $hardlinkAlias -Force
    Remove-Item -LiteralPath $hardlinkPath -Force

    $adsLeaf = 'windows-vulkan-fixture-evidence.pending-ads.json'
    $adsPath = Join-Path $publicationRoot $adsLeaf
    [IO.File]::WriteAllText($adsPath, '{}')
    [IO.File]::WriteAllText("$adsPath`:forbidden", 'x')
    $adsPrimary = [InvalidOperationException]::new('forced ADS cleanup primary failure')
    $observedAdsPrimary = $null
    try {
        $null = Complete-ScribeEvidencePendingReport $adsPath $finalPath $publicationRoot $adsLeaf $finalLeaf $adsPrimary @()
    }
    catch {
        $observedAdsPrimary = $_.Exception
    }
    if ($null -eq $observedAdsPrimary -or
        $observedAdsPrimary.Message -cne 'forced ADS cleanup primary failure' -or
        $observedAdsPrimary.Data.Count -eq 0 -or
        -not (Test-Path -LiteralPath $adsPath -PathType Leaf) -or
        (Test-Path -LiteralPath $finalPath)) {
        throw 'ADS-bearing pending cleanup did not fail closed while preserving the primary failure.'
    }
    Remove-Item -LiteralPath $adsPath -Force

    $statistics = [ordered]@{ p50_ms = 1; p95_ms = 1 }
    $coldRunSet = [ordered]@{
        end_to_end = $statistics
        end_to_end_ms = @(1, 1, 1, 1, 1)
        backend_processing = $statistics
        backend_processing_ms = @(1, 1, 1, 1, 1)
        model_load = $statistics
        model_load_ms = @(1, 1, 1, 1, 1)
    }
    $warmRunSet = [ordered]@{
        end_to_end = $statistics
        end_to_end_ms = @(1) * 20
        backend_processing = $statistics
        backend_processing_ms = @(1) * 20
        model_load = $null
        model_load_ms = $null
    }
    $validReport = [ordered]@{
        schema_version = 1
        fixture_only = $true
        untrusted = $true
        auto_eligible = $false
        source_revision = 'a' * 40 -join ''
        pack = [ordered]@{ id = 'fixture'; version = 'fixture'; digest = 'b' * 64 -join ''; security_epoch = 1; runtime_abi = 1 }
        model_sha256 = 'c' * 64 -join ''
        wav_sha256 = 'd' * 64 -join ''
        gpu = [ordered]@{ backend = 'vulkan'; provider = 'transcribe-cpp-ggml-vulkan'; vendor = 'nvidia'; device_class = 'discrete_gpu'; driver = 'fixture'; memory_total_bytes = 1 }
        nvidia_baseline = [ordered]@{ product = 'fixture'; driver = 'fixture'; memory_total_bytes = 1; memory_used_bytes = 0; gpu_utilization_percent = 0 }
        cold_runs_per_backend = 5
        warm_runs_per_backend = 20
        cpu = [ordered]@{ cold = $coldRunSet; warm = $warmRunSet }
        vulkan = [ordered]@{ cold = $coldRunSet; warm = $warmRunSet }
        expected_phrase_present_every_run = $true
        normalized_transcript_parity = $true
        same_device_internally_verified = $true
    }

    $bindingLeaf = 'windows-vulkan-fixture-evidence.pending-binding.json'
    $bindingPath = Join-Path $publicationRoot $bindingLeaf
    $bindingSwapPath = Join-Path $publicationRoot 'windows-vulkan-fixture-evidence.pending-binding-swap.json'
    $bindingBytes = [Text.UTF8Encoding]::new($false).GetBytes(($validReport | ConvertTo-Json -Depth 10 -Compress))
    [IO.File]::WriteAllBytes($bindingPath, $bindingBytes)
    $mismatchedLeafRejected = $false
    try {
        $mismatchedBinding = [ScribeEvidenceNative.BoundPendingFile]::Open(
            $publicationRoot,
            $bindingPath,
            'windows-vulkan-fixture-evidence.pending-different.json',
            1MB,
            $false,
            $false
        )
        $mismatchedBinding.Dispose()
    }
    catch {
        $mismatchedLeafRejected = $true
    }
    if (-not $mismatchedLeafRejected) { throw 'Handle publication accepted a mismatched pending leaf identity.' }
    $binding = [ScribeEvidenceNative.BoundPendingFile]::Open(
        $publicationRoot,
        $bindingPath,
        $bindingLeaf,
        1MB,
        $false,
        $false
    )
    try {
        $moveBlocked = $false
        try { [IO.File]::Move($bindingPath, $bindingSwapPath) } catch { $moveBlocked = $true }
        if (-not $moveBlocked) {
            [IO.File]::Move($bindingSwapPath, $bindingPath)
            throw 'Bound pending evidence identity remained replaceable.'
        }
        $writeBlocked = $false
        try {
            $writeAttempt = [IO.File]::Open($bindingPath, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::None)
            $writeAttempt.Dispose()
        }
        catch {
            $writeBlocked = $true
        }
        if (-not $writeBlocked) { throw 'Bound pending evidence identity remained writable.' }

        $rootMoveTarget = "$publicationRoot-moved"
        $rootMoveSucceeded = $false
        try {
            [IO.Directory]::Move($publicationRoot, $rootMoveTarget)
            $rootMoveSucceeded = $true
        }
        catch {}
        if ($rootMoveSucceeded) {
            [IO.Directory]::Move($rootMoveTarget, $publicationRoot)
            throw 'Bound pending evidence did not stabilize its directory topology.'
        }

        $boundRead = $binding.ReadAllAndHash()
        $expectedBoundDigest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bindingBytes)).ToLowerInvariant()
        if ($boundRead.Sha256 -cne $expectedBoundDigest -or
            -not [Linq.Enumerable]::SequenceEqual[byte]($boundRead.Bytes, $bindingBytes) -or
            $binding.Identity -cnotmatch '^[0-9a-f]{8}:[0-9a-f]{16}$') {
            throw 'Bound evidence read did not describe the exact locked identity and bytes.'
        }
    }
    finally {
        $binding.Dispose()
    }
    Remove-ScribeEvidencePendingReport $bindingPath $publicationRoot $bindingLeaf

    $collisionLeaf = 'windows-vulkan-fixture-evidence.pending-collision.json'
    $collisionPath = Join-Path $publicationRoot $collisionLeaf
    [IO.File]::WriteAllBytes($collisionPath, $bindingBytes)
    [IO.File]::WriteAllText($finalPath, 'destination-sentinel')
    $collisionFailure = $null
    try {
        $null = Complete-ScribeEvidencePendingReport $collisionPath $finalPath $publicationRoot $collisionLeaf $finalLeaf $null @()
    }
    catch {
        $collisionFailure = $_.Exception
    }
    if ($null -eq $collisionFailure -or
        (Test-Path -LiteralPath $collisionPath) -or
        [IO.File]::ReadAllText($finalPath) -cne 'destination-sentinel') {
        throw 'Handle publication replaced an existing final destination or retained its pending source.'
    }
    Remove-Item -LiteralPath $finalPath -Force

    $successLeaf = 'windows-vulkan-fixture-evidence.pending-success.json'
    $successPath = Join-Path $publicationRoot $successLeaf
    [IO.File]::WriteAllBytes($successPath, $bindingBytes)
    $published = Complete-ScribeEvidencePendingReport $successPath $finalPath $publicationRoot $successLeaf $finalLeaf $null @()
    $publishedBytes = [IO.File]::ReadAllBytes($finalPath)
    if ((Test-Path -LiteralPath $successPath) -or
        -not (Test-Path -LiteralPath $finalPath -PathType Leaf) -or
        [string]$published.Path -cne $finalPath -or
        [string]$published.Digest -cne $expectedBoundDigest -or
        [string]$published.Identity -cnotmatch '^[0-9a-f]{8}:[0-9a-f]{16}$' -or
        -not [Linq.Enumerable]::SequenceEqual[byte]($publishedBytes, $bindingBytes)) {
        throw 'Validated pending evidence was not atomically published to the final path.'
    }

    $expectedJson = [Text.UTF8Encoding]::new($false, $true).GetString($bindingBytes)
    $verified = Read-ScribeVerifiedEvidenceReport $finalPath $expectedBoundDigest
    if ($verified -isnot [ScribeEvidenceNative.BoundVerifiedEvidence] -or
        $verified.Sha256 -cne $expectedBoundDigest -or
        $verified.Utf8Json -cne $expectedJson -or
        $verified.Identity -cnotmatch '^[0-9a-f]{8}:[0-9a-f]{16}$' -or
        $verified.PSObject.Properties.Name -contains 'Bytes') {
        throw 'Consumer verifier did not return the exact validated immutable evidence representation.'
    }

    $strictSchemaCases = [ordered]@{
        comment = "/* fixture comment */$expectedJson"
        'trailing-comma' = $expectedJson.Substring(0, $expectedJson.Length - 1) + ',}'
        'duplicate-key' = '{"schema_version":1,' + $expectedJson.Substring(1)
        'nested-duplicate-key' = $expectedJson.Replace('"pack":{"id":"fixture",', '"pack":{"id":"fixture","id":"fixture",')
        'string-boolean' = $expectedJson.Replace('"fixture_only":true', '"fixture_only":"true"')
        'string-count' = $expectedJson.Replace('"cold_runs_per_backend":5', '"cold_runs_per_backend":"5"')
        'fractional-integer' = $expectedJson.Replace('"security_epoch":1', '"security_epoch":1.0')
        'exponent-integer' = $expectedJson.Replace('"runtime_abi":1', '"runtime_abi":1e0')
    }
    foreach ($strictSchemaCase in $strictSchemaCases.GetEnumerator()) {
        if ([string]$strictSchemaCase.Value -ceq $expectedJson) {
            throw "Strict-schema regression fixture did not mutate the valid JSON: $($strictSchemaCase.Key)"
        }
        $strictLeaf = "windows-vulkan-fixture-evidence-strict-$($strictSchemaCase.Key).json"
        $strictPath = Join-Path $publicationRoot $strictLeaf
        $strictBytes = [Text.UTF8Encoding]::new($false).GetBytes([string]$strictSchemaCase.Value)
        $strictDigest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($strictBytes)).ToLowerInvariant()
        [IO.File]::WriteAllBytes($strictPath, $strictBytes)
        $strictFailure = $null
        try {
            $null = Read-ScribeVerifiedEvidenceReport $strictPath $strictDigest
        }
        catch {
            $strictFailure = $_.Exception.GetBaseException().Message
        }
        finally {
            Remove-Item -LiteralPath $strictPath -Force -ErrorAction SilentlyContinue
        }
        if ($strictFailure -cne 'Evidence report violates the strict JSON schema.') {
            throw "Full consumer verifier did not reject $($strictSchemaCase.Key) at the strict-schema boundary."
        }
    }

    $wrongDigest = if ($expectedBoundDigest[0] -ceq '0') {
        '1' + $expectedBoundDigest.Substring(1)
    }
    else {
        '0' + $expectedBoundDigest.Substring(1)
    }
    $wrongDigestFailure = $null
    try { $null = Read-ScribeVerifiedEvidenceReport $finalPath $wrongDigest } catch { $wrongDigestFailure = $_.Exception }
    if ($null -eq $wrongDigestFailure -or
        $wrongDigestFailure.GetBaseException().Message -cne 'Published evidence SHA-256 does not match the independently supplied digest.') {
        throw 'Consumer verifier did not reject the wrong caller-supplied digest at the digest boundary.'
    }

    foreach ($invalidDigest in @('a' * 63 -join '', 'A' * 64 -join '', "$('a' * 64 -join '') ")) {
        $invalidDigestRejected = $false
        try { $null = Read-ScribeVerifiedEvidenceReport $finalPath $invalidDigest } catch { $invalidDigestRejected = $true }
        if (-not $invalidDigestRejected) { throw 'Consumer verifier accepted a noncanonical caller-supplied digest.' }
    }

    $publishedAlias = Join-Path $publicationRoot 'windows-vulkan-fixture-evidence-hardlink-alias.json'
    New-Item -ItemType HardLink -Path $publishedAlias -Target $finalPath | Out-Null
    $linkedVerified = Read-ScribeVerifiedEvidenceReport $finalPath $expectedBoundDigest
    if ($linkedVerified.Utf8Json -cne $expectedJson -or $linkedVerified.Sha256 -cne $expectedBoundDigest) {
        throw 'Consumer verifier treated link count as trust instead of verifying exact bound bytes.'
    }
    $tamperedBytes = [byte[]]$bindingBytes.Clone()
    $tamperedBytes[0] = if ($tamperedBytes[0] -eq 0x7b) { 0x5b } else { 0x7b }
    [IO.File]::WriteAllBytes($publishedAlias, $tamperedBytes)
    if ((Get-Item -LiteralPath $finalPath).Length -ne $bindingBytes.Length) {
        throw 'Hardlink tamper regression did not preserve the evidence byte length.'
    }
    Remove-Item -LiteralPath $publishedAlias -Force
    if (Test-Path -LiteralPath $publishedAlias) {
        throw 'Hardlink tamper regression did not remove the alias before consumer verification.'
    }
    $tamperFailure = $null
    try { $null = Read-ScribeVerifiedEvidenceReport $finalPath $expectedBoundDigest } catch { $tamperFailure = $_.Exception }
    if ($null -eq $tamperFailure -or
        $tamperFailure.GetBaseException().Message -cne 'Published evidence SHA-256 does not match the independently supplied digest.') {
        throw 'Consumer verifier did not reject same-length hardlink tampering at the digest boundary.'
    }
    if ($verified.Utf8Json -cne $expectedJson) {
        throw 'Previously verified immutable evidence changed after the on-disk identity was mutated.'
    }
}
finally {
    if (Test-Path -LiteralPath $publicationRoot) { Remove-Item -LiteralPath $publicationRoot -Recurse -Force }
}
$runner = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'run-windows-vulkan-evidence.ps1') -Raw
$runnerTokens = $null
$runnerParseErrors = $null
$runnerAst = [Management.Automation.Language.Parser]::ParseInput($runner, [ref]$runnerTokens, [ref]$runnerParseErrors)
if ($runnerParseErrors.Count -ne 0) { throw 'Runner source could not be parsed for retry-path tests.' }
$runnerRetryFunction = $runnerAst.Find({
    param($Ast)
    $Ast -is [Management.Automation.Language.FunctionDefinitionAst] -and
    $Ast.Name -ceq 'Invoke-ScribeEvidenceCargoWithCmakeRetry'
}, $true)
if ($null -eq $runnerRetryFunction) { throw 'Runner lost its Cargo retry function.' }
$runnerPinnedEnvironmentFunction = $runnerAst.Find({
    param($Ast)
    $Ast -is [Management.Automation.Language.FunctionDefinitionAst] -and
    $Ast.Name -ceq 'Invoke-ScribeEvidenceWithPinnedMsvcEnvironment'
}, $true)
if ($null -eq $runnerPinnedEnvironmentFunction) { throw 'Runner lost its pinned-environment function.' }
. ([scriptblock]::Create($runnerPinnedEnvironmentFunction.Extent.Text))
$originalRestoreFunction = (Get-Command Restore-ScribeEvidenceProcessEnvironment -CommandType Function).ScriptBlock
$previousRestoreFailureTest = $env:SCRIBE_EVIDENCE_RESTORE_FAILURE_TEST
try {
    $env:SCRIBE_EVIDENCE_RESTORE_FAILURE_TEST = 'ambient'
    Set-Item -LiteralPath Function:Restore-ScribeEvidenceProcessEnvironment -Value {
        param([psobject[]]$Previous)
        throw 'forced pinned restore failure'
    }
    $observedOperationFailure = $null
    try {
        Invoke-ScribeEvidenceWithPinnedMsvcEnvironment `
            ([ordered]@{ SCRIBE_EVIDENCE_RESTORE_FAILURE_TEST = 'pinned' }) `
            { throw 'forced pinned operation failure' }
    }
    catch {
        $observedOperationFailure = $_.Exception
    }
    if ($null -eq $observedOperationFailure -or
        $observedOperationFailure.Message -cne 'forced pinned operation failure' -or
        $observedOperationFailure.Data.Count -eq 0 -or
        @($observedOperationFailure.Data.Values) -cnotcontains 'forced pinned restore failure') {
        throw 'Pinned environment restoration masked the wrapped operation failure.'
    }
}
finally {
    Set-Item -LiteralPath Function:Restore-ScribeEvidenceProcessEnvironment -Value $originalRestoreFunction
    $env:SCRIBE_EVIDENCE_RESTORE_FAILURE_TEST = $previousRestoreFailureTest
}
$runnerRetryHarness = Join-Path ([IO.Path]::GetTempPath()) ("scribe-evidence-runner-retry-$([guid]::NewGuid().ToString('N')).ps1")
try {
    $runnerRetryHarnessTail = @'
$cargo = Join-Path $PSHOME 'pwsh.exe'
$script:BootstrapCount = 0
function Enable-ScribeEvidenceCmakeBootstrap([string]$CargoTarget, [string]$BuildEnvironment) {
    $script:BootstrapCount++
}
function ConvertTo-TestEncodedCommand([string]$Command) {
    return [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($Command))
}
function Invoke-TestRunnerFailure([string]$Command) {
    $script:BootstrapCount = 0
    $message = $null
    try {
        Invoke-ScribeEvidenceCargoWithCmakeRetry @('-NoProfile', '-EncodedCommand', (ConvertTo-TestEncodedCommand $Command)) 'runner case failed.' 'unused-target' 'unused-environment'
    }
    catch {
        $message = $_.Exception.Message
    }
    if ($message -cne 'runner case failed.' -or $script:BootstrapCount -ne 0) {
        throw 'Runner retried or leaked diagnostics for an ineligible failure.'
    }
}

$state = Join-Path ([IO.Path]::GetTempPath()) ("scribe-evidence-retry-state-$([guid]::NewGuid().ToString('N'))")
$previousState = $env:SCRIBE_EVIDENCE_RETRY_TEST_STATE
try {
    $env:SCRIBE_EVIDENCE_RETRY_TEST_STATE = $state
    $highVolumeSplitRetry = @"
`$state = `$env:SCRIBE_EVIDENCE_RETRY_TEST_STATE
if (Test-Path -LiteralPath `$state) { exit 0 }
[IO.File]::WriteAllText(`$state, 'first')
[Console]::Out.WriteLine('transcribe-cpp-sys: could not create short build junction C:\safe\tcs; building in OUT_DIR (may exceed Windows MAX_PATH in deep checkouts)')
[Console]::Out.WriteLine('error: failed to run custom build command for ``transcribe-cpp-sys v0.1.3``')
1..900 | ForEach-Object { [Console]::Out.WriteLine('bounded stdout noise') }
[Console]::Error.WriteLine('vulkan-shaders-gen: warning: object directory is near the configured limit')
1..900 | ForEach-Object { [Console]::Error.WriteLine('bounded stderr noise') }
[Console]::Error.WriteLine('CMAKE_OBJECT_PATH_MAX is in effect for this nested target')
[Console]::Error.WriteLine("LINK : fatal error LNK1104: cannot open file 'CMakeFiles\cmTC_1a2B3c.dir\intermediate.manifest'")
exit 17
"@
    Invoke-ScribeEvidenceCargoWithCmakeRetry @('-NoProfile', '-EncodedCommand', (ConvertTo-TestEncodedCommand $highVolumeSplitRetry)) 'high-volume retry failed.' 'unused-target' 'unused-environment'
    if ($script:BootstrapCount -ne 1 -or -not (Test-Path -LiteralPath $state)) {
        throw 'Runner did not perform exactly one eligible high-volume split-stream retry.'
    }

    foreach ($malformed in @(
        @"
[Console]::Out.WriteLine('error: failed to run custom build command for ``transcribe-cpp-sys v0.1.3``')
[Console]::Error.WriteLine('The directory name is invalid. (os error 267)')
exit 19
"@,
        @"
[Console]::Out.WriteLine('The directory name is invalid. (os error 267)')
[Console]::Error.WriteLine('Error: failed to execute command: cmake')
[Console]::Error.WriteLine('error: failed to run custom build command for ``transcribe-cpp-sys v0.1.3``')
exit 20
"@,
        @"
[Console]::Out.WriteLine('error: failed to run custom build command for ``transcribe-cpp-sys v0.1.3')
[Console]::Error.WriteLine('Error: failed to execute command: cmake')
[Console]::Error.WriteLine('The directory name is invalid. (os error 267)')
exit 21
"@,
        @"
[Console]::Out.WriteLine('error: failed to run custom build command for ``transcribe-cpp-sys v0.1.3``')
[Console]::Error.WriteLine('Error: failed to execute command: cmake')
[Console]::Error.WriteLine('access denied')
exit 22
"@
    )) {
        Invoke-TestRunnerFailure $malformed
    }

    $overflow = @"
[Console]::Out.WriteLine('error: failed to run custom build command for ``transcribe-cpp-sys v0.1.3``')
[Console]::Error.WriteLine('Error: failed to execute command: cmake')
[Console]::Error.WriteLine('The directory name is invalid. (os error 267)')
[Console]::Error.Write('x' * 1025)
exit 23
"@
    Invoke-TestRunnerFailure $overflow

    Remove-Item -LiteralPath $state -Force
    $alwaysFail = @"
`$state = `$env:SCRIBE_EVIDENCE_RETRY_TEST_STATE
[IO.File]::AppendAllText(`$state, 'x')
[Console]::Out.WriteLine('error: failed to run custom build command for ``transcribe-cpp-sys v0.1.3``')
[Console]::Error.WriteLine('Error: failed to execute command: cmake')
[Console]::Error.WriteLine('The directory name is invalid. (os error 267)')
exit 29
"@
    $script:BootstrapCount = 0
    $retryFailure = $null
    try {
        Invoke-ScribeEvidenceCargoWithCmakeRetry @('-NoProfile', '-EncodedCommand', (ConvertTo-TestEncodedCommand $alwaysFail)) 'bounded retry failed.' 'unused-target' 'unused-environment'
    }
    catch {
        $retryFailure = $_.Exception.Message
    }
    if ($retryFailure -cne 'bounded retry failed. after validated CMake bootstrap retry.' -or
        $script:BootstrapCount -ne 1 -or
        [IO.File]::ReadAllText($state).Length -ne 2) {
        throw 'Runner exceeded or lost its exact one-retry contract.'
    }
}
finally {
    $env:SCRIBE_EVIDENCE_RETRY_TEST_STATE = $previousState
    Remove-Item -LiteralPath $state -Force -ErrorAction SilentlyContinue
}
Write-Output 'runner retry harness passed'
'@
    [IO.File]::WriteAllText(
        $runnerRetryHarness,
        @(
            (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'windows-gpu-worker-cmake-bootstrap.ps1') -Raw),
            $runnerRetryFunction.Extent.Text,
            $runnerRetryHarnessTail
        ) -join "`r`n`r`n",
        [Text.UTF8Encoding]::new($false)
    )
    $runnerRetryOutput = @(& (Join-Path $PSHOME 'pwsh.exe') -NoProfile -File $runnerRetryHarness)
    if ($LASTEXITCODE -ne 0 -or $runnerRetryOutput.Count -ne 1 -or $runnerRetryOutput[0] -cne 'runner retry harness passed') {
        throw 'Runner bounded CMake retry harness failed.'
    }
}
finally {
    Remove-Item -LiteralPath $runnerRetryHarness -Force -ErrorAction SilentlyContinue
}
foreach ($required in @('--locked', '--offline', '-SigningMode Fixture', '--ignored', '--exact', '--test-threads=1', '--no-run', 'Invoke-ScribeEvidenceCargoWithCmakeRetry', 'Enable-ScribeEvidenceCmakeBootstrap', 'Assert-ScribeEvidenceNoReparseDescendants', 'Get-ScribeEvidencePinnedMsvcEnvironment', 'Invoke-ScribeEvidenceWithPinnedMsvcEnvironment', '-ToolchainCheckOnly -ExportPinnedMsvcEnvironment', 'Set-ScribeEvidenceWorkerBuildMode $true', 'Set-ScribeEvidenceWorkerBuildMode $false', 'previousWorkerDigest', 'previousBuildingWorker', 'transcribe-cpp-sys-[0-9a-f]{16}', 'onnx_worker::tests::windows_vulkan_fixture_evidence_captures_five_cold_and_twenty_warm_runs', 'gpu-auto-qualification-windows-x64.json', 'Production signing/release input is forbidden', 'Resolve-ScribeEvidenceFreshDirectory', 'Evidence output may not be under source')) {
    if ($runner -notmatch [regex]::Escape($required)) { throw "Runner is missing required source contract: $required" }
}
foreach ($required in @('$operationFailure = $null', '$restoreFailure = $null', 'Add-ScribeEvidenceSecondaryFailures $operationFailure @($restoreFailure)', 'if ($null -ne $restoreFailure) { throw $restoreFailure }')) {
    if ($runner -notmatch [regex]::Escape($required)) { throw "Pinned toolchain failure provenance is missing: $required" }
}
foreach ($required in @('Get-FileHash -LiteralPath $model', 'Get-FileHash -LiteralPath $wav', 'Test-ScribeEvidenceActivationPath', 'Test-ScribeEvidenceWithin', 'New-ScribeEvidenceShortCargoTarget', 'Assert-ScribeEvidenceSingleLinkFile', 'Get-ScribeVulkanEvidenceActualSystem32', 'fsutil.exe', 'manifest.json', 'Fixture pack build identity is not bound', 'New-ScribeEvidenceFixturePackVersion $revision', 'Fixture-only untrusted Vulkan evidence', 'previousEvidenceEnvironment')) {
    if ($runner -notmatch [regex]::Escape($required)) { throw "Runner is missing required safety contract: $required" }
}
foreach ($required in @('Independent consumer-bound SHA-256 (capture this stdout value)', 'The on-disk evidence path is untrusted without that independently captured digest', 'Read-ScribeVerifiedEvidenceReport')) {
    if ($runner -notmatch [regex]::Escape($required)) { throw "Runner is missing consumer-bound output guidance: $required" }
}
$compileAt = $runner.IndexOf("'--no-run'")
$baselineAt = $runner.IndexOf('Get-ScribeVulkanEvidenceNvidiaBaseline')
if ($compileAt -lt 0 -or $baselineAt -lt 0 -or $compileAt -ge $baselineAt) { throw 'Runner must precompile before NVIDIA baseline capture.' }
$cpuBuildAt = $runner.IndexOf("Invoke-ScribeEvidenceCargoWithCmakeRetry @('build'")
if ($cpuBuildAt -lt 0) { throw 'Runner lost the CPU worker build invocation.' }
$workerModeAt = $runner.IndexOf('Set-ScribeEvidenceWorkerBuildMode $true')
$harnessModeAt = $runner.IndexOf('Set-ScribeEvidenceWorkerBuildMode $false', $cpuBuildAt)
$ignoredTestAt = $runner.IndexOf("Invoke-ScribeEvidence `$cargo @('test'")
if ($ignoredTestAt -lt 0) { throw 'Runner lost the ignored Vulkan evidence test invocation.' }
$executionModeAt = $runner.LastIndexOf('Set-ScribeEvidenceWorkerBuildMode $false', $ignoredTestAt)
$pinnedCpuAt = $runner.LastIndexOf('Invoke-ScribeEvidenceWithPinnedMsvcEnvironment $pinnedMsvcEnvironment', $cpuBuildAt)
$packBuildAt = $runner.IndexOf('& $packBuilder -Backend Vulkan -PackVersion $packVersion')
$pinnedHarnessAt = $runner.LastIndexOf('Invoke-ScribeEvidenceWithPinnedMsvcEnvironment $pinnedMsvcEnvironment', $compileAt)
$pinnedExecutionAt = $runner.LastIndexOf('Invoke-ScribeEvidenceWithPinnedMsvcEnvironment $pinnedMsvcEnvironment', $ignoredTestAt)
if ($workerModeAt -lt 0 -or $cpuBuildAt -lt 0 -or $workerModeAt -ge $cpuBuildAt -or
    $harnessModeAt -lt $cpuBuildAt -or $harnessModeAt -ge $compileAt -or
    $executionModeAt -lt $compileAt -or $executionModeAt -ge $ignoredTestAt -or
    $pinnedCpuAt -lt 0 -or $pinnedCpuAt -ge $packBuildAt -or
    $pinnedHarnessAt -lt 0 -or $pinnedHarnessAt -gt $compileAt -or
    $pinnedExecutionAt -lt $compileAt -or $pinnedExecutionAt -ge $ignoredTestAt) {
    throw 'Runner worker-build mode is not isolated to the CPU worker build.'
}
if ($runner -match 'SigningMode Production|ProductionPrivateKeyPath|ProductionKeyId') { throw 'Fixture runner references a production signing input.' }
if ($runner -match '\$detail\s*=\s*\(\$first\s*\|\s*Out-String\)|throw\s+"\$Failure\s+\$detail"') { throw 'Runner can throw raw Cargo child output.' }
$preflight = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'windows-vulkan-evidence-preflight.ps1') -Raw
foreach ($required in @('GetSystemDirectory', 'nvidia-smi.exe', 'matching.Count -ne 1', '$utilization -gt 10', '$usedMib -gt ($totalMib / 4)', 'pci.bus_id')) {
    if ($preflight -notmatch [regex]::Escape($required)) { throw "Preflight is missing required source contract: $required" }
}
foreach ($required in @('CreateFileW', 'GetFileInformationByHandle', 'GetFileInformationByHandleEx', 'FileIdExtdDirectoryInfo', 'SetFileInformationByHandle', 'FileRenameInfo', 'FileDispositionInfo', 'ValidateOnlyUnnamedDataStream', 'ReadAllAndHash', 'RenameNoReplace', 'FileShareRead', 'OpenPublished', 'ReadAllAndVerify', 'CryptographicOperations.FixedTimeEquals', 'new UTF8Encoding(false, true)', 'BoundVerifiedEvidence', 'Read-ScribeVerifiedEvidenceReport', 'System.Text.Json', 'JsonDocument.Parse', 'AllowTrailingCommas = false', 'CommentHandling = JsonCommentHandling.Disallow', 'StringComparer.Ordinal', 'GetRawText()', 'NumberStyles.None', 'ConsumerApiVersion = 2', 'Restart PowerShell/session: incompatible native evidence type is already loaded.')) {
    if ($preflight -notmatch [regex]::Escape($required)) { throw "Handle-bound evidence publication is missing: $required" }
}
if ($preflight -match '\[IO\.File\]::Move\(' -or
    $preflight -match 'Get-FileHash\s+-LiteralPath\s+\$pending' -or
    $preflight -match 'Remove-Item\s+-LiteralPath\s+\$pending') {
    throw 'Evidence publication or cleanup reopened a validated pending path for mutation.'
}
$preflightTokens = $null
$preflightParseErrors = $null
$preflightAst = [Management.Automation.Language.Parser]::ParseInput($preflight, [ref]$preflightTokens, [ref]$preflightParseErrors)
if ($preflightParseErrors.Count -ne 0) { throw 'Preflight source could not be parsed for consumer-bound verifier tests.' }
$verifierFunction = $preflightAst.Find({
    param($Ast)
    $Ast -is [Management.Automation.Language.FunctionDefinitionAst] -and
    $Ast.Name -ceq 'Read-ScribeVerifiedEvidenceReport'
}, $true)
if ($null -eq $verifierFunction) { throw 'Consumer-bound verifier function is missing.' }
$strictSchemaFunction = $preflightAst.Find({
    param($Ast)
    $Ast -is [Management.Automation.Language.FunctionDefinitionAst] -and
    $Ast.Name -ceq 'Assert-ScribeEvidenceReportJson'
}, $true)
if ($null -eq $strictSchemaFunction) { throw 'Strict evidence schema function is missing.' }
$strictSchemaSource = $strictSchemaFunction.Extent.Text
$strictNativeAt = $strictSchemaSource.IndexOf('[ScribeEvidenceNative.StrictEvidenceJson]::Validate($Utf8Json)')
$convertFromJsonAt = $strictSchemaSource.IndexOf('$Utf8Json | ConvertFrom-Json')
if ($strictNativeAt -lt 0 -or $convertFromJsonAt -le $strictNativeAt) {
    throw 'Strict native JSON validation must precede PowerShell semantic materialization.'
}
$verifierSource = $verifierFunction.Extent.Text
$openPublishedAt = $verifierSource.IndexOf('::OpenPublished(')
$readVerifiedAt = $verifierSource.IndexOf('.ReadAllAndVerify(')
$schemaAt = $verifierSource.IndexOf('Assert-ScribeEvidenceReportJson $verified.Utf8Json')
$returnAt = $verifierSource.IndexOf('return $verified')
if ($openPublishedAt -lt 0 -or
    $readVerifiedAt -le $openPublishedAt -or
    $schemaAt -le $readVerifiedAt -or
    $returnAt -le $schemaAt -or
    $verifierSource.Substring($openPublishedAt) -match '\[IO\.File\]|Get-Content|Get-FileHash|ReadAllBytes|ReadAllText') {
    throw 'Consumer verifier reopened the path or lost its one-bound-representation validation order.'
}
$workerSource = Get-Content -LiteralPath (Join-Path $PSScriptRoot '..\src\onnx_worker.rs') -Raw
$evidenceTestAt = $workerSource.IndexOf('fn windows_vulkan_fixture_evidence_captures_five_cold_and_twenty_warm_runs()')
$probeShutdownAt = if ($evidenceTestAt -lt 0) { -1 } else { $workerSource.IndexOf('probe.shutdown().expect("fixture pack probe shutdown");', $evidenceTestAt) }
$probeDropAt = if ($probeShutdownAt -lt 0) { -1 } else { $workerSource.IndexOf('drop(probe);', $probeShutdownAt) }
$leaseCleanupAt = if ($probeDropAt -lt 0) { -1 } else { $workerSource.IndexOf('write_vulkan_evidence_after_cleanup(&inputs.output', $probeDropAt) }
if ($evidenceTestAt -lt 0 -or $probeShutdownAt -lt $evidenceTestAt -or
    $probeDropAt -le $probeShutdownAt -or $leaseCleanupAt -le $probeDropAt) {
    throw 'Vulkan evidence test must drop the probe handles before fallible lease cleanup.'
}
Write-Output 'Windows Vulkan fixture-evidence script contracts passed.'
