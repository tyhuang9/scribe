$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) "slfi-$([guid]::NewGuid().ToString('N'))"
$fixtureRoot = Join-Path $testRoot 'fixture'
$freezeRoot = Join-Path $testRoot 'freeze'
$bundleRoot = Join-Path $testRoot 'bundle'
$outputRoot = Join-Path $testRoot 'installer-output'
$compilerRoot = Join-Path $testRoot 'compiler'
$script:Assertions = 0

function Assert-True([bool]$Value, [string]$Description) {
    $script:Assertions++
    if (-not $Value) { throw $Description }
}

function Assert-Equal([object]$Actual, [object]$Expected, [string]$Description) {
    $script:Assertions++
    if ($Actual -cne $Expected) { throw "$Description expected '$Expected', got '$Actual'." }
}

function Invoke-ExpectedFailure([scriptblock]$Action, [string]$ExpectedText, [string]$ForbiddenText = '') {
    $script:Assertions++
    try {
        $null = @(& $Action)
    }
    catch {
        if (-not $_.Exception.Message.Contains($ExpectedText)) {
            throw "Expected failure containing '$ExpectedText', got: $($_.Exception.Message)"
        }
        if ($ForbiddenText.Length -gt 0) {
            $script:Assertions++
            if ($_.Exception.Message.Contains($ForbiddenText)) { throw 'Failure exposed forbidden raw diagnostic text.' }
        }
        return
    }
    throw "Expected failure containing '$ExpectedText', but the action succeeded."
}

function Assert-FixtureRemovalMetadataScannedAncestors([string]$Path, [string[]]$ObservedPaths) {
    $current = [IO.Path]::GetFullPath($Path)
    while ($true) {
        Assert-True ($ObservedPaths -contains $current) "Removal metadata observer skipped ancestor: $current"
        $parent = Split-Path -Parent $current
        if ([string]::IsNullOrEmpty($parent) -or
            [string]::Equals($parent, $current, [StringComparison]::OrdinalIgnoreCase)) {
            return
        }
        $current = $parent
    }
}

function Copy-FixtureSource([string]$RelativePath) {
    $source = Join-Path $repositoryRoot ($RelativePath -replace '/', '\')
    $destination = Join-Path $fixtureRoot ($RelativePath -replace '/', '\')
    New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
    Copy-Item -LiteralPath $source -Destination $destination
}

function Set-UInt16([byte[]]$Bytes, [int]$Offset, [uint16]$Value) {
    [BitConverter]::GetBytes($Value).CopyTo($Bytes, $Offset)
}

function Set-UInt32([byte[]]$Bytes, [int]$Offset, [uint32]$Value) {
    [BitConverter]::GetBytes($Value).CopyTo($Bytes, $Offset)
}

function Write-TestReviewedPe([string]$Path, [uint16]$Subsystem) {
    $bytes = [byte[]]::new(1024)
    $bytes[0] = 0x4D; $bytes[1] = 0x5A
    Set-UInt32 $bytes 0x3C 0x80
    Set-UInt32 $bytes 0x80 0x00004550
    Set-UInt16 $bytes 0x84 0x8664
    Set-UInt16 $bytes 0x86 1
    Set-UInt16 $bytes 0x94 0x00F0
    Set-UInt16 $bytes 0x98 0x020B
    Set-UInt16 $bytes 0xDC $Subsystem
    Set-UInt32 $bytes 0xD4 0x200
    Set-UInt32 $bytes 0x104 16
    Set-UInt32 $bytes 0x110 0x1000
    Set-UInt32 $bytes 0x114 40
    [Text.Encoding]::ASCII.GetBytes('.rdata').CopyTo($bytes, 0x188)
    Set-UInt32 $bytes 0x190 0x200
    Set-UInt32 $bytes 0x194 0x1000
    Set-UInt32 $bytes 0x198 0x200
    Set-UInt32 $bytes 0x19C 0x200
    Set-UInt32 $bytes 0x20C 0x1040
    [Text.Encoding]::ASCII.GetBytes('kernel32.dll').CopyTo($bytes, 0x240)
    $bytes[0x240 + 'kernel32.dll'.Length] = 0
    New-Item -ItemType Directory -Path (Split-Path -Parent $Path) -Force | Out-Null
    [IO.File]::WriteAllBytes($Path, $bytes)
}

function Write-Utf8([string]$Path, [string]$Text) {
    New-Item -ItemType Directory -Path (Split-Path -Parent $Path) -Force | Out-Null
    [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($false))
}

function Refresh-BundleInventory([string]$Root) {
    $files = @(Get-ChildItem -LiteralPath $Root -Recurse -File -Force |
        Where-Object { $_.Name -cne 'bundle-inventory.json' } |
        ForEach-Object {
            [ordered]@{
                path = Get-WindowsLocalFrozenRelativePath $Root $_.FullName
                size_bytes = [int64]$_.Length
                sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        } | Sort-Object path)
    $inventory = [ordered]@{
        schema_version = 1
        platform_triple = 'x86_64-pc-windows-msvc'
        files = $files
    }
    Write-Utf8 (Join-Path $Root 'bundle-inventory.json') ($inventory | ConvertTo-Json -Depth 6)
}

function Copy-Bundle([string]$Name) {
    $destination = Join-Path $testRoot $Name
    Copy-Item -LiteralPath $bundleRoot -Destination $destination -Recurse
    return $destination
}

function Invoke-Builder([string]$Bundle, [string]$Output, [string]$Compiler) {
    & (Join-Path $fixtureRoot 'scripts\build-windows-frozen-test-installer.ps1') `
        -BundlePath $Bundle `
        -FrozenCpuWorkerRecordPath (Join-Path $freezeRoot 'windows-frozen-cpu-worker-record.json') `
        -InnoCompilerPath $Compiler `
        -OutputDirectory $Output
}

function Add-FixtureObservationPack([string]$Root, [string]$Backend = 'cuda') {
    $digest = 'c' * 64 -join ''
    $packRoot = "workers/packs/c/1/$digest"
    $files = @(
        "$packRoot/manifest.json",
        "$packRoot/manifest.sig",
        "$packRoot/scribe-inference-worker.exe"
    )
    foreach ($relative in $files) {
        $path = Join-Path $Root ($relative -replace '/', '\')
        if ($relative.EndsWith('.exe')) { Write-TestReviewedPe $path 3 }
        else { Write-Utf8 $path "fixture $relative" }
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Fixture observation pack did not create $path."
        }
    }
    $installedSize = [int64]0
    foreach ($relative in $files) {
        $installedSize += [int64](Get-Item -LiteralPath (Join-Path $Root ($relative -replace '/', '\'))).Length
    }
    $catalog = [ordered]@{
        schema_version = 1
        packs = @([ordered]@{
            pack_id = 'c'; pack_version = '1'; pack_digest = $digest;
            security_epoch = [int64]1; runtime_abi_version = [int64]1;
            backend = $Backend; provider = $Backend; target_os = 'windows'; target_arch = 'x86_64';
            worker_relative_path = "$packRoot/scribe-inference-worker.exe"; root = $packRoot;
            installed_size_bytes = $installedSize; compressed_size_bytes = [int64]0; files = $files
        })
    }
    Write-Utf8 (Join-Path $Root 'worker-pack-catalog.json') ($catalog | ConvertTo-Json -Depth 6)
    Refresh-BundleInventory $Root
}

function New-FixtureCampaignReport([string]$Power, [string]$Backend = 'cuda') {
    # Independent synthetic producer, not a worker or an SCIF authenticator.
    # Literal target schedule and independent length-prefixed hashing ensure
    # the validator is not generating its own expected captures.
    $targets = @('cpu', 'gpu', 'cpu', 'gpu', 'gpu', 'cpu', 'cpu', 'gpu', 'gpu', 'cpu', 'cpu', 'gpu', 'cpu', 'gpu')
    $captures = @()
    for ($index = 0; $index -lt $targets.Count; $index++) {
        $sequence = $index + 1
        $frames = @()
        $bytes = [IO.MemoryStream]::new()
        try {
            foreach ($kind in @(1, 2)) {
                $frame = [byte[]]::new(26)
                $prefix = [byte[]](83, 67, 73, 70)
                $prefix.CopyTo($frame, 0)
                $frame[4] = [byte]$sequence; $frame[5] = [byte]$kind
                $length = [BitConverter]::GetBytes([uint64]$frame.Length)
                if (-not [BitConverter]::IsLittleEndian) { [Array]::Reverse($length) }
                $bytes.Write($length, 0, $length.Length)
                $bytes.Write($frame, 0, $frame.Length)
                $frames += [Convert]::ToHexString($frame).ToLowerInvariant()
            }
            $digest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes.ToArray())).ToLowerInvariant()
        }
        finally { $bytes.Dispose() }
        $captures += [ordered]@{
            logical_sequence = $sequence; generation_ref = ('generation-{0:D2}-{1}' -f $sequence, $digest.Substring(0, 24));
            digest_sha256 = $digest; target = $targets[$index];
            purpose = $(if ($index -lt 2) { 'preflight' } elseif ($index -lt 12) { 'cold' } else { 'prime' });
            hello_frame_hex = $frames[0]; ready_frame_hex = $frames[1]
        }
    }
    function New-FixtureCampaignRun([string]$Phase, [string]$Target, [int]$Order, [int]$Pair, [string]$Reference) {
        $notApplicable = { [ordered]@{ status = 'not_applicable'; reason = 'cpu_provider' } }
        $segment = { [ordered]@{
            sampled_max_current_usage_bytes = 8; sampled_max_current_reservation_bytes = 4;
            sampled_min_budget_bytes = 64; sampled_min_available_for_reservation_bytes = 32
        } }
        $provider = { [ordered]@{
            status = 'available'; backend = $Backend; provider_id = $Backend;
            stable_device = $global:WindowsLocalFrozenVerifierDevice; memory_total_bytes = 64;
            provider_reported_memory_free_bytes = 32; value_semantics = 'native_backend_defined'; admission_validity = 'unestablished'
        } }
        $availability = {
            if ($Backend -ceq 'cuda') {
                return [ordered]@{
                    status = 'observed'; backend = $Backend; provider_id = $Backend; stable_device = $global:WindowsLocalFrozenVerifierDevice;
                    memory_total_bytes = 64; available_memory_bytes = 32; source = [ordered]@{ method = 'cuda_mem_get_info' }
                }
            }
            return [ordered]@{
                status = 'observed'; backend = $Backend; provider_id = $Backend; stable_device = $global:WindowsLocalFrozenVerifierDevice;
                memory_total_bytes = 1024; available_memory_bytes = 384;
                source = [ordered]@{
                    method = 'vulkan_memory_budget'; heap_selection = 'device_local_heaps'; heaps = @(
                        [ordered]@{ heap_index = 0; size_bytes = 1024; flags = 1; budget_bytes = 512; usage_bytes = 128 },
                        [ordered]@{ heap_index = 1; size_bytes = 512; flags = 0; budget_bytes = 512; usage_bytes = 256 }
                    )
                }
            }
        }
        $gpu = $Target -ceq 'gpu'
        $run = [ordered]@{
            measured = $Phase -cne 'prime'; phase = $Phase; order_in_pair = $Order; target = $Target;
            generation_ref = $Reference; status = 'succeeded'; power_source_before = $Power; power_source_after = $Power;
            end_to_end_ms = 3; backend_ms = 1; model_load_ms = $(if ($Phase -ceq 'warm') { 0 } else { 1 });
            warm_reused = $Phase -ceq 'warm'; sampled_max_private_usage_bytes = 1; telemetry_sample_count = 1;
            video_memory = $(if ($gpu) { [ordered]@{ status = 'available'; local = & $segment; non_local = & $segment } } else { [ordered]@{ status = 'not_applicable' } });
            raw_provider_memory = [ordered]@{
                before = $(if ($gpu) { & $provider } else { & $notApplicable }); after = $(if ($gpu) { & $provider } else { & $notApplicable })
            };
            memory_availability = [ordered]@{
                before = $(if ($gpu) { & $availability } else { & $notApplicable }); after = $(if ($gpu) { & $availability } else { & $notApplicable })
            };
            worker_process_affinity = [ordered]@{
                before = [ordered]@{ status = 'available'; processor_group = 0; process_mask_hex = '0000000000000001'; system_mask_hex = '0000000000000003' };
                after = [ordered]@{ status = 'available'; processor_group = 0; process_mask_hex = '0000000000000002'; system_mask_hex = '0000000000000003' };
                changed = $true
            };
            normalized_transcript_sha256 = $(if ($gpu) { 'b' * 64 -join '' } else { 'a' * 64 -join '' })
        }
        if ($Phase -cne 'prime') { $run.pair_index = $Pair }
        return $run
    }
    $runs = @()
    for ($index = 0; $index -lt 10; $index++) {
        $runs += New-FixtureCampaignRun 'cold' $targets[$index + 2] (($index % 2) + 1) ([int][Math]::Floor($index / 2) + 1) $captures[$index + 2].generation_ref
    }
    $runs += New-FixtureCampaignRun 'prime' 'cpu' 1 0 $captures[12].generation_ref
    $runs += New-FixtureCampaignRun 'prime' 'gpu' 2 0 $captures[13].generation_ref
    for ($pair = 1; $pair -le 20; $pair++) {
        $pairTargets = if ($pair % 2 -eq 1) { @('cpu', 'gpu') } else { @('gpu', 'cpu') }
        for ($order = 1; $order -le 2; $order++) {
            $target = $pairTargets[$order - 1]
            $capture = if ($target -ceq 'cpu') { $captures[12] } else { $captures[13] }
            $runs += New-FixtureCampaignRun 'warm' $target $order $pair $capture.generation_ref
        }
    }
    return [ordered]@{
        schema_version = 2; kind = 'windows_gpu_capture_campaign'; unsigned = $true; unqualified = $true;
        auto_eligible = $false; release_approved = $false; collector_build_revision = $global:WindowsLocalFrozenVerifierRevision;
        expected_power = $Power; incomplete = $false; cleanup_complete = $true;
        inputs = [ordered]@{ model_sha256 = $global:WindowsLocalFrozenVerifierModelSha256; wav_sha256 = $global:WindowsLocalFrozenVerifierWavSha256 };
        gpu_identity = [ordered]@{
            backend = $Backend; provider = $Backend; stable_device = $global:WindowsLocalFrozenVerifierDevice;
            driver = 'fixture-driver'; device_class = 'discrete_gpu'; vendor = 'nvidia'; memory_total_bytes = 64;
            pack_id = 'c'; pack_version = '1'; pack_sha256 = ('c' * 64 -join ''); pack_security_epoch = 1; runtime_abi = 1
        };
        captures = $captures; runs = $runs;
        unavailable = [ordered]@{
            inference_thread_count = [ordered]@{ status = 'unavailable'; reason = 'unsupported_by_pinned_runtime_api' };
            thermal_state = [ordered]@{ status = 'unavailable'; reason = 'not_observed' }
        };
        environmental_controls = [ordered]@{
            background_load = [ordered]@{ status = 'unavailable'; reason = 'not_observed' };
            host_control = [ordered]@{ status = 'unavailable'; reason = 'not_observed' };
            affinity_control = [ordered]@{ status = 'unavailable'; reason = 'not_observed' };
            power_plan = [ordered]@{ status = 'unavailable'; reason = 'not_observed' }
        }
    }
}

function Set-FixtureVerifierSeams([string]$VerifierPath, [string]$IntegrityPath) {
    $integritySource = Get-Content -LiteralPath $IntegrityPath -Raw
    $descriptorCall = 'Get-WindowsLocalFrozenDeclaredPackFiles $root -VerifyCompiledDescriptors'
    if ([regex]::Matches($integritySource, [regex]::Escape($descriptorCall)).Count -ne 1) {
        throw 'Could not isolate the fixture-only compiled-pack descriptor seam.'
    }
    $fixturePackItem = '$item = Assert-WindowsFrozenCpuWorkerRegularFile (Join-Path $Root ($file -replace ''/'', ''\''))'
    if ([regex]::Matches($integritySource, [regex]::Escape($fixturePackItem)).Count -ne 1) {
        throw 'Could not isolate the fixture-only declared pack file seam.'
    }
    $fixturePackItemReplacement = @'
$fixturePackPath = Join-Path $Root ($file -replace '/', '\')
if (-not (Test-Path -LiteralPath $fixturePackPath -PathType Leaf)) {
    throw "Fixture declared GPU pack file is missing: $fixturePackPath"
}
try {
    $item = Assert-WindowsFrozenCpuWorkerRegularFile $fixturePackPath
}
catch {
    throw "Fixture declared GPU pack file could not be opened: $fixturePackPath ($($_.Exception.Message))"
}
'@.Trim()
    $integritySource = $integritySource.Replace($descriptorCall, 'Get-WindowsLocalFrozenDeclaredPackFiles $root')
    Write-Utf8 $IntegrityPath ($integritySource.Replace($fixturePackItem, $fixturePackItemReplacement))

    $source = Get-Content -LiteralPath $VerifierPath -Raw
    $start = $source.IndexOf('function Invoke-WindowsLocalFrozenInstallerProcess')
    $end = $source.IndexOf('function Assert-WindowsLocalFrozenObservationOutputDestination', $start)
    if ($start -lt 0 -or $end -le $start) {
        throw 'Could not isolate the fixture-only local installer process seam.'
    }
    $installerSeam = @'
function Invoke-WindowsLocalFrozenInstallerProcess([string]$Executable, [string[]]$Arguments, [string]$Description) {
    $global:WindowsLocalFrozenVerifierEvents.Add($Description)
    switch ($Description) {
        'local frozen installer' {
            Copy-Item -LiteralPath $global:WindowsLocalFrozenVerifierBundle -Destination $global:WindowsLocalFrozenVerifierInstalledRoot -Recurse
            [IO.File]::WriteAllBytes((Join-Path $global:WindowsLocalFrozenVerifierInstalledRoot 'unins000.exe'), [byte[]](0x4d, 0x5a))
            [IO.File]::WriteAllBytes((Join-Path $global:WindowsLocalFrozenVerifierInstalledRoot 'unins000.dat'), [byte[]](1))
            if ($global:WindowsLocalFrozenVerifierMode -ceq 'parity-failure') {
                [IO.File]::WriteAllText((Join-Path $global:WindowsLocalFrozenVerifierInstalledRoot 'README.txt'), 'tampered parity fixture', [Text.UTF8Encoding]::new($false))
            }
            return [pscustomobject]@{ ExitCode = 0; Stdout = ''; Stderr = '' }
        }
        'installed local frozen CPU smoke' {
            $global:WindowsLocalFrozenVerifierSmokeArguments = @($Arguments)
            if ($global:WindowsLocalFrozenVerifierMode -ceq 'installed-catalog-drift') {
                $catalogPath = Join-Path $global:WindowsLocalFrozenVerifierInstalledRoot 'worker-pack-catalog.json'
                $catalog = Get-Content -LiteralPath $catalogPath -Raw | ConvertFrom-Json
                $catalog.packs[0].pack_version = '2'
                [IO.File]::WriteAllText($catalogPath, ($catalog | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
            }
            return [pscustomobject]@{ ExitCode = 0; Stdout = '{"cancellation_verified":true,"capabilities":{"cancellation":true},"detected_architecture":"whisper"}'; Stderr = '' }
        }
        'local frozen installer uninstaller' {
            if ($global:WindowsLocalFrozenVerifierMode -ceq 'uninstall-failure') {
                return [pscustomobject]@{ ExitCode = 41; Stdout = ''; Stderr = 'fixture uninstall failed' }
            }
            Remove-Item -LiteralPath $global:WindowsLocalFrozenVerifierInstalledRoot -Recurse -Force
            return [pscustomobject]@{ ExitCode = 0; Stdout = ''; Stderr = '' }
        }
        'local frozen installer cleanup' {
            if (Test-Path -LiteralPath $global:WindowsLocalFrozenVerifierInstalledRoot) {
                Remove-Item -LiteralPath $global:WindowsLocalFrozenVerifierInstalledRoot -Recurse -Force
            }
            return [pscustomobject]@{ ExitCode = 0; Stdout = ''; Stderr = '' }
        }
        default { throw "Unexpected fixture verifier installer process: $Description" }
    }
}

'@
    $source = $source.Substring(0, $start) + $installerSeam + $source.Substring($end)

    $temporaryCleanupFunction = 'function Remove-WindowsLocalFrozenVerifierTemporaryRoot([string]$Path) {'
    if ([regex]::Matches($source, [regex]::Escape($temporaryCleanupFunction)).Count -ne 1) {
        throw 'Could not isolate the fixture-only temporary cleanup seam.'
    }
    $temporaryCleanupReplacement = @'
function Remove-WindowsLocalFrozenVerifierTemporaryRoot([string]$Path) {
    if ($global:WindowsLocalFrozenVerifierMode -ceq 'temporary-cleanup-failure' -and
        -not $global:WindowsLocalFrozenVerifierTemporaryCleanupFailureInjected) {
        $global:WindowsLocalFrozenVerifierTemporaryCleanupFailureInjected = $true
        throw 'Fixture temporary observation cleanup failed.'
    }
'@.TrimEnd()
    $source = $source.Replace($temporaryCleanupFunction, $temporaryCleanupReplacement)
    $contextCheck = 'Assert-WindowsFrozenCpuWorkerContextUnchanged $frozenCpuWorker.Context'
    if ([regex]::Matches($source, [regex]::Escape($contextCheck)).Count -ne 1) { throw 'Could not isolate the fixture-only post-uninstall source-context check.' }
    $contextFailureSeam = @'
if ($global:WindowsLocalFrozenVerifierMode -ceq 'context-check-failure') { throw 'Fixture source context check failed after uninstall.' }
Assert-WindowsFrozenCpuWorkerContextUnchanged $frozenCpuWorker.Context
'@.Trim()
    $source = $source.Replace($contextCheck, $contextFailureSeam)

    $installedRootStart = $source.IndexOf('$installedRoot = Join-Path ([Environment]::GetFolderPath')
    if ($installedRootStart -lt 0) {
        throw 'Could not isolate the fixture-only local installation root seam.'
    }
    $installedRootEnd = $source.IndexOf("`n", $installedRootStart)
    if ($installedRootEnd -lt 0) {
        throw 'Could not terminate the fixture-only local installation root seam.'
    }
    $source = $source.Substring(0, $installedRootStart) + '$installedRoot = $global:WindowsLocalFrozenVerifierInstalledRoot' + $source.Substring($installedRootEnd)

    $marker = '$campaignMode = $null -ne $observationRequest.CampaignPower'
    $campaignStart = $source.IndexOf('Invoke-WindowsLocalFrozenCampaignProcess', $source.IndexOf($marker))
    if ($campaignStart -lt 0) { throw 'Could not isolate the fixture-only installed campaign process seam.' }
    $source = $source.Substring(0, $campaignStart) + 'Invoke-FixtureInstalledGpuObserver' + $source.Substring($campaignStart + 'Invoke-WindowsLocalFrozenCampaignProcess'.Length)
    $observationStart = $source.IndexOf('Invoke-WindowsLocalFrozenBoundedProcess', $source.IndexOf($marker))
    if ($observationStart -lt 0) {
        throw 'Could not isolate the fixture-only installed observer process seam.'
    }
    $source = $source.Substring(0, $observationStart) + 'Invoke-FixtureInstalledGpuObserver' + $source.Substring($observationStart + 'Invoke-WindowsLocalFrozenBoundedProcess'.Length)
    $insertion = $source.IndexOf('Assert-WindowsFrozenCpuWorkerLocalOnlyEnvironment')
    if ($insertion -lt 0) {
        throw 'Could not insert the fixture-only installed observer process seam.'
    }
    $observerSeam = @'
function Invoke-FixtureInstalledGpuObserver(
    [string]$Executable,
    [string[]]$Arguments,
    [string]$Description,
    [int]$TimeoutMilliseconds,
    [int]$StreamDrainMilliseconds
) {
    $global:WindowsLocalFrozenVerifierEvents.Add($Description)
    $global:WindowsLocalFrozenVerifierObserverExecutable = $Executable
    $global:WindowsLocalFrozenVerifierObserverArguments = @($Arguments)
    if ($Description -cnotin @('installed local frozen GPU observation', 'installed local frozen GPU observation campaign') -or $TimeoutMilliseconds -ne 900000 -or $StreamDrainMilliseconds -ne 5000) {
        throw 'Fixture installed observer did not receive the fixed bounded process contract.'
    }
    $outputIndex = [array]::IndexOf($Arguments, '--output')
    if ($outputIndex -lt 0 -or $outputIndex -ge ($Arguments.Count - 1)) { throw 'Fixture installed observer output argument is missing.' }
    $output = $Arguments[$outputIndex + 1]
    $powerIndex = [array]::IndexOf($Arguments, '--campaign-power')
    if ($powerIndex -ge 0) {
        if ($Arguments.Count -ne 19 -or $powerIndex -ne 15 -or $outputIndex -ne 17) { throw 'Fixture campaign argv is not the exact nineteen-argument contract.' }
        $report = New-FixtureCampaignReport $Arguments[$powerIndex + 1] $global:WindowsLocalFrozenVerifierBackend
        switch ($global:WindowsLocalFrozenVerifierMode) {
            'campaign-timeout' { throw 'Fixture campaign timed out after the fixed campaign deadline.' }
            'campaign-overflow' { throw 'Fixture campaign exceeded the fixed 262144-character per-stream output bound.' }
            'campaign-missing' { return [pscustomobject]@{ ExitCode = 0; Stdout = ''; Stderr = '' } }
            'campaign-incomplete' { $report.incomplete = $true }
            'campaign-power-drift' { $report.runs[51].power_source_after = 'unknown' }
            'campaign-nonzero' { $report.incomplete = $true }
            'output-race' { [IO.File]::WriteAllText($global:WindowsLocalFrozenVerifierFinalReport, 'race sentinel', [Text.UTF8Encoding]::new($false)) }
        }
        $text = $report | ConvertTo-Json -Depth 20
        if ($global:WindowsLocalFrozenVerifierMode -ceq 'campaign-large') { $text = (' ' * 1048576) + $text }
        [IO.File]::WriteAllText($output, $text, [Text.UTF8Encoding]::new($false))
        return [pscustomobject]@{ ExitCode = $(if ($global:WindowsLocalFrozenVerifierMode -ceq 'campaign-nonzero') { 37 } else { 0 }); Stdout = ''; Stderr = 'sensitive fixture diagnostic must not escape' }
    }
    switch ($global:WindowsLocalFrozenVerifierMode) {
        'observer-timeout' { throw 'installed local frozen GPU observation timed out after 900000 milliseconds.' }
        'observer-nonzero' { return [pscustomobject]@{ ExitCode = 37; Stdout = ''; Stderr = 'fixture observer failed' } }
        'observer-missing-report' { return [pscustomobject]@{ ExitCode = 0; Stdout = ''; Stderr = '' } }
        'observer-malformed-report' { [IO.File]::WriteAllText($output, '{', [Text.UTF8Encoding]::new($false)); return [pscustomobject]@{ ExitCode = 0; Stdout = ''; Stderr = '' } }
        'observer-oversized-report' { [IO.File]::WriteAllBytes($output, [byte[]]::new(1048577)); return [pscustomobject]@{ ExitCode = 0; Stdout = ''; Stderr = '' } }
    }
    $cpuDigest = 'a' * 64 -join ''
    $gpuDigest = 'b' * 64 -join ''
    $worker = {
        param([string]$Digest)
        return [ordered]@{
            hello_frame_hex = ('ab' * 26 -join ''); ready_frame_hex = ('cd' * 26 -join '');
            power_source_before = 'ac'; power_source_after = 'ac'; elapsed_ms = [int64]1;
            sampled_max_private_usage_bytes = [int64]1; telemetry_sample_count = [int64]1;
            video_memory = @{}; provider_memory = @{}; memory_availability = @{};
            normalized_transcript_sha256 = $Digest
        }
    }
    $report = [ordered]@{
        schema_version = 3; kind = 'windows_gpu_capture_observation'; unsigned = $true;
        unqualified = $true; auto_eligible = $false; release_approved = $false;
        collector_build_revision = $global:WindowsLocalFrozenVerifierRevision;
        inputs = [ordered]@{ model_sha256 = $global:WindowsLocalFrozenVerifierModelSha256; wav_sha256 = $global:WindowsLocalFrozenVerifierWavSha256 };
        gpu_identity = [ordered]@{
            backend = 'cuda'; provider = 'cuda'; stable_device = $global:WindowsLocalFrozenVerifierDevice;
            driver = 'fixture-driver'; device_class = 'discrete_gpu'; vendor = 'nvidia'; memory_total_bytes = [int64]1;
            pack_id = 'c'; pack_version = '1'; pack_sha256 = ('c' * 64 -join '');
            pack_security_epoch = [int64]1; runtime_abi = [int64]1
        };
        cpu = & $worker $cpuDigest; gpu = & $worker $gpuDigest; transcript_parity = $false;
        unavailable = @{}
    }
    switch ($global:WindowsLocalFrozenVerifierMode) {
        'observer-tampered-pack' { $report.gpu_identity.pack_sha256 = 'd' * 64 -join '' }
        'observer-wrong-pack-version' { $report.gpu_identity.pack_version = '2' }
        'observer-wrong-backend' { $report.gpu_identity.backend = 'vulkan' }
        'observer-wrong-device' { $report.gpu_identity.stable_device = 'luid:00000000-00000002' }
        'observer-wrong-model' { $report.inputs.model_sha256 = 'd' * 64 -join '' }
        'observer-wrong-wav' { $report.inputs.wav_sha256 = 'd' * 64 -join '' }
        'observer-wrong-schema' { $report.schema_version = [int64]2 }
        'observer-wrong-kind' { $report.kind = 'windows_gpu_capture_campaign' }
        'observer-campaign-claim' { $report.campaign = @{ power = 'ac' } }
        'observer-string-unsigned' { $report.unsigned = 'true' }
        'observer-approval-claim' { $report.release_approved = $true }
        'observer-wrong-revision' { $report.collector_build_revision = '0' * 40 -join '' }
        'output-race' { [IO.File]::WriteAllText($global:WindowsLocalFrozenVerifierFinalReport, 'race sentinel', [Text.UTF8Encoding]::new($false)) }
    }
    [IO.File]::WriteAllText($output, ($report | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
    return [pscustomobject]@{ ExitCode = 0; Stdout = ''; Stderr = '' }
}

'@
    $source = $source.Substring(0, $insertion) + $observerSeam + $source.Substring($insertion)
    Write-Utf8 $VerifierPath $source
}

function Remove-FixtureVerifierInstalledRoot([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $resolved = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    $expectedPrefix = [IO.Path]::GetFullPath($global:WindowsLocalFrozenVerifierFixtureInstallParent).TrimEnd([char[]]@('\', '/')) + '\'
    if (-not $resolved.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path -Leaf $resolved) -cnotmatch '^[0-9a-f]{32}$') {
        throw 'Refused fixture verifier cleanup outside its exact temporary token-bound installation root.'
    }
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $resolved
    Remove-Item -LiteralPath $resolved -Recurse -Force
}

function Invoke-FixtureVerifier(
    [string]$Verifier,
    [string]$Bundle,
    [string]$FrozenRecord,
    [string]$Installer,
    [string]$InstallerRecord,
    [string]$Wav,
    [string]$WavSha256,
    [string]$Device,
    [string]$Report,
    [string]$Mode = '',
    [switch]$PartialRequest,
    [switch]$MissingPack,
    [string]$CampaignPower,
    [string]$Backend = 'cuda'
) {
    $global:WindowsLocalFrozenVerifierMode = $Mode
    $global:WindowsLocalFrozenVerifierEvents = [System.Collections.Generic.List[string]]::new()
    $global:WindowsLocalFrozenVerifierObserverExecutable = $null
    $global:WindowsLocalFrozenVerifierObserverArguments = @()
    $global:WindowsLocalFrozenVerifierSmokeArguments = @()
    $global:WindowsLocalFrozenVerifierTemporaryCleanupFailureInjected = $false
    $global:WindowsLocalFrozenVerifierFinalReport = $Report
    $global:WindowsLocalFrozenVerifierBundle = $Bundle
    $installerRecordValue = Get-Content -LiteralPath $InstallerRecord -Raw | ConvertFrom-Json
    $global:WindowsLocalFrozenVerifierInstalledRoot = Join-Path $global:WindowsLocalFrozenVerifierFixtureInstallParent ($installerRecordValue.local_test_token)
    $frozenRecordValue = Get-Content -LiteralPath $FrozenRecord -Raw | ConvertFrom-Json
    $global:WindowsLocalFrozenVerifierRevision = [string]$frozenRecordValue.source_revision
    $global:WindowsLocalFrozenVerifierModelSha256 = (Get-Content -LiteralPath (Join-Path $Bundle 'bundle-inventory.json') -Raw | ConvertFrom-Json).files |
        Where-Object { $_.path -ceq 'whisper-base.en-Q8_0.gguf' } | Select-Object -ExpandProperty sha256
    $global:WindowsLocalFrozenVerifierWavSha256 = $WavSha256
    $global:WindowsLocalFrozenVerifierDevice = $Device
    $global:WindowsLocalFrozenVerifierBackend = $Backend
    $parameters = @{
        BundlePath = $Bundle
        FrozenCpuWorkerRecordPath = $FrozenRecord
        InstallerPath = $Installer
        InstallerRecordPath = $InstallerRecord
    }
    if ($PartialRequest) {
        $parameters.ObservationWavPath = $Wav
    }
    else {
        $parameters.ObservationWavPath = $Wav
        $parameters.ObservationWavSha256 = $WavSha256
        $parameters.ObservationGpuPackId = if ($MissingPack) { 'missing-pack' } else { 'c' }
        $parameters.ObservationGpuBackend = $Backend
        $parameters.ObservationGpuDevice = $Device
        $parameters.ObservationReportPath = $Report
    }
    if ($PSBoundParameters.ContainsKey('CampaignPower')) { $parameters.ObservationCampaignPower = $CampaignPower }
    & $Verifier @parameters
}

function Remove-TestRootSafely([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $resolved = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
    if ((Split-Path -Parent $resolved) -cne $temp -or
        (Split-Path -Leaf $resolved) -cnotmatch '^slfi-[0-9a-f]{32}$') {
        throw 'Refused local frozen installer test cleanup outside its exact temporary root.'
    }
    foreach ($item in @((Get-Item -LiteralPath $resolved -Force)) + @(Get-ChildItem -LiteralPath $resolved -Recurse -Force)) {
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refused local frozen installer test cleanup through a reparse point: $($item.FullName)"
        }
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}

$previousCi = $env:CI
$previousGitHubActions = $env:GITHUB_ACTIONS
$previousMode = $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE
$previousCapture = $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE
$previousSourceMutation = $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_SOURCE_MUTATION
$previousGitEnvironment = @{}
foreach ($entry in @(Get-ChildItem Env:GIT_*)) { $previousGitEnvironment[$entry.Name] = $entry.Value }
try {
    $env:CI = $null
    $env:GITHUB_ACTIONS = $null
    foreach ($entry in @(Get-ChildItem Env:GIT_*)) { Remove-Item -LiteralPath "Env:$($entry.Name)" }
    $isolatedGitConfig = Join-Path $testRoot 'empty-git-config'
    $isolatedGitTemplate = Join-Path $testRoot 'empty-git-template'
    $isolatedGitHooks = Join-Path $testRoot 'empty-git-hooks'
    New-Item -ItemType Directory -Path $isolatedGitTemplate, $isolatedGitHooks | Out-Null
    Write-Utf8 $isolatedGitConfig ''
    $env:GIT_CONFIG_NOSYSTEM = '1'
    $env:GIT_CONFIG_GLOBAL = $isolatedGitConfig
    $env:GIT_TEMPLATE_DIR = $isolatedGitTemplate
    New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
    foreach ($path in @(
        'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/config.toml', 'build.rs', 'src/worker_identity.rs',
        'scripts/windows-cpu-worker-native-baseline.ps1',
        'scripts/new-windows-frozen-cpu-worker.ps1', 'scripts/build-windows-release.ps1',
        'scripts/windows-frozen-cpu-worker-integrity.ps1', 'scripts/windows-pe-imports.ps1',
        'scripts/windows-local-frozen-installer-integrity.ps1', 'scripts/build-windows-frozen-test-installer.ps1',
        'scripts/verify-windows-local-frozen-test-installer.ps1',
        'installer/scribe-local-frozen.iss', 'resources/licenses/Apache-2.0.txt',
        'resources/licenses/OpenAI-Whisper-MIT.txt', 'resources/licenses/Whisper-Base-En-NOTICE.txt',
        'resources/licenses/THIRD-PARTY-NOTICES.txt', 'native/transcribe-cpp-v0.1.3/LICENSE',
        'native/transcribe-cpp-v0.1.3/PROVENANCE.md', 'native/whisper-f049fff/LICENSE',
        'native/whisper-f049fff/PROVENANCE.md', 'native/sherpa-onnx-v1.13.5/PROVENANCE.md',
        'resources/silero-vad/LICENSE', 'resources/silero-vad/PROVENANCE.md'
    )) { Copy-FixtureSource $path }
    # Shorten only the copied builder's fixed compiler deadline. This runs the
    # real builder/launcher/cleanup path without a production test override or
    # waiting fifteen minutes for a deliberately hung fixture compiler.
    $fixtureBuilder = Join-Path $fixtureRoot 'scripts\build-windows-frozen-test-installer.ps1'
    $fixtureBuilderSource = Get-Content -LiteralPath $fixtureBuilder -Raw
    $compilerDeadline = '-TimeoutMilliseconds 900000'
    Assert-Equal ([regex]::Matches($fixtureBuilderSource, [regex]::Escape($compilerDeadline)).Count) 1 'Production compiler deadline occurrence count'
    Write-Utf8 $fixtureBuilder ($fixtureBuilderSource.Replace($compilerDeadline, '-TimeoutMilliseconds 1000'))
    $modelBytes = [byte[]](1, 2, 3, 4, 5)
    $modelSha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $modelHash = ([BitConverter]::ToString($modelSha256.ComputeHash($modelBytes))).Replace('-', '').ToLowerInvariant()
    } finally {
        $modelSha256.Dispose()
    }
    $modelManifest = [ordered]@{
        model_id = 'fixture-model'; artifact_filename = 'whisper-base.en-Q8_0.gguf';
        size_bytes = [int64]$modelBytes.Length; sha256 = $modelHash;
        platform_triple = 'x86_64-pc-windows-msvc'
    }
    Write-Utf8 (Join-Path $fixtureRoot 'runtime-manifests\whisper-base-en-q8_0-windows-x64.json') ($modelManifest | ConvertTo-Json -Depth 4)

    New-Item -ItemType Directory -Path $compilerRoot | Out-Null
    $fakeCompiler = Join-Path $compilerRoot 'ISCC.exe'
    $fakeCompilerSource = Join-Path $compilerRoot 'FakeIscc.cs'
    Write-Utf8 $fakeCompilerSource @'
using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Threading;
public static class FakeIscc {
  static string Arg(string[] args, string name) { var value = args.FirstOrDefault(x => x.StartsWith(name, StringComparison.Ordinal)); return value == null ? null : value.Substring(name.Length); }
  public static int Main(string[] args) {
    string mode = Environment.GetEnvironmentVariable("SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE") ?? "";
    if (mode == "campaign-live-parent") {
      using (Process self = Process.GetCurrentProcess()) {
        File.WriteAllLines(Environment.GetEnvironmentVariable("SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE"), new string[] {
          self.Id.ToString(), self.StartTime.ToUniversalTime().Ticks.ToString(), self.MainModule.FileName
        });
      }
      // Send readiness data through a real ReadAsync before any synthetic fault.
      Console.Write(new string('R', 8192)); Console.Out.Flush(); Thread.Sleep(60000); return 0;
    }
    if (mode == "campaign-exact-cap") { Console.Write(new string('a', 262144)); Console.Error.Write(new string('b', 262144)); return 0; }
    if (mode == "campaign-overflow-stdout") { Console.Write(new string('a', 262145)); Console.Out.Flush(); Thread.Sleep(60000); return 0; }
    if (mode == "campaign-overflow-stderr") { Console.Error.Write(new string('b', 262145)); Console.Error.Flush(); Thread.Sleep(60000); return 0; }
    if (args.Length > 0 && (args[0] == "--scribe-install-smoke-parent" || args[0] == "--scribe-windows-gpu-capture-observation")) {
      if (mode == "observer-hang") { Thread.Sleep(60000); return 0; }
      foreach (string arg in args) Console.WriteLine(Convert.ToBase64String(System.Text.Encoding.UTF8.GetBytes(arg)));
      return 0;
    }
    string output = Arg(args, "/DLocalFrozenInstallerOutputRoot=");
    string token = Arg(args, "/DLocalFrozenTestToken=");
    string payload = Arg(args, "/DLocalFrozenBundleRoot=");
    string capture = Environment.GetEnvironmentVariable("SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE") ?? "";
    if (!String.IsNullOrEmpty(capture)) File.WriteAllText(capture, String.Join("\n", args));
    if (mode == "mutate-staged") {
      try { File.WriteAllText(Path.Combine(payload, "README.txt"), "mutated"); if (!String.IsNullOrEmpty(capture)) File.AppendAllText(capture, "\nmutation=written"); }
      catch (IOException) { if (!String.IsNullOrEmpty(capture)) File.AppendAllText(capture, "\nmutation=blocked"); }
    }
    if (mode == "mutate-source") File.WriteAllText(Environment.GetEnvironmentVariable("SCRIBE_LOCAL_FROZEN_TEST_ISCC_SOURCE_MUTATION"), "source-drift");
    if (mode == "fail") return 19;
    if (mode == "hang") { Thread.Sleep(60000); return 0; }
    Directory.CreateDirectory(output);
    byte[] bytes = new byte[1024]; bytes[0] = 0x4d; bytes[1] = 0x5a;
    File.WriteAllBytes(Path.Combine(output, "Scribe-LOCAL-Frozen-Test-" + token + ".exe"), bytes);
    return 0;
  }
}
'@
    $csharpCompiler = 'C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe'
    if (-not (Test-Path -LiteralPath $csharpCompiler -PathType Leaf)) {
        throw 'The Windows .NET Framework C# compiler is required for the local installer test seam.'
    }
    & $csharpCompiler '/nologo' '/target:exe' "/out:$fakeCompiler" $fakeCompilerSource
    if ($LASTEXITCODE -ne 0) { throw 'Could not build the local installer test compiler seam.' }
    $fakeCompilerItem = Get-Item -LiteralPath $fakeCompiler
    $fakeCompilerHash = (Get-FileHash -LiteralPath $fakeCompiler -Algorithm SHA256).Hash.ToLowerInvariant()
    $provenance = [ordered]@{
        schema_version = 2; product = 'Inno Setup'; product_version = '7.1.0'; reviewed_utc_date = '2026-09-30';
        upstream_installer_url = 'https://github.com/jrsoftware/issrc/releases/download/is-7_1_0/innosetup-7.1.0-x64.exe';
        installer_size_bytes = [int64]14304168; installer_sha256 = '0362a383ed217d4c4239b5933866dd96d3eb2102737da92f80f6057a4b40df2f';
        compiler_relative_path = 'ISCC.exe'; compiler_size_bytes = [int64]$fakeCompilerItem.Length; compiler_sha256 = $fakeCompilerHash;
        verification_method = 'fixture only'; trust_scope = 'fixture only'
    }
    Write-Utf8 (Join-Path $fixtureRoot 'installer\inno-setup-7.1.0-provenance.json') ($provenance | ConvertTo-Json -Depth 4)
    Set-FixtureVerifierSeams `
        (Join-Path $fixtureRoot 'scripts\verify-windows-local-frozen-test-installer.ps1') `
        (Join-Path $fixtureRoot 'scripts\windows-local-frozen-installer-integrity.ps1')
    & git -c safe.directory=$fixtureRoot -c core.hooksPath=$isolatedGitHooks -c commit.gpgSign=false -C $fixtureRoot init -q
    & git -c safe.directory=$fixtureRoot -c core.hooksPath=$isolatedGitHooks -C $fixtureRoot add .
    & git -c safe.directory=$fixtureRoot -c core.hooksPath=$isolatedGitHooks -c commit.gpgSign=false -C $fixtureRoot -c user.email=fixture@example.invalid -c user.name='Scribe fixture' commit -qm fixture
    if ($LASTEXITCODE -ne 0) { throw 'Could not commit local frozen installer fixture source.' }

    . (Join-Path $fixtureRoot 'scripts\windows-frozen-cpu-worker-integrity.ps1')
    . (Join-Path $fixtureRoot 'scripts\windows-local-frozen-installer-integrity.ps1')
    foreach ($invalidInteger in @($null, $true, $false, '1024', '', [double]1, [single]1, [decimal]1, [uint64]::MaxValue)) {
        Invoke-ExpectedFailure { Assert-WindowsLocalFrozenInt64 $invalidInteger 'Fixture size' } 'must be an integer'
    }
    foreach ($validInteger in @([int32]0, [int32]1, [int64]2147483648, [int64]::MaxValue)) {
        Assert-Equal (Assert-WindowsLocalFrozenInt64 $validInteger 'Fixture size') $validInteger 'Exact integer scalar'
    }
    # Use the complete pinned manifest and the production argument builder;
    # PowerShell argument-mode parsing must not turn a cast into path text.
    $smokeManifest = Get-Content -LiteralPath (Join-Path $repositoryRoot 'runtime-manifests\whisper-base-en-q8_0-windows-x64.json') -Raw | ConvertFrom-Json
    foreach ($smokeRoot in @(
        (Join-Path $testRoot 'installed'),
        (Join-Path $testRoot 'installed smoke [β] & (literal)')
    )) {
        $smokeArguments = @(Get-WindowsLocalFrozenSmokeArguments $smokeRoot $smokeManifest)
        $expectedSmokeArguments = @(
            '--scribe-install-smoke-parent',
            [string]$smokeManifest.model_id,
            [IO.Path]::Combine($smokeRoot, 'whisper-base.en-Q8_0.gguf'),
            'gguf',
            [string]$smokeManifest.size_bytes,
            [string]$smokeManifest.sha256,
            'cpu'
        )
        Assert-Equal $smokeArguments.Count 7 'Installed smoke argument count'
        for ($index = 0; $index -lt $expectedSmokeArguments.Count; $index++) {
            Assert-Equal $smokeArguments[$index] $expectedSmokeArguments[$index] "Installed smoke argument $index"
        }
        $echo = Invoke-WindowsLocalFrozenBoundedProcess -Executable $fakeCompiler -Arguments $smokeArguments -Description 'fixture installed smoke argument echo'
        Assert-Equal $echo.ExitCode 0 'Installed smoke argument echo exit code'
        Assert-Equal $echo.Stderr '' 'Installed smoke argument echo stderr'
        $receivedArguments = @($echo.Stdout.TrimEnd([char[]]@("`r", "`n")) -split '\r?\n' | ForEach-Object {
            [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($_))
        })
        Assert-Equal $receivedArguments.Count 7 'Installed smoke child argument count'
        for ($index = 0; $index -lt $expectedSmokeArguments.Count; $index++) {
            Assert-Equal $receivedArguments[$index] $expectedSmokeArguments[$index] "Installed smoke child argument $index"
        }
    }
    $observerRoundTrip = @(
        '--scribe-windows-gpu-capture-observation',
        '--model', (Join-Path $testRoot 'model with space [β].gguf'), '--model-sha256', ('a' * 64 -join ''),
        '--wav', (Join-Path $testRoot 'wav with space [β] & (literal).wav'), '--wav-sha256', ('b' * 64 -join ''),
        '--gpu-pack-id', 'fixture-cuda', '--gpu-backend', 'cuda',
        '--gpu-device', 'luid:00000000-00000001', '--output', (Join-Path $testRoot 'report with space [β].json')
    )
    $observerEcho = Invoke-WindowsLocalFrozenBoundedProcess `
        -Executable $fakeCompiler `
        -Arguments $observerRoundTrip `
        -Description 'fixture installed observer argument echo'
    Assert-Equal $observerEcho.ExitCode 0 'Installed observer argument echo exit code'
    $receivedObserverArguments = @($observerEcho.Stdout.TrimEnd([char[]]@("`r", "`n")) -split '\r?\n' | ForEach-Object {
        [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($_))
    })
    Assert-Equal $receivedObserverArguments.Count $observerRoundTrip.Count 'Installed observer child argument count'
    for ($index = 0; $index -lt $observerRoundTrip.Count; $index++) {
        Assert-Equal $receivedObserverArguments[$index] $observerRoundTrip[$index] "Installed observer child argument $index"
    }
    $campaignRoundTrip = $observerRoundTrip[0..14] + @('--campaign-power', 'battery') + $observerRoundTrip[15..16]
    $campaignEcho = Invoke-WindowsLocalFrozenCampaignProcess -Executable $fakeCompiler -Arguments $campaignRoundTrip -Description 'fixture campaign argument echo'
    $campaignEchoArguments = @($campaignEcho.Stdout.TrimEnd([char[]]@("`r", "`n")) -split '\r?\n' | ForEach-Object {
        [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($_))
    })
    Assert-Equal $campaignEcho.ExitCode 0 'Campaign argument echo exit code'
    Assert-Equal $campaignEchoArguments.Count 19 'Campaign child nineteen-argument count'
    for ($index = 0; $index -lt 19; $index++) { Assert-Equal $campaignEchoArguments[$index] $campaignRoundTrip[$index] "Campaign child literal argument $index" }
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'campaign-exact-cap'
    $boundedCapture = Invoke-WindowsLocalFrozenCampaignProcess -Executable $fakeCompiler -Arguments @() -Description 'fixture campaign exact cap'
    Assert-Equal $boundedCapture.Stdout.Length 262144 'Campaign exact stdout cap'
    Assert-Equal $boundedCapture.Stderr.Length 262144 'Campaign exact stderr cap'
    foreach ($overflowMode in @('campaign-overflow-stdout', 'campaign-overflow-stderr')) {
        $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = $overflowMode
        $overflowClock = [Diagnostics.Stopwatch]::StartNew()
        Invoke-ExpectedFailure { Invoke-WindowsLocalFrozenCampaignProcess -Executable $fakeCompiler -Arguments @() -Description 'fixture campaign overflow' } '262144-character per-stream output bound'
        Assert-True ($overflowClock.ElapsedMilliseconds -lt 5000) 'Campaign overflow did not stop its sixty-second sleeping owned child promptly.'
    }
    Invoke-ExpectedFailure { Invoke-WindowsLocalFrozenCampaignProcess -Executable $fakeCompiler -Arguments @() -Description 'fixture invalid deadline' -TimeoutMilliseconds 250 } 'fixed supported bounds'
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'observer-hang'
    # Only shorten a parsed fixture copy; production has no timeout override.
    $campaignRunnerAst = [Management.Automation.Language.Parser]::ParseFile((Join-Path $fixtureRoot 'scripts\windows-local-frozen-installer-integrity.ps1'), [ref]$null, [ref]$null)
    $campaignRunnerFunction = $campaignRunnerAst.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq 'Invoke-WindowsLocalFrozenCampaignProcess' }, $false)
    $shortRunner = [scriptblock]::Create($campaignRunnerFunction.Extent.Text.Replace('900000', '250'))
    Invoke-ExpectedFailure {
        & { . $shortRunner; Invoke-WindowsLocalFrozenCampaignProcess -Executable $fakeCompiler -Arguments $campaignRoundTrip -Description 'fixture shortened campaign deadline' }
    } 'timed out after the fixed campaign deadline'
    # Exercise real owned-child cleanup with controlled Task outcomes in a
    # parsed copy only. These are not OS-originated pipe faults, and the child
    # has no descendants: inherited-pipe descendant retirement is a separate gate.
    $readerRunnerSource = $campaignRunnerFunction.Extent.Text
    $startedMarker = '$processStarted = $true'
    $stderrReadMarker = '$stderrTask = $process.StandardError.ReadAsync($stderrBuffer, 0, $stderrBuffer.Length)'
    Assert-Equal ([regex]::Matches($readerRunnerSource, [regex]::Escape($startedMarker)).Count) 1 'Reader fixture owned-start seam count'
    Assert-Equal ([regex]::Matches($readerRunnerSource, [regex]::Escape($stderrReadMarker)).Count) 2 'Reader fixture initial/reissued stderr seam count'
    $ownedStartSeam = @'
        $readerFaultState.Launches++
        $readerFaultState.Process = [Diagnostics.Process]::GetProcessById($process.Id)
        # Pin the independently held native handle while this exact child is
        # alive. Never reacquire a PID for assertions or emergency cleanup.
        $null = $readerFaultState.Process.SafeHandle
        $readerFaultState.Id = $readerFaultState.Process.Id
        $readerFaultState.StartTicks = $readerFaultState.Process.StartTime.ToUniversalTime().Ticks
        $readerFaultState.ExpectedExecutable = [IO.Path]::GetFullPath($Executable)
        if ($readerFaultState.Id -ne $process.Id -or
            $readerFaultState.StartTicks -ne $process.StartTime.ToUniversalTime().Ticks) {
            throw 'Reader fixture held handle does not match the launched child.'
        }
        $readerFaultState.IdentityBound = $true
'@
    $readerRunnerSource = $readerRunnerSource.Replace($startedMarker, "$startedMarker`n$ownedStartSeam")
    $readyAndFaultSeam = @'
        $readyCount = if ($stdoutTask.Wait(5000)) { $stdoutTask.GetAwaiter().GetResult() } else { -1 }
        if ($readyCount -lt 1 -or $readyCount -gt $stdoutBuffer.Length -or $stdoutBuffer[0] -cne [char]'R') {
            throw 'Reader fault fixture did not receive real child readiness.'
        }
        # MainModule can be unavailable immediately after Process.Start; query
        # it only after the owned child is executing and has flushed readiness.
        $ownedModule = $readerFaultState.Process.MainModule
        if ($null -eq $ownedModule) { throw 'Reader fixture live child has no main module after readiness.' }
        $readerFaultState.Executable = $ownedModule.FileName
        $receipt = [IO.File]::ReadAllLines($readerFaultReceipt)
        if ($receipt.Length -ne 3 -or [int]$receipt[0] -ne $readerFaultState.Id -or
            [long]$receipt[1] -ne $readerFaultState.StartTicks -or
            -not [string]::Equals($receipt[2], $readerFaultState.Executable, [StringComparison]::OrdinalIgnoreCase) -or
            -not [string]::Equals($readerFaultState.Executable, $Executable, [StringComparison]::OrdinalIgnoreCase) -or
            $readerFaultState.Process.HasExited) {
            throw 'Reader fault fixture did not bind its exact live owned child.'
        }
        $readerFaultState.Ready = $true
        $injectedTask = switch ($readerFaultCase.Outcome) {
            'fault' { [Threading.Tasks.Task]::FromException[int]([IO.IOException]::new('fixture-sensitive-read-marker')) }
            'cancel' { [Threading.Tasks.Task]::FromCanceled[int]([Threading.CancellationToken]::new($true)) }
            # Complete the selected task so its processing branch throws the
            # controlled primary IOException, without malformed read counts.
            'unexpected' { [Threading.Tasks.Task]::FromResult[int](1) }
            default { throw 'Unknown reader fixture outcome.' }
        }
        if ($readerFaultCase.Stream -ceq 'stdout') { $stdoutTask = $injectedTask }
        else { $stderrTask = $injectedTask }
'@
    $initialStderrOffset = $readerRunnerSource.IndexOf($stderrReadMarker, [StringComparison]::Ordinal)
    $readerRunnerSource = $readerRunnerSource.Insert($initialStderrOffset + $stderrReadMarker.Length, "`n$readyAndFaultSeam")
    # Array assignment never completes after a terminating pipeline error.
    # Retain each emitted object in a parent-held list, and prove with a positive
    # canary that output before a throw cannot disappear from the assertion.
    $retentionProbe = [Collections.Generic.List[object]]::new()
    Invoke-ExpectedFailure {
        & { 'fixture pre-failure output'; throw [IO.IOException]::new('fixture retention failure') } |
            ForEach-Object { [void]$retentionProbe.Add($_) }
    } 'fixture retention failure'
    Assert-Equal $retentionProbe.Count 1 'Reader output collector lost pre-failure output'
    Assert-Equal $retentionProbe[0] 'fixture pre-failure output' 'Reader output collector altered pre-failure output'
    $readerCaptureBefore = $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE
    try {
        foreach ($readerStream in @('stdout', 'stderr')) {
            foreach ($readerOutcome in @('fault', 'cancel', 'unexpected')) {
                $readerFaultCase = @{ Stream = $readerStream; Outcome = $readerOutcome }
                $readerFaultState = @{ Process = $null; Launches = 0; Ready = $false; IdentityBound = $false; Id = 0; StartTicks = 0; Executable = ''; ExpectedExecutable = '' }
                $readerFaultReceipt = Join-Path $testRoot "reader-$readerStream-$readerOutcome.txt"
                $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'campaign-live-parent'
                $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE = $readerFaultReceipt
                $caseSource = $readerRunnerSource
                if ($readerOutcome -ceq 'unexpected') {
                    $resultMarker = '$count = $' + $readerStream + 'Task.GetAwaiter().GetResult()'
                    Assert-Equal ([regex]::Matches($caseSource, [regex]::Escape($resultMarker)).Count) 1 "Unexpected $readerStream result seam count"
                    $caseSource = $caseSource.Replace($resultMarker, "throw [IO.IOException]::new('fixture unexpected $readerStream processing failure')")
                }
                $faultRunner = [scriptblock]::Create($caseSource)
                $readerClock = [Diagnostics.Stopwatch]::StartNew()
                $readerError = $null
                $readerResults = [Collections.Generic.List[object]]::new()
                try {
                    try {
                        & { . $faultRunner; Invoke-WindowsLocalFrozenCampaignProcess -Executable $fakeCompiler -Arguments @() -Description 'fixture reader fault' } |
                            ForEach-Object { [void]$readerResults.Add($_) }
                    }
                    catch { $readerError = $_ }
                    $readerClock.Stop()
                    Assert-True ($null -ne $readerError) "$readerStream $readerOutcome returned success instead of failing closed."
                    Assert-Equal $readerResults.Count 0 "$readerStream $readerOutcome published a successful capture result"
                    Assert-Equal $readerFaultState.Launches 1 "$readerStream $readerOutcome launched more than its one owned child"
                    Assert-True $readerFaultState.Ready "$readerStream $readerOutcome did not establish real child readiness: $($readerError.Exception.Message)"
                    Assert-True ($readerClock.ElapsedMilliseconds -lt 10000) "$readerStream $readerOutcome did not return promptly."
                    Assert-True $readerFaultState.Process.HasExited "$readerStream $readerOutcome left its sixty-second sleeping owned child running."
                    Assert-True ($readerFaultState.Process.WaitForExit(1000)) "$readerStream $readerOutcome did not reap its owned child."
                    if ($readerOutcome -ceq 'unexpected') {
                        Assert-True ($readerError.Exception -is [IO.IOException]) "Unexpected $readerStream failure lost its primary exception type."
                        Assert-Equal $readerError.Exception.Message "fixture unexpected $readerStream processing failure" "Unexpected $readerStream failure lost its primary error"
                    }
                    else {
                        Assert-Equal $readerError.Exception.Message 'fixture reader fault output capture failed.' "$readerStream $readerOutcome capture failure was not categorical"
                        Assert-True (-not $readerError.Exception.Message.Contains('fixture-sensitive-read-marker')) "$readerStream $readerOutcome exposed the raw reader diagnostic."
                    }
                }
                finally {
                    if ($null -ne $readerFaultState.Process) {
                        try {
                            if (-not $readerFaultState.Process.HasExited) {
                                # Launch-time handle/creation identity also
                                # permits safe cleanup before readiness or
                                # executable metadata is available.
                                if (-not $readerFaultState.IdentityBound -or
                                    $readerFaultState.Process.Id -ne $readerFaultState.Id -or
                                    $readerFaultState.Process.StartTime.ToUniversalTime().Ticks -ne $readerFaultState.StartTicks -or
                                    -not [string]::Equals($readerFaultState.ExpectedExecutable, $fakeCompiler, [StringComparison]::OrdinalIgnoreCase)) {
                                    throw 'Refused reader fixture cleanup without exact live child ownership.'
                                }
                                Stop-WindowsLocalFrozenProcessTree $readerFaultState.Process 'exact owned reader fixture child'
                            }
                        }
                        finally { $readerFaultState.Process.Dispose() }
                    }
                }
            }
        }
    }
    finally { $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE = $readerCaptureBefore }
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'observer-hang'
    Invoke-ExpectedFailure {
        Invoke-WindowsLocalFrozenBoundedProcess `
            -Executable $fakeCompiler `
            -Arguments $observerRoundTrip `
            -Description 'fixture installed GPU observer hang' `
            -TimeoutMilliseconds 250 `
            -StreamDrainMilliseconds 250
    } 'timed out after 250 milliseconds'
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = ''
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'hang'
    $timeoutStopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    Invoke-ExpectedFailure {
        Invoke-WindowsLocalFrozenBoundedProcess `
            -Executable $fakeCompiler `
            -Arguments @() `
            -Description 'fixture hung compiler' `
            -TimeoutMilliseconds 250 `
            -StreamDrainMilliseconds 250
    } 'timed out after 250 milliseconds'
    $timeoutStopwatch.Stop()
    Assert-True ($timeoutStopwatch.ElapsedMilliseconds -lt 5000) 'Bounded local frozen process timeout did not return promptly after terminating the process tree.'
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = ''
    $context = Get-WindowsFrozenCpuWorkerSourceContext $fixtureRoot
    New-Item -ItemType Directory -Path $freezeRoot | Out-Null
    $frozenWorker = Join-Path $freezeRoot 'scribe-inference-worker.exe'
    Write-TestReviewedPe $frozenWorker 3
    $frozenHash = Get-WindowsFrozenCpuWorkerFileSha256 $frozenWorker
    $frozenRecord = New-WindowsFrozenCpuWorkerRecord $context (Get-Item -LiteralPath $frozenWorker).Length $frozenHash
    $frozenRecordPath = Join-Path $freezeRoot 'windows-frozen-cpu-worker-record.json'
    Write-Utf8 $frozenRecordPath ($frozenRecord | ConvertTo-Json -Depth 5)
    $frozenRecordHash = Get-WindowsFrozenCpuWorkerFileSha256 $frozenRecordPath
    Write-Utf8 (Join-Path $freezeRoot 'WINDOWS-FROZEN-CPU-WORKER-LOCAL-ONLY.txt') (Get-WindowsFrozenCpuWorkerMarkerText $frozenRecordHash ([pscustomobject]$frozenRecord))

    New-Item -ItemType Directory -Path $bundleRoot | Out-Null
    foreach ($path in $script:WindowsLocalFrozenBaseFiles) {
        $destination = Join-Path $bundleRoot ($path -replace '/', '\')
        switch ($path) {
            'local-transcriber.exe' { Write-TestReviewedPe $destination 2 }
            'scribe-inference-worker.exe' { Copy-Item -LiteralPath $frozenWorker -Destination $destination }
            'whisper-base.en-Q8_0.gguf' { New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null; [IO.File]::WriteAllBytes($destination, $modelBytes) }
            'bundled-model-manifest.json' { Copy-Item -LiteralPath (Join-Path $fixtureRoot 'runtime-manifests\whisper-base-en-q8_0-windows-x64.json') -Destination $destination }
            'worker-pack-catalog.json' { Write-Utf8 $destination '{"schema_version":1,"packs":[]}' }
            'WINDOWS-FROZEN-CPU-WORKER-LOCAL-ONLY.txt' { Write-Utf8 $destination (Get-WindowsFrozenCpuWorkerBundleMarkerText $frozenRecordHash ([pscustomobject]$frozenRecord)) }
            default {
                $sourceRelative = switch ($path) {
                    'licenses/transcribe.cpp-MIT.txt' { 'native/transcribe-cpp-v0.1.3/LICENSE' }
                    'licenses/transcribe.cpp-PROVENANCE.md' { 'native/transcribe-cpp-v0.1.3/PROVENANCE.md' }
                    'licenses/whisper.cpp-MIT.txt' { 'native/whisper-f049fff/LICENSE' }
                    'licenses/whisper.cpp-PROVENANCE.md' { 'native/whisper-f049fff/PROVENANCE.md' }
                    'licenses/sherpa-onnx-PROVENANCE.md' { 'native/sherpa-onnx-v1.13.5/PROVENANCE.md' }
                    'licenses/Silero-VAD-MIT.txt' { 'resources/silero-vad/LICENSE' }
                    'licenses/Silero-VAD-PROVENANCE.md' { 'resources/silero-vad/PROVENANCE.md' }
                    'README.txt' { $null }
                    default { 'resources/licenses/' + (Split-Path -Leaf $path) }
                }
                if ($null -eq $sourceRelative) { Write-Utf8 $destination 'fixture local frozen bundle' }
                else { New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null; Copy-Item -LiteralPath (Join-Path $fixtureRoot ($sourceRelative -replace '/', '\')) -Destination $destination }
            }
        }
    }
    Refresh-BundleInventory $bundleRoot
    $openedFrozen = Open-ValidatedWindowsFrozenCpuWorker $frozenRecordPath $fixtureRoot
    try { $fixtureBundle = Assert-WindowsLocalFrozenBundle $bundleRoot $openedFrozen } finally { $openedFrozen.WorkerStream.Dispose() }

    $template = Get-Content -LiteralPath (Join-Path $repositoryRoot 'installer\scribe-local-frozen.iss') -Raw
    foreach ($required in @('CloseApplications=no', 'CreateUninstallRegKey=no', 'UsePreviousAppDir=no', 'DisableDirPage=yes', 'SetupArchitecture=x86', 'ArchitecturesAllowed=x64compatible', 'ArchitecturesInstallIn64BitMode=x64compatible', "ExpandConstant('{param:DIR|}')", 'HasNoReparseAncestors', 'RejectLocalFrozenWizardDestination')) {
        Assert-True $template.Contains($required) "Local frozen installer template omitted $required."
    }
    foreach ($forbidden in @('[Icons]', '[Run]', '[Tasks]', 'CloseApplications=yes', 'StableAppIdGuid')) {
        Assert-True (-not $template.Contains($forbidden)) "Local frozen installer template unexpectedly contains $forbidden."
    }
    $builderSource = Get-Content -LiteralPath (Join-Path $repositoryRoot 'scripts\build-windows-frozen-test-installer.ps1') -Raw
    Assert-True (-not $builderSource.Contains('InnoProvenancePath')) 'Local frozen builder accepted a caller-provided Inno provenance path.'
    Assert-True $builderSource.Contains("Join-Path `$repositoryRoot 'installer\inno-setup-7.1.0-provenance.json'") 'Local frozen builder did not use the repository-pinned Inno provenance.'
    $productionVerifierSource = Get-Content -LiteralPath (Join-Path $repositoryRoot 'scripts\verify-windows-release-package.ps1') -Raw
    Assert-True (-not $productionVerifierSource.Contains('LocalFrozen')) 'Production package verifier was changed to admit local frozen markers.'

    $fixtureProvenancePath = Join-Path $fixtureRoot 'installer\inno-setup-7.1.0-provenance.json'
    $validProvenanceJson = Get-Content -LiteralPath $fixtureProvenancePath -Raw
    foreach ($mutation in @(
        @{ Name = 'legacy schema1 record'; Expected = 'reviewed 7.1.0 compiler'; Action = { param($record) $record.schema_version = [int64]1 } },
        @{
            Name = 'legacy schema1 package-wrapper record'; Expected = 'unexpected or missing'; Action = {
                param($record)
                $record.schema_version = [int64]1
                $record.PSObject.Properties.Remove('installer_size_bytes')
                $record.PSObject.Properties.Remove('installer_sha256')
                foreach ($field in @{
                    package_url = 'https://fixture.invalid/inno'; package_size_bytes = [int64]1; package_sha256 = ('0' * 64);
                    embedded_installer_path = 'tools/innosetup.exe'; embedded_installer_size_bytes = [int64]1;
                    embedded_installer_sha256 = ('0' * 64); embedded_package_verification_path = 'legal/VERIFICATION.txt'
                }.GetEnumerator()) {
                    $record | Add-Member -NotePropertyName $field.Key -NotePropertyValue $field.Value
                }
            }
        },
        @{ Name = 'unsupported schema'; Expected = 'reviewed 7.1.0 compiler'; Action = { param($record) $record.schema_version = [int64]3 } },
        @{ Name = 'wrong product version'; Expected = 'reviewed 7.1.0 compiler'; Action = { param($record) $record.product_version = '7.0.0' } },
        @{ Name = 'unknown field'; Expected = 'unexpected or missing'; Action = { param($record) $record | Add-Member -NotePropertyName unexpected -NotePropertyValue $true } },
        @{ Name = 'missing required installer digest'; Expected = 'unexpected or missing'; Action = { param($record) $record.PSObject.Properties.Remove('installer_sha256') } },
        @{ Name = 'wrong installer size'; Expected = 'reviewed 7.1.0 compiler'; Action = { param($record) $record.installer_size_bytes = [int64]1 } },
        @{ Name = 'wrong installer digest'; Expected = 'reviewed 7.1.0 compiler'; Action = { param($record) $record.installer_sha256 = ('0' * 64) } }
    )) {
        $mutatedProvenance = $validProvenanceJson | ConvertFrom-Json
        & $mutation.Action $mutatedProvenance
        Write-Utf8 $fixtureProvenancePath ($mutatedProvenance | ConvertTo-Json -Depth 4)
        try {
            Invoke-ExpectedFailure {
                Invoke-Builder $bundleRoot (Join-Path $testRoot "invalid-provenance-$($mutation.Name -replace ' ', '-')") $fakeCompiler
            } $mutation.Expected
        }
        finally {
            Write-Utf8 $fixtureProvenancePath $validProvenanceJson
        }
    }

    $capture = Join-Path $testRoot 'compiler-arguments.txt'
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = ''
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE = $capture
    $result = @(Invoke-Builder $bundleRoot $outputRoot $fakeCompiler)
    Assert-Equal $result.Count 1 'Local frozen installer builder result count'
    $result = $result[0]
    Assert-True (Test-Path -LiteralPath $result.InstallerPath -PathType Leaf) 'Local frozen installer builder did not publish its installer.'
    Assert-True (Test-Path -LiteralPath $result.RecordPath -PathType Leaf) 'Local frozen installer builder did not publish its record.'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $outputRoot 'payload'))) 'Local frozen installer builder retained a duplicate payload in final output.'
    Assert-True ((Get-Content -LiteralPath $capture -Raw).Contains('/DLocalFrozenBundleRoot=')) 'Local frozen installer compiler missed staged payload input.'
    Assert-True ((Get-Content -LiteralPath $capture -Raw).Contains('/DLocalFrozenTestToken=')) 'Local frozen installer compiler missed generated local token.'
    $publishedRecord = Get-Content -LiteralPath $result.RecordPath -Raw | ConvertFrom-Json
    Assert-Equal $publishedRecord.bundle_inventory_sha256 ((Get-FileHash -LiteralPath (Join-Path $bundleRoot 'bundle-inventory.json') -Algorithm SHA256).Hash.ToLowerInvariant()) 'Published local frozen installer inventory binding'
    $verifiedInstallerRecord = Assert-WindowsLocalFrozenInstallerRecord $result.RecordPath $openedFrozen $fixtureBundle $result.InstallerPath
    Assert-Equal $verifiedInstallerRecord.local_test_token $publishedRecord.local_test_token 'Published local frozen installer record token binding'

    # Exercise the copied real verifier's local-only observation path. The
    # fixture process seams preserve its ordering and cleanup boundary without
    # a real installer, signing key, or GPU hardware request.
    $observationBundle = Copy-Bundle 'o'
    Add-FixtureObservationPack $observationBundle
    $observationOutputRoot = Join-Path $testRoot 'i'
    $observationInstallerResult = @(Invoke-Builder $observationBundle $observationOutputRoot $fakeCompiler)[0]
    $fixtureVerifier = Join-Path $fixtureRoot 'scripts\verify-windows-local-frozen-test-installer.ps1'
    $observationWav = Join-Path $testRoot 'fixture wav [β] & (literal).wav'
    [IO.File]::WriteAllBytes($observationWav, [byte[]](1, 2, 3, 4))
    $observationWavSha256 = (Get-FileHash -LiteralPath $observationWav -Algorithm SHA256).Hash.ToLowerInvariant()
    $observationDevice = 'luid:00000000-00000001'
    $previousHubOffline = $env:HF_HUB_OFFLINE
    $previousTransformersOffline = $env:TRANSFORMERS_OFFLINE
    $env:HF_HUB_OFFLINE = 'inherited-hub-setting'
    $env:TRANSFORMERS_OFFLINE = 'inherited-transformers-setting'
    $global:WindowsLocalFrozenVerifierFixtureInstallParent = Join-Path $testRoot 'v'
    New-Item -ItemType Directory -Path $global:WindowsLocalFrozenVerifierFixtureInstallParent -Force | Out-Null
    try {
        $successReport = Join-Path $testRoot 'published observation [β].json'
        Invoke-FixtureVerifier `
            $fixtureVerifier $observationBundle $frozenRecordPath `
            $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
            $observationWav $observationWavSha256 $observationDevice $successReport
        Assert-Equal ($global:WindowsLocalFrozenVerifierEvents -join '|') 'local frozen installer|installed local frozen CPU smoke|installed local frozen GPU observation|local frozen installer uninstaller' 'Installed observer verifier success ordering'
        Assert-True (Test-Path -LiteralPath $successReport -PathType Leaf) 'Installed observer verifier did not publish its validated report.'
        Assert-True (-not (Test-Path -LiteralPath $global:WindowsLocalFrozenVerifierInstalledRoot)) 'Installed observer verifier left its successful token-bound installation.'
        Assert-Equal $env:HF_HUB_OFFLINE 'inherited-hub-setting' 'Installed observer verifier restored HF offline environment'
        Assert-Equal $env:TRANSFORMERS_OFFLINE 'inherited-transformers-setting' 'Installed observer verifier restored transformers offline environment'
        Assert-Equal $global:WindowsLocalFrozenVerifierObserverExecutable (Join-Path $global:WindowsLocalFrozenVerifierInstalledRoot 'local-transcriber.exe') 'Installed observer verifier bound the installed collector path'
        $expectedObserverArguments = @(
            '--scribe-windows-gpu-capture-observation',
            '--model', (Join-Path $global:WindowsLocalFrozenVerifierInstalledRoot 'whisper-base.en-Q8_0.gguf'), '--model-sha256', $modelHash,
            '--wav', $observationWav, '--wav-sha256', $observationWavSha256,
            '--gpu-pack-id', 'c', '--gpu-backend', 'cuda',
            '--gpu-device', $observationDevice
        )
        Assert-Equal $global:WindowsLocalFrozenVerifierObserverArguments.Count 17 'Installed observer verifier argument count'
        for ($index = 0; $index -lt $expectedObserverArguments.Count; $index++) {
            Assert-Equal $global:WindowsLocalFrozenVerifierObserverArguments[$index] $expectedObserverArguments[$index] "Installed observer verifier argument $index"
        }
        Assert-Equal $global:WindowsLocalFrozenVerifierObserverArguments[15] '--output' 'Installed observer verifier output flag'
        Assert-True ($global:WindowsLocalFrozenVerifierObserverArguments[16] -like (Join-Path ([IO.Path]::GetTempPath()) 'scribe-local-frozen-installer-verification-*\gpu-observation.json')) 'Installed observer verifier owns a temporary no-replace output path.'

        $campaignBundle = Copy-Bundle 'campaign-vulkan'
        Add-FixtureObservationPack $campaignBundle 'vulkan'
        $campaignInstaller = @(Invoke-Builder $campaignBundle (Join-Path $testRoot 'campaign-vulkan-installer') $fakeCompiler)[0]
        foreach ($lane in @(
            @{ Backend = 'cuda'; Power = 'ac'; Bundle = $observationBundle; Installer = $observationInstallerResult; Mode = 'campaign-large' },
            @{ Backend = 'cuda'; Power = 'battery'; Bundle = $observationBundle; Installer = $observationInstallerResult; Mode = '' },
            @{ Backend = 'vulkan'; Power = 'ac'; Bundle = $campaignBundle; Installer = $campaignInstaller; Mode = '' }
        )) {
            $campaignOutput = Join-Path $testRoot "campaign-$($lane.Backend)-$($lane.Power).json"
            Invoke-FixtureVerifier $fixtureVerifier $lane.Bundle $frozenRecordPath $lane.Installer.InstallerPath $lane.Installer.RecordPath `
                $observationWav $observationWavSha256 $observationDevice $campaignOutput -CampaignPower $lane.Power -Backend $lane.Backend -Mode $lane.Mode
            $campaign = Get-Content -LiteralPath $campaignOutput -Raw | ConvertFrom-Json -Depth 32
            Assert-Equal ($global:WindowsLocalFrozenVerifierEvents -join '|') 'local frozen installer|installed local frozen CPU smoke|installed local frozen GPU observation campaign|local frozen installer uninstaller' 'Campaign installed verification and cleanup ordering'
            Assert-Equal $global:WindowsLocalFrozenVerifierObserverExecutable (Join-Path $global:WindowsLocalFrozenVerifierInstalledRoot 'local-transcriber.exe') 'Campaign launches only the installed collector'
            Assert-Equal $global:WindowsLocalFrozenVerifierObserverArguments.Count 19 'Campaign installed nineteen-argument count'
            $expectedCampaignArguments = @(
                '--scribe-windows-gpu-capture-observation', '--model', (Join-Path $global:WindowsLocalFrozenVerifierInstalledRoot 'whisper-base.en-Q8_0.gguf'), '--model-sha256', $modelHash,
                '--wav', $observationWav, '--wav-sha256', $observationWavSha256, '--gpu-pack-id', 'c', '--gpu-backend', $lane.Backend,
                '--gpu-device', $observationDevice, '--campaign-power', $lane.Power, '--output'
            )
            for ($index = 0; $index -lt 18; $index++) { Assert-Equal $global:WindowsLocalFrozenVerifierObserverArguments[$index] $expectedCampaignArguments[$index] "Campaign installed exact argument $index" }
            Assert-Equal (Split-Path -Leaf $global:WindowsLocalFrozenVerifierObserverArguments[18]) 'gpu-campaign.json' 'Campaign temporary filename'
            Assert-True (-not (Test-Path -LiteralPath (Split-Path -Parent $global:WindowsLocalFrozenVerifierObserverArguments[18]))) 'Campaign publication preceded scratch cleanup'
            Assert-True (-not (Test-Path -LiteralPath $global:WindowsLocalFrozenVerifierInstalledRoot)) 'Campaign publication preceded trusted uninstall'
            Assert-Equal $campaign.captures.Count 14 'Campaign exact generation count'
            Assert-Equal $campaign.runs.Count 52 'Campaign exact run count'
            Assert-Equal @($campaign.runs | Where-Object { $_.measured -and $_.phase -ceq 'cold' }).Count 10 'Campaign five measured cold CPU/GPU pairs'
            Assert-Equal @($campaign.runs | Where-Object { $_.measured -and $_.phase -ceq 'warm' }).Count 40 'Campaign twenty measured warm CPU/GPU pairs'
            Assert-True ($campaign.unsigned -and $campaign.unqualified -and -not $campaign.auto_eligible -and -not $campaign.release_approved) 'Campaign incorrectly became qualification evidence'
            Assert-True ($campaign.runs[0].normalized_transcript_sha256 -cne $campaign.runs[1].normalized_transcript_sha256) 'Campaign lost diagnostic transcript differences'
            Assert-True $campaign.runs[0].worker_process_affinity.changed 'Campaign lost diagnostic affinity changes'
            if ($lane.Mode -ceq 'campaign-large') { Assert-True ((Get-Item -LiteralPath $campaignOutput).Length -gt 1MB) 'Campaign did not exercise its distinct larger report bound' }
        }

        foreach ($case in @(
            @{ Mode = 'campaign-nonzero'; Expected = 'failed with exit code 37.' },
            @{ Mode = 'campaign-timeout'; Expected = 'fixed campaign deadline' },
            @{ Mode = 'campaign-overflow'; Expected = '262144-character per-stream output bound' },
            @{ Mode = 'campaign-missing'; Expected = 'report file is missing' },
            @{ Mode = 'campaign-incomplete'; Expected = 'invalid local-only completion' },
            @{ Mode = 'campaign-power-drift'; Expected = 'request sequence or completion state' },
            @{ Mode = 'uninstall-failure'; Expected = 'uninstaller exited with 41' },
            @{ Mode = 'context-check-failure'; Expected = 'Fixture source context check failed after uninstall' },
            @{ Mode = 'temporary-cleanup-failure'; Expected = 'Fixture temporary observation cleanup failed' }
        )) {
            $campaignFailure = Join-Path $testRoot "campaign-$($case.Mode)-failure.json"
            Invoke-ExpectedFailure {
                Invoke-FixtureVerifier $fixtureVerifier $observationBundle $frozenRecordPath $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                    $observationWav $observationWavSha256 $observationDevice $campaignFailure -CampaignPower 'ac' -Mode $case.Mode
            } $case.Expected 'sensitive fixture diagnostic must not escape'
            Assert-True (-not (Test-Path -LiteralPath $campaignFailure)) "$($case.Mode) published a final campaign report"
            Assert-True (-not (Test-Path -LiteralPath $global:WindowsLocalFrozenVerifierInstalledRoot)) "$($case.Mode) left the trusted fixture installation"
            Assert-Equal @($global:WindowsLocalFrozenVerifierEvents | Where-Object { $_ -ceq 'installed local frozen GPU observation campaign' }).Count 1 'Failed campaign replayed the collector'
        }
        $campaignRace = Join-Path $testRoot 'campaign-race.json'
        Invoke-ExpectedFailure {
            Invoke-FixtureVerifier $fixtureVerifier $observationBundle $frozenRecordPath $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                $observationWav $observationWavSha256 $observationDevice $campaignRace -CampaignPower 'ac' -Mode 'output-race'
        } 'already exists'
        Assert-Equal (Get-Content -LiteralPath $campaignRace -Raw) 'race sentinel' 'Campaign replaced a racing output'
        Assert-Equal @(Get-ChildItem -LiteralPath $testRoot -Filter '.campaign-race.json.staging-*' -Force).Count 0 'Campaign race retained staging output'
        foreach ($invalidPower in @('', 'AC', 'Battery', 'unknown')) {
            Invoke-ExpectedFailure {
                Invoke-FixtureVerifier $fixtureVerifier $observationBundle $frozenRecordPath $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                    $observationWav $observationWavSha256 $observationDevice (Join-Path $testRoot 'invalid-power.json') -CampaignPower $invalidPower
            } $(if ($invalidPower -cin @('AC', 'Battery')) { 'lowercase ac or battery' } else { 'does not belong to the set' })
            Assert-Equal $global:WindowsLocalFrozenVerifierEvents.Count 0 'Invalid campaign power started installation'
        }
        Invoke-ExpectedFailure { Get-WindowsLocalFrozenCaptureObservationRequest @{ ObservationCampaignPower = 'ac' } } 'must be supplied together'

        # Strict report boundary tests are pure synthetic data. They use the
        # same reader as the installed path, but no signing key or GPU worker.
        $campaignExpected = [pscustomobject]@{
            CollectorBuildRevision = $global:WindowsLocalFrozenVerifierRevision; ModelSha256 = $modelHash; WavSha256 = $observationWavSha256;
            PackId = 'c'; PackVersion = '1'; PackSha256 = ('c' * 64 -join ''); PackSecurityEpoch = 1; RuntimeAbi = 1;
            Backend = 'cuda'; Provider = 'cuda'; StableDevice = $observationDevice; CampaignPower = 'ac'
        }
        $syntheticCampaignPath = Join-Path $testRoot 'synthetic-campaign.json'
        foreach ($case in @(
            @{ Change = { param($r) $r.schema_version = 3 }; Expected = 'completion or identity flags' },
            @{ Change = { param($r) $r.unsigned = 'true' }; Expected = 'completion or identity flags' },
            @{ Change = { param($r) $r.auto_eligible = $true }; Expected = 'completion or identity flags' },
            @{ Change = { param($r) $r.release_approved = $true }; Expected = 'completion or identity flags' },
            @{ Change = { param($r) $r.cleanup_complete = $false }; Expected = 'completion or identity flags' },
            @{ Change = { param($r) $r.collector_build_revision = '0' * 40 }; Expected = 'completion or identity flags' },
            @{ Change = { param($r) $r.expected_power = 'battery' }; Expected = 'completion or identity flags' },
            @{ Change = { param($r) $r.inputs.model_sha256 = 'd' * 64 }; Expected = 'WAV identities' },
            @{ Change = { param($r) $r.inputs.wav_sha256 = 'd' * 64 }; Expected = 'WAV identities' },
            @{ Change = { param($r) $r.gpu_identity.pack_sha256 = 'd' * 64 }; Expected = 'verified pack/backend/stable device' },
            @{ Change = { param($r) $r.gpu_identity.pack_version = '2' }; Expected = 'verified pack/backend/stable device' },
            @{ Change = { param($r) $r.gpu_identity.runtime_abi = 2 }; Expected = 'verified pack/backend/stable device' },
            @{ Change = { param($r) $r.gpu_identity.pack_security_epoch = 2 }; Expected = 'verified pack/backend/stable device' },
            @{ Change = { param($r) $r.gpu_identity.stable_device = 'different-device' }; Expected = 'verified pack/backend/stable device' },
            @{ Change = { param($r) $r.gpu_identity.provider = 'vulkan' }; Expected = 'verified pack/backend/stable device' },
            @{ Change = { param($r) $r.unavailable.thermal_state.reason = 'observed' }; Expected = 'required unavailable observation' },
            @{ Change = { param($r) $r.environmental_controls.background_load.status = 'available' }; Expected = 'required unavailable observation' },
            @{ Change = { param($r) $r.captures = $r.captures[0..12] }; Expected = 'exact capture and request counts' },
            @{ Change = { param($r) $r.runs = $r.runs[0..50] }; Expected = 'exact capture and request counts' },
            @{ Change = { param($r) $r.captures[0].hello_frame_hex = 'AB' }; Expected = 'handshake frame is invalid' },
            @{ Change = { param($r) $r.captures[1].digest_sha256 = $r.captures[0].digest_sha256 }; Expected = 'digest-bound' },
            @{ Change = { param($r) $r.captures[2].logical_sequence = 4 }; Expected = 'digest-bound' },
            @{ Change = { param($r) $r.captures[2].target = 'gpu' }; Expected = 'digest-bound' },
            @{ Change = { param($r) $r.captures[2].hello_frame_hex = $r.captures[0].hello_frame_hex }; Expected = 'digest-bound' },
            @{ Change = { param($r) $r.runs[0].pair_index = 2 }; Expected = 'pair index is invalid' },
            @{ Change = { param($r) $r.runs[2].target = 'cpu' }; Expected = 'request sequence or completion state' },
            @{ Change = { param($r) $r.runs[10].measured = $true }; Expected = 'request sequence or completion state' },
            @{ Change = { param($r) $r.runs[12].generation_ref = $r.captures[2].generation_ref }; Expected = 'request sequence or completion state' },
            @{ Change = { param($r) $r.runs[12].warm_reused = $false }; Expected = 'request sequence or completion state' },
            @{ Change = { param($r) $r.runs[12].model_load_ms = 1 }; Expected = 'warm request reloaded its model' },
            @{ Change = { param($r) $r.runs[51].status = 'cancelled' }; Expected = 'request sequence or completion state' },
            @{ Change = { param($r) $r.runs[0].telemetry_sample_count = 0 }; Expected = 'nonnegative integer range' },
            @{ Change = { param($r) $r.runs[0].backend_ms = '1' }; Expected = 'must be an integer' },
            @{ Change = { param($r) $r.runs[1].video_memory.local.sampled_min_budget_bytes = -1 }; Expected = 'nonnegative integer range' },
            @{ Change = { param($r) $r.runs[1].raw_provider_memory.before.provider_reported_memory_free_bytes = 65 }; Expected = 'provider memory is inconsistent' },
            @{ Change = { param($r) $r.runs[1].memory_availability.after.source.method = 'wrong' }; Expected = 'CUDA memory source is invalid' },
            @{ Change = { param($r) $r.runs[0].worker_process_affinity.changed = $false }; Expected = 'change result is inconsistent' },
            @{ Change = { param($r) $r.runs[0].worker_process_affinity.after.process_mask_hex = '0000000000000004' }; Expected = 'masks are inconsistent' },
            @{ Change = { param($r) $r.runs[0].worker_process_affinity.before = $null }; Expected = 'omitted an endpoint' },
            @{ Change = { param($r) $r.runs[0].unexpected = 'claim' }; Expected = 'unexpected or missing fields' }
        )) {
            $syntheticCampaign = New-FixtureCampaignReport 'ac'
            & $case.Change $syntheticCampaign
            Write-Utf8 $syntheticCampaignPath ($syntheticCampaign | ConvertTo-Json -Depth 24)
            Invoke-ExpectedFailure { Read-WindowsLocalFrozenCaptureCampaignReport $syntheticCampaignPath $campaignExpected } $case.Expected
        }
        $validCampaignJson = (New-FixtureCampaignReport 'ac') | ConvertTo-Json -Depth 24 -Compress
        foreach ($invalidJson in @(
            $validCampaignJson.Replace('"unsigned":true', '"unsigned":true,"unsigned":true'),
            $validCampaignJson.Replace('"unsigned":true', '"unsigned":true,"Unsigned":true'),
            $validCampaignJson.Replace('"status":"succeeded"', '"status":"succeeded","Status":"succeeded"'),
            ($validCampaignJson.Substring(0, $validCampaignJson.Length - 1) + ',}'),
            ('/* invalid comment */' + $validCampaignJson)
        )) {
            Write-Utf8 $syntheticCampaignPath $invalidJson
            Invoke-ExpectedFailure { Read-WindowsLocalFrozenCaptureCampaignReport $syntheticCampaignPath $campaignExpected } 'not strict duplicate-free JSON'
        }
        [IO.File]::WriteAllBytes($syntheticCampaignPath, [byte[]](0xff, 0xfe))
        Invoke-ExpectedFailure { Read-WindowsLocalFrozenCaptureCampaignReport $syntheticCampaignPath $campaignExpected } 'UTF-8'
        $oversizedCampaign = [IO.File]::Open($syntheticCampaignPath, [IO.FileMode]::Create)
        try { $oversizedCampaign.SetLength(32MB + 1) } finally { $oversizedCampaign.Dispose() }
        Invoke-ExpectedFailure { Read-WindowsLocalFrozenCaptureCampaignReport $syntheticCampaignPath $campaignExpected } 'between 1 and 33554432 bytes'
        foreach ($invalidLimit in @(0, (1MB + 1), 31MB, (32MB + 1))) {
            Invoke-ExpectedFailure { Publish-WindowsLocalFrozenNewReport (Join-Path $testRoot 'invalid-publication.json') ([byte[]](1)) $invalidLimit } 'publication bytes are outside the supported bound'
        }

        $uncertainCampaign = New-FixtureCampaignReport 'ac'
        $uncertainCampaign.runs[1].raw_provider_memory.before = [ordered]@{ status = 'unavailable'; reason = 'provider_query_failed' }
        $uncertainCampaign.runs[1].memory_availability.after = [ordered]@{ status = 'unavailable'; reason = 'stable_device_ambiguous' }
        $uncertainCampaign.runs[0].worker_process_affinity.before = [ordered]@{ status = 'unavailable'; reason = 'unsupported_processor_group_topology' }
        $uncertainCampaign.runs[0].worker_process_affinity.changed = $null
        Write-Utf8 $syntheticCampaignPath ($uncertainCampaign | ConvertTo-Json -Depth 24)
        $acceptedUncertain = Read-WindowsLocalFrozenCaptureCampaignReport $syntheticCampaignPath $campaignExpected
        Assert-Equal $acceptedUncertain.Report.runs[1].memory_availability.after.reason 'stable_device_ambiguous' 'Campaign diagnostic lost unavailable telemetry'
        Assert-True ($null -eq $acceptedUncertain.Report.runs[0].worker_process_affinity.changed) 'Campaign diagnostic invented affinity certainty'

        $campaignExpected.Backend = 'vulkan'; $campaignExpected.Provider = 'vulkan'
        $vulkanCampaign = New-FixtureCampaignReport 'ac' 'vulkan'
        Write-Utf8 $syntheticCampaignPath ($vulkanCampaign | ConvertTo-Json -Depth 24)
        $acceptedVulkan = Read-WindowsLocalFrozenCaptureCampaignReport $syntheticCampaignPath $campaignExpected
        Assert-True ($acceptedVulkan.Report.runs[1].memory_availability.before.memory_total_bytes -ne $acceptedVulkan.Report.gpu_identity.memory_total_bytes) 'Vulkan derived heap total was incorrectly forced to match provider identity total'
        foreach ($run in $vulkanCampaign.runs | Where-Object { $_.target -ceq 'gpu' }) {
            foreach ($endpoint in @($run.memory_availability.before, $run.memory_availability.after)) {
                $endpoint.source.heap_selection = 'all_heaps_integrated'; $endpoint.memory_total_bytes = 1536; $endpoint.available_memory_bytes = 640
            }
        }
        $vulkanCampaign.gpu_identity.device_class = 'integrated_gpu'
        Write-Utf8 $syntheticCampaignPath ($vulkanCampaign | ConvertTo-Json -Depth 24)
        Assert-Equal (Read-WindowsLocalFrozenCaptureCampaignReport $syntheticCampaignPath $campaignExpected).Report.runs[1].memory_availability.before.available_memory_bytes 640 'Vulkan integrated all-heap headroom'
        foreach ($case in @(
            @{ Change = { param($r) $r.runs[1].memory_availability.before.available_memory_bytes = 385 }; Expected = 'heap inventory is inconsistent' },
            @{ Change = { param($r) $r.runs[1].memory_availability.before.source.heaps[0].heap_index = 1 }; Expected = 'heap is noncanonical' },
            @{ Change = { param($r) $r.runs[1].memory_availability.before.source.heaps[0].flags = 3 }; Expected = 'heap is noncanonical' },
            @{ Change = { param($r) $r.runs[1].memory_availability.before.source.heaps[0].budget_bytes = 1025 }; Expected = 'heap is noncanonical' },
            @{ Change = { param($r) $r.runs[1].memory_availability.before.source.heap_selection = 'all_heaps_integrated' }; Expected = 'memory source is invalid' }
        )) {
            $vulkanCampaign = New-FixtureCampaignReport 'ac' 'vulkan'
            & $case.Change $vulkanCampaign
            Write-Utf8 $syntheticCampaignPath ($vulkanCampaign | ConvertTo-Json -Depth 24)
            Invoke-ExpectedFailure { Read-WindowsLocalFrozenCaptureCampaignReport $syntheticCampaignPath $campaignExpected } $case.Expected
        }

        $partialReport = Join-Path $testRoot 'partial-observation.json'
        Invoke-ExpectedFailure {
            Invoke-FixtureVerifier `
                $fixtureVerifier $observationBundle $frozenRecordPath `
                $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                $observationWav $observationWavSha256 $observationDevice $partialReport -PartialRequest
        } 'must be supplied together'
        Assert-Equal $global:WindowsLocalFrozenVerifierEvents.Count 0 'Partial installed observation request launched an installer process'
        Assert-True (-not (Test-Path -LiteralPath $partialReport)) 'Partial installed observation request published output'

        $missingPackReport = Join-Path $testRoot 'missing-pack-observation.json'
        Invoke-ExpectedFailure {
            Invoke-FixtureVerifier `
                $fixtureVerifier $observationBundle $frozenRecordPath `
                $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                $observationWav $observationWavSha256 $observationDevice $missingPackReport -MissingPack
        } 'absent or ambiguous'
        Assert-Equal $global:WindowsLocalFrozenVerifierEvents.Count 0 'Absent installed observation pack launched an installer process'
        Assert-True (-not (Test-Path -LiteralPath $missingPackReport)) 'Absent installed observation pack published output'

        $overlapReport = Join-Path $observationBundle 'forbidden-observation.json'
        Invoke-ExpectedFailure {
            Invoke-FixtureVerifier `
                $fixtureVerifier $observationBundle $frozenRecordPath `
                $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                $observationWav $observationWavSha256 $observationDevice $overlapReport
        } 'cannot overlap'
        Assert-Equal $global:WindowsLocalFrozenVerifierEvents.Count 0 'Overlapping installed observation output launched an installer process'

        $frozenOverlapReport = Join-Path $freezeRoot 'forbidden-observation.json'
        Invoke-ExpectedFailure {
            Invoke-FixtureVerifier `
                $fixtureVerifier $observationBundle $frozenRecordPath `
                $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                $observationWav $observationWavSha256 $observationDevice $frozenOverlapReport
        } 'cannot overlap'
        Assert-Equal $global:WindowsLocalFrozenVerifierEvents.Count 0 'Frozen CPU worker overlap launched an installer process'

        $missingParentReport = Join-Path $testRoot 'missing-observation-report-parent\report.json'
        Invoke-ExpectedFailure {
            Invoke-FixtureVerifier `
                $fixtureVerifier $observationBundle $frozenRecordPath `
                $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                $observationWav $observationWavSha256 $observationDevice $missingParentReport
        } 'report parent directory is missing'
        Assert-Equal $global:WindowsLocalFrozenVerifierEvents.Count 0 'Missing installed observation output parent launched an installer process'

        $collisionReport = Join-Path $testRoot 'existing-observation-report.json'
        [IO.File]::WriteAllText($collisionReport, 'existing report', [Text.UTF8Encoding]::new($false))
        Invoke-ExpectedFailure {
            Invoke-FixtureVerifier `
                $fixtureVerifier $observationBundle $frozenRecordPath `
                $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                $observationWav $observationWavSha256 $observationDevice $collisionReport
        } 'already exists'
        Assert-Equal (Get-Content -LiteralPath $collisionReport -Raw) 'existing report' 'Existing installed observation output was replaced'
        Assert-Equal $global:WindowsLocalFrozenVerifierEvents.Count 0 'Existing installed observation output launched an installer process'

        foreach ($case in @(
            @{ Mode = 'observer-nonzero'; Expected = 'failed with exit code 37' },
            @{ Mode = 'observer-timeout'; Expected = 'timed out after 900000 milliseconds' },
            @{ Mode = 'observer-missing-report'; Expected = 'file is missing' },
            @{ Mode = 'observer-malformed-report'; Expected = 'not valid JSON' },
            @{ Mode = 'observer-oversized-report'; Expected = 'between 1 and 1048576 bytes' },
            @{ Mode = 'observer-tampered-pack'; Expected = 'does not bind the requested verified pack' },
            @{ Mode = 'observer-wrong-pack-version'; Expected = 'does not bind the requested verified pack' },
            @{ Mode = 'observer-wrong-backend'; Expected = 'does not bind the requested verified pack' },
            @{ Mode = 'observer-wrong-device'; Expected = 'does not bind the requested verified pack' },
            @{ Mode = 'observer-wrong-model'; Expected = 'does not bind the installed model and requested WAV identities' },
            @{ Mode = 'observer-wrong-wav'; Expected = 'does not bind the installed model and requested WAV identities' },
            @{ Mode = 'observer-wrong-schema'; Expected = 'invalid local-only qualification flags' },
            @{ Mode = 'observer-wrong-kind'; Expected = 'invalid local-only qualification flags' },
            @{ Mode = 'observer-campaign-claim'; Expected = 'unexpected or missing fields' },
            @{ Mode = 'observer-string-unsigned'; Expected = 'invalid local-only qualification flags' },
            @{ Mode = 'observer-approval-claim'; Expected = 'invalid local-only qualification flags' },
            @{ Mode = 'observer-wrong-revision'; Expected = 'invalid local-only qualification flags' },
            @{ Mode = 'uninstall-failure'; Expected = 'uninstaller exited with 41' }
        )) {
            $failureReport = Join-Path $testRoot "$($case.Mode)-report.json"
            Invoke-ExpectedFailure {
                Invoke-FixtureVerifier `
                    $fixtureVerifier $observationBundle $frozenRecordPath `
                    $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                    $observationWav $observationWavSha256 $observationDevice $failureReport -Mode $case.Mode
            } $case.Expected
            Assert-True (-not (Test-Path -LiteralPath $failureReport)) "$($case.Mode) published an apparently successful installed observation report."
            Assert-True (-not (Test-Path -LiteralPath $global:WindowsLocalFrozenVerifierInstalledRoot)) "$($case.Mode) left a token-bound test installation after verifier cleanup."
        }

        $installedCatalogDriftReport = Join-Path $testRoot 'installed-catalog-drift-report.json'
        Invoke-ExpectedFailure {
            Invoke-FixtureVerifier `
                $fixtureVerifier $observationBundle $frozenRecordPath `
                $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                $observationWav $observationWavSha256 $observationDevice $installedCatalogDriftReport -Mode 'installed-catalog-drift'
        } 'pack binding changed at PackVersion after payload parity'
        Assert-Equal ($global:WindowsLocalFrozenVerifierEvents -join '|') 'local frozen installer|installed local frozen CPU smoke|local frozen installer cleanup' 'Installed catalog drift did not stop before observer invocation.'
        Assert-True (-not (Test-Path -LiteralPath $installedCatalogDriftReport)) 'Installed catalog drift published an apparently successful observation report.'
        Assert-True (-not (Test-Path -LiteralPath $global:WindowsLocalFrozenVerifierInstalledRoot)) 'Installed catalog drift did not receive normal verifier cleanup.'

        $temporaryCleanupFailureReport = Join-Path $testRoot 'temporary-cleanup-failure-report.json'
        Invoke-ExpectedFailure {
            Invoke-FixtureVerifier `
                $fixtureVerifier $observationBundle $frozenRecordPath `
                $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                $observationWav $observationWavSha256 $observationDevice $temporaryCleanupFailureReport -Mode 'temporary-cleanup-failure'
        } 'Fixture temporary observation cleanup failed'
        Assert-Equal ($global:WindowsLocalFrozenVerifierEvents -join '|') 'local frozen installer|installed local frozen CPU smoke|installed local frozen GPU observation|local frozen installer uninstaller' 'Temporary cleanup failure changed verifier process ordering.'
        Assert-True (-not (Test-Path -LiteralPath $temporaryCleanupFailureReport)) 'Temporary cleanup failure published an apparently successful observation report.'
        Assert-True (-not (Test-Path -LiteralPath $global:WindowsLocalFrozenVerifierInstalledRoot)) 'Temporary cleanup failure left its token-bound test installation.'

        $raceReport = Join-Path $testRoot 'race-observation-report.json'
        Invoke-ExpectedFailure {
            Invoke-FixtureVerifier `
                $fixtureVerifier $observationBundle $frozenRecordPath `
                $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                $observationWav $observationWavSha256 $observationDevice $raceReport -Mode 'output-race'
        } 'already exists'
        Assert-Equal (Get-Content -LiteralPath $raceReport -Raw) 'race sentinel' 'Racing installed observation output was replaced'
        Assert-True (-not (Test-Path -LiteralPath $global:WindowsLocalFrozenVerifierInstalledRoot)) 'Output-race observation left its token-bound test installation.'
        Assert-Equal (@(Get-ChildItem -LiteralPath $testRoot -Filter '.race-observation-report.json.staging-*' -Force).Count) 0 'Output-race observation retained an owned staging sibling'
        Remove-Item -LiteralPath $raceReport -Force

        $parityReport = Join-Path $testRoot 'parity-failure-report.json'
        Invoke-ExpectedFailure {
            Invoke-FixtureVerifier `
                $fixtureVerifier $observationBundle $frozenRecordPath `
                $observationInstallerResult.InstallerPath $observationInstallerResult.RecordPath `
                $observationWav $observationWavSha256 $observationDevice $parityReport -Mode 'parity-failure'
        } 'payload parity mismatch'
        Assert-Equal ($global:WindowsLocalFrozenVerifierEvents -join '|') 'local frozen installer' 'Parity failure launched smoke, observer, or uninstaller'
        Assert-True (-not (Test-Path -LiteralPath $parityReport)) 'Parity failure published an installed observation report'
        Assert-True (Test-Path -LiteralPath $global:WindowsLocalFrozenVerifierInstalledRoot) 'Parity failure did not retain its untrusted test installation for inspection'
        Remove-FixtureVerifierInstalledRoot $global:WindowsLocalFrozenVerifierInstalledRoot
    }
    finally {
        $env:HF_HUB_OFFLINE = $previousHubOffline
        $env:TRANSFORMERS_OFFLINE = $previousTransformersOffline
        Remove-FixtureVerifierInstalledRoot $global:WindowsLocalFrozenVerifierInstalledRoot
    }

    $originalInstallerBytes = [IO.File]::ReadAllBytes($result.InstallerPath)
    $tamperedInstallerBytes = [byte[]]$originalInstallerBytes.Clone()
    $tamperedInstallerBytes[100] = $tamperedInstallerBytes[100] -bxor 1
    [IO.File]::WriteAllBytes($result.InstallerPath, $tamperedInstallerBytes)
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenInstallerRecord $result.RecordPath $openedFrozen $fixtureBundle $result.InstallerPath } 'bytes do not match'
    [IO.File]::WriteAllBytes($result.InstallerPath, $originalInstallerBytes)

    $originalInstallerRecordText = Get-Content -LiteralPath $result.RecordPath -Raw
    $coercedInstallerRecord = $originalInstallerRecordText | ConvertFrom-Json
    $coercedInstallerRecord.installer_size_bytes = [string]$coercedInstallerRecord.installer_size_bytes
    Write-Utf8 $result.RecordPath ($coercedInstallerRecord | ConvertTo-Json -Depth 5)
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenInstallerRecord $result.RecordPath $openedFrozen $fixtureBundle $result.InstallerPath } 'must be an integer'
    Write-Utf8 $result.RecordPath $originalInstallerRecordText
    $tamperedInstallerRecord = $originalInstallerRecordText | ConvertFrom-Json
    $tamperedInstallerRecord.bundle_inventory_sha256 = ('0' * 64)
    Write-Utf8 $result.RecordPath ($tamperedInstallerRecord | ConvertTo-Json -Depth 5)
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenInstallerRecord $result.RecordPath $openedFrozen $fixtureBundle $result.InstallerPath } 'does not bind'
    Write-Utf8 $result.RecordPath $originalInstallerRecordText

    $tamperedInstallerRecord = $originalInstallerRecordText | ConvertFrom-Json
    $tamperedInstallerRecord.local_test_token = ('a' * 31)
    Write-Utf8 $result.RecordPath ($tamperedInstallerRecord | ConvertTo-Json -Depth 5)
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenInstallerRecord $result.RecordPath $openedFrozen $fixtureBundle $result.InstallerPath } 'invalid installer or local-test identity'
    Write-Utf8 $result.RecordPath $originalInstallerRecordText

    $tamperedInstallerRecord = $originalInstallerRecordText | ConvertFrom-Json
    $tamperedInstallerRecord.local_test_token = ('b' * 32)
    $tamperedInstallerRecord.install_relative_path = "Scribe/LOCAL-Frozen-Test/$($tamperedInstallerRecord.local_test_token)"
    Write-Utf8 $result.RecordPath ($tamperedInstallerRecord | ConvertTo-Json -Depth 5)
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenInstallerRecord $result.RecordPath $openedFrozen $fixtureBundle $result.InstallerPath } 'invalid installer or local-test identity'
    Write-Utf8 $result.RecordPath $originalInstallerRecordText

    $installedParityFixture = Join-Path $testRoot 'installed-parity-fixture'
    Copy-Item -LiteralPath $bundleRoot -Destination $installedParityFixture -Recurse
    [IO.File]::WriteAllBytes((Join-Path $installedParityFixture 'unins000.exe'), [byte[]](0x4D, 0x5A))
    [IO.File]::WriteAllBytes((Join-Path $installedParityFixture 'unins000.dat'), [byte[]](1, 2, 3))
    $referenceParityBundle = Assert-WindowsLocalFrozenBundle $bundleRoot
    $installedParityBundle = Assert-WindowsLocalFrozenPayloadParity $bundleRoot $installedParityFixture
    Assert-Equal $installedParityBundle.Root ([IO.Path]::GetFullPath($installedParityFixture)) 'Payload parity descriptor resolves the installed root'
    Assert-Equal $installedParityBundle.InventorySha256 $referenceParityBundle.InventorySha256 'Payload parity descriptor retains the verified inventory hash'
    Assert-Equal $installedParityBundle.InventoryEntries.Count $referenceParityBundle.InventoryEntries.Count 'Payload parity descriptor retains the verified inventory entries'
    $parityReadme = Join-Path $installedParityFixture 'README.txt'
    [IO.File]::WriteAllText($parityReadme, 'tampered installed payload', [Text.UTF8Encoding]::new($false))
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenPayloadParity $bundleRoot $installedParityFixture } 'payload parity mismatch'
    $missingParityFixture = Join-Path $testRoot 'missing-parity-fixture'
    Copy-Item -LiteralPath $bundleRoot -Destination $missingParityFixture -Recurse
    [IO.File]::WriteAllBytes((Join-Path $missingParityFixture 'unins000.exe'), [byte[]](0x4D, 0x5A))
    [IO.File]::WriteAllBytes((Join-Path $missingParityFixture 'unins000.dat'), [byte[]](1, 2, 3))
    Remove-Item -LiteralPath (Join-Path $missingParityFixture 'README.txt')
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenPayloadParity $bundleRoot $missingParityFixture } 'payload differs from its exact declared inventory'
    $unexpectedParityFixture = Join-Path $testRoot 'unexpected-parity-fixture'
    Copy-Item -LiteralPath $bundleRoot -Destination $unexpectedParityFixture -Recurse
    [IO.File]::WriteAllBytes((Join-Path $unexpectedParityFixture 'unins000.exe'), [byte[]](0x4D, 0x5A))
    [IO.File]::WriteAllBytes((Join-Path $unexpectedParityFixture 'unins000.dat'), [byte[]](1, 2, 3))
    [IO.File]::WriteAllBytes((Join-Path $unexpectedParityFixture 'unexpected.bin'), [byte[]](4))
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenPayloadParity $bundleRoot $unexpectedParityFixture } 'payload differs from its exact declared inventory'

    $validSmokeDiagnostics = [pscustomobject]@{
        cancellation_verified = $true
        capabilities = [pscustomobject]@{ cancellation = $true }
        detected_architecture = 'whisper'
    }
    Assert-WindowsLocalFrozenSmokeDiagnostics $validSmokeDiagnostics
    Invoke-ExpectedFailure {
        Assert-WindowsLocalFrozenSmokeDiagnostics ([pscustomobject]@{
            cancellation_verified = $true
            capabilities = [pscustomobject]@{}
            detected_architecture = 'whisper'
        })
    } 'cancellation contract'
    Invoke-ExpectedFailure {
        Assert-WindowsLocalFrozenSmokeDiagnostics ([pscustomobject]@{
            cancellation_verified = $false
            capabilities = [pscustomobject]@{ cancellation = $true }
            detected_architecture = 'whisper'
        })
    } 'cancellation contract'
    Invoke-ExpectedFailure {
        Assert-WindowsLocalFrozenSmokeDiagnostics ([pscustomobject]@{
            cancellation_verified = $true
            capabilities = [pscustomobject]@{ cancellation = $false }
            detected_architecture = 'whisper'
        })
    } 'cancellation contract'
    Invoke-ExpectedFailure {
        Assert-WindowsLocalFrozenSmokeDiagnostics ([pscustomobject]@{
            cancellation_verified = 'false'
            capabilities = [pscustomobject]@{ cancellation = $true }
            detected_architecture = 'whisper'
        })
    } 'cancellation contract'
    Invoke-ExpectedFailure {
        Assert-WindowsLocalFrozenSmokeDiagnostics ([pscustomobject]@{
            cancellation_verified = $true
            capabilities = [pscustomobject]@{ cancellation = 1 }
            detected_architecture = 'whisper'
        })
    } 'cancellation contract'

    # This fixture deterministically recreates deletion at the production
    # metadata-observation boundary. The one-call reader must classify that
    # disappearance and still scan every surviving ancestor.
    $removalRaceRoot = Join-Path $testRoot 'metadata-disappearance-race'
    New-Item -ItemType Directory -Path $removalRaceRoot | Out-Null
    $savedRemovalPathMetadata = (Get-Command Get-WindowsLocalFrozenRemovalPathMetadata -CommandType Function).ScriptBlock
    $script:RemovalRacePath = $removalRaceRoot
    $script:RemovalRaceInjected = $false
    $script:RemovalRaceObservedPaths = [System.Collections.Generic.List[string]]::new()
    try {
        function Get-WindowsLocalFrozenRemovalPathMetadata([string]$Path) {
            $script:RemovalRaceObservedPaths.Add($Path)
            if (-not $script:RemovalRaceInjected -and
                [string]::Equals($Path, $script:RemovalRacePath, [StringComparison]::OrdinalIgnoreCase)) {
                $script:RemovalRaceInjected = $true
                Remove-Item -LiteralPath $Path -Force
            }
            return [IO.File]::GetAttributes($Path)
        }
        Wait-WindowsLocalFrozenInstallRootRemoved $removalRaceRoot 1000
        Assert-True $script:RemovalRaceInjected 'Removal-race fixture did not reach the metadata disappearance boundary.'
        Assert-True (-not (Test-Path -LiteralPath $removalRaceRoot)) 'Removal-race fixture did not remove the installation leaf.'
        Assert-FixtureRemovalMetadataScannedAncestors $removalRaceRoot @($script:RemovalRaceObservedPaths)
    }
    finally {
        Set-Item -Path Function:Get-WindowsLocalFrozenRemovalPathMetadata -Value $savedRemovalPathMetadata
    }

    foreach ($timeout in @(0, -1, 30001)) {
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $removalRaceRoot $timeout } 'timeout is outside the supported bounds'
    }

    $alreadyAbsentRemovalRoot = Join-Path $testRoot 'already-absent-uninstall-removal'
    Wait-WindowsLocalFrozenInstallRootRemoved $alreadyAbsentRemovalRoot 1000
    Assert-True (-not (Test-Path -LiteralPath $alreadyAbsentRemovalRoot)) 'Already-absent installation leaf was unexpectedly recreated.'

    Add-Type -TypeDefinition @'
using System;
using System.IO;
public sealed class FixtureRemovalFileNotFoundException : FileNotFoundException {
    public FixtureRemovalFileNotFoundException(int hresult) : base("fixture missing file") { HResult = hresult; }
}
public sealed class FixtureRemovalDirectoryNotFoundException : DirectoryNotFoundException {
    public FixtureRemovalDirectoryNotFoundException(int hresult) : base("fixture missing directory") { HResult = hresult; }
}
public sealed class FixtureRemovalIOException : IOException {
    public FixtureRemovalIOException(int hresult) : base("fixture unrelated IO failure") { HResult = hresult; }
}
public sealed class FixtureRemovalUnauthorizedAccessException : UnauthorizedAccessException {
    public FixtureRemovalUnauthorizedAccessException(int hresult) : base("fixture access denied") { HResult = hresult; }
}
'@

    $savedRemovalPathMetadata = (Get-Command Get-WindowsLocalFrozenRemovalPathMetadata -CommandType Function).ScriptBlock
    $script:FixtureRemovalMetadataOutcomes = @{}
    $script:FixtureRemovalMetadataObserved = [System.Collections.Generic.List[string]]::new()
    try {
        function Get-WindowsLocalFrozenRemovalPathMetadata([string]$Path) {
            $script:FixtureRemovalMetadataObserved.Add($Path)
            if ($script:FixtureRemovalMetadataOutcomes.ContainsKey($Path)) {
                $outcome = $script:FixtureRemovalMetadataOutcomes[$Path]
                if ($outcome -is [System.Exception]) {
                    throw $outcome
                }
                return [System.IO.FileAttributes]$outcome
            }
            return [System.IO.FileAttributes]::Directory
        }

        $syntheticLeaf = Join-Path $testRoot 'metadata-synthetic-leaf'
        $script:FixtureRemovalMetadataOutcomes = @{
            $syntheticLeaf = [FixtureRemovalFileNotFoundException]::new(-2147024894)
        }
        $script:FixtureRemovalMetadataObserved.Clear()
        Wait-WindowsLocalFrozenInstallRootRemoved $syntheticLeaf 1000
        Assert-FixtureRemovalMetadataScannedAncestors $syntheticLeaf @($script:FixtureRemovalMetadataObserved)

        $missingIntermediate = Join-Path $testRoot 'metadata-missing-intermediate'
        $missingIntermediateLeaf = Join-Path $missingIntermediate 'leaf'
        $script:FixtureRemovalMetadataOutcomes = @{
            $missingIntermediateLeaf = [FixtureRemovalDirectoryNotFoundException]::new(-2147024893)
            $missingIntermediate = [FixtureRemovalFileNotFoundException]::new(-2147024894)
        }
        $script:FixtureRemovalMetadataObserved.Clear()
        Wait-WindowsLocalFrozenInstallRootRemoved $missingIntermediateLeaf 1000
        Assert-FixtureRemovalMetadataScannedAncestors $missingIntermediateLeaf @($script:FixtureRemovalMetadataObserved)

        $persistentRemovalRoot = Join-Path $testRoot 'persistent-uninstall-removal'
        $script:FixtureRemovalMetadataOutcomes = @{
            $persistentRemovalRoot = [System.IO.FileAttributes]::Directory
        }
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $persistentRemovalRoot 40 } 'left its token-bound program directory'

        $noSurvivorRoot = Join-Path $testRoot 'missing-without-surviving-ancestor'
        $noSurvivorOutcomes = @{}
        $noSurvivorCurrent = [IO.Path]::GetFullPath($noSurvivorRoot)
        while ($true) {
            $noSurvivorOutcomes[$noSurvivorCurrent] = [FixtureRemovalFileNotFoundException]::new(-2147024894)
            $noSurvivorParent = Split-Path -Parent $noSurvivorCurrent
            if ([string]::IsNullOrEmpty($noSurvivorParent) -or
                [string]::Equals($noSurvivorParent, $noSurvivorCurrent, [StringComparison]::OrdinalIgnoreCase)) {
                break
            }
            $noSurvivorCurrent = $noSurvivorParent
        }
        $script:FixtureRemovalMetadataOutcomes = $noSurvivorOutcomes
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $noSurvivorRoot 40 } 'left its token-bound program directory'

        $fileReplacement = Join-Path $testRoot 'metadata-file-replacement'
        $script:FixtureRemovalMetadataOutcomes = @{
            $fileReplacement = [System.IO.FileAttributes]::Archive
        }
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $fileReplacement 1000 } 'non-directory replacement'

        $missingWithFileAncestor = Join-Path $testRoot 'metadata-file-ancestor'
        $missingWithFileLeaf = Join-Path $missingWithFileAncestor 'leaf'
        $script:FixtureRemovalMetadataOutcomes = @{
            $missingWithFileLeaf = [FixtureRemovalDirectoryNotFoundException]::new(-2147024893)
            $missingWithFileAncestor = [System.IO.FileAttributes]::Archive
        }
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $missingWithFileLeaf 1000 } 'non-directory replacement'

        $missingWithReparseAncestor = Join-Path $testRoot 'metadata-reparse-ancestor'
        $missingWithReparseLeaf = Join-Path $missingWithReparseAncestor 'leaf'
        $script:FixtureRemovalMetadataOutcomes = @{
            $missingWithReparseLeaf = [FixtureRemovalDirectoryNotFoundException]::new(-2147024893)
            $missingWithReparseAncestor = [System.IO.FileAttributes]::Directory -bor [System.IO.FileAttributes]::ReparsePoint
        }
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $missingWithReparseLeaf 1000 } 'cannot cross a symbolic link or reparse point'

        $accessDeniedRoot = Join-Path $testRoot 'metadata-access-denied'
        $script:FixtureRemovalMetadataOutcomes = @{
            $accessDeniedRoot = [FixtureRemovalUnauthorizedAccessException]::new(-2147024891)
        }
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $accessDeniedRoot 1000 } 'metadata inspection failed'

        $unrelatedIoRoot = Join-Path $testRoot 'metadata-unrelated-io'
        $script:FixtureRemovalMetadataOutcomes = @{
            $unrelatedIoRoot = [FixtureRemovalIOException]::new(-2147024894)
        }
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $unrelatedIoRoot 1000 } 'metadata inspection failed'

        $wrongMissingCodeRoot = Join-Path $testRoot 'metadata-wrong-missing-code'
        $script:FixtureRemovalMetadataOutcomes = @{
            $wrongMissingCodeRoot = [FixtureRemovalFileNotFoundException]::new(-2147024893)
        }
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $wrongMissingCodeRoot 1000 } 'metadata inspection failed'

        $wrongFacilityMissingRoot = Join-Path $testRoot 'metadata-wrong-facility-missing'
        $script:FixtureRemovalMetadataOutcomes = @{
            $wrongFacilityMissingRoot = [FixtureRemovalFileNotFoundException]::new(-1878589438)
        }
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $wrongFacilityMissingRoot 1000 } 'metadata inspection failed'
    }
    finally {
        Set-Item -Path Function:Get-WindowsLocalFrozenRemovalPathMetadata -Value $savedRemovalPathMetadata
    }

    $junctionFixtureRoot = Join-Path $testRoot 'removal-junctions'
    $junctionTarget = Join-Path $junctionFixtureRoot 'target'
    $junctionTargetLeaf = Join-Path $junctionTarget 'leaf'
    $junctionLeaf = Join-Path $junctionFixtureRoot 'leaf-junction'
    $junctionAncestor = Join-Path $junctionFixtureRoot 'ancestor-junction'
    $junctionAncestorLeaf = Join-Path $junctionAncestor 'leaf'
    $danglingJunction = Join-Path $junctionFixtureRoot 'dangling-junction'
    New-Item -ItemType Directory -Path $junctionTarget | Out-Null
    try {
        New-Item -ItemType Junction -Path $junctionLeaf -Target $junctionTarget | Out-Null
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $junctionLeaf 1000 } 'cannot cross a symbolic link or reparse point'
        Remove-Item -LiteralPath $junctionLeaf -Force

        New-Item -ItemType Junction -Path $junctionAncestor -Target $junctionTarget | Out-Null
        New-Item -ItemType Directory -Path $junctionAncestorLeaf | Out-Null
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $junctionAncestorLeaf 1000 } 'cannot cross a symbolic link or reparse point'
        Remove-Item -LiteralPath $junctionAncestor -Force

        Remove-Item -LiteralPath $junctionTargetLeaf -Force
        New-Item -ItemType Junction -Path $danglingJunction -Target $junctionTarget | Out-Null
        Remove-Item -LiteralPath $junctionTarget -Force
        Invoke-ExpectedFailure { Wait-WindowsLocalFrozenInstallRootRemoved $danglingJunction 1000 } 'cannot cross a symbolic link or reparse point'
    }
    finally {
        Remove-Item -LiteralPath $junctionLeaf -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $junctionAncestor -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $danglingJunction -Force -ErrorAction SilentlyContinue
    }

    $delayedRemovalRoot = Join-Path $testRoot 'delayed-uninstall-removal'
    $delayedRemovalReady = Join-Path $testRoot 'delayed-uninstall-ready'
    New-Item -ItemType Directory -Path $delayedRemovalRoot | Out-Null
    $delayedRemovalJob = Start-Job -ScriptBlock {
        param([string]$Path, [string]$ReadyPath)
        [IO.File]::WriteAllText($ReadyPath, 'ready')
        Start-Sleep -Milliseconds 250
        Remove-Item -LiteralPath $Path
    } -ArgumentList $delayedRemovalRoot, $delayedRemovalReady
    try {
        $jobStartDeadline = [System.Diagnostics.Stopwatch]::StartNew()
        while (-not (Test-Path -LiteralPath $delayedRemovalReady -PathType Leaf)) {
            if ($jobStartDeadline.ElapsedMilliseconds -ge 5000) {
                throw 'Delayed local frozen uninstaller-removal fixture did not become ready within five seconds.'
            }
            Start-Sleep -Milliseconds 25
        }
        Wait-WindowsLocalFrozenInstallRootRemoved $delayedRemovalRoot 2000
        Assert-True (-not (Test-Path -LiteralPath $delayedRemovalRoot)) 'Bounded local frozen uninstaller removal wait returned before delayed deletion completed.'
    }
    finally {
        if ($delayedRemovalJob.State -eq 'Running') {
            Stop-Job -Job $delayedRemovalJob
        }
        $null = Wait-Job -Job $delayedRemovalJob -Timeout 5
        Receive-Job -Job $delayedRemovalJob | Out-Null
        Remove-Job -Job $delayedRemovalJob -Force
    }

    $verifierCampaignSource = Get-Content -LiteralPath (Join-Path $repositoryRoot 'scripts\verify-windows-local-frozen-test-installer.ps1') -Raw
    $integrityCampaignSource = Get-Content -LiteralPath (Join-Path $repositoryRoot 'scripts\windows-local-frozen-installer-integrity.ps1') -Raw
    Assert-True ($verifierCampaignSource.Contains("[ValidateSet('ac', 'battery')]")) 'Campaign power parameter is not restricted to the two explicit sources.'
    Assert-True ($verifierCampaignSource.Contains("'--campaign-power', `$observationRequest.CampaignPower")) 'Campaign invocation does not append the exact requested power argument.'
    Assert-True ($verifierCampaignSource.Contains('-TimeoutMilliseconds 900000')) 'Campaign invocation does not retain the fixed fifteen-minute deadline.'
    Assert-True ($integrityCampaignSource.Contains('262144')) 'Campaign process does not retain the fixed per-stream character cap.'
    Assert-True ($integrityCampaignSource.Contains('Read-WindowsLocalFrozenCaptureCampaignReport')) 'Campaign report has no separate strict validation entry point.'

    $existingOutput = Join-Path $testRoot 'existing-output'
    New-Item -ItemType Directory -Path $existingOutput | Out-Null
    Invoke-ExpectedFailure { Invoke-Builder $bundleRoot $existingOutput $fakeCompiler } 'already exists'
    Invoke-ExpectedFailure { Invoke-Builder $bundleRoot (Join-Path $bundleRoot 'overlap') $fakeCompiler } 'cannot overlap'

    $tamperedBundle = Copy-Bundle 'tampered-cpu-bundle'
    $tamperedWorker = Join-Path $tamperedBundle 'scribe-inference-worker.exe'
    $tamperedBytes = [IO.File]::ReadAllBytes($tamperedWorker); $tamperedBytes[900] = $tamperedBytes[900] -bxor 1; [IO.File]::WriteAllBytes($tamperedWorker, $tamperedBytes)
    Refresh-BundleInventory $tamperedBundle
    Invoke-ExpectedFailure { Invoke-Builder $tamperedBundle (Join-Path $testRoot 'tampered-output') $fakeCompiler } 'does not preserve'

    $wrongCompiler = Join-Path $compilerRoot 'wrong-ISCC.exe'
    Copy-Item -LiteralPath $fakeCompiler -Destination $wrongCompiler
    Invoke-ExpectedFailure { Invoke-Builder $bundleRoot (Join-Path $testRoot 'wrong-compiler-output') $wrongCompiler } 'reviewed ISCC.exe filename'

    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'fail'
    Invoke-ExpectedFailure { Invoke-Builder $bundleRoot (Join-Path $testRoot 'compiler-failure-output') $fakeCompiler } 'compilation failed'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $testRoot 'compiler-failure-output'))) 'Failed local frozen compiler run left final output.'

    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'hang'
    $hungOutput = Join-Path $testRoot 'compiler-hang-output'
    Invoke-ExpectedFailure { Invoke-Builder $bundleRoot $hungOutput $fakeCompiler } 'Pinned Inno Setup compilation timed out after 1000 milliseconds'
    Assert-True (-not (Test-Path -LiteralPath $hungOutput)) 'Hung local compiler left final output.'
    Assert-Equal (@(Get-ChildItem -LiteralPath $testRoot -Directory -Filter 'compiler-hang-output.staging-*').Count) 0 'Hung compiler staging cleanup'
    # Reusing the same destination also proves cleanup released retained input
    # handles and did not leave a stale staging sibling blocking retry.
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = ''
    $afterHang = @(Invoke-Builder $bundleRoot $hungOutput $fakeCompiler)[0]
    Assert-True (Test-Path -LiteralPath $afterHang.InstallerPath -PathType Leaf) 'Normal build after compiler timeout did not succeed.'

    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'mutate-staged'
    $stagedCapture = Join-Path $testRoot 'staged-mutation-arguments.txt'
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE = $stagedCapture
    $stagedMutationResult = @(Invoke-Builder $bundleRoot (Join-Path $testRoot 'staged-mutation-output') $fakeCompiler)[0]
    Assert-True ((Get-Content -LiteralPath $stagedCapture -Raw).Contains('mutation=blocked')) 'Staged bundle read handles did not block compiler-time payload mutation.'
    Assert-True (Test-Path -LiteralPath $stagedMutationResult.InstallerPath) 'Blocked staged mutation prevented valid local installer publication.'

    $driftBundle = Copy-Bundle 'source-drift-bundle'
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'mutate-source'
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_SOURCE_MUTATION = Join-Path $driftBundle 'README.txt'
    Invoke-ExpectedFailure { Invoke-Builder $driftBundle (Join-Path $testRoot 'source-drift-output') $fakeCompiler } 'revalidation failed after compilation; refusing to publish'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $testRoot 'source-drift-output'))) 'Source-drift failure left final installer output.'
}
finally {
    $env:CI = $previousCi
    $env:GITHUB_ACTIONS = $previousGitHubActions
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = $previousMode
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE = $previousCapture
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_SOURCE_MUTATION = $previousSourceMutation
    foreach ($entry in @(Get-ChildItem Env:GIT_*)) { Remove-Item -LiteralPath "Env:$($entry.Name)" }
    foreach ($name in $previousGitEnvironment.Keys) { Set-Item -LiteralPath "Env:$name" -Value $previousGitEnvironment[$name] }
    Remove-TestRootSafely $testRoot
}

Write-Output "Windows local frozen test-installer tests passed ($script:Assertions assertions; real ISCC/install/uninstall is a separate manual gate)."
