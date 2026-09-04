[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$ToolPath = Join-Path $PSScriptRoot 'qualify-windows-gpu-evidence.ps1'
$AutoManifestPath = Join-Path $RepositoryRoot 'runtime-manifests\gpu-auto-qualification-windows-x64.json'
$AuthorityPath = Join-Path $RepositoryRoot 'runtime-manifests\windows-gpu-qualification-production-authority.json'
$CheckedPlanPath = Join-Path $RepositoryRoot 'runtime-manifests\windows-gpu-qualification-plan-x64.json'
$ToolchainPath = Join-Path $RepositoryRoot 'runtime-manifests\gpu-worker-toolchain-windows-x64.json'
$ExpectedAuto = '{"schema_version":2,"mode":"default_deny","target_os":"windows","target_arch":"x86_64","entries":[]}' + "`n"
$ExpectedAuthority = '{"approved_plans":[],"kind":"windows_gpu_qualification_production_authority","schema_version":2}' + "`n"
$RequiredScenarios = @('clean_installer', 'device_loss', 'disabled_device', 'driver_change', 'insufficient_vram', 'mixed_gpu', 'power_ac', 'power_battery', 'suspend_resume')
$ZeroSha256 = '0' * 64
$Utf8 = [Text.UTF8Encoding]::new($false, $true)
$AttestationDomain = [Text.Encoding]::ASCII.GetBytes("SCRIBE-WINDOWS-GPU-QUALIFICATION-LANE-ATTESTATION-V1`0")
$CaseCounter = 0
$FixtureKey = [Security.Cryptography.ECDsa]::Create([Security.Cryptography.ECCurve+NamedCurves]::nistP256)
$FixtureSpki = $FixtureKey.ExportSubjectPublicKeyInfo()
$FixtureSpkiBase64 = [Convert]::ToBase64String($FixtureSpki)
$FixtureKeyId = 'p256:' + [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($FixtureSpki)).ToLowerInvariant()

function Assert-True([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Get-Digest([string]$Label) { [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::ASCII.GetBytes($Label))).ToLowerInvariant() }
function Get-FileDigest([string]$Path) { (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant() }

function ConvertTo-SortedNode($Value) {
    if ($Value -is [Collections.IDictionary]) {
        $result = [ordered]@{}
        foreach ($key in @($Value.Keys | Sort-Object -CaseSensitive)) { $result[$key] = ConvertTo-SortedNode $Value[$key] }
        return $result
    }
    if ($Value -is [Collections.IList] -and $Value -isnot [string]) { return ,([object[]]@($Value | ForEach-Object { ConvertTo-SortedNode $_ })) }
    return $Value
}

function Get-CanonicalBytes($Value) {
    # Match System.Text.Json's default encoder used by the evaluator. Base64
    # may contain '+', which that encoder emits as \u002B.
    $json = (ConvertTo-SortedNode $Value) | ConvertTo-Json -Compress -Depth 64
    return ,$Utf8.GetBytes($json.Replace('+', '\u002B') + "`n")
}
function Get-CanonicalDigest($Value) { [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData((Get-CanonicalBytes $Value))).ToLowerInvariant() }
function Write-Canonical([string]$Path, $Value) { [IO.File]::WriteAllBytes($Path, (Get-CanonicalBytes $Value)) }
function Copy-Document($Value) { $Utf8.GetString((Get-CanonicalBytes $Value)) | ConvertFrom-Json -AsHashtable -Depth 64 }

function New-ScifFrame($Control) {
    [byte[]]$body = $Utf8.GetBytes(($Control | ConvertTo-Json -Compress -Depth 64))
    [byte[]]$frame = [byte[]]::new(26 + $body.Length)
    [Array]::Copy([Text.Encoding]::ASCII.GetBytes('SCIF'), 0, $frame, 0, 4)
    $frame[4] = 5; $frame[5] = 1
    [Array]::Copy([BitConverter]::GetBytes([UInt32]$body.Length), 0, $frame, 6, 4)
    [Array]::Copy([BitConverter]::GetBytes([UInt64]0), 0, $frame, 10, 8)
    [Array]::Copy([BitConverter]::GetBytes([UInt64]0), 0, $frame, 18, 8)
    [Array]::Copy($body, 0, $frame, 26, $body.Length)
    return [Convert]::ToBase64String($frame)
}

function Get-ScifControl([string]$Base64) {
    [byte[]]$frame = [Convert]::FromBase64String($Base64)
    [int]$length = [BitConverter]::ToUInt32($frame, 6)
    return $Utf8.GetString($frame, 26, $length) | ConvertFrom-Json -AsHashtable -Depth 64
}

function Get-NoncanonicalBase64([string]$Value) {
    $alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
    $padding = if ($Value.EndsWith('==')) { 2 } elseif ($Value.EndsWith('=')) { 1 } else { 0 }
    if ($padding -eq 0) { return $Value + "`n" }
    $position = $Value.Length - $padding - 1; $index = $alphabet.IndexOf($Value[$position])
    $replacement = if ($padding -eq 2) { ($index -band 0x30) -bor 1 } else { ($index -band 0x3c) -bor 1 }
    return $Value.Substring(0, $position) + $alphabet[$replacement] + $Value.Substring($position + 1)
}

function Update-CaptureControl($Capture, [string]$Which, [scriptblock]$Mutation) {
    $field = if ($Which -ceq 'request') { 'request_frame_base64' } else { 'response_frame_base64' }
    $control = Get-ScifControl $Capture[$field]
    & $Mutation $control
    $Capture[$field] = New-ScifFrame $control
}

function Update-CaptureFrameBytes($Capture, [string]$Which, [scriptblock]$Mutation) {
    $field = if ($Which -ceq 'request') { 'request_frame_base64' } else { 'response_frame_base64' }
    [byte[]]$frame = [Convert]::FromBase64String($Capture[$field])
    & $Mutation $frame
    $Capture[$field] = [Convert]::ToBase64String($frame)
}

function Set-CaptureBodyText($Capture, [string]$Which, [string]$Body) {
    [byte[]]$bodyBytes = $Utf8.GetBytes($Body); [byte[]]$frame = [byte[]]::new(26 + $bodyBytes.Length)
    [Array]::Copy([Text.Encoding]::ASCII.GetBytes('SCIF'), 0, $frame, 0, 4); $frame[4] = 5; $frame[5] = 1
    [Array]::Copy([BitConverter]::GetBytes([UInt32]$bodyBytes.Length), 0, $frame, 6, 4); [Array]::Copy($bodyBytes, 0, $frame, 26, $bodyBytes.Length)
    $field = if ($Which -ceq 'request') { 'request_frame_base64' } else { 'response_frame_base64' }; $Capture[$field] = [Convert]::ToBase64String($frame)
}

function New-Worker([string]$Backend, [string]$Label, [string]$Build) {
    return [ordered]@{ backend = $Backend; protocol_version = 5; provider_id = $Backend; runtime_abi = 1; worker_build_id = $Build; worker_sha256 = Get-Digest "$Label-worker" }
}

function New-FixtureIdentity() {
    $revision = (Get-Digest 'fixture-build-revision').Substring(0, 40)
    $appBuild = "local-transcriber@0.1.0#$revision"
    $workerBuild = "scribe-inference-worker@0.1.0#$revision"
    $stable = 'native:0000:01:00.0'
    $driver = 'windows-display:32.0.16.1088'
    $identity = [ordered]@{
        acquisition = [ordered]@{
            batch_id = 'fixture-batch-001'
            controls = [ordered]@{ background_load_policy = 'isolated'; gpu_power_profile = 'fixed_maximum_performance'; power_plan_sha256 = Get-Digest 'balanced-ac-plan'; power_source = 'ac'; thermal_policy = 'no_throttling_observed' }
            device_set = [ordered]@{
                device_count = 3
                devices = @(
                    [ordered]@{ device_class = 'discrete_gpu'; driver = $driver; provider_eligible = $true; stable_device_id = $stable; total_memory_bytes = [Int64]12000000000; vendor = 'nvidia' },
                    [ordered]@{ device_class = 'discrete_gpu'; driver = $driver; provider_eligible = $true; stable_device_id = 'native:0000:02:00.0'; total_memory_bytes = [Int64]10000000000; vendor = 'nvidia' },
                    [ordered]@{ device_class = 'integrated_gpu'; driver = 'none'; provider_eligible = $false; stable_device_id = 'native:luid:0000000000000002'; total_memory_bytes = [Int64]8000000000; vendor = 'intel' }
                )
                mixed_gpu = $true
                snapshot_sha256 = $ZeroSha256
            }
            host = [ordered]@{ cpu_arch = 'x86_64'; cpu_model_sha256 = Get-Digest 'cpu-model'; logical_cpus = 16; physical_cores = 8; total_memory_bytes = [Int64]32000000000 }
            machine_id_sha256 = Get-Digest 'machine'
            options_sha256 = Get-Digest 'model-options'
            ordering = [ordered]@{ scheme = 'paired_alternating_cpu_first_v1'; warm_priming_runs = 1 }
            protocol = [ordered]@{ harness_sha256 = Get-Digest 'windows-qualification-harness'; protocol_id = 'scribe-windows-gpu-qualification'; protocol_version = 1 }
            telemetry = [ordered]@{ sample_interval_ms = 100; scope = 'worker_process_and_selected_device'; source = 'windows_counters_and_provider' }
            threading = [ordered]@{ cpu_affinity_sha256 = Get-Digest 'cpu-affinity'; cpu_worker_threads = 8; gpu_affinity_sha256 = Get-Digest 'gpu-affinity'; gpu_worker_threads = 4 }
        }
        app_build_id = $appBuild
        backend = 'cuda'
        cpu_baseline = New-Worker 'cpu' 'fixture-cpu' $workerBuild
        device = [ordered]@{ device_class = 'discrete_gpu'; memory_model = 'dedicated_vram'; qualified_minimum_available_memory_bytes = [Int64]9000000000; qualified_minimum_total_memory_bytes = [Int64]12000000000; stable_device_id = $stable; total_memory_bytes = [Int64]12000000000; vendor = 'nvidia' }
        driver = [ordered]@{ kind = 'exact'; value = $driver }
        gpu_worker = New-Worker 'cuda' 'fixture-cuda' $workerBuild
        installation = [ordered]@{ catalog_sha256 = Get-Digest 'catalog'; clean_machine_image_sha256 = Get-Digest 'clean-windows-image'; package_kind = 'installer'; package_sha256 = Get-Digest 'installer' }
        lane_id = 'fixture-windows-nvidia-cuda'
        model = [ordered]@{ model_digest = Get-Digest 'model'; model_id = 'whisper-base-en-q8_0' }
        pack = [ordered]@{ pack_digest = Get-Digest 'pack'; pack_id = 'scribe-cuda-windows-x64'; pack_version = '0.1.0-fixture'; runtime_abi = 1; security_epoch = 1 }
        provider_id = 'transcribe-cpp-ggml-cuda'
        target_arch = 'x86_64'
        windows_version = '10.0.26100'
        workload = [ordered]@{ audio_sha256 = Get-Digest 'audio'; expected_transcript_sha256 = Get-Digest 'expected-transcript'; workload_id = 'fixture-english-30s' }
    }
    $identity.acquisition.device_set.snapshot_sha256 = Get-CanonicalDigest $identity.acquisition.device_set.devices
    return $identity
}

function New-Run($Identity, [string]$Mode, [string]$Target, [int]$Sequence, [int]$GpuWarmMs) {
    $endToEnd = if ($Mode -ceq 'cold') { if ($Target -ceq 'cpu') { 200 + $Sequence } else { 180 + $Sequence } } elseif ($Target -ceq 'cpu') { 100 } else { $GpuWarmMs }
    $worker = if ($Target -ceq 'cpu') { $Identity.cpu_baseline } else { $Identity.gpu_worker }
    $generation = if ($Mode -ceq 'cold') { "$($Identity.acquisition.batch_id):$($Identity.lane_id):$Mode`:$Target`:$('{0:d2}' -f $Sequence)" } else { "$($Identity.acquisition.batch_id):$($Identity.lane_id):warm:$Target" }
    return [ordered]@{
        acquisition_batch_id = $Identity.acquisition.batch_id; artifact_path = "$($Identity.lane_id)/runs/$Mode/$Target/$('{0:d2}' -f $Sequence).evidence"; artifact_sha256 = Get-Digest "pending-$Mode-$Target-$Sequence"
        available_device_memory_bytes_after = if ($Target -ceq 'cpu') { [Int64]0 } else { [Int64]8500000000 }; available_device_memory_bytes_before = if ($Target -ceq 'cpu') { [Int64]0 } else { [Int64]9000000000 }
        backend_ms = $endToEnd - 10; device_set_sha256 = $Identity.acquisition.device_set.snapshot_sha256; end_to_end_ms = $endToEnd
        execution = [ordered]@{
            backend = if ($Target -ceq 'cpu') { 'cpu' } else { $Identity.backend }; capture_sha256 = Get-Digest "capture-$generation"; device_memory_kind = if ($Target -ceq 'cpu') { 'none' } else { $Identity.device.memory_model }
            driver = if ($Target -ceq 'cpu') { 'cpu:none' } else { $Identity.driver.value }; model_digest = $Identity.model.model_digest; options_sha256 = $Identity.acquisition.options_sha256
            pack_digest = if ($Target -ceq 'cpu') { $ZeroSha256 } else { $Identity.pack.pack_digest }; protocol_version = $worker.protocol_version; provider_id = $worker.provider_id; runtime_abi = $worker.runtime_abi
            stable_device_id = if ($Target -ceq 'cpu') { 'cpu:host' } else { $Identity.device.stable_device_id }; windows_version = $Identity.windows_version; worker_build_id = $worker.worker_build_id; worker_generation = $generation; worker_sha256 = $worker.worker_sha256
        }
        failure_category = 'none'; machine_id_sha256 = $Identity.acquisition.machine_id_sha256; outcome = 'success'; pair_id = "$($Identity.acquisition.batch_id):$Mode`:$('{0:d2}' -f $Sequence)"
        pair_order = if ($Sequence % 2) { 'cpu_then_gpu' } else { 'gpu_then_cpu' }; peak_process_memory_bytes = [Int64](600000000 + $Sequence * 1024); peak_shared_device_memory_bytes = [Int64]0
        peak_vram_bytes = if ($Target -ceq 'cpu') { [Int64]0 } else { [Int64](800000000 + $Sequence * 2048) }; priming_runs = if ($Mode -ceq 'cold') { 0 } else { 1 }
        reset_state = if ($Mode -ceq 'cold') { 'fresh_process_fresh_model' } else { 'same_process_primed_model' }; sequence = $Sequence
        session_id = if ($Mode -ceq 'cold') { "$($Identity.acquisition.batch_id):$Mode`:$('{0:d2}' -f $Sequence):session" } else { "$($Identity.acquisition.batch_id):warm:session" }
        transcript_sha256 = $Identity.workload.expected_transcript_sha256
    }
}

function New-Scenario($Identity, [string]$Name) {
    $selectedBackend = if (@('device_loss', 'disabled_device', 'insufficient_vram', 'power_battery') -ccontains $Name) { 'cpu' } else { $Identity.backend }
    return [ordered]@{
        active_request_migrated = $false; artifact_path = "$($Identity.lane_id)/scenarios/$($Name.Replace('_', '-')).evidence"; artifact_sha256 = Get-Digest "pending-$Name"
        available_device_memory_bytes = if ($Name -ceq 'insufficient_vram') { [Int64]100000000 } else { [Int64]9000000000 }; capture_after_sha256 = $ZeroSha256; capture_before_sha256 = $ZeroSha256
        clean_machine = $Name -ceq 'clean_installer'; device_set_sha256 = $Identity.acquisition.device_set.snapshot_sha256; driver_after = $Identity.driver.value
        driver_before = if ($Name -ceq 'driver_change') { 'windows-display:32.0.16.1000' } else { $Identity.driver.value }
        observed_failure_category = if ($Name -ceq 'device_loss') { 'device_loss' } elseif (@('disabled_device', 'insufficient_vram') -ccontains $Name) { 'unavailable' } else { 'none' }
        package_sha256 = $Identity.installation.package_sha256; partial_output_replayed = $false; power_source = if ($Name -ceq 'power_battery') { 'battery' } else { 'ac' }
        process_index_after = -1; process_index_before = -1; recovered_next_request = $true; requested_mode = 'auto'; result = 'pass'; scenario = $Name
        selected_backend = $selectedBackend; selected_stable_device_id = if ($selectedBackend -ceq 'cpu') { 'cpu:host' } else { $Identity.device.stable_device_id }; selection_reevaluated = $true
    }
}

function Get-WirePackExpectation($Identity) {
    return [ordered]@{
        pack_id = $Identity.pack.pack_id; pack_version = $Identity.pack.pack_version; pack_digest = $Identity.pack.pack_digest
        security_epoch = $Identity.pack.security_epoch; runtime_abi = $Identity.pack.runtime_abi; backend = $Identity.backend; provider = $Identity.provider_id
    }
}

function Get-WireExpectation($Identity, [bool]$Cpu) {
    $worker = if ($Cpu) { $Identity.cpu_baseline } else { $Identity.gpu_worker }
    $value = [ordered]@{
        app_build = $Identity.app_build_id; worker_build = $worker.worker_build_id; bundled_worker_sha256 = $worker.worker_sha256
        abi = $worker.runtime_abi; role = 'inference'; provider = if ($Cpu) { 'cpu' } else { $Identity.backend }
    }
    if (-not $Cpu) { $value.pack = Get-WirePackExpectation $Identity }
    return $value
}

function New-WireDevice($Source, [int]$ProcessIndex) {
    return [ordered]@{
        stable_device_identity = $Source.stable_device_id; process_index = $ProcessIndex; display_name = "Fixture $($Source.vendor) adapter"
        driver_version = $Source.driver; device_class = $Source.device_class; vendor = $Source.vendor; memory_total_bytes = [Int64]$Source.total_memory_bytes
        memory_available_bytes = [Int64]([Math]::Min([Int64]9000000000, [Int64]$Source.total_memory_bytes))
    }
}

function New-Capture($Identity, [string]$Generation, [string]$Scope, [int]$SelectedProcessIndex = 3) {
    $cpu = $Scope -ceq 'cpu'
    $challenge = Get-Digest "challenge-$Generation"
    $expected = Get-WireExpectation $Identity $cpu
    $request = [ordered]@{ command = 'hello'; challenge = $challenge; expected = $expected }
    $capability = [ordered]@{
        challenge = $challenge; app_build = $expected.app_build; worker_build = $expected.worker_build; bundled_worker_sha256 = $expected.bundled_worker_sha256
        abi = $expected.abi; role = 'inference'; provider = $expected.provider
        artifacts = @([ordered]@{ artifact = 'gguf'; target = 'windows-x86_64' }, [ordered]@{ artifact = 'onnx_asr'; target = 'windows-x86_64' })
    }
    if (-not $cpu) {
        $providerDevices = @($Identity.acquisition.device_set.devices | Where-Object { $_.provider_eligible })
        [object[]]$wireDevices = if ($Scope -ceq 'provider_discovery') {
            @(
                for ($index = 0; $index -lt $providerDevices.Count; $index++) { New-WireDevice $providerDevices[$index] @(3, 9)[$index] }
            )
        }
        else {
            $selected = @($providerDevices | Where-Object { $_.stable_device_id -ceq $Identity.device.stable_device_id })[0]
            @(New-WireDevice $selected $SelectedProcessIndex)
        }
        $capability.pack = [ordered]@{ expectation = Get-WirePackExpectation $Identity; devices = $wireDevices }
    }
    $response = [ordered]@{ command = 'ready'; capability = $capability }
    return [ordered]@{
        artifact_path = "$($Identity.lane_id)/captures/$((Get-Digest $Generation).Substring(0, 24)).evidence"
        artifact_sha256 = Get-Digest "pending-capture-$Generation"; generation = $Generation; launch_scope = $Scope
        request_frame_base64 = New-ScifFrame $request; response_frame_base64 = New-ScifFrame $response
    }
}

function Get-RecordWithoutArtifact($Value) {
    $record = [ordered]@{}
    foreach ($key in $Value.Keys) { if (@('artifact_path', 'artifact_sha256') -cnotcontains $key) { $record[$key] = $Value[$key] } }
    return $record
}

function Sync-DeviceSetBindings($Documents) {
    $lane = $Documents.Evidence.lanes[0]
    $lane.identity.acquisition.device_set.snapshot_sha256 = Get-CanonicalDigest $lane.identity.acquisition.device_set.devices
    foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($lane.run_sets[$mode][$target])) { $run.device_set_sha256 = $lane.identity.acquisition.device_set.snapshot_sha256 } } }
    foreach ($scenario in @($lane.scenarios)) { $scenario.device_set_sha256 = $lane.identity.acquisition.device_set.snapshot_sha256 }
}

function Sync-Captures($Documents) {
    $lane = $Documents.Evidence.lanes[0]
    $identity = $lane.identity
    $captures = [Collections.Generic.List[object]]::new()
    foreach ($mode in @('cold', 'warm')) {
        foreach ($target in @('cpu', 'gpu')) {
            foreach ($generation in @($lane.run_sets[$mode][$target].execution.worker_generation | Sort-Object -Unique)) {
                $scope = if ($target -ceq 'cpu') { 'cpu' } else { 'selected_device' }
                $capture = New-Capture $identity $generation $scope
                $digest = Get-CanonicalDigest (Get-RecordWithoutArtifact $capture)
                foreach ($run in @($lane.run_sets[$mode][$target] | Where-Object { $_.execution.worker_generation -ceq $generation })) { $run.execution.capture_sha256 = $digest }
                $captures.Add($capture)
            }
        }
    }
    $discoveryGeneration = "$($identity.acquisition.batch_id):$($identity.lane_id):provider_discovery"
    $captures.Add((New-Capture $identity $discoveryGeneration 'provider_discovery'))
    $mixed = @($lane.scenarios | Where-Object { $_.scenario -ceq 'mixed_gpu' })[0]
    $before = New-Capture $identity "$($identity.acquisition.batch_id):$($identity.lane_id):scenario:mixed_gpu:before" 'selected_device' 3
    $after = New-Capture $identity "$($identity.acquisition.batch_id):$($identity.lane_id):scenario:mixed_gpu:after" 'selected_device' 11
    $mixed.capture_before_sha256 = Get-CanonicalDigest (Get-RecordWithoutArtifact $before)
    $mixed.capture_after_sha256 = Get-CanonicalDigest (Get-RecordWithoutArtifact $after)
    $mixed.process_index_before = 3; $mixed.process_index_after = 11
    $captures.Add($before); $captures.Add($after)
    $lane.captures = @($captures | Sort-Object generation)
}

function New-FixtureDocuments([int]$GpuWarmMs = 110) {
    $identity = New-FixtureIdentity
    $lane = [ordered]@{
        acquisition_artifact_path = "$($identity.lane_id)/acquisition.evidence"; acquisition_artifact_sha256 = Get-Digest 'pending-acquisition'
        artifact_inventory = @(); attestation = [ordered]@{}; captures = @(); identity = $identity
        run_sets = [ordered]@{
            cold = [ordered]@{ cpu = @(1..5 | ForEach-Object { New-Run $identity 'cold' 'cpu' $_ $GpuWarmMs }); gpu = @(1..5 | ForEach-Object { New-Run $identity 'cold' 'gpu' $_ $GpuWarmMs }) }
            warm = [ordered]@{ cpu = @(1..20 | ForEach-Object { New-Run $identity 'warm' 'cpu' $_ $GpuWarmMs }); gpu = @(1..20 | ForEach-Object { New-Run $identity 'warm' 'gpu' $_ $GpuWarmMs }) }
        }
        scenarios = @($RequiredScenarios | ForEach-Object { New-Scenario $identity $_ })
    }
    $plan = [ordered]@{
        capture_authority = [ordered]@{ campaign_nonce = Get-Digest 'fixture-campaign-001'; capture_key_id = $FixtureKeyId; fixture_capture_public_key_spki_base64 = $FixtureSpkiBase64 }
        capture_contract = [ordered]@{
            artifact_targets = @([ordered]@{ artifact = 'gguf'; target = 'windows-x86_64' }, [ordered]@{ artifact = 'onnx_asr'; target = 'windows-x86_64' })
            cold_captures_per_target = 5; control_kind = 1; header_bytes = 26; launch_scopes = @('cpu', 'provider_discovery', 'selected_device')
            max_control_body_bytes = 262144; protocol_magic = 'SCIF'; protocol_version = 5; request_id = 0; session_id = 0; warm_captures_per_target = 1
        }
        cold_runs = 5
        contract_bindings = [ordered]@{ auto_manifest_sha256 = Get-FileDigest $AutoManifestPath; evaluator_sha256 = Get-FileDigest $ToolPath; toolchain_contract_sha256 = Get-FileDigest $ToolchainPath }
        fixture_only = $true; kind = 'windows_gpu_release_qualification_plan'; maximum_gpu_p95_cpu_percent = 110
        required_lanes = @([ordered]@{ evidence_sha256 = Get-Digest 'pending-lane'; identity = $identity }); required_scenarios = $RequiredScenarios
        runtime_bucket_complete = $false; schema_version = 2; target_arch = 'x86_64'; target_os = 'windows'; warm_runs = 20
    }
    $evidence = [ordered]@{ fixture_only = $true; kind = 'windows_gpu_release_qualification_evidence'; lanes = @($lane); plan_sha256 = Get-Digest 'pending-plan'; schema_version = 2 }
    $documents = [pscustomobject]@{ Plan = $plan; Evidence = $evidence }
    Sync-Captures $documents
    return $documents
}

function Set-VulkanFixture($Documents, [string]$Vendor, [string]$VendorId) {
    $lane = $Documents.Evidence.lanes[0]; $identity = $lane.identity
    $identity.backend = 'vulkan'; $identity.provider_id = 'transcribe-cpp-ggml-vulkan'; $identity.gpu_worker.backend = 'vulkan'; $identity.gpu_worker.provider_id = 'vulkan'
    $identity.gpu_worker.worker_sha256 = Get-Digest 'fixture-vulkan-worker'; $identity.pack.pack_id = 'scribe-vulkan-windows-x64'; $identity.device.vendor = $Vendor
    $driver = "vulkan:$VendorId`:00000001:00000136:00112233445566778899aabbccddeeff"
    $identity.driver.value = $driver
    foreach ($device in @($identity.acquisition.device_set.devices | Where-Object { $_.provider_eligible })) { $device.vendor = $Vendor; $device.driver = $driver }
    foreach ($mode in @('cold', 'warm')) { foreach ($run in @($lane.run_sets[$mode].gpu)) { $run.execution.backend = 'vulkan'; $run.execution.provider_id = 'vulkan'; $run.execution.worker_sha256 = $identity.gpu_worker.worker_sha256; $run.execution.driver = $driver } }
    foreach ($scenario in @($lane.scenarios)) { if ($scenario.selected_backend -cne 'cpu') { $scenario.selected_backend = 'vulkan' }; $scenario.driver_after = $driver; $scenario.driver_before = if ($scenario.scenario -ceq 'driver_change') { "vulkan:$VendorId`:00000001:00000135:00112233445566778899aabbccddeeff" } else { $driver } }
    Sync-DeviceSetBindings $Documents; Sync-Captures $Documents
}

function Set-IntegratedFixture($Documents) {
    $lane = $Documents.Evidence.lanes[0]; $identity = $lane.identity
    $identity.device.device_class = 'integrated_gpu'; $identity.device.memory_model = 'shared_host_memory'; $identity.acquisition.device_set.devices[0].device_class = 'integrated_gpu'
    Sync-DeviceSetBindings $Documents
    foreach ($mode in @('cold', 'warm')) { foreach ($run in @($lane.run_sets[$mode].gpu)) { $run.execution.device_memory_kind = 'shared_host_memory'; $run.peak_shared_device_memory_bytes = $run.peak_vram_bytes; $run.peak_vram_bytes = [Int64]0 } }
    $battery = @($lane.scenarios | Where-Object { $_.scenario -ceq 'power_battery' })[0]; $battery.selected_backend = $identity.backend; $battery.selected_stable_device_id = $identity.device.stable_device_id
    Sync-Captures $Documents
}

function Write-Envelope([string]$Path, [string]$Kind, $Record) {
    [IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
    Write-Canonical $Path ([ordered]@{ kind = $Kind; record = $Record; schema_version = 1 })
    return Get-FileDigest $Path
}

function Get-ArtifactReferences($Lane) {
    $references = [Collections.Generic.List[object]]::new()
    $references.Add([ordered]@{ artifact_path = $Lane.acquisition_artifact_path; artifact_sha256 = $Lane.acquisition_artifact_sha256 })
    foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($Lane.run_sets[$mode][$target])) { $references.Add([ordered]@{ artifact_path = $run.artifact_path; artifact_sha256 = $run.artifact_sha256 }) } } }
    foreach ($scenario in @($Lane.scenarios)) { $references.Add([ordered]@{ artifact_path = $scenario.artifact_path; artifact_sha256 = $scenario.artifact_sha256 }) }
    foreach ($capture in @($Lane.captures)) { $references.Add([ordered]@{ artifact_path = $capture.artifact_path; artifact_sha256 = $capture.artifact_sha256 }) }
    [object[]]$ordered = $references.ToArray()
    $comparer = [Collections.Generic.Comparer[object]]::Create([Comparison[object]]{ param($left, $right) [StringComparer]::Ordinal.Compare([string]$left.artifact_path, [string]$right.artifact_path) })
    [Array]::Sort($ordered, $comparer)
    return ,$ordered
}

function Get-CaptureContractProjection($Plan, $Identity) {
    return [ordered]@{
        campaign_nonce = $Plan.capture_authority.campaign_nonce; capture_contract = $Plan.capture_contract; capture_key_id = $Plan.capture_authority.capture_key_id
        contract_bindings = $Plan.contract_bindings; lane_identity = $Identity
        required_lane_identities = @($Plan.required_lanes | ForEach-Object { $_.identity })
        policy = [ordered]@{ cold_runs = $Plan.cold_runs; fixture_only = $Plan.fixture_only; kind = $Plan.kind; maximum_gpu_p95_cpu_percent = $Plan.maximum_gpu_p95_cpu_percent; required_scenarios = $Plan.required_scenarios; runtime_bucket_complete = $Plan.runtime_bucket_complete; schema_version = $Plan.schema_version; target_arch = $Plan.target_arch; target_os = $Plan.target_os; warm_runs = $Plan.warm_runs }
        schema_version = 1
    }
}

function Get-UnsignedLane($Lane) {
    $unsigned = [ordered]@{}
    foreach ($key in $Lane.Keys) { if ($key -cne 'attestation') { $unsigned[$key] = $Lane[$key] } }
    return $unsigned
}

function New-Attestation($Plan, $Lane, [Security.Cryptography.ECDsa]$Key = $FixtureKey) {
    $record = [ordered]@{
        acquisition_batch_id = $Lane.identity.acquisition.batch_id; artifact_inventory_sha256 = Get-CanonicalDigest $Lane.artifact_inventory
        campaign_nonce = $Plan.capture_authority.campaign_nonce; capture_contract_sha256 = Get-CanonicalDigest (Get-CaptureContractProjection $Plan $Lane.identity)
        kind = 'windows_gpu_qualification_lane_attestation'; lane_id = $Lane.identity.lane_id; lane_payload_sha256 = Get-CanonicalDigest (Get-UnsignedLane $Lane); schema_version = 1
    }
    [byte[]]$recordBytes = Get-CanonicalBytes $record
    [byte[]]$length = [BitConverter]::GetBytes([UInt64]$recordBytes.Length)
    [byte[]]$preimage = [byte[]]::new($AttestationDomain.Length + 8 + $recordBytes.Length)
    [Array]::Copy($AttestationDomain, 0, $preimage, 0, $AttestationDomain.Length); [Array]::Copy($length, 0, $preimage, $AttestationDomain.Length, 8); [Array]::Copy($recordBytes, 0, $preimage, $AttestationDomain.Length + 8, $recordBytes.Length)
    [byte[]]$signature = $Key.SignData($preimage, [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.DSASignatureFormat]::IeeeP1363FixedFieldConcatenation)
    return [ordered]@{ key_id = $Plan.capture_authority.capture_key_id; record = $record; signature_base64 = [Convert]::ToBase64String($signature); signature_scheme = 'ecdsa-p256-sha256-ieee-p1363' }
}

function Update-Bindings($Documents, [string]$ArtifactRoot, [bool]$WriteArtifacts) {
    foreach ($lane in @($Documents.Evidence.lanes)) {
        if ($WriteArtifacts) { $lane.acquisition_artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($lane.acquisition_artifact_path.Replace('/', '\'))) 'windows_gpu_qualification_acquisition_artifact' $lane.identity.acquisition }
        foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($lane.run_sets[$mode][$target])) { if ($WriteArtifacts) { $run.artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($run.artifact_path.Replace('/', '\'))) 'windows_gpu_qualification_run_artifact' (Get-RecordWithoutArtifact $run) } } } }
        foreach ($scenario in @($lane.scenarios)) { if ($WriteArtifacts) { $scenario.artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($scenario.artifact_path.Replace('/', '\'))) 'windows_gpu_qualification_scenario_artifact' (Get-RecordWithoutArtifact $scenario) } }
        foreach ($capture in @($lane.captures)) { if ($WriteArtifacts) { $capture.artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($capture.artifact_path.Replace('/', '\'))) 'windows_gpu_qualification_raw_scif_capture' (Get-RecordWithoutArtifact $capture) } }
        $lane.artifact_inventory = Get-ArtifactReferences $lane
        $lane.attestation = New-Attestation $Documents.Plan $lane
    }
    $Documents.Plan.required_lanes = @($Documents.Evidence.lanes | ForEach-Object { [ordered]@{ evidence_sha256 = Get-CanonicalDigest $_; identity = $_.identity } })
    # required_lanes.evidence_sha256 is deliberately excluded from the signed
    # capture projection, so updating it after signing does not create a cycle.
    $Documents.Evidence.plan_sha256 = Get-CanonicalDigest $Documents.Plan
}

function New-Bundle($Documents, [string]$Name, [bool]$WriteArtifacts = $true) {
    $script:CaseCounter++
    $root = Join-Path $TestRoot ('{0:d2}-{1}' -f $script:CaseCounter, $Name); $artifactRoot = Join-Path $root 'artifacts'
    [IO.Directory]::CreateDirectory($artifactRoot) | Out-Null
    Update-Bindings $Documents $artifactRoot $WriteArtifacts
    $planPath = Join-Path $root 'plan.json'; $evidencePath = Join-Path $root 'evidence.json'
    Write-Canonical $planPath $Documents.Plan; Write-Canonical $evidencePath $Documents.Evidence
    return [pscustomobject]@{ Root = $root; ArtifactRoot = $artifactRoot; PlanPath = $planPath; EvidencePath = $evidencePath; Documents = $Documents }
}

function Rewrite-BundleEvidence($Bundle) { Write-Canonical $Bundle.EvidencePath $Bundle.Documents.Evidence }

function Invoke-Evaluator($Bundle, [bool]$AllowFixture = $true, [bool]$RequireEligible = $false, [string]$PlanOverride = '', [string]$EvidenceOverride = '', [string]$ArtifactOverride = '') {
    $start = [Diagnostics.ProcessStartInfo]::new(); $start.FileName = (Get-Command pwsh).Source; $start.UseShellExecute = $false; $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    foreach ($argument in @('-NoProfile', '-File', $ToolPath, '-PlanPath', $(if ($PlanOverride) { $PlanOverride } else { $Bundle.PlanPath }), '-EvidencePath', $(if ($EvidenceOverride) { $EvidenceOverride } else { $Bundle.EvidencePath }), '-ArtifactRoot', $(if ($ArtifactOverride) { $ArtifactOverride } else { $Bundle.ArtifactRoot }))) { $start.ArgumentList.Add($argument) }
    if ($AllowFixture) { $start.ArgumentList.Add('-AllowFixture') }; if ($RequireEligible) { $start.ArgumentList.Add('-RequireEligible') }
    $process = [Diagnostics.Process]::Start($start); $stdout = $process.StandardOutput.ReadToEnd(); $stderr = $process.StandardError.ReadToEnd(); $process.WaitForExit()
    return [pscustomobject]@{ ExitCode = $process.ExitCode; Stdout = $stdout; Stderr = $stderr }
}

function Assert-Rejected($Result, [string]$Label, [string]$Expected = '') {
    Assert-True ($Result.ExitCode -eq 1) "$Label was not rejected: exit=$($Result.ExitCode) stdout=$($Result.Stdout) stderr=$($Result.Stderr)"
    if ($Expected) { Assert-True ($Result.Stderr.Contains($Expected)) "$Label did not reach '$Expected': $($Result.Stderr)" }
}

function Get-Capture($Documents, [string]$Scope, [int]$Ordinal = 0) { return @($Documents.Evidence.lanes[0].captures | Where-Object { $_.launch_scope -ceq $Scope })[$Ordinal] }

function Assert-MutatedCaptureRejected([string]$Name, [string]$Scope, [string]$Which, [scriptblock]$Mutation, [string]$Expected = '') {
    $documents = New-FixtureDocuments; $capture = Get-Capture $documents $Scope; Update-CaptureControl $capture $Which $Mutation
    Assert-Rejected (Invoke-Evaluator (New-Bundle $documents $Name)) $Name $Expected
}

$TestRoot = Join-Path ([IO.Path]::GetTempPath()) ("scribe-windows-gpu-qualification-" + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($TestRoot) | Out-Null
try {
    $immutablePaths = [ordered]@{ auto = $AutoManifestPath; authority = $AuthorityPath; checked_plan = $CheckedPlanPath; evaluator = $ToolPath; toolchain = $ToolchainPath }
    $immutableBefore = [ordered]@{}; foreach ($name in $immutablePaths.Keys) { $immutableBefore[$name] = [IO.File]::ReadAllBytes($immutablePaths[$name]) }
    Assert-True ([IO.File]::ReadAllText($AutoManifestPath, $Utf8) -ceq $ExpectedAuto) 'Windows Auto manifest is not exact default deny.'
    Assert-True ([IO.File]::ReadAllText($AuthorityPath, $Utf8) -ceq $ExpectedAuthority) 'Windows qualification authority is not exact empty schema v2.'
    $checkedPlanRaw = [IO.File]::ReadAllText($CheckedPlanPath, $Utf8)
    $checkedPlan = $checkedPlanRaw | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-True ($checkedPlan.schema_version -eq 2 -and -not $checkedPlan.fixture_only -and $checkedPlan.required_lanes.Count -eq 0 -and -not $checkedPlan.runtime_bucket_complete) 'Checked-in plan is not canonical production default deny.'
    Assert-True (-not $checkedPlan.capture_authority.ContainsKey('fixture_capture_public_key_spki_base64')) 'Checked-in production plan contains a fixture capture key.'
    Assert-True ($checkedPlanRaw -ceq $Utf8.GetString((Get-CanonicalBytes $checkedPlan))) 'Checked-in plan is not canonical LF JSON.'
    $checkedEvidencePath = Join-Path $TestRoot 'checked-empty-evidence.json'; Write-Canonical $checkedEvidencePath ([ordered]@{ fixture_only = $false; kind = 'windows_gpu_release_qualification_evidence'; lanes = @(); plan_sha256 = Get-FileDigest $CheckedPlanPath; schema_version = 2 })
    $checkedProbe = [pscustomobject]@{ PlanPath = $CheckedPlanPath; EvidencePath = $checkedEvidencePath; ArtifactRoot = $TestRoot }
    Assert-Rejected (Invoke-Evaluator $checkedProbe $false) 'Checked-in default-deny plan validation' 'not approved by the protected production authority'

    $valid = New-Bundle (New-FixtureDocuments) 'valid'
    $validResult = Invoke-Evaluator $valid
    Assert-True ($validResult.ExitCode -eq 0) "Passing signed raw-SCIF fixture failed: $($validResult.Stderr)"
    $decision = $validResult.Stdout | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-True ($decision.schema_version -eq 2 -and $decision.qualification_passed -and -not $decision.auto_eligible -and $decision.decision_reason -ceq 'fixture_only_never_auto_eligible') 'Passing fixture decision semantics are wrong.'
    Assert-True ($decision.artifact_count -eq 75) 'Passing fixture did not consume the exact acquisition, run, scenario, and raw-capture inventory.'
    Assert-True ((Invoke-Evaluator $valid $false).ExitCode -eq 1) 'Fixture was accepted without -AllowFixture.'
    Assert-True ((Invoke-Evaluator $valid $true $true).ExitCode -eq 2) 'Fixture -RequireEligible did not return valid-ineligible exit 2.'
    $wrongToolchainVersion = New-FixtureDocuments; $wrongVersionIdentity = $wrongToolchainVersion.Evidence.lanes[0].identity; $revision = $wrongVersionIdentity.app_build_id.Split('#')[1]
    $wrongVersionIdentity.app_build_id = "local-transcriber@9.9.9#$revision"; $wrongWorkerBuild = "scribe-inference-worker@9.9.9#$revision"
    $wrongVersionIdentity.cpu_baseline.worker_build_id = $wrongWorkerBuild; $wrongVersionIdentity.gpu_worker.worker_build_id = $wrongWorkerBuild
    foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($wrongToolchainVersion.Evidence.lanes[0].run_sets[$mode][$target])) { $run.execution.worker_build_id = $wrongWorkerBuild } } }
    Sync-Captures $wrongToolchainVersion
    Assert-Rejected (Invoke-Evaluator (New-Bundle $wrongToolchainVersion 'wrong-toolchain-version')) 'App version not bound to toolchain' 'does not match the bound Windows toolchain'

    # Attestation/key/campaign fail-closed cases.
    $payloadTamper = New-Bundle (New-FixtureDocuments) 'signed-payload-tamper'; $payloadTamper.Documents.Evidence.lanes[0].run_sets.warm.gpu[0].end_to_end_ms++
    Rewrite-BundleEvidence $payloadTamper; Assert-Rejected (Invoke-Evaluator $payloadTamper) 'Signed lane payload tamper' 'attestation record does not bind'
    $signatureTamper = New-Bundle (New-FixtureDocuments) 'signature-tamper'; [byte[]]$signatureBytes = [Convert]::FromBase64String($signatureTamper.Documents.Evidence.lanes[0].attestation.signature_base64); $signatureBytes[0] = $signatureBytes[0] -bxor 1
    $signatureTamper.Documents.Evidence.lanes[0].attestation.signature_base64 = [Convert]::ToBase64String($signatureBytes); Rewrite-BundleEvidence $signatureTamper
    Assert-Rejected (Invoke-Evaluator $signatureTamper) 'Signature tamper' 'attestation signature is invalid'
    $badSignatureBase64 = New-Bundle (New-FixtureDocuments) 'signature-base64'; $badSignatureBase64.Documents.Evidence.lanes[0].attestation.signature_base64 = $badSignatureBase64.Documents.Evidence.lanes[0].attestation.signature_base64.TrimEnd('=')
    Rewrite-BundleEvidence $badSignatureBase64; Assert-Rejected (Invoke-Evaluator $badSignatureBase64) 'Noncanonical signature base64' 'canonical base64'
    $badSignatureScheme = New-Bundle (New-FixtureDocuments) 'signature-scheme'; $badSignatureScheme.Documents.Evidence.lanes[0].attestation.signature_scheme = 'ecdsa-p384-sha384-ieee-p1363'
    Rewrite-BundleEvidence $badSignatureScheme; Assert-Rejected (Invoke-Evaluator $badSignatureScheme) 'Unsupported signature scheme' 'signature scheme is unsupported'
    $shortSignature = New-Bundle (New-FixtureDocuments) 'signature-length'; [byte[]]$shortSignatureBytes = [Convert]::FromBase64String($shortSignature.Documents.Evidence.lanes[0].attestation.signature_base64)[0..62]
    $shortSignature.Documents.Evidence.lanes[0].attestation.signature_base64 = [Convert]::ToBase64String($shortSignatureBytes); Rewrite-BundleEvidence $shortSignature
    Assert-Rejected (Invoke-Evaluator $shortSignature) 'Wrong signature length' '64-byte IEEE-P1363 signature'
    $wrongAttestationKey = New-Bundle (New-FixtureDocuments) 'attestation-key-id'; $wrongAttestationKey.Documents.Evidence.lanes[0].attestation.key_id = 'p256:' + (Get-Digest 'other-key')
    Rewrite-BundleEvidence $wrongAttestationKey; Assert-Rejected (Invoke-Evaluator $wrongAttestationKey) 'Wrong attestation key ID' 'does not match the plan capture authority'
    $campaignReplay = New-Bundle (New-FixtureDocuments) 'campaign-replay'; $campaignReplay.Documents.Plan.capture_authority.campaign_nonce = Get-Digest 'different-campaign'; $campaignReplay.Documents.Evidence.plan_sha256 = Get-CanonicalDigest $campaignReplay.Documents.Plan
    Write-Canonical $campaignReplay.PlanPath $campaignReplay.Documents.Plan; Rewrite-BundleEvidence $campaignReplay
    Assert-Rejected (Invoke-Evaluator $campaignReplay) 'Cross-campaign replay' 'attestation record does not bind'
    foreach ($recordReplay in @(
        [pscustomobject]@{ Name = 'lane-replay'; Field = 'lane_id'; Value = 'different-lane' },
        [pscustomobject]@{ Name = 'batch-replay'; Field = 'acquisition_batch_id'; Value = 'different-batch' },
        [pscustomobject]@{ Name = 'capture-contract-replay'; Field = 'capture_contract_sha256'; Value = Get-Digest 'different-capture-contract' }
    )) {
        $documents = New-Bundle (New-FixtureDocuments) $recordReplay.Name; $documents.Documents.Evidence.lanes[0].attestation.record[$recordReplay.Field] = $recordReplay.Value
        Rewrite-BundleEvidence $documents; Assert-Rejected (Invoke-Evaluator $documents) "Attestation $($recordReplay.Name)" 'attestation record does not bind'
    }
    $laneMatrixReplay = New-Bundle (New-FixtureDocuments) 'lane-matrix-replay'; $firstLane = $laneMatrixReplay.Documents.Evidence.lanes[0]
    $secondLane = Copy-Document $firstLane
    $secondLane.identity.lane_id = 'fixture-windows-nvidia-cuda-z'
    $laneMatrixReplay.Documents.Evidence.lanes = @($firstLane, $secondLane)
    $laneMatrixReplay.Documents.Plan.required_lanes = @(
        $laneMatrixReplay.Documents.Plan.required_lanes[0],
        [ordered]@{ evidence_sha256 = Get-CanonicalDigest $secondLane; identity = $secondLane.identity }
    )
    $laneMatrixReplay.Documents.Evidence.plan_sha256 = Get-CanonicalDigest $laneMatrixReplay.Documents.Plan
    Write-Canonical $laneMatrixReplay.PlanPath $laneMatrixReplay.Documents.Plan; Rewrite-BundleEvidence $laneMatrixReplay
    Assert-Rejected (Invoke-Evaluator $laneMatrixReplay) 'Other lane identity replay' 'attestation record does not bind'
    $malformedSpki = New-FixtureDocuments; $malformedSpki.Plan.capture_authority.fixture_capture_public_key_spki_base64 = '!!!!'
    Assert-Rejected (Invoke-Evaluator (New-Bundle $malformedSpki 'malformed-spki')) 'Malformed SPKI base64' 'canonical base64'
    $otherP256 = [Security.Cryptography.ECDsa]::Create([Security.Cryptography.ECCurve+NamedCurves]::nistP256)
    try {
        $wrongP256 = New-FixtureDocuments; $wrongP256.Plan.capture_authority.fixture_capture_public_key_spki_base64 = [Convert]::ToBase64String($otherP256.ExportSubjectPublicKeyInfo())
        Assert-Rejected (Invoke-Evaluator (New-Bundle $wrongP256 'wrong-p256-key')) 'Wrong P-256 capture key' 'does not match capture_key_id'
    }
    finally { $otherP256.Dispose() }
    $p384 = [Security.Cryptography.ECDsa]::Create([Security.Cryptography.ECCurve+NamedCurves]::nistP384)
    try {
        $wrongCurve = New-FixtureDocuments; [byte[]]$p384Spki = $p384.ExportSubjectPublicKeyInfo(); $wrongCurve.Plan.capture_authority.fixture_capture_public_key_spki_base64 = [Convert]::ToBase64String($p384Spki); $wrongCurve.Plan.capture_authority.capture_key_id = 'p256:' + [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($p384Spki)).ToLowerInvariant()
        Assert-Rejected (Invoke-Evaluator (New-Bundle $wrongCurve 'wrong-curve')) 'Non-P256 capture key' 'must be NIST P-256'
    }
    finally { $p384.Dispose() }
    $productionRelabel = New-FixtureDocuments; $productionRelabel.Plan.fixture_only = $false; $productionRelabel.Plan.capture_authority.Remove('fixture_capture_public_key_spki_base64'); $productionRelabel.Evidence.fixture_only = $false
    Assert-Rejected (Invoke-Evaluator (New-Bundle $productionRelabel 'fixture-production-relabel') $false) 'Fixture relabeled production' 'not approved by the protected production authority'

    # Raw SCIF framing and strict JSON.
    foreach ($frameCase in @(
        [pscustomobject]@{ Name = 'bad-magic'; Mutate = { param($b) $b[0] = [byte][char]'X' }; Expected = 'invalid SCIF magic' },
        [pscustomobject]@{ Name = 'bad-version'; Mutate = { param($b) $b[4] = 4 }; Expected = 'SCIF v5 control frame' },
        [pscustomobject]@{ Name = 'bad-kind'; Mutate = { param($b) $b[5] = 2 }; Expected = 'SCIF v5 control frame' },
        [pscustomobject]@{ Name = 'bad-length'; Mutate = { param($b) $b[6] = $b[6] + 1 }; Expected = 'body length is invalid' },
        [pscustomobject]@{ Name = 'bad-session'; Mutate = { param($b) $b[10] = 1 }; Expected = 'session/request 0/0' },
        [pscustomobject]@{ Name = 'bad-request-id'; Mutate = { param($b) $b[18] = 1 }; Expected = 'session/request 0/0' }
    )) {
        $documents = New-FixtureDocuments; Update-CaptureFrameBytes (Get-Capture $documents 'cpu') 'request' $frameCase.Mutate
        Assert-Rejected (Invoke-Evaluator (New-Bundle $documents $frameCase.Name)) $frameCase.Name $frameCase.Expected
    }
    $trailingFrame = New-FixtureDocuments; $trailingCapture = Get-Capture $trailingFrame 'cpu'; [byte[]]$rawTrailing = [Convert]::FromBase64String($trailingCapture.request_frame_base64); $trailingCapture.request_frame_base64 = [Convert]::ToBase64String([byte[]]($rawTrailing + 0))
    Assert-Rejected (Invoke-Evaluator (New-Bundle $trailingFrame 'frame-trailing')) 'SCIF trailing byte' 'trailing bytes'
    $oversizedFrame = New-FixtureDocuments; $oversizedCapture = Get-Capture $oversizedFrame 'cpu'; [byte[]]$tooLarge = [byte[]]::new(26 + 262145); [Array]::Copy([Text.Encoding]::ASCII.GetBytes('SCIF'), 0, $tooLarge, 0, 4); $tooLarge[4] = 5; $tooLarge[5] = 1; [Array]::Copy([BitConverter]::GetBytes([UInt32]262145), 0, $tooLarge, 6, 4); $oversizedCapture.request_frame_base64 = [Convert]::ToBase64String($tooLarge)
    Assert-Rejected (Invoke-Evaluator (New-Bundle $oversizedFrame 'frame-oversized')) 'Oversized SCIF control frame'
    $noncanonicalFrameBase64 = New-FixtureDocuments; $base64Capture = Get-Capture $noncanonicalFrameBase64 'cpu'; $base64Capture.request_frame_base64 = Get-NoncanonicalBase64 $base64Capture.request_frame_base64
    Assert-Rejected (Invoke-Evaluator (New-Bundle $noncanonicalFrameBase64 'frame-base64')) 'Noncanonical SCIF frame base64'
    $invalidUtf8 = New-FixtureDocuments; $utf8Capture = Get-Capture $invalidUtf8 'cpu'; [byte[]]$utf8Frame = [Convert]::FromBase64String($utf8Capture.request_frame_base64); $utf8Frame[26] = 0xff; $utf8Capture.request_frame_base64 = [Convert]::ToBase64String($utf8Frame)
    Assert-Rejected (Invoke-Evaluator (New-Bundle $invalidUtf8 'frame-invalid-utf8')) 'Invalid wire UTF-8' 'strict UTF-8/JSON'
    $duplicateBody = New-FixtureDocuments; $duplicateCapture = Get-Capture $duplicateBody 'cpu'; $bodyText = ($Utf8.GetString([Convert]::FromBase64String($duplicateCapture.request_frame_base64), 26, ([Convert]::FromBase64String($duplicateCapture.request_frame_base64).Length - 26))).Replace('{"command":"hello",', '{"command":"hello","Command":"hello",')
    Set-CaptureBodyText $duplicateCapture 'request' $bodyText
    Assert-Rejected (Invoke-Evaluator (New-Bundle $duplicateBody 'duplicate-wire-json')) 'Duplicate/case-colliding wire JSON' 'duplicate or case-colliding'
    Assert-MutatedCaptureRejected 'unknown-request-field' 'cpu' 'request' { param($c) $c.unexpected = 1 } 'unexpected or missing fields'
    Assert-MutatedCaptureRejected 'uppercase-challenge' 'cpu' 'request' { param($c) $c.challenge = $c.challenge.ToUpperInvariant() } 'lowercase hexadecimal'
    Assert-MutatedCaptureRejected 'challenge-echo' 'cpu' 'response' { param($c) $c.capability.challenge = Get-Digest 'other-challenge' } 'did not echo'

    # Actual worker wire schema/build/provider/ABI/artifact/pack bindings.
    Assert-MutatedCaptureRejected 'wrong-app-build' 'cpu' 'request' { param($c) $c.expected.app_build = 'local-transcriber@0.1.0#0000000000000000000000000000000000000000' } 'app_build differs'
    Assert-MutatedCaptureRejected 'wrong-worker-build' 'cpu' 'response' { param($c) $c.capability.worker_build = 'scribe-inference-worker@0.2.0#0000000000000000000000000000000000000000' } 'worker_build differs'
    Assert-MutatedCaptureRejected 'wrong-bundled-digest' 'selected_device' 'response' { param($c) $c.capability.bundled_worker_sha256 = Get-Digest 'wrong-worker' } 'bundled worker digest differs'
    Assert-MutatedCaptureRejected 'wrong-abi' 'selected_device' 'request' { param($c) $c.expected.abi = 2 } 'ABI, role, or provider'
    Assert-MutatedCaptureRejected 'wrong-provider' 'selected_device' 'response' { param($c) $c.capability.provider = 'vulkan' } 'ABI, role, or provider'
    Assert-MutatedCaptureRejected 'wrong-role' 'cpu' 'response' { param($c) $c.capability.role = 'vad' } 'ABI, role, or provider'
    Assert-MutatedCaptureRejected 'reordered-artifacts' 'cpu' 'response' { param($c) $c.capability.artifacts = @($c.capability.artifacts[1], $c.capability.artifacts[0]) } 'exact ordered Windows inference targets'
    Assert-MutatedCaptureRejected 'wrong-artifact-target' 'selected_device' 'response' { param($c) $c.capability.artifacts[0].target = 'linux-x86_64' } 'exact ordered Windows inference targets'
    Assert-MutatedCaptureRejected 'cpu-pack-present' 'cpu' 'request' { param($c) $c.expected.pack = [ordered]@{} } 'unexpected or missing fields'
    Assert-MutatedCaptureRejected 'wrong-pack-digest' 'selected_device' 'request' { param($c) $c.expected.pack.pack_digest = Get-Digest 'wrong-pack' } 'does not match the reviewed pack'
    Assert-MutatedCaptureRejected 'wrong-pack-provider' 'selected_device' 'response' { param($c) $c.capability.pack.expectation.provider = 'transcribe-cpp-ggml-vulkan' } 'backend/provider does not match'
    Assert-MutatedCaptureRejected 'unknown-pack-field' 'selected_device' 'request' { param($c) $c.expected.pack.unexpected = 1 } 'unexpected or missing fields'

    # Launch-scope separation, capture reuse, and stable remapping.
    $discoveryNarrow = New-FixtureDocuments; $capture = Get-Capture $discoveryNarrow 'provider_discovery'; Update-CaptureControl $capture 'response' { param($c) $c.capability.pack.devices = @($c.capability.pack.devices[0]) }
    Assert-Rejected (Invoke-Evaluator (New-Bundle $discoveryNarrow 'discovery-narrow')) 'Incomplete provider discovery' 'does not match its discovery/selected launch scope'
    $selectedBroad = New-FixtureDocuments; $discovery = Get-Capture $selectedBroad 'provider_discovery'; $selected = Get-Capture $selectedBroad 'selected_device'; $discoveryControl = Get-ScifControl $discovery.response_frame_base64; Update-CaptureControl $selected 'response' { param($c) $c.capability.pack.devices = $discoveryControl.capability.pack.devices }
    Assert-Rejected (Invoke-Evaluator (New-Bundle $selectedBroad 'selected-broad')) 'Selected launch returned full inventory' 'does not match its discovery/selected launch scope'
    $wrongScope = New-FixtureDocuments; (Get-Capture $wrongScope 'selected_device').launch_scope = 'provider_discovery'
    Assert-Rejected (Invoke-Evaluator (New-Bundle $wrongScope 'run-bound-discovery')) 'Measured run bound discovery capture' 'discovery/selected launch scope'
    $duplicateChallenge = New-FixtureDocuments; $captures = @($duplicateChallenge.Evidence.lanes[0].captures); $firstChallenge = (Get-ScifControl $captures[0].request_frame_base64).challenge
    Update-CaptureControl $captures[1] 'request' { param($c) $c.challenge = $firstChallenge }; Update-CaptureControl $captures[1] 'response' { param($c) $c.capability.challenge = $firstChallenge }
    Assert-Rejected (Invoke-Evaluator (New-Bundle $duplicateChallenge 'duplicate-challenge')) 'Duplicate capture challenge' 'unique challenge'
    $captureReuse = New-FixtureDocuments; $reuseRuns = $captureReuse.Evidence.lanes[0].run_sets.cold.cpu; $reuseRuns[1].execution.capture_sha256 = $reuseRuns[0].execution.capture_sha256
    Assert-Rejected (Invoke-Evaluator (New-Bundle $captureReuse 'capture-reuse')) 'One capture reused by distinct generations' 'Distinct worker generations reused one raw capture'
    $missingColdCapture = New-FixtureDocuments; $missingColdCapture.Evidence.lanes[0].captures = @($missingColdCapture.Evidence.lanes[0].captures | Select-Object -Skip 1)
    Assert-Rejected (Invoke-Evaluator (New-Bundle $missingColdCapture 'missing-cold-capture')) 'Missing cold generation capture' 'exactly one raw SCIF capture'
    $sameMixedIndex = New-FixtureDocuments; $mixed = @($sameMixedIndex.Evidence.lanes[0].scenarios | Where-Object scenario -eq 'mixed_gpu')[0]; $mixed.process_index_after = $mixed.process_index_before
    $afterCapture = @($sameMixedIndex.Evidence.lanes[0].captures | Where-Object generation -like '*mixed_gpu:after')[0]; Update-CaptureControl $afterCapture 'response' { param($c) $c.capability.pack.devices[0].process_index = 3 }; $mixed.capture_after_sha256 = Get-CanonicalDigest (Get-RecordWithoutArtifact $afterCapture)
    Assert-Rejected (Invoke-Evaluator (New-Bundle $sameMixedIndex 'same-remap-index')) 'Mixed GPU same process index' 'do not prove stable-device remapping'
    $sameMixedChallenge = New-FixtureDocuments; $mixedCaptures = @($sameMixedChallenge.Evidence.lanes[0].captures | Where-Object generation -like '*scenario:mixed_gpu:*' | Sort-Object generation); $beforeChallenge = (Get-ScifControl $mixedCaptures[0].request_frame_base64).challenge
    Update-CaptureControl $mixedCaptures[1] 'request' { param($c) $c.challenge = $beforeChallenge }; Update-CaptureControl $mixedCaptures[1] 'response' { param($c) $c.capability.challenge = $beforeChallenge }
    Assert-Rejected (Invoke-Evaluator (New-Bundle $sameMixedChallenge 'same-remap-challenge')) 'Mixed GPU same challenge' 'unique challenge'

    # Vulkan vendor matrix: NVIDIA, both AMD PCI IDs used on Windows, Intel.
    foreach ($vendorCase in @(
        [pscustomobject]@{ Vendor = 'nvidia'; Id = '10de' }, [pscustomobject]@{ Vendor = 'amd'; Id = '1002' },
        [pscustomobject]@{ Vendor = 'amd'; Id = '1022' }, [pscustomobject]@{ Vendor = 'intel'; Id = '8086' }
    )) {
        $documents = New-FixtureDocuments; Set-VulkanFixture $documents $vendorCase.Vendor $vendorCase.Id
        $result = Invoke-Evaluator (New-Bundle $documents "vulkan-$($vendorCase.Vendor)-$($vendorCase.Id)")
        Assert-True ($result.ExitCode -eq 0) "Positive Vulkan $($vendorCase.Vendor)/$($vendorCase.Id) fixture failed: $($result.Stderr)"
    }
    $vulkanMismatch = New-FixtureDocuments; Set-VulkanFixture $vulkanMismatch 'amd' '10de'
    Assert-Rejected (Invoke-Evaluator (New-Bundle $vulkanMismatch 'vulkan-vendor-mismatch')) 'Vulkan vendor mismatch' 'driver.value is not canonical'
    $cudaAmd = New-FixtureDocuments; $cudaAmd.Evidence.lanes[0].identity.device.vendor = 'amd'; $cudaAmd.Evidence.lanes[0].identity.acquisition.device_set.devices[0].vendor = 'amd'; Sync-DeviceSetBindings $cudaAmd; Sync-Captures $cudaAmd
    Assert-Rejected (Invoke-Evaluator (New-Bundle $cudaAmd 'cuda-amd')) 'CUDA AMD mismatch' 'driver.value is not canonical'

    foreach ($mode in @('cpu', 'gpu')) {
        $documents = New-FixtureDocuments; $documents.Evidence.lanes[0].scenarios[0].requested_mode = $mode
        Assert-Rejected (Invoke-Evaluator (New-Bundle $documents "scenario-requested-$mode")) "Scenario requested_mode $mode" 'must exercise Auto'
    }

    # Existing qualification, inventory, and filesystem invariants remain live.
    $slow = New-Bundle (New-FixtureDocuments 111) 'slow'; $slowDecision = (Invoke-Evaluator $slow).Stdout | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-True (-not $slowDecision.qualification_passed -and $slowDecision.lanes[0].reasons -ccontains 'gpu_p95_exceeds_cpu_boundary') 'GPU p95 above 110 percent did not fail closed.'
    $parityDocuments = New-FixtureDocuments; $parityDocuments.Evidence.lanes[0].run_sets.warm.gpu[0].transcript_sha256 = Get-Digest 'wrong-transcript'; $parityDecision = (Invoke-Evaluator (New-Bundle $parityDocuments 'parity')).Stdout | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-True (-not $parityDecision.qualification_passed -and $parityDecision.lanes[0].reasons -ccontains 'correctness_not_equivalent') 'Transcript parity failure was not ineligible.'
    $inventoryTamper = New-Bundle (New-FixtureDocuments) 'artifact-tamper'; $tamperedPath = Join-Path $inventoryTamper.ArtifactRoot 'fixture-windows-nvidia-cuda\runs\warm\gpu\01.evidence'; [IO.File]::WriteAllText($tamperedPath, 'tampered', $Utf8)
    Assert-Rejected (Invoke-Evaluator $inventoryTamper) 'Signed inventory artifact tamper' 'digest does not match'
    $unsignedExtra = New-Bundle (New-FixtureDocuments) 'unsigned-extra'; [IO.File]::WriteAllText((Join-Path $unsignedExtra.ArtifactRoot 'extra.evidence'), 'extra', $Utf8)
    $extraResult = Invoke-Evaluator $unsignedExtra; Assert-True ($extraResult.ExitCode -eq 0) "Unreferenced filesystem file changed signed-inventory evaluation: $($extraResult.Stderr)"
    $inventoryEntryTamper = New-Bundle (New-FixtureDocuments) 'inventory-entry-tamper'; $inventoryEntryTamper.Documents.Evidence.lanes[0].artifact_inventory[0].artifact_sha256 = Get-Digest 'other-artifact'; Rewrite-BundleEvidence $inventoryEntryTamper
    Assert-Rejected (Invoke-Evaluator $inventoryEntryTamper) 'Signed inventory entry tamper' 'attestation record does not bind'
    $missingArtifacts = New-Bundle (New-FixtureDocuments) 'missing-artifacts' $false; Assert-Rejected (Invoke-Evaluator $missingArtifacts) 'Missing signed artifacts' 'Could not read'
    $integrated = New-FixtureDocuments; Set-IntegratedFixture $integrated; $integrated.Plan.runtime_bucket_complete = $true
    Assert-Rejected (Invoke-Evaluator (New-Bundle $integrated 'integrated-bucket')) 'Integrated v2 incomplete battery bucket' 'cannot mark an integrated or unified GPU runtime bucket complete'

    $noncanonical = New-Bundle (New-FixtureDocuments) 'noncanonical'; [IO.File]::WriteAllText($noncanonical.EvidencePath, ($noncanonical.Documents.Evidence | ConvertTo-Json -Depth 64), $Utf8)
    Assert-Rejected (Invoke-Evaluator $noncanonical) 'Noncanonical evidence JSON' 'not canonical JSON'
    $hardlink = New-Bundle (New-FixtureDocuments) 'hardlink'; $hardlinkAlias = Join-Path $hardlink.Root 'plan-hardlink.json'; New-Item -ItemType HardLink -Path $hardlinkAlias -Target $hardlink.PlanPath | Out-Null
    Assert-Rejected (Invoke-Evaluator $hardlink $true $false $hardlinkAlias) 'Hardlinked input' 'exactly one hard link'
    $ads = New-Bundle (New-FixtureDocuments) 'ads'; [IO.File]::WriteAllText($ads.EvidencePath + ':hidden', 'hidden', $Utf8); Assert-Rejected (Invoke-Evaluator $ads) 'ADS input' 'unnamed data stream'
    $junction = New-Bundle (New-FixtureDocuments) 'junction'; $junctionPath = Join-Path $junction.Root 'artifact-junction'; New-Item -ItemType Junction -Path $junctionPath -Target $junction.ArtifactRoot | Out-Null
    Assert-Rejected (Invoke-Evaluator $junction $true $false '' '' $junctionPath) 'Junction artifact root' 'physical Windows directory'
    $replacementRace = New-Bundle (New-FixtureDocuments) 'retained-read-replacement'; $replacementPlan = Join-Path $replacementRace.Root 'replacement-plan.json'; [IO.File]::WriteAllBytes($replacementPlan, [IO.File]::ReadAllBytes($replacementRace.PlanPath))
    $planDigestBefore = Get-FileDigest $replacementRace.PlanPath; $replacementBlocked = $false
    $heldPlan = [IO.FileStream]::new($replacementRace.PlanPath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read, 4096, [IO.FileOptions]::SequentialScan)
    try {
        try { [IO.File]::Move($replacementPlan, $replacementRace.PlanPath, $true) }
        catch [IO.IOException] { $replacementBlocked = $true }
        catch [UnauthorizedAccessException] { $replacementBlocked = $true }
        Assert-True $replacementBlocked 'A retained read handle allowed the qualification plan path to be replaced.'
        $heldReadResult = Invoke-Evaluator $replacementRace
        Assert-True ($heldReadResult.ExitCode -eq 0) "Evaluator could not read through a compatible retained handle: $($heldReadResult.Stderr)"
    }
    finally { $heldPlan.Dispose() }
    Assert-True ((Get-FileDigest $replacementRace.PlanPath) -ceq $planDigestBefore -and (Test-Path -LiteralPath $replacementPlan -PathType Leaf)) 'Blocked replacement changed either the bound plan or replacement candidate.'

    foreach ($name in $immutablePaths.Keys) {
        [byte[]]$after = [IO.File]::ReadAllBytes($immutablePaths[$name]); Assert-True ([Security.Cryptography.CryptographicOperations]::FixedTimeEquals($immutableBefore[$name], $after)) "Qualification tests modified immutable repository input: $name."
    }
    Write-Output 'Windows GPU qualification signed raw-SCIF contract tests passed.'
}
finally {
    $FixtureKey.Dispose()
    if (Test-Path -LiteralPath $TestRoot) { Remove-Item -LiteralPath $TestRoot -Recurse -Force }
}
