[CmdletBinding()]
param([switch]$PerformanceSmokeOnly, [switch]$PerformanceOnly)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$ToolPath = Join-Path $PSScriptRoot 'qualify-windows-gpu-evidence.ps1'
$AutoManifestPath = Join-Path $RepositoryRoot 'runtime-manifests\gpu-auto-qualification-windows-x64.json'
$AuthorityPath = Join-Path $RepositoryRoot 'runtime-manifests\windows-gpu-qualification-production-authority.json'
$PerformanceAuthorityPath = Join-Path $RepositoryRoot 'runtime-manifests\windows-gpu-performance-authority.json'
$CheckedPlanPath = Join-Path $RepositoryRoot 'runtime-manifests\windows-gpu-qualification-plan-x64.json'
$ToolchainPath = Join-Path $RepositoryRoot 'runtime-manifests\gpu-worker-toolchain-windows-x64.json'
$ExpectedAuto = '{"schema_version":2,"mode":"default_deny","target_os":"windows","target_arch":"x86_64","entries":[]}' + "`n"
$ExpectedAuthority = '{"approved_plans":[],"kind":"windows_gpu_qualification_production_authority","schema_version":2}' + "`n"
$ExpectedPerformanceAuthority = '{"keys":[],"kind":"windows_gpu_performance_campaign_authority","minimum_policy_epoch":1,"schema_version":1}' + "`n"
$RequiredScenarios = @('clean_installer', 'device_loss', 'disabled_device', 'driver_change', 'insufficient_vram', 'mixed_gpu', 'power_ac', 'power_battery', 'suspend_resume')
$ZeroSha256 = '0' * 64
$Utf8 = [Text.UTF8Encoding]::new($false, $true)
$AttestationDomain = [Text.Encoding]::ASCII.GetBytes("SCRIBE-WINDOWS-GPU-QUALIFICATION-LANE-ATTESTATION-V1`0")
$PerformanceAuthorizationDomain = [Text.Encoding]::ASCII.GetBytes("SCRIBE-WINDOWS-GPU-PERFORMANCE-AUTHORIZATION-V1`0")
$PerformanceAttestationDomain = [Text.Encoding]::ASCII.GetBytes("SCRIBE-WINDOWS-GPU-PERFORMANCE-LANE-ATTESTATION-V1`0")
$FixtureNow = [Int64]2000000000
$CaseCounter = 0
$FixtureKey = [Security.Cryptography.ECDsa]::Create([Security.Cryptography.ECCurve+NamedCurves]::nistP256)
$FixtureSpki = $FixtureKey.ExportSubjectPublicKeyInfo()
$FixtureSpkiBase64 = [Convert]::ToBase64String($FixtureSpki)
$FixtureKeyId = 'p256:' + [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($FixtureSpki)).ToLowerInvariant()
$PerformanceApprovalKey = [Security.Cryptography.ECDsa]::Create([Security.Cryptography.ECCurve+NamedCurves]::nistP256)
$PerformanceApprovalSpki = $PerformanceApprovalKey.ExportSubjectPublicKeyInfo()
$PerformanceApprovalSpkiBase64 = [Convert]::ToBase64String($PerformanceApprovalSpki)
$PerformanceApprovalKeyId = 'performance-approval-p256:' + [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($PerformanceApprovalSpki)).ToLowerInvariant()

function Assert-True([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Assert-Condition([bool]$Condition, [string]$Message) { Assert-True $Condition $Message }
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

function Get-DomainPreimage([byte[]]$Domain, [byte[]]$RecordBytes) {
    [byte[]]$length = [BitConverter]::GetBytes([UInt64]$RecordBytes.Length)
    [byte[]]$preimage = [byte[]]::new($Domain.Length + 8 + $RecordBytes.Length)
    [Array]::Copy($Domain, 0, $preimage, 0, $Domain.Length); [Array]::Copy($length, 0, $preimage, $Domain.Length, 8); [Array]::Copy($RecordBytes, 0, $preimage, $Domain.Length + 8, $RecordBytes.Length)
    return ,$preimage
}

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

function New-Run($Identity, [string]$Mode, [string]$Target, [int]$Sequence, [int]$GpuWarmMs, $Acquisition = $null, [string]$PowerSource = '') {
    if ($null -eq $Acquisition) { $Acquisition = $Identity.acquisition }
    $schema3 = -not [string]::IsNullOrEmpty($PowerSource)
    $endToEnd = if ($Mode -ceq 'cold') { if ($Target -ceq 'cpu') { 200 + $Sequence } else { 180 + $Sequence } } elseif ($Target -ceq 'cpu') { 100 } else { $GpuWarmMs }
    $worker = if ($Target -ceq 'cpu') { $Identity.cpu_baseline } else { $Identity.gpu_worker }
    $powerComponent = if ($schema3) { ":$PowerSource" } else { '' }
    $generation = if ($Mode -ceq 'cold') { "$($Acquisition.batch_id):$($Identity.lane_id)$powerComponent`:$Mode`:$Target`:$('{0:d2}' -f $Sequence)" } else { "$($Acquisition.batch_id):$($Identity.lane_id)$powerComponent`:warm:$Target" }
    $run = [ordered]@{
        acquisition_batch_id = $Acquisition.batch_id; artifact_path = "$($Identity.lane_id)/$(if ($schema3) { "$PowerSource/" })runs/$Mode/$Target/$('{0:d2}' -f $Sequence).evidence"; artifact_sha256 = Get-Digest "pending-$PowerSource-$Mode-$Target-$Sequence"
        available_device_memory_bytes_after = if ($Target -ceq 'cpu') { [Int64]0 } else { [Int64]8500000000 }; available_device_memory_bytes_before = if ($Target -ceq 'cpu') { [Int64]0 } else { [Int64]9000000000 }
        backend_ms = $endToEnd - 10; device_set_sha256 = $Acquisition.device_set.snapshot_sha256; end_to_end_ms = $endToEnd
        execution = [ordered]@{
            backend = if ($Target -ceq 'cpu') { 'cpu' } else { $Identity.backend }; capture_sha256 = Get-Digest "capture-$generation"; device_memory_kind = if ($Target -ceq 'cpu') { 'none' } else { $Identity.device.memory_model }
            driver = if ($Target -ceq 'cpu') { 'cpu:none' } else { $Identity.driver.value }; model_digest = $Identity.model.model_digest; options_sha256 = $Acquisition.options_sha256
            pack_digest = if ($Target -ceq 'cpu') { $ZeroSha256 } else { $Identity.pack.pack_digest }; protocol_version = $worker.protocol_version; provider_id = $worker.provider_id; runtime_abi = $worker.runtime_abi
            stable_device_id = if ($Target -ceq 'cpu') { 'cpu:host' } else { $Identity.device.stable_device_id }; windows_version = $Identity.windows_version; worker_build_id = $worker.worker_build_id; worker_generation = $generation; worker_sha256 = $worker.worker_sha256
        }
        failure_category = 'none'; machine_id_sha256 = $Acquisition.machine_id_sha256; outcome = 'success'; pair_id = "$($Acquisition.batch_id)$powerComponent`:$Mode`:$('{0:d2}' -f $Sequence)"
        pair_order = if ($Sequence % 2) { 'cpu_then_gpu' } else { 'gpu_then_cpu' }; peak_process_memory_bytes = [Int64](600000000 + $Sequence * 1024); peak_shared_device_memory_bytes = [Int64]0
        peak_vram_bytes = if ($Target -ceq 'cpu') { [Int64]0 } else { [Int64](800000000 + $Sequence * 2048) }; priming_runs = if ($Mode -ceq 'cold') { 0 } else { 1 }
        reset_state = if ($Mode -ceq 'cold') { 'fresh_process_fresh_model' } else { 'same_process_primed_model' }; sequence = $Sequence
        session_id = if ($Mode -ceq 'cold') { "$($Acquisition.batch_id)$powerComponent`:$Mode`:$('{0:d2}' -f $Sequence):session" } else { "$($Acquisition.batch_id)$powerComponent`:warm:session" }
        transcript_sha256 = $Identity.workload.expected_transcript_sha256
    }
    if ($schema3) {
        $run.acquisition_sha256 = Get-CanonicalDigest $Acquisition
        $run.power_source_before = $PowerSource
        $run.power_source_after = $PowerSource
    }
    return $run
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

function New-Capture($Identity, [string]$Generation, [string]$Scope, [int]$SelectedProcessIndex = 3, $Acquisition = $null, [string]$PowerSource = '') {
    if ($null -eq $Acquisition) { $Acquisition = $Identity.acquisition }
    $schema3 = -not [string]::IsNullOrEmpty($PowerSource)
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
        $providerDevices = @($Acquisition.device_set.devices | Where-Object { $_.provider_eligible })
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
    $capture = [ordered]@{
        artifact_path = "$($Identity.lane_id)/$(if ($schema3) { "$PowerSource/" })captures/$((Get-Digest $Generation).Substring(0, 24)).evidence"
        artifact_sha256 = Get-Digest "pending-capture-$Generation"; generation = $Generation; launch_scope = $Scope
        request_frame_base64 = New-ScifFrame $request; response_frame_base64 = New-ScifFrame $response
    }
    if ($schema3) {
        $capture.acquisition_sha256 = Get-CanonicalDigest $Acquisition
        $capture.power_source_before = $PowerSource
        $capture.power_source_after = $PowerSource
    }
    return $capture
}

function Get-RecordWithoutArtifact($Value) {
    $record = [ordered]@{}
    foreach ($key in $Value.Keys) { if (@('artifact_path', 'artifact_sha256') -cnotcontains $key) { $record[$key] = $Value[$key] } }
    return $record
}

function Sync-DeviceSetBindings($Documents) {
    $lane = $Documents.Evidence.lanes[0]
    $lane.identity.acquisition.device_set.snapshot_sha256 = Get-CanonicalDigest $lane.identity.acquisition.device_set.devices
    foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($lane.run_sets[$mode][$target])) { $run.device_set_sha256 = $lane.identity.acquisition.device_set.snapshot_sha256; if ($Documents.Plan.schema_version -eq 3) { $run.acquisition_sha256 = Get-CanonicalDigest $lane.identity.acquisition } } } }
    if ($Documents.Plan.schema_version -eq 3 -and $null -ne $lane.identity.battery_acquisition) {
        $lane.identity.battery_acquisition.device_set.snapshot_sha256 = Get-CanonicalDigest $lane.identity.battery_acquisition.device_set.devices
        foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($lane.battery.run_sets[$mode][$target])) { $run.device_set_sha256 = $lane.identity.battery_acquisition.device_set.snapshot_sha256; $run.acquisition_sha256 = Get-CanonicalDigest $lane.identity.battery_acquisition } } }
    }
    foreach ($scenario in @($lane.scenarios)) {
        $acquisition = if ($Documents.Plan.schema_version -eq 3 -and $scenario.power_source -ceq 'battery' -and $null -ne $lane.identity.battery_acquisition) { $lane.identity.battery_acquisition } else { $lane.identity.acquisition }
        $scenario.device_set_sha256 = $acquisition.device_set.snapshot_sha256
    }
}

function New-PowerCaptures($Block, $Identity, $Acquisition, [string]$PowerSource, [bool]$IncludeMixed, [bool]$Schema3) {
    $captures = [Collections.Generic.List[object]]::new()
    foreach ($mode in @('cold', 'warm')) {
        foreach ($target in @('cpu', 'gpu')) {
            foreach ($generation in @($Block.run_sets[$mode][$target].execution.worker_generation | Sort-Object -Unique)) {
                $scope = if ($target -ceq 'cpu') { 'cpu' } else { 'selected_device' }
                $capture = New-Capture $Identity $generation $scope 3 $Acquisition $(if ($Schema3) { $PowerSource } else { '' })
                $digest = Get-CanonicalDigest (Get-RecordWithoutArtifact $capture)
                foreach ($run in @($Block.run_sets[$mode][$target] | Where-Object { $_.execution.worker_generation -ceq $generation })) { $run.execution.capture_sha256 = $digest }
                $captures.Add($capture)
            }
        }
    }
    $powerComponent = if ($Schema3) { ":$PowerSource" } else { '' }
    $discoveryGeneration = "$($Acquisition.batch_id):$($Identity.lane_id)$powerComponent`:provider_discovery"
    $captures.Add((New-Capture $Identity $discoveryGeneration 'provider_discovery' 3 $Acquisition $(if ($Schema3) { $PowerSource } else { '' })))
    if ($IncludeMixed) {
        $before = New-Capture $Identity "$($Acquisition.batch_id):$($Identity.lane_id)$powerComponent`:scenario:mixed_gpu:before" 'selected_device' 3 $Acquisition $(if ($Schema3) { $PowerSource } else { '' })
        $after = New-Capture $Identity "$($Acquisition.batch_id):$($Identity.lane_id)$powerComponent`:scenario:mixed_gpu:after" 'selected_device' 11 $Acquisition $(if ($Schema3) { $PowerSource } else { '' })
        $captures.Add($before); $captures.Add($after)
    }
    return ,@($captures | Sort-Object generation)
}

function Sync-Captures($Documents) {
    $lane = $Documents.Evidence.lanes[0]
    $identity = $lane.identity
    $schema3 = $Documents.Plan.schema_version -eq 3
    $lane.captures = New-PowerCaptures $lane $identity $identity.acquisition 'ac' $true $schema3
    $mixed = @($lane.scenarios | Where-Object { $_.scenario -ceq 'mixed_gpu' })[0]
    $mixedCaptures = @($lane.captures | Where-Object { $_.generation -like '*scenario:mixed_gpu:*' } | Sort-Object generation)
    $mixed.capture_after_sha256 = Get-CanonicalDigest (Get-RecordWithoutArtifact $mixedCaptures[0])
    $mixed.capture_before_sha256 = Get-CanonicalDigest (Get-RecordWithoutArtifact $mixedCaptures[1])
    $mixed.process_index_before = 3; $mixed.process_index_after = 11
    # Sort order is after,before while the assigned process indexes are defined
    # by the generation names, not the array position.
    foreach ($capture in $mixedCaptures) {
        $control = Get-ScifControl $capture.response_frame_base64
        $index = $control.capability.pack.devices[0].process_index
        if ($capture.generation -like '*:before') { $mixed.capture_before_sha256 = Get-CanonicalDigest (Get-RecordWithoutArtifact $capture); $mixed.process_index_before = $index }
        else { $mixed.capture_after_sha256 = Get-CanonicalDigest (Get-RecordWithoutArtifact $capture); $mixed.process_index_after = $index }
    }
    if ($schema3 -and $null -ne $lane.battery) {
        $lane.battery.captures = New-PowerCaptures $lane.battery $identity $identity.battery_acquisition 'battery' $false $true
    }
    if ($schema3) {
        foreach ($scenario in @($lane.scenarios)) {
            $discreteBattery = $scenario.scenario -ceq 'power_battery' -and $identity.device.device_class -ceq 'discrete_gpu'
            $acquisition = if ($scenario.power_source -ceq 'battery') { $identity.battery_acquisition } else { $identity.acquisition }
            $scenario.acquisition_sha256 = if ($discreteBattery) { $ZeroSha256 } else { Get-CanonicalDigest $acquisition }
            $scenario.power_source_before = $scenario.power_source
            $scenario.power_source_after = $scenario.power_source
            if ($scenario.selected_backend -ceq 'cpu') { $scenario.selected_capture_sha256 = $ZeroSha256 }
            else {
                $captures = if ($scenario.power_source -ceq 'battery') { $lane.battery.captures } else { $lane.captures }
                $scenario.selected_capture_sha256 = Get-CanonicalDigest (Get-RecordWithoutArtifact @($captures | Where-Object { $_.launch_scope -ceq 'selected_device' -and $_.generation -like '*:warm:gpu' })[0])
            }
        }
    }
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
    foreach ($acquisition in @($identity.acquisition, $(if ($Documents.Plan.schema_version -eq 3) { $identity.battery_acquisition })) | Where-Object { $null -ne $_ }) {
        foreach ($device in @($acquisition.device_set.devices | Where-Object { $_.provider_eligible })) { $device.vendor = $Vendor; $device.driver = $driver }
    }
    $runSets = @($lane.run_sets)
    if ($Documents.Plan.schema_version -eq 3 -and $null -ne $lane.battery) { $runSets += $lane.battery.run_sets }
    foreach ($sets in $runSets) { foreach ($mode in @('cold', 'warm')) { foreach ($run in @($sets[$mode].gpu)) { $run.execution.backend = 'vulkan'; $run.execution.provider_id = 'vulkan'; $run.execution.worker_sha256 = $identity.gpu_worker.worker_sha256; $run.execution.driver = $driver } } }
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

function New-PowerRunSets($Identity, $Acquisition, [string]$PowerSource, [int]$GpuColdMs, [int]$GpuWarmMs) {
    $sets = [ordered]@{
        cold = [ordered]@{ cpu = @(1..5 | ForEach-Object { New-Run $Identity 'cold' 'cpu' $_ $GpuWarmMs $Acquisition $PowerSource }); gpu = @(1..5 | ForEach-Object { New-Run $Identity 'cold' 'gpu' $_ $GpuWarmMs $Acquisition $PowerSource }) }
        warm = [ordered]@{ cpu = @(1..20 | ForEach-Object { New-Run $Identity 'warm' 'cpu' $_ $GpuWarmMs $Acquisition $PowerSource }); gpu = @(1..20 | ForEach-Object { New-Run $Identity 'warm' 'gpu' $_ $GpuWarmMs $Acquisition $PowerSource }) }
    }
    foreach ($run in @($sets.cold.gpu)) { $run.end_to_end_ms = $GpuColdMs; $run.backend_ms = $GpuColdMs - 10 }
    if ($Identity.device.memory_model -ceq 'shared_host_memory') {
        foreach ($mode in @('cold', 'warm')) { foreach ($run in @($sets[$mode].gpu)) { $run.peak_shared_device_memory_bytes = $run.peak_vram_bytes; $run.peak_vram_bytes = [Int64]0 } }
    }
    return $sets
}

function New-V3FixtureDocuments([string]$DeviceClass = 'integrated_gpu', [int]$AcGpuColdMs = 220, [int]$AcGpuWarmMs = 110, [int]$BatteryGpuColdMs = 220, [int]$BatteryGpuWarmMs = 110) {
    Assert-True (@('discrete_gpu', 'integrated_gpu', 'unified_gpu') -ccontains $DeviceClass) "Unsupported v3 fixture class $DeviceClass"
    $documents = New-FixtureDocuments $AcGpuWarmMs
    if ($DeviceClass -cne 'discrete_gpu') {
        Set-IntegratedFixture $documents
        $identity = $documents.Evidence.lanes[0].identity
        $identity.device.device_class = $DeviceClass
        $identity.acquisition.device_set.devices[0].device_class = $DeviceClass
    }
    $documents.Plan.schema_version = 3
    $documents.Evidence.schema_version = 3
    $documents.Plan.capture_contract.power_policy = 'ac_for_discrete_ac_and_battery_for_integrated_or_unified'
    $lane = $documents.Evidence.lanes[0]
    $identity = $lane.identity
    $identity.acquisition.protocol.protocol_version = 2
    $identity.acquisition.controls.gpu_power_profile = 'system_managed'
    $identity.battery_acquisition = $null
    $lane.run_sets = New-PowerRunSets $identity $identity.acquisition 'ac' $AcGpuColdMs $AcGpuWarmMs
    $lane.battery = $null
    if ($DeviceClass -cne 'discrete_gpu') {
        $batteryAcquisition = Copy-Document $identity.acquisition
        $batteryAcquisition.batch_id = 'fixture-battery-batch-001'
        $batteryAcquisition.controls.power_source = 'battery'
        $batteryAcquisition.controls.power_plan_sha256 = Get-Digest 'balanced-battery-plan'
        $identity.battery_acquisition = $batteryAcquisition
        $lane.battery = [ordered]@{
            acquisition_artifact_path = "$($identity.lane_id)/battery/acquisition.evidence"
            acquisition_artifact_sha256 = Get-Digest 'pending-battery-acquisition'
            captures = @()
            run_sets = New-PowerRunSets $identity $batteryAcquisition 'battery' $BatteryGpuColdMs $BatteryGpuWarmMs
        }
    }
    foreach ($scenario in @($lane.scenarios)) {
        $scenario.acquisition_sha256 = $ZeroSha256
        $scenario.power_source_before = $scenario.power_source
        $scenario.power_source_after = $scenario.power_source
        $scenario.selected_capture_sha256 = $ZeroSha256
    }
    Sync-DeviceSetBindings $documents
    Sync-Captures $documents
    return $documents
}

function Get-PerformanceContractProjection($Plan) {
    return [ordered]@{
        approval_key_id = $Plan.authorization.key_id
        capture_authority = $Plan.capture_authority
        capture_contract = $Plan.capture_contract
        cold_runs = $Plan.cold_runs
        contract_bindings = $Plan.contract_bindings
        fixture_only = $Plan.fixture_only
        kind = 'windows_gpu_performance_capture_contract'
        maximum_gpu_p95_cpu_percent = $Plan.maximum_gpu_p95_cpu_percent
        required_lane_identities = @($Plan.required_lanes | ForEach-Object { $_.identity })
        schema_version = 1
        source = $Plan.source
        target_arch = $Plan.target_arch
        target_os = $Plan.target_os
        warm_runs = $Plan.warm_runs
    }
}

function New-PerformanceAuthorization($Plan, [Security.Cryptography.ECDsa]$Key = $PerformanceApprovalKey) {
    $record = [ordered]@{
        campaign_nonce = $Plan.capture_authority.campaign_nonce
        expires_at_unix_seconds = $FixtureNow + 300
        issued_at_unix_seconds = $FixtureNow - 300
        kind = 'windows_gpu_performance_campaign_authorization'
        performance_contract_sha256 = Get-CanonicalDigest (Get-PerformanceContractProjection $Plan)
        policy_epoch = 1
        schema_version = 1
        source_revision = $Plan.source.revision
    }
    [byte[]]$signature = $Key.SignData((Get-DomainPreimage $PerformanceAuthorizationDomain (Get-CanonicalBytes $record)), [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.DSASignatureFormat]::IeeeP1363FixedFieldConcatenation)
    return [ordered]@{
        fixture_approval_public_key_spki_base64 = $PerformanceApprovalSpkiBase64
        key_id = $PerformanceApprovalKeyId
        record = $record
        signature_base64 = [Convert]::ToBase64String($signature)
        signature_scheme = 'ecdsa-p256-sha256-ieee-p1363'
    }
}

function New-PerformanceDocuments([string]$DeviceClass = 'integrated_gpu', [int]$AcGpuColdMs = 220, [int]$AcGpuWarmMs = 110, [int]$BatteryGpuColdMs = 220, [int]$BatteryGpuWarmMs = 110, [string]$LaneId = '', [string]$VulkanVendor = '') {
    $source = New-V3FixtureDocuments $DeviceClass $AcGpuColdMs $AcGpuWarmMs $BatteryGpuColdMs $BatteryGpuWarmMs
    if ($VulkanVendor) { Set-VulkanFixture $source $VulkanVendor $(if ($VulkanVendor -ceq 'intel') { '8086' } else { '1002' }) }
    $lane = $source.Evidence.lanes[0]
    if ($LaneId) {
        $lane.identity.lane_id = $LaneId
        $lane.identity.acquisition.batch_id = "$LaneId-ac-batch"
        $lane.acquisition_artifact_path = "$LaneId/acquisition.evidence"
        $lane.run_sets = New-PowerRunSets $lane.identity $lane.identity.acquisition 'ac' $AcGpuColdMs $AcGpuWarmMs
        if ($null -ne $lane.battery) {
            $lane.identity.battery_acquisition.batch_id = "$LaneId-battery-batch"
            $lane.battery.acquisition_artifact_path = "$LaneId/battery/acquisition.evidence"
            $lane.battery.run_sets = New-PowerRunSets $lane.identity $lane.identity.battery_acquisition 'battery' $BatteryGpuColdMs $BatteryGpuWarmMs
        }
    }
    $lane.identity.Remove('installation')
    $lane.identity.device.Remove('qualified_minimum_total_memory_bytes')
    $lane.identity.device.Remove('qualified_minimum_available_memory_bytes')
    $lane.Remove('scenarios')
    $lane.captures = New-PowerCaptures $lane $lane.identity $lane.identity.acquisition 'ac' $false $true
    if ($null -ne $lane.battery) { $lane.battery.captures = New-PowerCaptures $lane.battery $lane.identity $lane.identity.battery_acquisition 'battery' $false $true }
    $revision = ([string]$lane.identity.app_build_id).Substring(([string]$lane.identity.app_build_id).LastIndexOf('#') + 1)
    $plan = [ordered]@{
        authorization = [ordered]@{ key_id = $PerformanceApprovalKeyId }
        capture_authority = [ordered]@{ campaign_nonce = Get-Digest 'performance-fixture-campaign'; capture_key_id = $FixtureKeyId; capture_public_key_spki_base64 = $FixtureSpkiBase64 }
        capture_contract = Copy-Document $source.Plan.capture_contract
        cold_runs = 5
        contract_bindings = [ordered]@{ base_auto_manifest_sha256 = Get-FileDigest $AutoManifestPath; evaluator_sha256 = Get-FileDigest $ToolPath; toolchain_contract_sha256 = Get-FileDigest $ToolchainPath }
        fixture_only = $true
        kind = 'windows_gpu_performance_candidate_plan'
        maximum_gpu_p95_cpu_percent = 110
        required_lanes = @([ordered]@{ evidence_sha256 = Get-Digest 'pending-performance-lane'; identity = $lane.identity })
        schema_version = 1
        source = [ordered]@{ app_version = '0.1.0'; ref = 'refs/heads/main'; repository = 'tyhuang9/scribe'; revision = $revision }
        target_arch = 'x86_64'
        target_os = 'windows'
        warm_runs = 20
    }
    $plan.authorization = New-PerformanceAuthorization $plan
    $evidence = [ordered]@{ fixture_only = $true; kind = 'windows_gpu_performance_candidate_evidence'; lanes = @($lane); plan_sha256 = Get-Digest 'pending-performance-plan'; schema_version = 1 }
    return [pscustomobject]@{ Plan = $plan; Evidence = $evidence }
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
    if ($Lane.Contains('scenarios')) { foreach ($scenario in @($Lane.scenarios)) { $references.Add([ordered]@{ artifact_path = $scenario.artifact_path; artifact_sha256 = $scenario.artifact_sha256 }) } }
    foreach ($capture in @($Lane.captures)) { $references.Add([ordered]@{ artifact_path = $capture.artifact_path; artifact_sha256 = $capture.artifact_sha256 }) }
    if ($Lane.Contains('battery') -and $null -ne $Lane.battery) {
        $references.Add([ordered]@{ artifact_path = $Lane.battery.acquisition_artifact_path; artifact_sha256 = $Lane.battery.acquisition_artifact_sha256 })
        foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($Lane.battery.run_sets[$mode][$target])) { $references.Add([ordered]@{ artifact_path = $run.artifact_path; artifact_sha256 = $run.artifact_sha256 }) } } }
        foreach ($capture in @($Lane.battery.captures)) { $references.Add([ordered]@{ artifact_path = $capture.artifact_path; artifact_sha256 = $capture.artifact_sha256 }) }
    }
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

function New-PerformanceAttestation($Plan, $Lane, [Security.Cryptography.ECDsa]$Key = $FixtureKey, [byte[]]$Domain = $PerformanceAttestationDomain) {
    $record = [ordered]@{
        acquisition_batch_id = $Lane.identity.acquisition.batch_id
        artifact_inventory_sha256 = Get-CanonicalDigest $Lane.artifact_inventory
        authorization_sha256 = Get-CanonicalDigest $Plan.authorization
        campaign_nonce = $Plan.capture_authority.campaign_nonce
        kind = 'windows_gpu_performance_lane_attestation'
        lane_id = $Lane.identity.lane_id
        lane_payload_sha256 = Get-CanonicalDigest (Get-UnsignedLane $Lane)
        performance_contract_sha256 = Get-CanonicalDigest (Get-PerformanceContractProjection $Plan)
        schema_version = 1
    }
    [byte[]]$signature = $Key.SignData((Get-DomainPreimage $Domain (Get-CanonicalBytes $record)), [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.DSASignatureFormat]::IeeeP1363FixedFieldConcatenation)
    return [ordered]@{ key_id = $Plan.capture_authority.capture_key_id; record = $record; signature_base64 = [Convert]::ToBase64String($signature); signature_scheme = 'ecdsa-p256-sha256-ieee-p1363' }
}

function Update-PerformanceBindings($Documents, [string]$ArtifactRoot, [bool]$WriteArtifacts) {
    $Documents.Plan.required_lanes = @($Documents.Evidence.lanes | ForEach-Object { [ordered]@{ evidence_sha256 = Get-Digest 'pending-performance-lane'; identity = $_.identity } })
    $Documents.Plan.authorization = [ordered]@{ key_id = $PerformanceApprovalKeyId }
    $Documents.Plan.authorization = New-PerformanceAuthorization $Documents.Plan
    foreach ($lane in @($Documents.Evidence.lanes)) {
        if ($WriteArtifacts) { $lane.acquisition_artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($lane.acquisition_artifact_path.Replace('/', '\'))) 'windows_gpu_performance_acquisition_artifact' $lane.identity.acquisition }
        foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($lane.run_sets[$mode][$target])) { if ($WriteArtifacts) { $run.artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($run.artifact_path.Replace('/', '\'))) 'windows_gpu_performance_run_artifact' (Get-RecordWithoutArtifact $run) } } } }
        foreach ($capture in @($lane.captures)) { if ($WriteArtifacts) { $capture.artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($capture.artifact_path.Replace('/', '\'))) 'windows_gpu_performance_raw_scif_capture' (Get-RecordWithoutArtifact $capture) } }
        if ($null -ne $lane.battery) {
            if ($WriteArtifacts) { $lane.battery.acquisition_artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($lane.battery.acquisition_artifact_path.Replace('/', '\'))) 'windows_gpu_performance_acquisition_artifact' $lane.identity.battery_acquisition }
            foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($lane.battery.run_sets[$mode][$target])) { if ($WriteArtifacts) { $run.artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($run.artifact_path.Replace('/', '\'))) 'windows_gpu_performance_run_artifact' (Get-RecordWithoutArtifact $run) } } } }
            foreach ($capture in @($lane.battery.captures)) { if ($WriteArtifacts) { $capture.artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($capture.artifact_path.Replace('/', '\'))) 'windows_gpu_performance_raw_scif_capture' (Get-RecordWithoutArtifact $capture) } }
        }
        $lane.artifact_inventory = Get-ArtifactReferences $lane
        $lane.attestation = New-PerformanceAttestation $Documents.Plan $lane
    }
    $Documents.Plan.required_lanes = @($Documents.Evidence.lanes | ForEach-Object { [ordered]@{ evidence_sha256 = Get-CanonicalDigest $_; identity = $_.identity } })
    $Documents.Evidence.plan_sha256 = Get-CanonicalDigest $Documents.Plan
}

function Resign-PerformanceBundle($Bundle, [byte[]]$AuthorizationDomain = $PerformanceAuthorizationDomain, [byte[]]$LaneDomain = $PerformanceAttestationDomain) {
    $record = $Bundle.Documents.Plan.authorization.record
    [byte[]]$signature = $PerformanceApprovalKey.SignData((Get-DomainPreimage $AuthorizationDomain (Get-CanonicalBytes $record)), [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.DSASignatureFormat]::IeeeP1363FixedFieldConcatenation)
    $Bundle.Documents.Plan.authorization.signature_base64 = [Convert]::ToBase64String($signature)
    foreach ($lane in @($Bundle.Documents.Evidence.lanes)) { $lane.attestation = New-PerformanceAttestation $Bundle.Documents.Plan $lane $FixtureKey $LaneDomain }
    $Bundle.Documents.Plan.required_lanes = @($Bundle.Documents.Evidence.lanes | ForEach-Object { [ordered]@{ evidence_sha256 = Get-CanonicalDigest $_; identity = $_.identity } })
    $Bundle.Documents.Evidence.plan_sha256 = Get-CanonicalDigest $Bundle.Documents.Plan
    Write-Canonical $Bundle.PlanPath $Bundle.Documents.Plan; Write-Canonical $Bundle.EvidencePath $Bundle.Documents.Evidence
}

function Update-Bindings($Documents, [string]$ArtifactRoot, [bool]$WriteArtifacts) {
    if ($Documents.Plan.kind -ceq 'windows_gpu_performance_candidate_plan') { Update-PerformanceBindings $Documents $ArtifactRoot $WriteArtifacts; return }
    if (@($Documents.Plan.required_lanes).Count -eq @($Documents.Evidence.lanes).Count) {
        for ($laneIndex = 0; $laneIndex -lt @($Documents.Evidence.lanes).Count; $laneIndex++) { $Documents.Plan.required_lanes[$laneIndex].identity = $Documents.Evidence.lanes[$laneIndex].identity }
    }
    foreach ($lane in @($Documents.Evidence.lanes)) {
        if ($WriteArtifacts) { $lane.acquisition_artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($lane.acquisition_artifact_path.Replace('/', '\'))) 'windows_gpu_qualification_acquisition_artifact' $lane.identity.acquisition }
        foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($lane.run_sets[$mode][$target])) { if ($WriteArtifacts) { $run.artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($run.artifact_path.Replace('/', '\'))) 'windows_gpu_qualification_run_artifact' (Get-RecordWithoutArtifact $run) } } } }
        foreach ($scenario in @($lane.scenarios)) { if ($WriteArtifacts) { $scenario.artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($scenario.artifact_path.Replace('/', '\'))) 'windows_gpu_qualification_scenario_artifact' (Get-RecordWithoutArtifact $scenario) } }
        foreach ($capture in @($lane.captures)) { if ($WriteArtifacts) { $capture.artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($capture.artifact_path.Replace('/', '\'))) 'windows_gpu_qualification_raw_scif_capture' (Get-RecordWithoutArtifact $capture) } }
        if ($Documents.Plan.schema_version -eq 3 -and $null -ne $lane.battery) {
            if ($WriteArtifacts) { $lane.battery.acquisition_artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($lane.battery.acquisition_artifact_path.Replace('/', '\'))) 'windows_gpu_qualification_acquisition_artifact' $lane.identity.battery_acquisition }
            foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($lane.battery.run_sets[$mode][$target])) { if ($WriteArtifacts) { $run.artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($run.artifact_path.Replace('/', '\'))) 'windows_gpu_qualification_run_artifact' (Get-RecordWithoutArtifact $run) } } } }
            foreach ($capture in @($lane.battery.captures)) { if ($WriteArtifacts) { $capture.artifact_sha256 = Write-Envelope (Join-Path $ArtifactRoot ($capture.artifact_path.Replace('/', '\'))) 'windows_gpu_qualification_raw_scif_capture' (Get-RecordWithoutArtifact $capture) } }
        }
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

function Refresh-BundleSignatures($Bundle) {
    foreach ($lane in @($Bundle.Documents.Evidence.lanes)) { $lane.attestation = New-Attestation $Bundle.Documents.Plan $lane }
    $Bundle.Documents.Plan.required_lanes = @($Bundle.Documents.Evidence.lanes | ForEach-Object { [ordered]@{ evidence_sha256 = Get-CanonicalDigest $_; identity = $_.identity } })
    $Bundle.Documents.Evidence.plan_sha256 = Get-CanonicalDigest $Bundle.Documents.Plan
    Write-Canonical $Bundle.PlanPath $Bundle.Documents.Plan
    Write-Canonical $Bundle.EvidencePath $Bundle.Documents.Evidence
}

function Invoke-Evaluator($Bundle, [bool]$AllowFixture = $true, [bool]$RequireEligible = $false, [string]$PlanOverride = '', [string]$EvidenceOverride = '', [string]$ArtifactOverride = '', [bool]$AddPerformanceClock = $true, [string[]]$ExtraArguments = @()) {
    $start = [Diagnostics.ProcessStartInfo]::new(); $start.FileName = (Get-Command pwsh).Source; $start.UseShellExecute = $false; $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    foreach ($argument in @('-NoProfile', '-File', $ToolPath, '-PlanPath', $(if ($PlanOverride) { $PlanOverride } else { $Bundle.PlanPath }), '-EvidencePath', $(if ($EvidenceOverride) { $EvidenceOverride } else { $Bundle.EvidencePath }), '-ArtifactRoot', $(if ($ArtifactOverride) { $ArtifactOverride } else { $Bundle.ArtifactRoot }))) { $start.ArgumentList.Add($argument) }
    if ($AllowFixture) { $start.ArgumentList.Add('-AllowFixture') }; if ($RequireEligible) { $start.ArgumentList.Add('-RequireEligible') }
    $bundleDocuments = $Bundle.PSObject.Properties['Documents']
    if ($AddPerformanceClock -and $null -ne $bundleDocuments -and $null -ne $Bundle.Documents -and $Bundle.Documents.Plan.kind -ceq 'windows_gpu_performance_candidate_plan') { $start.ArgumentList.Add('-FixtureNowUnixSeconds'); $start.ArgumentList.Add([string]$FixtureNow) }
    foreach ($argument in $ExtraArguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start); $stdout = $process.StandardOutput.ReadToEnd(); $stderr = $process.StandardError.ReadToEnd(); $process.WaitForExit()
    return [pscustomobject]@{ ExitCode = $process.ExitCode; Stdout = $stdout; Stderr = $stderr }
}

function Invoke-PassingPerformanceFixture($Documents, [string]$Name, [int]$ExpectedArtifacts) {
    $bundle = New-Bundle $Documents $Name
    $result = Invoke-Evaluator $bundle
    Assert-True ($result.ExitCode -eq 0) "$Name failed performance evaluator execution: $($result.Stderr)"
    $decision = $result.Stdout | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-True ($decision.performance_passed -and -not $decision.auto_eligible -and -not $decision.release_approved -and $decision.decision_reason -ceq 'fixture_pending_final_installer_qualification') "$Name did not produce a passing, non-releasable fixture decision."
    Assert-True ($decision.artifact_count -eq $ExpectedArtifacts -and $decision.candidate_policy -is [string] -and $decision.candidate_policy.EndsWith("`n")) "$Name did not emit the expected bounded canonical policy candidate."
    [byte[]]$policyBytes = $Utf8.GetBytes($decision.candidate_policy)
    $policyDigest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($policyBytes)).ToLowerInvariant()
    Assert-True ($decision.candidate_policy_sha256 -ceq $policyDigest) "$Name candidate policy digest does not cover its exact UTF-8 bytes including LF."
    $candidatePath = Join-Path $bundle.Root 'candidate-policy.json'; [IO.File]::WriteAllBytes($candidatePath, $policyBytes)
    $report = & (Join-Path $PSScriptRoot 'report-windows-gpu-auto-qualification.ps1') -ManifestPath $candidatePath
    Assert-True (($report -join "`n").Contains("qualified_entries: $(@($decision.lanes).Count)")) "$Name candidate was not accepted by the existing runtime-manifest report validator."
    Assert-True (-not $decision.candidate_policy.Contains('installation') -and -not $decision.candidate_policy.Contains('qualified_minimum_') -and -not $decision.candidate_policy.Contains('scenario')) "$Name candidate leaked installer, declared-floor, or scenario fields."
    $eligibleResult = Invoke-Evaluator $bundle $true $true
    Assert-True ($eligibleResult.ExitCode -eq 2) "$Name -RequireEligible did not preserve the non-release candidate boundary."
    return [pscustomobject]@{ Bundle = $bundle; Decision = $decision; Result = $result }
}

function Invoke-FailingPerformanceFixture($Documents, [string]$Name, [string]$ExpectedReason) {
    $bundle = New-Bundle $Documents $Name
    $result = Invoke-Evaluator $bundle
    Assert-True ($result.ExitCode -eq 0) "$Name was structurally rejected instead of evaluated: $($result.Stderr)"
    $decision = $result.Stdout | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-True (-not $decision.performance_passed -and $decision.lanes[0].reasons -ccontains $ExpectedReason) "$Name did not report $ExpectedReason."
    Assert-True ($null -eq $decision.candidate_policy -and $null -eq $decision.candidate_policy_sha256 -and $null -eq $decision.lanes[0].candidate_entry) "$Name emitted a candidate policy or entry for failing evidence."
    return $decision
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

function Set-RunFailure($Run, [string]$Category) {
    $Run.outcome = 'failure'
    $Run.failure_category = $Category
    $Run.transcript_sha256 = $ZeroSha256
}

function Invoke-PassingFixture($Documents, [string]$Name, [int]$ExpectedArtifacts) {
    $bundle = New-Bundle $Documents $Name
    $result = Invoke-Evaluator $bundle
    Assert-True ($result.ExitCode -eq 0) "$Name failed evaluator execution: $($result.Stderr)"
    $decision = $result.Stdout | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-True ($decision.qualification_passed -and -not $decision.auto_eligible -and $decision.decision_reason -ceq 'fixture_only_never_auto_eligible') "$Name did not pass as fixture-only/ineligible."
    Assert-True ($decision.artifact_count -eq $ExpectedArtifacts) "$Name consumed $($decision.artifact_count), expected $ExpectedArtifacts artifacts."
    return [pscustomobject]@{ Bundle = $bundle; Decision = $decision; Result = $result }
}

function Invoke-FailingFixture($Documents, [string]$Name, [string]$ExpectedReason) {
    $bundle = New-Bundle $Documents $Name
    $result = Invoke-Evaluator $bundle
    Assert-True ($result.ExitCode -eq 0) "$Name was structurally rejected instead of evaluated: $($result.Stderr)"
    $decision = $result.Stdout | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-True (-not $decision.qualification_passed -and $decision.lanes[0].reasons -ccontains $ExpectedReason) "$Name did not report $ExpectedReason."
    Assert-True ($null -eq $decision.lanes[0].auto_entry_projection) "$Name emitted an Auto projection for a failing lane."
    return $decision
}

$TestRoot = Join-Path ([IO.Path]::GetTempPath()) ("scribe-windows-gpu-qualification-" + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($TestRoot) | Out-Null
try {
    $immutablePaths = [ordered]@{ auto = $AutoManifestPath; authority = $AuthorityPath; performance_authority = $PerformanceAuthorityPath; checked_plan = $CheckedPlanPath; evaluator = $ToolPath; toolchain = $ToolchainPath }
    $immutableBefore = [ordered]@{}; foreach ($name in $immutablePaths.Keys) { $immutableBefore[$name] = [IO.File]::ReadAllBytes($immutablePaths[$name]) }
    $evaluatorSource = [IO.File]::ReadAllText($ToolPath, $Utf8)
    foreach ($bound in @('$MaxInputBytes = 16MB', '$MaxLanes = 64', '$MaxArtifacts = 4096', '$MaxArtifactBytes = [UInt64](512MB)', '$MaxControlBytes = 256KB')) { Assert-True ($evaluatorSource.Contains($bound)) "Qualification evaluator changed or removed bound: $bound" }
    Assert-True ($evaluatorSource.Contains('Test-ArtifactBudget ($Context.Count + $Inventory.Count) $Context.Bytes')) 'Evaluator does not reject cumulative declared artifact overflow before opening the next lane inventory.'
    $budgetTokens = $null; $budgetErrors = $null
    $evaluatorAst = [Management.Automation.Language.Parser]::ParseFile($ToolPath, [ref]$budgetTokens, [ref]$budgetErrors)
    Assert-True ($budgetErrors.Count -eq 0) 'Could not parse evaluator for isolated artifact-budget boundary checks.'
    $budgetFunctionAst = $evaluatorAst.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq 'Test-ArtifactBudget' }, $true)
    Assert-True ($null -ne $budgetFunctionAst) 'Evaluator artifact-budget helper is missing.'
    $MaxArtifacts = 4096; [UInt64]$MaxArtifactBytes = 512MB
    Invoke-Expression $budgetFunctionAst.Extent.Text
    Assert-True (Test-ArtifactBudget 4096 ([UInt64](512MB))) 'Isolated evaluator budget helper rejected exact count/byte bounds.'
    Assert-True (-not (Test-ArtifactBudget 4097 ([UInt64](512MB)))) 'Isolated evaluator budget helper accepted count bound + 1.'
    Assert-True (-not (Test-ArtifactBudget 4096 ([UInt64](512MB) + 1))) 'Isolated evaluator budget helper accepted byte bound + 1.'
    Assert-True ([IO.File]::ReadAllText($AutoManifestPath, $Utf8) -ceq $ExpectedAuto) 'Windows Auto manifest is not exact default deny.'
    Assert-True ([IO.File]::ReadAllText($AuthorityPath, $Utf8) -ceq $ExpectedAuthority) 'Windows qualification authority is not exact empty schema v2.'
    Assert-True ([IO.File]::ReadAllText($PerformanceAuthorityPath, $Utf8) -ceq $ExpectedPerformanceAuthority) 'Windows performance authority is not exact empty schema v1.'
    $performanceSmoke = Invoke-PassingPerformanceFixture (New-PerformanceDocuments 'integrated_gpu') 'performance-integrated-smoke' 128
    Assert-True ($performanceSmoke.Decision.lanes[0].evidence_memory_floor.common_minimum_available_memory_bytes -eq 9000000000) 'Performance smoke did not derive its memory floor exclusively from successful GPU starts.'
    $invalidPerformanceSignature = New-Bundle (New-PerformanceDocuments 'integrated_gpu') 'performance-invalid-authorization-signature'
    $invalidPerformanceSignature.Documents.Plan.authorization.signature_base64 = [Convert]::ToBase64String(([byte[]](1..64)))
    Write-Canonical $invalidPerformanceSignature.PlanPath $invalidPerformanceSignature.Documents.Plan
    Assert-Rejected (Invoke-Evaluator $invalidPerformanceSignature) 'Performance invalid authorization signature' 'authorization signature is invalid'
    if ($PerformanceSmokeOnly) { Write-Output 'Windows GPU performance candidate focused smoke tests passed.'; return }

    $performanceDiscrete = Invoke-PassingPerformanceFixture (New-PerformanceDocuments 'discrete_gpu') 'performance-discrete' 64
    Assert-True ($null -eq $performanceDiscrete.Decision.lanes[0].metrics.battery) 'Discrete performance candidate unexpectedly contains battery metrics.'
    $performanceUnified = Invoke-PassingPerformanceFixture (New-PerformanceDocuments 'unified_gpu' 220 110 220 110 'fixture-performance-unified-vulkan' 'intel') 'performance-unified-intel-vulkan' 128

    $repeatResult = Invoke-Evaluator $performanceUnified.Bundle
    Assert-True ($repeatResult.ExitCode -eq 0 -and $repeatResult.Stdout -ceq $performanceUnified.Result.Stdout) 'Repeated stateless performance evaluation was not byte-deterministic.'
    $resigned = Invoke-PassingPerformanceFixture (New-PerformanceDocuments 'unified_gpu' 220 110 220 110 'fixture-performance-unified-vulkan' 'intel') 'performance-resigned-same-policy' 128
    Assert-True ($resigned.Decision.authorization_sha256 -cne $performanceUnified.Decision.authorization_sha256 -and $resigned.Decision.candidate_policy -ceq $performanceUnified.Decision.candidate_policy -and $resigned.Decision.candidate_policy_sha256 -ceq $performanceUnified.Decision.candidate_policy_sha256) 'Re-signing an unchanged performance contract changed candidate policy bytes.'

    $multiLane = New-PerformanceDocuments 'integrated_gpu' 220 110 220 110 'fixture-performance-a-cuda'
    $multiLaneSecond = New-PerformanceDocuments 'unified_gpu' 220 105 220 99 'fixture-performance-b-vulkan' 'intel'
    $multiLane.Evidence.lanes = @($multiLane.Evidence.lanes[0], $multiLaneSecond.Evidence.lanes[0])
    $multiLaneResult = Invoke-PassingPerformanceFixture $multiLane 'performance-multi-lane' 256
    $multiPolicy = $multiLaneResult.Decision.candidate_policy | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-True (@($multiPolicy.entries).Count -eq 2) 'Multi-lane performance candidate did not contain every required lane.'
    $multiEntryJson = @($multiPolicy.entries | ForEach-Object { $_ | ConvertTo-Json -Compress -Depth 64 })
    Assert-True ([StringComparer]::Ordinal.Compare($multiEntryJson[0], $multiEntryJson[1]) -lt 0) 'Performance candidate entries are not strict ordinal runtime-entry ordered.'

    $overlap = New-PerformanceDocuments 'integrated_gpu' 220 110 220 110 'fixture-performance-overlap-a'
    $overlapSecond = New-PerformanceDocuments 'integrated_gpu' 220 110 220 110 'fixture-performance-overlap-b'
    foreach ($run in @($overlapSecond.Evidence.lanes[0].run_sets.cold.gpu) + @($overlapSecond.Evidence.lanes[0].run_sets.warm.gpu) + @($overlapSecond.Evidence.lanes[0].battery.run_sets.cold.gpu) + @($overlapSecond.Evidence.lanes[0].battery.run_sets.warm.gpu)) { $run.available_device_memory_bytes_before = [Int64]8500000000 }
    $overlap.Evidence.lanes = @($overlap.Evidence.lanes[0], $overlapSecond.Evidence.lanes[0])
    Assert-Rejected (Invoke-Evaluator (New-Bundle $overlap 'performance-overlapping-coverage')) 'Performance overlapping coverage' 'duplicate or ambiguous runtime coverage'

    $forbiddenInstallation = New-PerformanceDocuments; $forbiddenInstallation.Evidence.lanes[0].identity.installation = [ordered]@{}
    Assert-Rejected (Invoke-Evaluator (New-Bundle $forbiddenInstallation 'performance-forbidden-installation')) 'Performance installation field' 'unexpected or missing fields'
    $forbiddenFloor = New-PerformanceDocuments; $forbiddenFloor.Evidence.lanes[0].identity.device.qualified_minimum_available_memory_bytes = [Int64]9000000000
    Assert-Rejected (Invoke-Evaluator (New-Bundle $forbiddenFloor 'performance-forbidden-floor')) 'Performance declared floor field' 'unexpected or missing fields'
    $forbiddenScenarios = New-PerformanceDocuments; $forbiddenScenarios.Evidence.lanes[0].scenarios = @()
    Assert-Rejected (Invoke-Evaluator (New-Bundle $forbiddenScenarios 'performance-forbidden-scenarios')) 'Performance scenarios field' 'unexpected or missing fields'

    $missingCapture = New-PerformanceDocuments; $missingCapture.Evidence.lanes[0].battery.captures = @($missingCapture.Evidence.lanes[0].battery.captures | Select-Object -Skip 1)
    Assert-Rejected (Invoke-Evaluator (New-Bundle $missingCapture 'performance-missing-capture')) 'Performance missing capture' 'exactly one raw SCIF capture'
    $powerTransitionPerformance = New-PerformanceDocuments; $powerTransitionPerformance.Evidence.lanes[0].battery.run_sets.warm.gpu[0].power_source_after = 'ac'
    Assert-Rejected (Invoke-Evaluator (New-Bundle $powerTransitionPerformance 'performance-power-transition')) 'Performance power transition' 'power transition'
    $wrongCountPerformance = New-PerformanceDocuments; $wrongCountPerformance.Evidence.lanes[0].run_sets.cold.gpu = @($wrongCountPerformance.Evidence.lanes[0].run_sets.cold.gpu | Select-Object -First 4)
    Assert-Rejected (Invoke-Evaluator (New-Bundle $wrongCountPerformance 'performance-four-cold')) 'Performance four cold runs' 'wrong run count'

    foreach ($thresholdCase in @(
        [ordered]@{ Name = 'ac-cold'; Power = 'ac'; Mode = 'cold'; Value = 226 },
        [ordered]@{ Name = 'ac-warm'; Power = 'ac'; Mode = 'warm'; Value = 111 },
        [ordered]@{ Name = 'battery-cold'; Power = 'battery'; Mode = 'cold'; Value = 226 },
        [ordered]@{ Name = 'battery-warm'; Power = 'battery'; Mode = 'warm'; Value = 111 }
    )) {
        $documents = New-PerformanceDocuments
        $block = if ($thresholdCase.Power -ceq 'ac') { $documents.Evidence.lanes[0] } else { $documents.Evidence.lanes[0].battery }
        foreach ($run in @($block.run_sets[$thresholdCase.Mode].gpu)) { $run.end_to_end_ms = $thresholdCase.Value; $run.backend_ms = $thresholdCase.Value - 10 }
        $decision = Invoke-FailingPerformanceFixture $documents "performance-threshold-$($thresholdCase.Name)" "$($thresholdCase.Power)_gpu_p95_exceeds_cpu_boundary"
        Assert-True ($decision.lanes[0].checks.by_power[$thresholdCase.Power].performance_passed -eq $false) "Performance $($thresholdCase.Name) threshold did not fail its own power bucket."
    }
    $batteryParityPerformance = New-PerformanceDocuments; $batteryParityPerformance.Evidence.lanes[0].battery.run_sets.warm.gpu[0].transcript_sha256 = Get-Digest 'performance-battery-wrong-transcript'
    $null = Invoke-FailingPerformanceFixture $batteryParityPerformance 'performance-battery-parity' 'battery_correctness_not_equivalent'

    $asymmetricPerformance = New-PerformanceDocuments; foreach ($run in @($asymmetricPerformance.Evidence.lanes[0].run_sets.cold.gpu) + @($asymmetricPerformance.Evidence.lanes[0].run_sets.warm.gpu)) { $run.available_device_memory_bytes_before = [Int64]8000000000 }
    $asymmetricResult = Invoke-PassingPerformanceFixture $asymmetricPerformance 'performance-asymmetric-floor' 128
    Assert-True ($asymmetricResult.Decision.lanes[0].evidence_memory_floor.per_power_minimum_available_memory_bytes.ac -eq 8000000000 -and $asymmetricResult.Decision.lanes[0].evidence_memory_floor.per_power_minimum_available_memory_bytes.battery -eq 9000000000 -and $asymmetricResult.Decision.lanes[0].evidence_memory_floor.common_minimum_available_memory_bytes -eq 9000000000) 'Performance common floor is not the conservative maximum of per-power GPU-start minima.'
    $asymmetricPolicy = $asymmetricResult.Decision.candidate_policy | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-True ($asymmetricPolicy.entries[0].minimum_available_memory_bytes -eq 9000000000) 'Performance candidate did not use the derived common memory floor.'

    $allGpuFailed = New-PerformanceDocuments
    foreach ($run in @($allGpuFailed.Evidence.lanes[0].run_sets.cold.gpu) + @($allGpuFailed.Evidence.lanes[0].run_sets.warm.gpu) + @($allGpuFailed.Evidence.lanes[0].battery.run_sets.cold.gpu) + @($allGpuFailed.Evidence.lanes[0].battery.run_sets.warm.gpu)) { Set-RunFailure $run 'provider_error' }
    $allGpuFailedDecision = Invoke-FailingPerformanceFixture $allGpuFailed 'performance-all-gpu-failed' 'ac_correctness_not_equivalent'
    Assert-True ($null -eq $allGpuFailedDecision.lanes[0].evidence_memory_floor.common_minimum_available_memory_bytes) 'All-failed GPU evidence fabricated a memory floor.'

    $captureEqualsApproval = New-PerformanceDocuments
    $captureEqualsApproval.Plan.capture_authority.capture_key_id = 'p256:' + $PerformanceApprovalKeyId.Substring('performance-approval-p256:'.Length)
    $captureEqualsApproval.Plan.capture_authority.capture_public_key_spki_base64 = $PerformanceApprovalSpkiBase64
    Assert-Rejected (Invoke-Evaluator (New-Bundle $captureEqualsApproval 'performance-key-role-reuse')) 'Performance key role reuse' 'must be distinct'

    $pairedPins = New-Bundle (New-PerformanceDocuments) 'performance-paired-pins'
    $authDigest = Get-CanonicalDigest $pairedPins.Documents.Plan.authorization
    Assert-Rejected (Invoke-Evaluator $pairedPins $true $false '' '' '' $true @('-ExpectedAuthorizationSha256', $authDigest)) 'Performance unpaired pin' 'must be supplied together'
    Assert-Rejected (Invoke-Evaluator $pairedPins $true $false '' '' '' $true @('-ExpectedAuthorizationSha256', $authDigest, '-ExpectedCampaignNonce', (Get-Digest 'wrong-campaign-pin'))) 'Performance wrong nonce pin' 'differs from the caller pin'
    Assert-Rejected (Invoke-Evaluator $pairedPins $true $false '' '' '' $true @('-ExpectedAuthorizationSha256', (Get-Digest 'wrong-authorization-pin'), '-ExpectedCampaignNonce', $pairedPins.Documents.Plan.capture_authority.campaign_nonce)) 'Performance wrong authorization pin' 'differs from the caller pin'
    Assert-Rejected (Invoke-Evaluator $pairedPins $false) 'Performance fixture without admission' '-FixtureNowUnixSeconds is allowed only'

    $issuedAtNow = New-Bundle (New-PerformanceDocuments) 'performance-issued-at-now'; $issuedAtNow.Documents.Plan.authorization.record.issued_at_unix_seconds = $FixtureNow; $issuedAtNow.Documents.Plan.authorization.record.expires_at_unix_seconds = $FixtureNow + 300; Resign-PerformanceBundle $issuedAtNow
    $issuedAtNowResult = Invoke-Evaluator $issuedAtNow
    Assert-True ($issuedAtNowResult.ExitCode -eq 0) "Performance authorization rejected issued==now: $($issuedAtNowResult.Stderr)"
    foreach ($timeCase in @(
        [ordered]@{ Name = 'now-equals-expires'; Issued = $FixtureNow - 300; Expires = $FixtureNow; Expected = 'not currently valid' },
        [ordered]@{ Name = 'future-issued'; Issued = $FixtureNow + 1; Expires = $FixtureNow + 300; Expected = 'not currently valid' },
        [ordered]@{ Name = 'window-over-seven-days'; Issued = $FixtureNow; Expires = $FixtureNow + 604801; Expected = 'exceeds seven days' },
        [ordered]@{ Name = 'zero-issued'; Issued = 0; Expires = $FixtureNow + 300; Expected = 'bounded JSON integer' },
        [ordered]@{ Name = 'negative-issued'; Issued = -1; Expires = $FixtureNow + 300; Expected = 'bounded JSON integer' },
        [ordered]@{ Name = 'timestamp-overflow'; Issued = $FixtureNow; Expires = [Int64]253402300800; Expected = 'bounded JSON integer' }
    )) {
        $bundle = New-Bundle (New-PerformanceDocuments) "performance-$($timeCase.Name)"
        $bundle.Documents.Plan.authorization.record.issued_at_unix_seconds = [Int64]$timeCase.Issued; $bundle.Documents.Plan.authorization.record.expires_at_unix_seconds = [Int64]$timeCase.Expires
        Resign-PerformanceBundle $bundle
        Assert-Rejected (Invoke-Evaluator $bundle) "Performance $($timeCase.Name)" $timeCase.Expected
    }
    foreach ($clockCase in @(
        [ordered]@{ Name = 'zero-clock'; Value = '0' },
        [ordered]@{ Name = 'negative-clock'; Value = '-1' },
        [ordered]@{ Name = 'overflow-clock'; Value = '253402300800' }
    )) { Assert-Rejected (Invoke-Evaluator $pairedPins $true $false '' '' '' $false @('-FixtureNowUnixSeconds', $clockCase.Value)) "Performance $($clockCase.Name)" 'positive bounded Unix timestamp' }

    $wrongAuthorizationDomain = New-Bundle (New-PerformanceDocuments) 'performance-wrong-authorization-domain'; Resign-PerformanceBundle $wrongAuthorizationDomain $AttestationDomain
    Assert-Rejected (Invoke-Evaluator $wrongAuthorizationDomain) 'Performance wrong authorization domain' 'authorization signature is invalid'
    $wrongLaneDomain = New-Bundle (New-PerformanceDocuments) 'performance-wrong-lane-domain'; Resign-PerformanceBundle $wrongLaneDomain $PerformanceAuthorizationDomain $AttestationDomain
    Assert-Rejected (Invoke-Evaluator $wrongLaneDomain) 'Performance wrong lane domain' 'attestation signature is invalid'

    $expired = New-Bundle (New-PerformanceDocuments) 'performance-expired-authorization'; $expired.Documents.Plan.authorization.record.expires_at_unix_seconds = $FixtureNow
    Write-Canonical $expired.PlanPath $expired.Documents.Plan
    Assert-Rejected (Invoke-Evaluator $expired) 'Performance expired authorization' 'not currently valid'
    foreach ($epochCase in @(
        [ordered]@{ Name = 'zero'; Value = [Int64]0 },
        [ordered]@{ Name = 'negative'; Value = [Int64]-1 },
        [ordered]@{ Name = 'overflow'; Value = [Int64]4294967296 }
    )) {
        $epochBundle = New-Bundle (New-PerformanceDocuments) "performance-$($epochCase.Name)-epoch"; $epochBundle.Documents.Plan.authorization.record.policy_epoch = $epochCase.Value
        Resign-PerformanceBundle $epochBundle
        Assert-Rejected (Invoke-Evaluator $epochBundle) "Performance $($epochCase.Name) epoch" 'bounded JSON integer'
    }
    $contractMutation = New-Bundle (New-PerformanceDocuments) 'performance-contract-mutation'; $contractMutation.Documents.Plan.capture_contract.power_policy = 'different-power-policy'
    Write-Canonical $contractMutation.PlanPath $contractMutation.Documents.Plan
    Assert-Rejected (Invoke-Evaluator $contractMutation) 'Performance capture contract mutation' 'power policy is unsupported'
    $commaJoinedScopes = New-PerformanceDocuments; $commaJoinedScopes.Plan.capture_contract.launch_scopes = @('cpu,provider_discovery,selected_device')
    Assert-Rejected (Invoke-Evaluator (New-Bundle $commaJoinedScopes 'performance-comma-joined-launch-scopes')) 'Performance comma-joined launch scopes' 'launch scopes are not canonical'

    $legacyNewArgument = New-Bundle (New-FixtureDocuments) 'legacy-performance-argument'
    foreach ($legacyArgument in @(
        [ordered]@{ Name = '-ExpectedAuthorizationSha256'; Value = Get-Digest 'legacy-auth-arg' },
        [ordered]@{ Name = '-ExpectedCampaignNonce'; Value = Get-Digest 'legacy-nonce-arg' },
        [ordered]@{ Name = '-FixtureNowUnixSeconds'; Value = [string]$FixtureNow }
    )) { Assert-Rejected (Invoke-Evaluator $legacyNewArgument $true $false '' '' '' $false @($legacyArgument.Name, $legacyArgument.Value)) "Legacy performance argument $($legacyArgument.Name)" 'Performance-only arguments' }

    $wrongSource = New-PerformanceDocuments; $wrongSource.Plan.source.revision = ('a' * 40)
    Assert-Rejected (Invoke-Evaluator (New-Bundle $wrongSource 'performance-wrong-source-revision')) 'Performance wrong source revision' 'app build does not match'
    $cpuMismatch = New-PerformanceDocuments 'integrated_gpu' 220 110 220 110 'fixture-performance-cpu-a'
    $cpuMismatchSecond = New-PerformanceDocuments 'unified_gpu' 220 110 220 110 'fixture-performance-cpu-b' 'intel'; $cpuMismatchSecond.Evidence.lanes[0].identity.cpu_baseline.worker_sha256 = Get-Digest 'different-cpu-worker'
    $cpuMismatch.Evidence.lanes = @($cpuMismatch.Evidence.lanes[0], $cpuMismatchSecond.Evidence.lanes[0])
    Assert-Rejected (Invoke-Evaluator (New-Bundle $cpuMismatch 'performance-cpu-identity-mismatch')) 'Performance CPU identity mismatch' 'one exact CPU baseline'
    $packMismatch = New-PerformanceDocuments 'integrated_gpu' 220 110 220 110 'fixture-performance-pack-a'
    $packMismatchSecond = New-PerformanceDocuments 'integrated_gpu' 220 110 220 110 'fixture-performance-pack-b'; $packMismatchSecond.Evidence.lanes[0].identity.pack.pack_version = '0.1.0-other'; $packMismatchSecond.Evidence.lanes[0].identity.pack.pack_digest = Get-Digest 'other-pack'
    $packMismatch.Evidence.lanes = @($packMismatch.Evidence.lanes[0], $packMismatchSecond.Evidence.lanes[0])
    Assert-Rejected (Invoke-Evaluator (New-Bundle $packMismatch 'performance-backend-pack-mismatch')) 'Performance backend pack mismatch' 'mix pack/worker release identities'

    $partialFailure = New-PerformanceDocuments 'integrated_gpu' 220 110 220 110 'fixture-performance-a-pass-lane'
    $partialFailureSecond = New-PerformanceDocuments 'unified_gpu' 220 110 220 110 'fixture-performance-b-fail-lane' 'intel'; Set-RunFailure $partialFailureSecond.Evidence.lanes[0].battery.run_sets.warm.gpu[0] 'timeout'
    $partialFailure.Evidence.lanes = @($partialFailure.Evidence.lanes[0], $partialFailureSecond.Evidence.lanes[0])
    $partialFailureBundle = New-Bundle $partialFailure 'performance-partial-multi-lane-failure'; $partialFailureResult = Invoke-Evaluator $partialFailureBundle
    Assert-True ($partialFailureResult.ExitCode -eq 0) "Partial multi-lane failure was structurally rejected: $($partialFailureResult.Stderr)"
    $partialFailureDecision = $partialFailureResult.Stdout | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-True (-not $partialFailureDecision.performance_passed -and $null -eq $partialFailureDecision.candidate_policy -and $null -eq $partialFailureDecision.candidate_policy_sha256) 'A partially failed multi-lane campaign emitted a partial candidate policy.'

    $productionRelabel = New-Bundle (New-PerformanceDocuments) 'performance-production-relabel'
    $productionRelabel.Documents.Plan.fixture_only = $false; $productionRelabel.Documents.Evidence.fixture_only = $false
    $productionRelabel.Documents.Plan.authorization.Remove('fixture_approval_public_key_spki_base64')
    $productionRelabel.Documents.Plan.authorization.record.performance_contract_sha256 = Get-CanonicalDigest (Get-PerformanceContractProjection $productionRelabel.Documents.Plan)
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds(); $productionRelabel.Documents.Plan.authorization.record.issued_at_unix_seconds = $now - 3600; $productionRelabel.Documents.Plan.authorization.record.expires_at_unix_seconds = $now + 3600
    Resign-PerformanceBundle $productionRelabel
    $productionAuthDigest = Get-CanonicalDigest $productionRelabel.Documents.Plan.authorization
    Assert-Rejected (Invoke-Evaluator $productionRelabel $false $false '' '' '' $false @('-ExpectedAuthorizationSha256', $productionAuthDigest, '-ExpectedCampaignNonce', $productionRelabel.Documents.Plan.capture_authority.campaign_nonce)) 'Performance production relabel' 'not approved by the protected campaign authority'

    $performanceTamper = New-Bundle (New-PerformanceDocuments) 'performance-artifact-tamper'
    $performanceTamperPath = Join-Path $performanceTamper.ArtifactRoot ($performanceTamper.Documents.Evidence.lanes[0].battery.run_sets.warm.gpu[0].artifact_path.Replace('/', '\'))
    [IO.File]::WriteAllText($performanceTamperPath, 'tampered', $Utf8)
    Assert-Rejected (Invoke-Evaluator $performanceTamper) 'Performance artifact tamper' 'digest does not match the supplied file'

    $gitReadAst = $evaluatorAst.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq 'Invoke-GitRead' }, $true)
    $sourceCheckoutAst = $evaluatorAst.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq 'Assert-ProductionSourceCheckout' }, $true)
    Assert-True ($null -ne $gitReadAst -and $null -ne $sourceCheckoutAst) 'Evaluator production source-check helpers are missing.'
    Invoke-Expression $gitReadAst.Extent.Text; Invoke-Expression $sourceCheckoutAst.Extent.Text
    $sourceFixture = Join-Path $TestRoot 'source-checkout-fixture'; [IO.Directory]::CreateDirectory($sourceFixture) | Out-Null
    & git -C $sourceFixture init --quiet; Assert-True ($LASTEXITCODE -eq 0) 'Could not initialize isolated source-check fixture.'
    [IO.File]::WriteAllText((Join-Path $sourceFixture 'tracked.txt'), 'tracked', $Utf8)
    & git -C $sourceFixture add tracked.txt; & git -C $sourceFixture -c user.name=ScribeFixture -c user.email=fixture@example.invalid -c commit.gpgsign=false -c core.hooksPath=NUL commit --quiet --no-gpg-sign -m fixture
    Assert-True ($LASTEXITCODE -eq 0) 'Could not commit isolated source-check fixture.'
    $sourceHead = (& git -C $sourceFixture rev-parse HEAD).Trim()
    Assert-ProductionSourceCheckout $sourceFixture $sourceHead
    $savedGitEnvironment = [ordered]@{}
    foreach ($name in @('GIT_DIR', 'GIT_CONFIG_COUNT', 'GIT_CONFIG_KEY_0', 'GIT_CONFIG_VALUE_0')) { $savedGitEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
    try {
        $env:GIT_DIR = Join-Path $TestRoot 'redirected-git-dir'
        $env:GIT_CONFIG_COUNT = '1'; $env:GIT_CONFIG_KEY_0 = 'core.repositoryformatversion'; $env:GIT_CONFIG_VALUE_0 = '999'
        Assert-ProductionSourceCheckout $sourceFixture $sourceHead
    }
    finally {
        foreach ($name in $savedGitEnvironment.Keys) { [Environment]::SetEnvironmentVariable($name, $savedGitEnvironment[$name], 'Process') }
    }
    [IO.File]::WriteAllText((Join-Path $sourceFixture 'untracked.txt'), 'untracked', $Utf8)
    $untrackedRejected = $false; try { Assert-ProductionSourceCheckout $sourceFixture $sourceHead } catch { $untrackedRejected = $_.Exception.Message.Contains('untracked changes') }
    Assert-True $untrackedRejected 'Production source check accepted an untracked file.'
    $wrongHeadRejected = $false; try { Assert-ProductionSourceCheckout $sourceFixture ('f' * 40) } catch { $wrongHeadRejected = $_.Exception.Message.Contains('HEAD differs') }
    Assert-True $wrongHeadRejected 'Production source check accepted the wrong authorized HEAD.'
    if ($PerformanceOnly) { Write-Output 'Windows GPU performance candidate focused contract tests passed.'; return }
    $checkedPlanRaw = [IO.File]::ReadAllText($CheckedPlanPath, $Utf8)
    $checkedPlan = $checkedPlanRaw | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-True ($checkedPlan.schema_version -eq 3 -and $checkedPlan.capture_contract.power_policy -ceq 'ac_for_discrete_ac_and_battery_for_integrated_or_unified' -and -not $checkedPlan.fixture_only -and $checkedPlan.required_lanes.Count -eq 0 -and -not $checkedPlan.runtime_bucket_complete) 'Checked-in plan is not canonical schema-v3 production default deny.'
    Assert-True (-not $checkedPlan.capture_authority.ContainsKey('fixture_capture_public_key_spki_base64')) 'Checked-in production plan contains a fixture capture key.'
    Assert-True ($checkedPlanRaw -ceq $Utf8.GetString((Get-CanonicalBytes $checkedPlan))) 'Checked-in plan is not canonical LF JSON.'
    $checkedEvidencePath = Join-Path $TestRoot 'checked-empty-evidence.json'; Write-Canonical $checkedEvidencePath ([ordered]@{ fixture_only = $false; kind = 'windows_gpu_release_qualification_evidence'; lanes = @(); plan_sha256 = Get-FileDigest $CheckedPlanPath; schema_version = 3 })
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

    # Schema v3 proves AC and battery independently for shared-memory GPUs,
    # while discrete GPUs keep the exact AC-only evidence inventory.
    $v3Integrated = Invoke-PassingFixture (New-V3FixtureDocuments 'integrated_gpu') 'v3-integrated' 139
    Assert-True ($v3Integrated.Decision.schema_version -eq 3 -and $null -ne $v3Integrated.Decision.lanes[0].metrics.battery) 'Integrated v3 decision did not report battery metrics.'
    Assert-True ((Invoke-Evaluator $v3Integrated.Bundle $true $true).ExitCode -eq 2) 'Passing v3 fixture -RequireEligible did not return valid-ineligible exit 2.'
    $v3Unified = Invoke-PassingFixture (New-V3FixtureDocuments 'unified_gpu') 'v3-unified' 139
    foreach ($completeClass in @('integrated_gpu', 'unified_gpu')) {
        $completeDocuments = New-V3FixtureDocuments $completeClass; $completeDocuments.Plan.runtime_bucket_complete = $true
        $completeDecision = (Invoke-PassingFixture $completeDocuments "v3-complete-$completeClass" 139).Decision
        Assert-True ($completeDecision.qualification_passed -and -not $completeDecision.activation_manifest_complete -and -not $completeDecision.auto_eligible) "Complete v3 $completeClass fixture did not pass qualification while remaining inactive/ineligible against empty Auto."
    }
    $v3Discrete = Invoke-PassingFixture (New-V3FixtureDocuments 'discrete_gpu') 'v3-discrete' 75
    Assert-True ($null -eq $v3Discrete.Decision.lanes[0].metrics.battery) 'Discrete v3 decision invented battery performance metrics.'
    $discreteBatteryScenarioFailure = New-V3FixtureDocuments 'discrete_gpu'; $discreteBatteryScenario = @($discreteBatteryScenarioFailure.Evidence.lanes[0].scenarios | Where-Object scenario -ceq 'power_battery')[0]
    $discreteBatteryScenario.result = 'fail'; $discreteBatteryScenario.active_request_migrated = $true; $discreteBatteryScenario.partial_output_replayed = $true
    $discreteFailureDecision = Invoke-FailingFixture $discreteBatteryScenarioFailure 'v3-discrete-battery-scenario-failure' 'battery_scenario_evidence_failed'
    Assert-True ($null -eq $discreteFailureDecision.lanes[0].auto_entry_projection -and -not $discreteFailureDecision.lanes[0].checks.scenarios_passed) 'Discrete v3 battery scenario failure did not suppress its projection/global scenario check.'
    $v3IntelDocuments = New-V3FixtureDocuments 'integrated_gpu'; Set-VulkanFixture $v3IntelDocuments 'intel' '8086'
    $null = Invoke-PassingFixture $v3IntelDocuments 'v3-integrated-intel-vulkan' 139
    $v3ProductionRelabel = New-V3FixtureDocuments; $v3ProductionRelabel.Plan.fixture_only = $false; $v3ProductionRelabel.Plan.capture_authority.Remove('fixture_capture_public_key_spki_base64'); $v3ProductionRelabel.Evidence.fixture_only = $false
    Assert-Rejected (Invoke-Evaluator (New-Bundle $v3ProductionRelabel 'v3-fixture-production-relabel') $false) 'V3 fixture relabeled production' 'not approved by the protected production authority'

    # Strict schema separation and required/null paired-power shapes.
    $mixedSchema = New-V3FixtureDocuments; $mixedSchema.Evidence.schema_version = 2
    Assert-Rejected (Invoke-Evaluator (New-Bundle $mixedSchema 'v3-mixed-schema')) 'Mixed plan/evidence schemas' 'schema versions differ'
    $v2PowerPolicy = New-FixtureDocuments; $v2PowerPolicy.Plan.capture_contract.power_policy = 'ac_for_discrete_ac_and_battery_for_integrated_or_unified'
    Assert-Rejected (Invoke-Evaluator (New-Bundle $v2PowerPolicy 'v2-power-policy')) 'Schema v2 accepted v3 power policy' 'unexpected or missing fields'
    $missingBatteryIdentity = New-V3FixtureDocuments; $missingBatteryIdentity.Evidence.lanes[0].identity.Remove('battery_acquisition'); $missingBatteryIdentity.Evidence.lanes[0].battery = $null
    Assert-Rejected (Invoke-Evaluator (New-Bundle $missingBatteryIdentity 'v3-missing-battery-identity')) 'Missing v3 battery acquisition' 'unexpected or missing fields'
    $nullBatteryBlock = New-V3FixtureDocuments; $nullBatteryBlock.Evidence.lanes[0].battery = $null
    Assert-Rejected (Invoke-Evaluator (New-Bundle $nullBatteryBlock 'v3-null-battery-block')) 'Null shared-memory battery block' 'is required'

    # The paired acquisitions share machine/options/harness/topology/threading
    # and device inventory, but use distinct power-bound batches and plans.
    foreach ($pairedCase in @(
        [pscustomobject]@{ Name = 'machine'; Mutate = { param($a) $a.machine_id_sha256 = Get-Digest 'other-machine' }; Expected = 'machine_id_sha256 values differ' },
        [pscustomobject]@{ Name = 'options'; Mutate = { param($a) $a.options_sha256 = Get-Digest 'other-options' }; Expected = 'options_sha256 values differ' },
        [pscustomobject]@{ Name = 'harness'; Mutate = { param($a) $a.protocol.harness_sha256 = Get-Digest 'other-harness' }; Expected = 'protocol identities differ' },
        [pscustomobject]@{ Name = 'topology'; Mutate = { param($a) $a.host.total_memory_bytes++ }; Expected = 'host identities differ' },
        [pscustomobject]@{ Name = 'threading'; Mutate = { param($a) $a.threading.gpu_affinity_sha256 = Get-Digest 'other-affinity' }; Expected = 'threading identities differ' },
        [pscustomobject]@{ Name = 'devices'; Mutate = { param($a) $a.device_set.snapshot_sha256 = Get-Digest 'other-device-set' }; Expected = 'does not match the canonical complete device inventory' }
    )) {
        $documents = New-V3FixtureDocuments; & $pairedCase.Mutate $documents.Evidence.lanes[0].identity.battery_acquisition
        Assert-Rejected (Invoke-Evaluator (New-Bundle $documents "v3-paired-$($pairedCase.Name)")) "V3 paired acquisition $($pairedCase.Name)" $pairedCase.Expected
    }
    $selfConsistentDeviceMismatch = New-V3FixtureDocuments; $selfConsistentBattery = $selfConsistentDeviceMismatch.Evidence.lanes[0].identity.battery_acquisition
    $selfConsistentBattery.device_set.devices[1].total_memory_bytes--
    Sync-DeviceSetBindings $selfConsistentDeviceMismatch; Sync-Captures $selfConsistentDeviceMismatch
    Assert-Rejected (Invoke-Evaluator (New-Bundle $selfConsistentDeviceMismatch 'v3-paired-self-consistent-device-set')) 'Self-consistent cross-power device-set mismatch' 'device_set identities differ'
    $sameBatch = New-V3FixtureDocuments; $sameBatch.Evidence.lanes[0].identity.battery_acquisition.batch_id = $sameBatch.Evidence.lanes[0].identity.acquisition.batch_id
    Assert-Rejected (Invoke-Evaluator (New-Bundle $sameBatch 'v3-same-batch')) 'Reused AC/battery batch' 'batches must differ'

    # Power metadata, sessions, generations, captures, challenges, and scenario
    # bindings cannot be substituted across AC and battery.
    $powerTransition = New-V3FixtureDocuments; $powerTransition.Evidence.lanes[0].battery.run_sets.warm.gpu[0].power_source_after = 'ac'
    Assert-Rejected (Invoke-Evaluator (New-Bundle $powerTransition 'v3-power-transition')) 'Battery run power transition' 'power transition'
    $captureRelabel = New-V3FixtureDocuments; $captureRelabel.Evidence.lanes[0].battery.captures[0].power_source_before = 'ac'
    Assert-Rejected (Invoke-Evaluator (New-Bundle $captureRelabel 'v3-capture-power-relabel')) 'Battery capture relabeled AC' 'power transition'
    $crossSession = New-V3FixtureDocuments; $crossSession.Evidence.lanes[0].battery.run_sets.warm.cpu[0].session_id = $crossSession.Evidence.lanes[0].run_sets.warm.cpu[0].session_id
    Assert-Rejected (Invoke-Evaluator (New-Bundle $crossSession 'v3-cross-session')) 'Cross-power session reuse' 'session violates'
    $crossGeneration = New-V3FixtureDocuments; $crossGeneration.Evidence.lanes[0].battery.run_sets.cold.gpu[0].execution.worker_generation = $crossGeneration.Evidence.lanes[0].run_sets.cold.gpu[0].execution.worker_generation
    Assert-Rejected (Invoke-Evaluator (New-Bundle $crossGeneration 'v3-cross-generation')) 'Cross-power generation reuse' 'worker_generation'
    $crossCapture = New-V3FixtureDocuments; $crossCapture.Evidence.lanes[0].battery.run_sets.cold.gpu[0].execution.capture_sha256 = $crossCapture.Evidence.lanes[0].run_sets.cold.gpu[0].execution.capture_sha256
    Assert-Rejected (Invoke-Evaluator (New-Bundle $crossCapture 'v3-cross-capture')) 'Cross-power capture reuse' 'reused one raw capture'
    $crossChallenge = New-V3FixtureDocuments; $acCapture = $crossChallenge.Evidence.lanes[0].captures[0]; $batteryCapture = $crossChallenge.Evidence.lanes[0].battery.captures[0]; $challenge = (Get-ScifControl $acCapture.request_frame_base64).challenge
    Update-CaptureControl $batteryCapture 'request' { param($c) $c.challenge = $challenge }; Update-CaptureControl $batteryCapture 'response' { param($c) $c.capability.challenge = $challenge }
    Assert-Rejected (Invoke-Evaluator (New-Bundle $crossChallenge 'v3-cross-challenge')) 'Cross-power challenge reuse' 'unique challenge'
    $crossScenarioCapture = New-V3FixtureDocuments; $batteryScenario = @($crossScenarioCapture.Evidence.lanes[0].scenarios | Where-Object scenario -ceq 'power_battery')[0]; $batteryScenario.selected_capture_sha256 = Get-CanonicalDigest (Get-RecordWithoutArtifact @($crossScenarioCapture.Evidence.lanes[0].captures | Where-Object { $_.generation -like '*:warm:gpu' })[0])
    Assert-Rejected (Invoke-Evaluator (New-Bundle $crossScenarioCapture 'v3-cross-scenario-capture')) 'Battery scenario borrowed AC capture' 'matching power acquisition'
    $reboundNonBatteryScenario = New-V3FixtureDocuments; $reboundScenarioLane = $reboundNonBatteryScenario.Evidence.lanes[0]; $reboundScenario = @($reboundScenarioLane.scenarios | Where-Object scenario -ceq 'suspend_resume')[0]
    $reboundScenario.power_source = 'battery'; $reboundScenario.power_source_before = 'battery'; $reboundScenario.power_source_after = 'battery'; $reboundScenario.acquisition_sha256 = Get-CanonicalDigest $reboundScenarioLane.identity.battery_acquisition
    $reboundScenario.selected_capture_sha256 = Get-CanonicalDigest (Get-RecordWithoutArtifact @($reboundScenarioLane.battery.captures | Where-Object { $_.generation -like '*:warm:gpu' })[0]); $reboundScenario.device_set_sha256 = $reboundScenarioLane.identity.battery_acquisition.device_set.snapshot_sha256
    Assert-Rejected (Invoke-Evaluator (New-Bundle $reboundNonBatteryScenario 'v3-rebound-nonbattery-scenario')) 'Non-battery scenario rebound to battery' 'canonical power source'

    # Exact independent cardinalities and acquisition ordering controls.
    foreach ($countCase in @(
        [pscustomobject]@{ Name = 'cold-4'; Mode = 'cold'; Count = 4 }, [pscustomobject]@{ Name = 'cold-6'; Mode = 'cold'; Count = 6 },
        [pscustomobject]@{ Name = 'warm-19'; Mode = 'warm'; Count = 19 }, [pscustomobject]@{ Name = 'warm-21'; Mode = 'warm'; Count = 21 }
    )) {
        $documents = New-V3FixtureDocuments; $runs = @($documents.Evidence.lanes[0].battery.run_sets[$countCase.Mode].gpu)
        if ($countCase.Count -lt $runs.Count) { $documents.Evidence.lanes[0].battery.run_sets[$countCase.Mode].gpu = @($runs | Select-Object -First $countCase.Count) }
        else {
            $extraRun = Copy-Document $runs[-1]
            $extraRun.artifact_path = "fixture-windows-nvidia-cuda/battery/runs/$($countCase.Mode)/gpu/$('{0:d2}' -f $countCase.Count).evidence"
            $extraRun.sequence = $countCase.Count
            $documents.Evidence.lanes[0].battery.run_sets[$countCase.Mode].gpu = @($runs + $extraRun)
        }
        Assert-Rejected (Invoke-Evaluator (New-Bundle $documents "v3-$($countCase.Name)")) "V3 $($countCase.Name)" 'wrong run count'
    }
    foreach ($controlCase in @(
        [pscustomobject]@{ Name = 'order'; Mutate = { param($r) $r.pair_order = 'gpu_then_cpu' }; Expected = 'order violates' },
        [pscustomobject]@{ Name = 'reset'; Mutate = { param($r) $r.reset_state = 'fresh_process_fresh_model' }; Expected = 'reset state violates' },
        [pscustomobject]@{ Name = 'priming'; Mutate = { param($r) $r.priming_runs = 0 }; Expected = 'priming violates' }
    )) {
        $documents = New-V3FixtureDocuments; & $controlCase.Mutate $documents.Evidence.lanes[0].battery.run_sets.warm.gpu[0]
        Assert-Rejected (Invoke-Evaluator (New-Bundle $documents "v3-$($controlCase.Name)")) "V3 $($controlCase.Name)" $controlCase.Expected
    }

    # Inclusive exact 110% boundary for both modes and powers.
    $exactBoundaryDocuments = New-V3FixtureDocuments 'integrated_gpu' 220 110 220 110
    foreach ($sets in @($exactBoundaryDocuments.Evidence.lanes[0].run_sets, $exactBoundaryDocuments.Evidence.lanes[0].battery.run_sets)) {
        foreach ($run in @($sets.cold.cpu)) { $run.end_to_end_ms = 200; $run.backend_ms = 190 }
    }
    $exactBoundary = Invoke-PassingFixture $exactBoundaryDocuments 'v3-exact-110-boundary' 139
    foreach ($performanceCase in @(
        [pscustomobject]@{ Name = 'ac-cold'; Power = 'ac'; Mode = 'cold'; Value = 221 },
        [pscustomobject]@{ Name = 'ac-warm'; Power = 'ac'; Mode = 'warm'; Value = 111 },
        [pscustomobject]@{ Name = 'battery-cold'; Power = 'battery'; Mode = 'cold'; Value = 221 },
        [pscustomobject]@{ Name = 'battery-warm'; Power = 'battery'; Mode = 'warm'; Value = 111 }
    )) {
        $documents = New-V3FixtureDocuments 'integrated_gpu' 220 110 220 110
        $sets = if ($performanceCase.Power -ceq 'ac') { $documents.Evidence.lanes[0].run_sets } else { $documents.Evidence.lanes[0].battery.run_sets }
        if ($performanceCase.Mode -ceq 'cold') { foreach ($cpuRun in @($sets.cold.cpu)) { $cpuRun.end_to_end_ms = 200; $cpuRun.backend_ms = 190 } }
        foreach ($run in @($sets[$performanceCase.Mode].gpu)) { $run.end_to_end_ms = $performanceCase.Value; $run.backend_ms = $performanceCase.Value - 10 }
        $null = Invoke-FailingFixture $documents "v3-over-$($performanceCase.Name)" "$($performanceCase.Power)_gpu_p95_exceeds_cpu_boundary"
    }

    # Battery failures are not hidden by successful AC evidence.
    foreach ($failureCase in @(
        [pscustomobject]@{ Name = 'timeout'; Category = 'timeout' },
        [pscustomobject]@{ Name = 'cancelled'; Category = 'cancelled' },
        [pscustomobject]@{ Name = 'partial-output'; Category = 'partial_output' }
    )) {
        $documents = New-V3FixtureDocuments; Set-RunFailure $documents.Evidence.lanes[0].battery.run_sets.warm.gpu[0] $failureCase.Category
        $decision = Invoke-FailingFixture $documents "v3-battery-$($failureCase.Name)" 'battery_reliability_not_equivalent'
        Assert-True ($decision.lanes[0].reasons -ccontains 'battery_correctness_not_equivalent') "Battery $($failureCase.Name) did not also suppress correctness."
    }
    $batteryParity = New-V3FixtureDocuments; $batteryParity.Evidence.lanes[0].battery.run_sets.warm.gpu[0].transcript_sha256 = Get-Digest 'battery-wrong-transcript'
    $null = Invoke-FailingFixture $batteryParity 'v3-battery-parity' 'battery_correctness_not_equivalent'

    # The common floor is max(per-power minima); lower valid AC observations
    # remain acceptable and battery/AC scenarios contribute only to their power.
    $asymmetricMemory = New-V3FixtureDocuments; $asymmetricLane = $asymmetricMemory.Evidence.lanes[0]
    foreach ($mode in @('cold', 'warm')) { foreach ($run in @($asymmetricLane.run_sets[$mode].gpu)) { $run.available_device_memory_bytes_before = [Int64]8000000000 } }
    foreach ($scenario in @($asymmetricLane.scenarios | Where-Object { $_.power_source -ceq 'ac' -and $_.selected_backend -ceq $asymmetricLane.identity.backend })) { $scenario.available_device_memory_bytes = [Int64]8000000000 }
    $asymmetricLane.identity.device.qualified_minimum_available_memory_bytes = [Int64]9000000000
    $asymmetricDecision = (Invoke-PassingFixture $asymmetricMemory 'v3-asymmetric-memory' 139).Decision
    Assert-True ($asymmetricDecision.lanes[0].evidence_memory_floor.per_power_minimum_available_memory_bytes.ac -eq 8000000000 -and $asymmetricDecision.lanes[0].evidence_memory_floor.per_power_minimum_available_memory_bytes.battery -eq 9000000000 -and $asymmetricDecision.lanes[0].evidence_memory_floor.common_minimum_available_memory_bytes -eq 9000000000) 'V3 memory floor did not use max(per-power minima).'
    $lowerDeclaredFloor = Copy-Document $asymmetricMemory; $lowerDeclaredFloor.Evidence.lanes[0].identity.device.qualified_minimum_available_memory_bytes = [Int64]8000000000
    Assert-Rejected (Invoke-Evaluator (New-Bundle $lowerDeclaredFloor 'v3-lower-declared-floor')) 'Lower common memory floor' 'conservative maximum'
    $batteryScenarioFloor = New-V3FixtureDocuments; $batteryScenarioLane = $batteryScenarioFloor.Evidence.lanes[0]; @($batteryScenarioLane.scenarios | Where-Object scenario -ceq 'power_battery')[0].available_device_memory_bytes = [Int64]7000000000
    $batteryScenarioLane.identity.device.qualified_minimum_available_memory_bytes = [Int64]9000000000
    # AC remains 9 GB, so a battery-only 7 GB observation cannot lower the
    # common max floor below the independently exercised AC requirement.
    $null = Invoke-PassingFixture $batteryScenarioFloor 'v3-battery-scenario-attribution' 139

    # Select the warm p95 pair from the worst same-power ratio, never from
    # independent maxima. Battery wins despite a lower absolute GPU time.
    $worstRatio = New-V3FixtureDocuments 'integrated_gpu' 220 100 220 88
    foreach ($run in @($worstRatio.Evidence.lanes[0].battery.run_sets.warm.cpu)) { $run.end_to_end_ms = 80; $run.backend_ms = 70 }
    $worstRatioDecision = (Invoke-PassingFixture $worstRatio 'v3-worst-same-power-ratio' 139).Decision
    $worstEvidence = $worstRatioDecision.lanes[0].auto_entry_projection.evidence
    Assert-True ($worstEvidence.gpu_p95_ms -eq 88 -and $worstEvidence.cpu_p95_ms -eq 80) 'Worst-ratio projection did not retain the same-power battery pair.'
    $ratioTie = New-V3FixtureDocuments 'integrated_gpu' 220 110 220 99
    foreach ($run in @($ratioTie.Evidence.lanes[0].battery.run_sets.warm.cpu)) { $run.end_to_end_ms = 90; $run.backend_ms = 80 }
    $tieEvidence = (Invoke-PassingFixture $ratioTie 'v3-ratio-tie-ac' 139).Decision.lanes[0].auto_entry_projection.evidence
    Assert-True ($tieEvidence.gpu_p95_ms -eq 110 -and $tieEvidence.cpu_p95_ms -eq 100) 'Equal ratios did not deterministically choose AC.'

    # Both powers are included in projection digests; changing battery cold or
    # warm evidence changes the corresponding digest, and parity corruption
    # suppresses the projection entirely.
    $baselineProjection = $v3Integrated.Decision.lanes[0].auto_entry_projection.evidence
    $changedBatteryCold = New-V3FixtureDocuments; $changedBatteryCold.Evidence.lanes[0].battery.run_sets.cold.gpu[0].end_to_end_ms++; $changedBatteryCold.Evidence.lanes[0].battery.run_sets.cold.gpu[0].backend_ms++
    $changedColdProjection = (Invoke-PassingFixture $changedBatteryCold 'v3-battery-cold-digest' 139).Decision.lanes[0].auto_entry_projection.evidence
    Assert-True ($changedColdProjection.cold_evidence_sha256 -cne $baselineProjection.cold_evidence_sha256) 'Battery cold evidence did not affect projected cold digest.'
    $changedBatteryWarm = New-V3FixtureDocuments; $changedBatteryWarm.Evidence.lanes[0].battery.run_sets.warm.gpu[0].end_to_end_ms++; $changedBatteryWarm.Evidence.lanes[0].battery.run_sets.warm.gpu[0].backend_ms++
    $changedWarmProjection = (Invoke-PassingFixture $changedBatteryWarm 'v3-battery-warm-digest' 139).Decision.lanes[0].auto_entry_projection.evidence
    Assert-True ($changedWarmProjection.warm_evidence_sha256 -cne $baselineProjection.warm_evidence_sha256) 'Battery warm evidence did not affect projected warm digest.'
    Assert-True ($changedWarmProjection.transcript_parity_evidence_sha256 -ceq $baselineProjection.transcript_parity_evidence_sha256) 'Timing-only mutation unexpectedly changed transcript parity digest.'
    $wrongToolchainVersion = New-FixtureDocuments; $wrongVersionIdentity = $wrongToolchainVersion.Evidence.lanes[0].identity; $revision = $wrongVersionIdentity.app_build_id.Split('#')[1]
    $wrongVersionIdentity.app_build_id = "local-transcriber@9.9.9#$revision"; $wrongWorkerBuild = "scribe-inference-worker@9.9.9#$revision"
    $wrongVersionIdentity.cpu_baseline.worker_build_id = $wrongWorkerBuild; $wrongVersionIdentity.gpu_worker.worker_build_id = $wrongWorkerBuild
    foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($wrongToolchainVersion.Evidence.lanes[0].run_sets[$mode][$target])) { $run.execution.worker_build_id = $wrongWorkerBuild } } }
    Sync-Captures $wrongToolchainVersion
    Assert-Rejected (Invoke-Evaluator (New-Bundle $wrongToolchainVersion 'wrong-toolchain-version')) 'App version not bound to toolchain' 'does not match the bound Windows toolchain'

    # Attestation/key/campaign fail-closed cases.
    $payloadTamper = New-Bundle (New-FixtureDocuments) 'signed-payload-tamper'; $payloadTamper.Documents.Evidence.lanes[0].run_sets.warm.gpu[0].end_to_end_ms++
    Rewrite-BundleEvidence $payloadTamper; Assert-Rejected (Invoke-Evaluator $payloadTamper) 'Signed lane payload tamper' 'attestation record does not bind'
    $batteryPayloadTamper = New-Bundle (New-V3FixtureDocuments) 'signed-battery-payload-tamper'; $batteryPayloadTamper.Documents.Evidence.lanes[0].battery.run_sets.warm.gpu[0].end_to_end_ms++
    Rewrite-BundleEvidence $batteryPayloadTamper; Assert-Rejected (Invoke-Evaluator $batteryPayloadTamper) 'Signed battery payload tamper' 'attestation record does not bind'
    $powerContractTamper = New-Bundle (New-V3FixtureDocuments) 'signed-power-contract-tamper'; $powerContractTamper.Documents.Plan.capture_contract.power_policy = 'different-power-policy'; $powerContractTamper.Documents.Evidence.plan_sha256 = Get-CanonicalDigest $powerContractTamper.Documents.Plan
    Write-Canonical $powerContractTamper.PlanPath $powerContractTamper.Documents.Plan; Rewrite-BundleEvidence $powerContractTamper
    Assert-Rejected (Invoke-Evaluator $powerContractTamper) 'Signed v3 capture power contract tamper' 'power policy is unsupported'
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
    $batteryArtifactTamper = New-Bundle (New-V3FixtureDocuments) 'battery-artifact-tamper'; $batteryTamperedPath = Join-Path $batteryArtifactTamper.ArtifactRoot 'fixture-windows-nvidia-cuda\battery\runs\warm\gpu\01.evidence'; [IO.File]::WriteAllText($batteryTamperedPath, 'tampered', $Utf8)
    Assert-Rejected (Invoke-Evaluator $batteryArtifactTamper) 'Signed battery artifact tamper' 'digest does not match'
    $signedLeftover = New-Bundle (New-V3FixtureDocuments) 'v3-signed-leftover'; $leftoverRelative = 'fixture-windows-nvidia-cuda/zz-signed-leftover.evidence'; $leftoverPath = Join-Path $signedLeftover.ArtifactRoot ($leftoverRelative.Replace('/', '\'))
    $leftoverDigest = Write-Envelope $leftoverPath 'windows_gpu_qualification_run_artifact' ([ordered]@{ marker = 'unreferenced' })
    $signedLeftover.Documents.Evidence.lanes[0].artifact_inventory = @($signedLeftover.Documents.Evidence.lanes[0].artifact_inventory + [ordered]@{ artifact_path = $leftoverRelative; artifact_sha256 = $leftoverDigest })
    Refresh-BundleSignatures $signedLeftover
    Assert-Rejected (Invoke-Evaluator $signedLeftover) 'Signed v3 inventory leftover' 'contains unreferenced files'
    $unsignedExtra = New-Bundle (New-FixtureDocuments) 'unsigned-extra'; [IO.File]::WriteAllText((Join-Path $unsignedExtra.ArtifactRoot 'extra.evidence'), 'extra', $Utf8)
    $extraResult = Invoke-Evaluator $unsignedExtra; Assert-True ($extraResult.ExitCode -eq 0) "Unreferenced filesystem file changed signed-inventory evaluation: $($extraResult.Stderr)"
    $inventoryEntryTamper = New-Bundle (New-FixtureDocuments) 'inventory-entry-tamper'; $inventoryEntryTamper.Documents.Evidence.lanes[0].artifact_inventory[0].artifact_sha256 = Get-Digest 'other-artifact'; Rewrite-BundleEvidence $inventoryEntryTamper
    Assert-Rejected (Invoke-Evaluator $inventoryEntryTamper) 'Signed inventory entry tamper' 'attestation record does not bind'
    $missingArtifacts = New-Bundle (New-FixtureDocuments) 'missing-artifacts' $false; Assert-Rejected (Invoke-Evaluator $missingArtifacts) 'Missing signed artifacts' 'Could not read'
    $oversizedArtifact = New-Bundle (New-V3FixtureDocuments) 'v3-oversized-artifact'; $oversizedArtifactPath = Join-Path $oversizedArtifact.ArtifactRoot 'fixture-windows-nvidia-cuda\battery\runs\cold\cpu\01.evidence'
    $oversizedStream = [IO.File]::Open($oversizedArtifactPath, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $oversizedStream.SetLength(16MB + 1) } finally { $oversizedStream.Dispose() }
    Assert-Rejected (Invoke-Evaluator $oversizedArtifact) 'V3 per-file 16 MiB overflow' 'empty or oversized'
    $declaredOverflow = New-Bundle (New-V3FixtureDocuments) 'v3-declared-artifact-overflow'
    $declaredOverflow.Documents.Evidence.lanes[0].artifact_inventory = @(
        0..4096 | ForEach-Object { [ordered]@{ artifact_path = ('overflow/{0:d4}.evidence' -f $_); artifact_sha256 = Get-Digest "overflow-$_" } }
    )
    Refresh-BundleSignatures $declaredOverflow
    Assert-Rejected (Invoke-Evaluator $declaredOverflow) 'V3 declared artifact-count overflow' 'inventory is empty or oversized'
    $cumulativeOverflow = New-Bundle (New-V3FixtureDocuments) 'v3-cumulative-artifact-overflow'; $firstCumulativeLane = $cumulativeOverflow.Documents.Evidence.lanes[0]; $secondCumulativeLane = Copy-Document $firstCumulativeLane
    $secondCumulativeLane.identity.lane_id = 'fixture-windows-nvidia-cuda-z'
    $secondCumulativeLane.artifact_inventory = @(0..3957 | ForEach-Object { [ordered]@{ artifact_path = ('second-overflow/{0:d4}.evidence' -f $_); artifact_sha256 = Get-Digest "second-overflow-$_" } })
    $cumulativeOverflow.Documents.Plan.required_lanes = @(
        [ordered]@{ evidence_sha256 = Get-Digest 'first-cumulative-placeholder'; identity = $firstCumulativeLane.identity },
        [ordered]@{ evidence_sha256 = Get-Digest 'second-cumulative-placeholder'; identity = $secondCumulativeLane.identity }
    )
    $firstCumulativeLane.attestation = New-Attestation $cumulativeOverflow.Documents.Plan $firstCumulativeLane
    $secondCumulativeLane.attestation = New-Attestation $cumulativeOverflow.Documents.Plan $secondCumulativeLane
    $cumulativeOverflow.Documents.Evidence.lanes = @($firstCumulativeLane, $secondCumulativeLane)
    $cumulativeOverflow.Documents.Plan.required_lanes = @(
        [ordered]@{ evidence_sha256 = Get-CanonicalDigest $firstCumulativeLane; identity = $firstCumulativeLane.identity },
        [ordered]@{ evidence_sha256 = Get-CanonicalDigest $secondCumulativeLane; identity = $secondCumulativeLane.identity }
    )
    $cumulativeOverflow.Documents.Evidence.plan_sha256 = Get-CanonicalDigest $cumulativeOverflow.Documents.Plan
    Write-Canonical $cumulativeOverflow.PlanPath $cumulativeOverflow.Documents.Plan; Rewrite-BundleEvidence $cumulativeOverflow
    Assert-Rejected (Invoke-Evaluator $cumulativeOverflow) 'V3 cumulative 4097 artifact overflow before second inventory read' 'artifact-count bound'
    $integratedV2Memory = New-FixtureDocuments; Set-IntegratedFixture $integratedV2Memory; $integratedV2Lane = $integratedV2Memory.Evidence.lanes[0]
    @($integratedV2Lane.scenarios | Where-Object scenario -ceq 'power_battery')[0].available_device_memory_bytes = [Int64]7000000000
    $integratedV2Lane.identity.device.qualified_minimum_available_memory_bytes = [Int64]7000000000
    $integratedV2Decision = (Invoke-PassingFixture $integratedV2Memory 'v2-integrated-heterogeneous-memory' 75).Decision
    Assert-True ($integratedV2Decision.lanes[0].evidence_memory_floor.minimum_available_memory_bytes -eq 7000000000) 'Schema v2 no longer pools its legacy successful GPU scenario memory evidence.'
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
    $PerformanceApprovalKey.Dispose()
    if (Test-Path -LiteralPath $TestRoot) { Remove-Item -LiteralPath $TestRoot -Recurse -Force }
}
