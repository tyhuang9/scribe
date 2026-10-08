$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# This helper validates a local frozen bundle only.  It is deliberately not a
# release trust format and does not alter the production package verifier.

. (Join-Path $PSScriptRoot 'windows-frozen-cpu-worker-integrity.ps1')
. (Join-Path $PSScriptRoot 'windows-pe-imports.ps1')

$script:WindowsLocalFrozenBaseFiles = @(
    'bundled-model-manifest.json',
    'licenses/Apache-2.0.txt',
    'licenses/OpenAI-Whisper-MIT.txt',
    'licenses/Silero-VAD-MIT.txt',
    'licenses/Silero-VAD-PROVENANCE.md',
    'licenses/THIRD-PARTY-NOTICES.txt',
    'licenses/Whisper-Base-En-NOTICE.txt',
    'licenses/sherpa-onnx-PROVENANCE.md',
    'licenses/transcribe.cpp-MIT.txt',
    'licenses/transcribe.cpp-PROVENANCE.md',
    'licenses/whisper.cpp-MIT.txt',
    'licenses/whisper.cpp-PROVENANCE.md',
    'local-transcriber.exe',
    'README.txt',
    'scribe-inference-worker.exe',
    'worker-pack-catalog.json',
    'whisper-base.en-Q8_0.gguf',
    (Get-WindowsFrozenCpuWorkerMarkerFileName)
)

function Get-WindowsLocalFrozenNormalizedFullPath([string]$Path) {
    return Get-WindowsFrozenCpuWorkerNormalizedFullPath $Path
}

function Test-WindowsLocalFrozenPathIsWithin([string]$CandidatePath, [string]$RootPath) {
    $candidate = Get-WindowsLocalFrozenNormalizedFullPath $CandidatePath
    $root = Get-WindowsLocalFrozenNormalizedFullPath $RootPath
    return [string]::Equals($candidate, $root, [System.StringComparison]::OrdinalIgnoreCase) -or
        $candidate.StartsWith($root + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)
}

function Assert-WindowsLocalFrozenRegularDirectory([string]$Path, [string]$Description) {
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $Path
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "$Description directory is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Description directory must be regular and non-reparse: $Path"
    }
    Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $Path
    return $item
}

function Get-WindowsLocalFrozenRelativePath([string]$Root, [string]$Path) {
    $normalizedRoot = Get-WindowsLocalFrozenNormalizedFullPath $Root
    $normalizedPath = Get-WindowsLocalFrozenNormalizedFullPath $Path
    $rootUri = [System.Uri]::new($normalizedRoot.TrimEnd([char[]]@('\', '/')) + '\')
    $pathUri = [System.Uri]::new($normalizedPath)
    return [System.Uri]::UnescapeDataString($rootUri.MakeRelativeUri($pathUri).ToString()).Replace('\', '/')
}

function Assert-WindowsLocalFrozenSafeRelativePath([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path) -or
        [System.IO.Path]::IsPathRooted($Path) -or
        $Path.Contains('\') -or $Path.Contains(':')) {
        throw "Local frozen installer payload path is unsafe: $Path"
    }
    $segments = @($Path.Split('/'))
    if ($segments.Count -eq 0 -or $segments.Count -gt 32) {
        throw "Local frozen installer payload path depth is unsafe: $Path"
    }
    foreach ($segment in $segments) {
        if ([string]::IsNullOrWhiteSpace($segment) -or $segment -in @('.', '..') -or
            $segment.Length -gt 128 -or $segment.EndsWith('.') -or $segment.EndsWith(' ')) {
            throw "Local frozen installer payload path segment is unsafe: $Path"
        }
        $stem = $segment.Split('.')[0].ToUpperInvariant()
        if ($stem -in @('CON', 'PRN', 'AUX', 'NUL', 'CLOCK$') -or $stem -match '^(COM|LPT)[1-9]$') {
            throw "Local frozen installer payload path uses a reserved Windows name: $Path"
        }
    }
}

function Assert-WindowsLocalFrozenExactProperties(
    [psobject]$Value,
    [string[]]$ExpectedProperties,
    [string]$Description
) {
    if ($null -eq $Value) {
        throw "$Description is missing."
    }
    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $expected = @($ExpectedProperties | Sort-Object)
    if ($actual.Count -ne $expected.Count -or
        (Compare-Object -ReferenceObject $expected -DifferenceObject $actual -CaseSensitive)) {
        throw "$Description has unexpected or missing fields."
    }
}

function Assert-WindowsLocalFrozenInt64([object]$Value, [string]$Description) {
    if ($Value -isnot [int32] -and $Value -isnot [int64]) {
        throw "$Description must be an integer."
    }
    return [int64]$Value
}

function Get-WindowsLocalFrozenCaptureObservationRequest([Collections.IDictionary]$BoundParameters) {
    $names = @(
        'ObservationWavPath', 'ObservationWavSha256',
        'ObservationGpuPackId', 'ObservationGpuBackend',
        'ObservationGpuDevice', 'ObservationReportPath'
    )
    $present = @($names | Where-Object { @($BoundParameters.Keys) -ccontains $_ })
    $hasCampaignPower = @($BoundParameters.Keys) -ccontains 'ObservationCampaignPower'
    if ($present.Count -eq 0 -and -not $hasCampaignPower) {
        return $null
    }
    if ($present.Count -ne $names.Count) {
        throw 'Installed GPU observation arguments must be supplied together as one single-pair request.'
    }
    if ($hasCampaignPower -and
        ($BoundParameters['ObservationCampaignPower'] -isnot [string] -or
         [string]$BoundParameters['ObservationCampaignPower'] -cnotin @('ac', 'battery'))) {
        throw 'Installed GPU observation campaign power must be lowercase ac or battery.'
    }

    $wavPath = [string]$BoundParameters['ObservationWavPath']
    $wavSha256 = [string]$BoundParameters['ObservationWavSha256']
    $packId = [string]$BoundParameters['ObservationGpuPackId']
    $backend = [string]$BoundParameters['ObservationGpuBackend']
    $device = [string]$BoundParameters['ObservationGpuDevice']
    $reportPath = [string]$BoundParameters['ObservationReportPath']
    if (-not [IO.Path]::IsPathFullyQualified($wavPath) -or
        -not [IO.Path]::IsPathFullyQualified($reportPath)) {
        throw 'Installed GPU observation WAV and report paths must be absolute.'
    }
    if ($wavSha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Installed GPU observation WAV SHA-256 must be canonical lowercase hexadecimal.'
    }
    if ($packId -cnotmatch '^[A-Za-z0-9._:-]{1,96}$') {
        throw 'Installed GPU observation pack ID is not canonical.'
    }
    if ($backend -cnotin @('cuda', 'vulkan')) {
        throw 'Installed GPU observation backend must be cuda or vulkan.'
    }
    $deviceHasControl = @($device.ToCharArray() | Where-Object { [char]::IsControl($_) }).Count -gt 0
    if ([string]::IsNullOrWhiteSpace($device) -or $device.Length -gt 256 -or
        $device -cne $device.Trim() -or $deviceHasControl) {
        throw 'Installed GPU observation stable device selector is not canonical.'
    }
    return [pscustomobject]@{
        WavPath = Get-WindowsLocalFrozenNormalizedFullPath $wavPath
        WavSha256 = $wavSha256
        GpuPackId = $packId
        GpuBackend = $backend
        GpuDevice = $device
        ReportPath = Get-WindowsLocalFrozenNormalizedFullPath $reportPath
        CampaignPower = if ($hasCampaignPower) { [string]$BoundParameters['ObservationCampaignPower'] } else { $null }
    }
}

function Get-WindowsLocalFrozenVerifiedInventoryFile(
    [psobject]$Bundle,
    [string]$RelativePath
) {
    if ($null -eq $Bundle -or $null -eq $Bundle.PSObject.Properties['InventoryEntries']) {
        throw 'Local frozen installer bundle has no verified inventory entries.'
    }
    Assert-WindowsLocalFrozenSafeRelativePath $RelativePath
    $matches = @($Bundle.InventoryEntries | Where-Object {
        [string]::Equals([string]$_.path, $RelativePath, [StringComparison]::OrdinalIgnoreCase)
    })
    if ($matches.Count -ne 1) {
        throw "Local frozen installer verified inventory has no unique entry for $RelativePath."
    }
    $entry = $matches[0]
    $size = Assert-WindowsLocalFrozenInt64 $entry.size_bytes "Local frozen installer inventory size for $RelativePath"
    if ($size -lt 0 -or $entry.sha256 -isnot [string] -or $entry.sha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw "Local frozen installer verified inventory entry for $RelativePath is malformed."
    }
    return [pscustomobject]@{
        RelativePath = [string]$entry.path
        SizeBytes = $size
        Sha256 = [string]$entry.sha256
    }
}

function Assert-WindowsLocalFrozenBundleCompiledAdmission(
    [psobject]$Bundle,
    [psobject]$DesktopContext,
    [psobject]$FrozenCpuWorker
) {
    $desktop = Get-WindowsLocalFrozenVerifiedInventoryFile $Bundle 'local-transcriber.exe'
    $desktopPath = Join-Path $Bundle.Root ($desktop.RelativePath -replace '/', '\')
    return Assert-WindowsFrozenCpuWorkerCompiledAdmission `
        -Executable $desktopPath `
        -ExpectedSize $desktop.SizeBytes `
        -ExpectedSha256 $desktop.Sha256 `
        -DesktopContext $DesktopContext `
        -FrozenCpuWorker $FrozenCpuWorker
}

function Get-WindowsLocalFrozenCapturePackBinding(
    [psobject]$Bundle,
    [string]$PackId,
    [string]$Backend
) {
    if ($null -eq $Bundle -or $null -eq $Bundle.PSObject.Properties['Root']) {
        throw 'Installed GPU observation requires a verified local frozen bundle.'
    }
    $catalogPath = Join-Path $Bundle.Root 'worker-pack-catalog.json'
    $catalogText = Read-WindowsFrozenCpuWorkerBoundedUtf8File $catalogPath (4MB)
    try {
        $catalog = $catalogText.Text | ConvertFrom-Json -Depth 10
    }
    catch {
        throw "Installed GPU observation worker-pack catalog is not valid JSON: $($_.Exception.Message)"
    }
    Assert-WindowsLocalFrozenExactProperties $catalog @('schema_version', 'packs') 'Installed GPU observation worker-pack catalog'
    if (($catalog.schema_version -isnot [int32] -and $catalog.schema_version -isnot [int64]) -or [int]$catalog.schema_version -ne 1) {
        throw 'Installed GPU observation worker-pack catalog has an unsupported schema.'
    }
    $matches = @($catalog.packs | Where-Object {
        [string]$_.pack_id -ceq $PackId -and [string]$_.backend -ceq $Backend
    })
    if ($matches.Count -ne 1) {
        throw 'Installed GPU observation requested pack is absent or ambiguous in the verified bundle catalog.'
    }
    $pack = $matches[0]
    Assert-WindowsLocalFrozenExactProperties $pack @(
        'pack_id', 'pack_version', 'pack_digest', 'security_epoch',
        'runtime_abi_version', 'backend', 'provider', 'target_os',
        'target_arch', 'worker_relative_path', 'root',
        'installed_size_bytes', 'compressed_size_bytes', 'files'
    ) 'Installed GPU observation worker-pack catalog entry'
    foreach ($property in @('pack_id', 'pack_version', 'provider')) {
        if ($pack.$property -isnot [string] -or [string]$pack.$property -cnotmatch '^[A-Za-z0-9._:-]{1,96}$') {
            throw "Installed GPU observation worker-pack catalog has an invalid $property."
        }
    }
    if ($pack.pack_digest -isnot [string] -or $pack.pack_digest -cnotmatch '^[0-9a-f]{64}$' -or
        $pack.backend -isnot [string] -or [string]$pack.backend -cne $Backend -or
        [string]$pack.pack_id -cne $PackId -or [string]$pack.target_os -cne 'windows' -or
        [string]$pack.target_arch -cne 'x86_64') {
        throw 'Installed GPU observation worker-pack catalog identity is invalid.'
    }
    $securityEpoch = Assert-WindowsLocalFrozenInt64 $pack.security_epoch 'Installed GPU observation pack security epoch'
    $runtimeAbi = Assert-WindowsLocalFrozenInt64 $pack.runtime_abi_version 'Installed GPU observation pack runtime ABI'
    if ($securityEpoch -lt 0 -or $runtimeAbi -lt 0 -or $runtimeAbi -gt [uint16]::MaxValue) {
        throw 'Installed GPU observation worker-pack catalog numeric identity is invalid.'
    }
    return [pscustomobject]@{
        PackId = [string]$pack.pack_id
        PackVersion = [string]$pack.pack_version
        PackSha256 = [string]$pack.pack_digest
        PackSecurityEpoch = $securityEpoch
        RuntimeAbi = $runtimeAbi
        Backend = [string]$pack.backend
        Provider = [string]$pack.provider
    }
}

function Assert-WindowsLocalFrozenCaptureWorkerReport([psobject]$Worker, [string]$Description) {
    Assert-WindowsLocalFrozenExactProperties $Worker @(
        'hello_frame_hex', 'ready_frame_hex', 'power_source_before', 'power_source_after',
        'elapsed_ms', 'sampled_max_private_usage_bytes', 'telemetry_sample_count',
        'video_memory', 'provider_memory', 'memory_availability', 'normalized_transcript_sha256'
    ) $Description
    foreach ($frame in @('hello_frame_hex', 'ready_frame_hex')) {
        $value = [string]$Worker.$frame
        if ($value -cnotmatch '^[0-9a-f]{52,524340}$' -or $value.Length % 2 -ne 0) {
            throw "$Description has an invalid $frame."
        }
    }
    foreach ($power in @('power_source_before', 'power_source_after')) {
        if ([string]$Worker.$power -cnotin @('ac', 'battery')) {
            throw "$Description has an unsupported $power."
        }
    }
    foreach ($number in @('elapsed_ms', 'sampled_max_private_usage_bytes', 'telemetry_sample_count')) {
        if ((Assert-WindowsLocalFrozenInt64 $Worker.$number "$Description $number") -lt 0) {
            throw "$Description has a negative $number."
        }
    }
    if ($Worker.normalized_transcript_sha256 -isnot [string] -or
        $Worker.normalized_transcript_sha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw "$Description has an invalid normalized transcript SHA-256."
    }
}

function Read-WindowsLocalFrozenCaptureObservationReport(
    [string]$ReportPath,
    [psobject]$Expected
) {
    if (-not (Test-Path -LiteralPath $ReportPath -PathType Leaf)) {
        throw 'Installed GPU observation report file is missing.'
    }
    $serialized = Read-WindowsFrozenCpuWorkerBoundedUtf8File $ReportPath (1MB)
    try {
        $report = $serialized.Text | ConvertFrom-Json -Depth 12
    }
    catch {
        throw "Installed GPU observation report is not valid JSON: $($_.Exception.Message)"
    }
    Assert-WindowsLocalFrozenExactProperties $report @(
        'schema_version', 'kind', 'unsigned', 'unqualified', 'auto_eligible',
        'release_approved', 'collector_build_revision', 'inputs', 'gpu_identity',
        'cpu', 'gpu', 'transcript_parity', 'unavailable'
    ) 'Installed GPU observation report'
    if (($report.schema_version -isnot [int32] -and $report.schema_version -isnot [int64]) -or
        [int]$report.schema_version -ne 3 -or
        [string]$report.kind -cne 'windows_gpu_capture_observation' -or
        $report.unsigned -isnot [bool] -or -not $report.unsigned -or
        $report.unqualified -isnot [bool] -or -not $report.unqualified -or
        $report.auto_eligible -isnot [bool] -or $report.auto_eligible -or
        $report.release_approved -isnot [bool] -or $report.release_approved -or
        [string]$report.collector_build_revision -cne [string]$Expected.CollectorBuildRevision) {
        throw 'Installed GPU observation report has invalid local-only qualification flags or collector identity.'
    }
    Assert-WindowsLocalFrozenExactProperties $report.inputs @('model_sha256', 'wav_sha256') 'Installed GPU observation report inputs'
    if ([string]$report.inputs.model_sha256 -cne [string]$Expected.ModelSha256 -or
        [string]$report.inputs.wav_sha256 -cne [string]$Expected.WavSha256) {
        throw 'Installed GPU observation report does not bind the installed model and requested WAV identities.'
    }
    Assert-WindowsLocalFrozenExactProperties $report.gpu_identity @(
        'backend', 'provider', 'stable_device', 'driver', 'device_class', 'vendor',
        'memory_total_bytes', 'pack_id', 'pack_version', 'pack_sha256',
        'pack_security_epoch', 'runtime_abi'
    ) 'Installed GPU observation GPU identity'
    foreach ($property in @('backend', 'provider', 'stable_device', 'driver', 'device_class', 'vendor', 'pack_id', 'pack_version', 'pack_sha256')) {
        if ($report.gpu_identity.$property -isnot [string] -or [string]::IsNullOrWhiteSpace([string]$report.gpu_identity.$property)) {
            throw "Installed GPU observation GPU identity has an invalid $property."
        }
    }
    if ([string]$report.gpu_identity.backend -cne [string]$Expected.Backend -or
        [string]$report.gpu_identity.provider -cne [string]$Expected.Provider -or
        [string]$report.gpu_identity.stable_device -cne [string]$Expected.StableDevice -or
        [string]$report.gpu_identity.pack_id -cne [string]$Expected.PackId -or
        [string]$report.gpu_identity.pack_version -cne [string]$Expected.PackVersion -or
        [string]$report.gpu_identity.pack_sha256 -cne [string]$Expected.PackSha256 -or
        (Assert-WindowsLocalFrozenInt64 $report.gpu_identity.pack_security_epoch 'Installed GPU observation report pack security epoch') -ne [int64]$Expected.PackSecurityEpoch -or
        (Assert-WindowsLocalFrozenInt64 $report.gpu_identity.runtime_abi 'Installed GPU observation report runtime ABI') -ne [int64]$Expected.RuntimeAbi -or
        (Assert-WindowsLocalFrozenInt64 $report.gpu_identity.memory_total_bytes 'Installed GPU observation report memory total') -le 0) {
        throw 'Installed GPU observation report does not bind the requested verified pack/backend/stable device.'
    }
    Assert-WindowsLocalFrozenCaptureWorkerReport $report.cpu 'Installed GPU observation CPU worker report'
    Assert-WindowsLocalFrozenCaptureWorkerReport $report.gpu 'Installed GPU observation GPU worker report'
    if ($report.transcript_parity -isnot [bool] -or
        $report.transcript_parity -ne ([string]$report.cpu.normalized_transcript_sha256 -ceq [string]$report.gpu.normalized_transcript_sha256)) {
        throw 'Installed GPU observation report transcript parity is inconsistent with its transcript digests.'
    }
    return [pscustomobject]@{
        Report = $report
        Bytes = $serialized.Bytes
    }
}

function Assert-WindowsLocalFrozenJsonElementHasNoPropertyCollisions(
    [System.Text.Json.JsonElement]$Element,
    [int]$Depth = 0
) {
    if ($Depth -gt 32) {
        throw 'Installed GPU observation campaign JSON exceeds the supported nesting depth.'
    }
    if ($Element.ValueKind -eq [System.Text.Json.JsonValueKind]::Object) {
        $ordinal = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        $ignoreCase = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($property in $Element.EnumerateObject()) {
            if (-not $ordinal.Add($property.Name) -or -not $ignoreCase.Add($property.Name)) {
                throw 'Installed GPU observation campaign JSON contains duplicate or case-colliding properties.'
            }
            Assert-WindowsLocalFrozenJsonElementHasNoPropertyCollisions $property.Value ($Depth + 1)
        }
    }
    elseif ($Element.ValueKind -eq [System.Text.Json.JsonValueKind]::Array) {
        foreach ($item in $Element.EnumerateArray()) {
            Assert-WindowsLocalFrozenJsonElementHasNoPropertyCollisions $item ($Depth + 1)
        }
    }
}

function Assert-WindowsLocalFrozenCampaignJson([string]$Text) {
    $options = [System.Text.Json.JsonDocumentOptions]::new()
    $options.AllowTrailingCommas = $false
    $options.CommentHandling = [System.Text.Json.JsonCommentHandling]::Disallow
    $options.MaxDepth = 32
    $document = $null
    try {
        $document = [System.Text.Json.JsonDocument]::Parse($Text, $options)
        Assert-WindowsLocalFrozenJsonElementHasNoPropertyCollisions $document.RootElement
    }
    catch {
        throw 'Installed GPU observation campaign report is not strict duplicate-free JSON.'
    }
    finally {
        if ($null -ne $document) { $document.Dispose() }
    }
}

function Assert-WindowsLocalFrozenCampaignInteger(
    [object]$Value,
    [string]$Description,
    [switch]$Positive
) {
    $number = Assert-WindowsLocalFrozenInt64 $Value $Description
    if ($number -lt 0 -or ($Positive -and $number -eq 0)) {
        throw "$Description is outside the supported nonnegative integer range."
    }
    return $number
}

function Assert-WindowsLocalFrozenCampaignUnavailableField(
    [psobject]$Value,
    [string]$Reason,
    [string]$Description
) {
    Assert-WindowsLocalFrozenExactProperties $Value @('status', 'reason') $Description
    if ($Value.status -isnot [string] -or [string]$Value.status -cne 'unavailable' -or
        $Value.reason -isnot [string] -or [string]$Value.reason -cne $Reason) {
        throw "$Description is not the required unavailable observation."
    }
}

function Get-WindowsLocalFrozenCampaignHandshakeDigest(
    [string]$HelloFrameHex,
    [string]$ReadyFrameHex
) {
    try {
        $hello = [Convert]::FromHexString($HelloFrameHex)
        $ready = [Convert]::FromHexString($ReadyFrameHex)
    }
    catch {
        throw 'Installed GPU observation campaign handshake capture is not canonical hexadecimal.'
    }
    $hash = [Security.Cryptography.IncrementalHash]::CreateHash([Security.Cryptography.HashAlgorithmName]::SHA256)
    try {
        foreach ($frame in @($hello, $ready)) {
            $length = [BitConverter]::GetBytes([uint64]$frame.Length)
            if (-not [BitConverter]::IsLittleEndian) { [Array]::Reverse($length) }
            $hash.AppendData($length)
            $hash.AppendData($frame)
        }
        return [Convert]::ToHexString($hash.GetHashAndReset()).ToLowerInvariant()
    }
    finally {
        $hash.Dispose()
    }
}

function Assert-WindowsLocalFrozenCampaignGpuIdentity(
    [psobject]$Identity,
    [psobject]$Expected
) {
    Assert-WindowsLocalFrozenExactProperties $Identity @(
        'backend', 'provider', 'stable_device', 'driver', 'device_class', 'vendor',
        'memory_total_bytes', 'pack_id', 'pack_version', 'pack_sha256',
        'pack_security_epoch', 'runtime_abi'
    ) 'Installed GPU observation campaign GPU identity'
    foreach ($property in @(
        'backend', 'provider', 'stable_device', 'driver', 'device_class', 'vendor',
        'pack_id', 'pack_version', 'pack_sha256'
    )) {
        if ($Identity.$property -isnot [string] -or
            [string]::IsNullOrWhiteSpace([string]$Identity.$property) -or
            [string]$Identity.$property -cne ([string]$Identity.$property).Trim() -or
            [string]$Identity.$property -match '[\x00-\x1f\x7f]') {
            throw "Installed GPU observation campaign GPU identity has an invalid $property."
        }
    }
    if ([string]$Identity.backend -cne [string]$Expected.Backend -or
        [string]$Identity.provider -cne [string]$Expected.Provider -or
        [string]$Identity.stable_device -cne [string]$Expected.StableDevice -or
        [string]$Identity.pack_id -cne [string]$Expected.PackId -or
        [string]$Identity.pack_version -cne [string]$Expected.PackVersion -or
        [string]$Identity.pack_sha256 -cne [string]$Expected.PackSha256 -or
        (Assert-WindowsLocalFrozenCampaignInteger $Identity.pack_security_epoch 'Installed GPU observation campaign pack security epoch') -ne [int64]$Expected.PackSecurityEpoch -or
        (Assert-WindowsLocalFrozenCampaignInteger $Identity.runtime_abi 'Installed GPU observation campaign runtime ABI') -ne [int64]$Expected.RuntimeAbi -or
        (Assert-WindowsLocalFrozenCampaignInteger $Identity.memory_total_bytes 'Installed GPU observation campaign memory total' -Positive) -le 0) {
        throw 'Installed GPU observation campaign report does not bind the requested verified pack/backend/stable device.'
    }
}

function Assert-WindowsLocalFrozenCampaignSegmentSummary(
    [psobject]$Summary,
    [string]$Description
) {
    Assert-WindowsLocalFrozenExactProperties $Summary @(
        'sampled_max_current_usage_bytes', 'sampled_max_current_reservation_bytes',
        'sampled_min_budget_bytes', 'sampled_min_available_for_reservation_bytes'
    ) $Description
    foreach ($property in $Summary.PSObject.Properties.Name) {
        $null = Assert-WindowsLocalFrozenCampaignInteger $Summary.$property "$Description $property"
    }
}

function Assert-WindowsLocalFrozenCampaignVideoMemory(
    [psobject]$VideoMemory,
    [string]$Target
) {
    if ($Target -ceq 'cpu') {
        Assert-WindowsLocalFrozenExactProperties $VideoMemory @('status') 'Installed GPU observation campaign CPU video memory'
        if ($VideoMemory.status -isnot [string] -or [string]$VideoMemory.status -cne 'not_applicable') {
            throw 'Installed GPU observation campaign CPU video memory must be not applicable.'
        }
        return
    }
    Assert-WindowsLocalFrozenExactProperties $VideoMemory @('status', 'local', 'non_local') 'Installed GPU observation campaign GPU video memory'
    if ($VideoMemory.status -isnot [string] -or [string]$VideoMemory.status -cne 'available') {
        throw 'Installed GPU observation campaign GPU video memory must be available.'
    }
    Assert-WindowsLocalFrozenCampaignSegmentSummary $VideoMemory.local 'Installed GPU observation campaign local video memory'
    Assert-WindowsLocalFrozenCampaignSegmentSummary $VideoMemory.non_local 'Installed GPU observation campaign non-local video memory'
}

function Assert-WindowsLocalFrozenCampaignProviderMemoryEndpoint(
    [psobject]$Observation,
    [string]$Target,
    [psobject]$GpuIdentity
) {
    if ($Target -ceq 'cpu') {
        Assert-WindowsLocalFrozenExactProperties $Observation @('status', 'reason') 'Installed GPU observation campaign CPU provider memory'
        if ($Observation.status -isnot [string] -or [string]$Observation.status -cne 'not_applicable' -or
            $Observation.reason -isnot [string] -or [string]$Observation.reason -cne 'cpu_provider') {
            throw 'Installed GPU observation campaign CPU provider memory is invalid.'
        }
        return
    }
    if ($Observation.status -isnot [string]) {
        throw 'Installed GPU observation campaign GPU provider memory status is invalid.'
    }
    if ([string]$Observation.status -ceq 'unavailable') {
        Assert-WindowsLocalFrozenExactProperties $Observation @('status', 'reason') 'Installed GPU observation campaign unavailable GPU provider memory'
        if ($Observation.reason -isnot [string] -or
            [string]$Observation.reason -cnotin @('memory_total_unreported', 'provider_query_failed')) {
            throw 'Installed GPU observation campaign GPU provider memory unavailability is invalid.'
        }
        return
    }
    Assert-WindowsLocalFrozenExactProperties $Observation @(
        'status', 'backend', 'provider_id', 'stable_device', 'memory_total_bytes',
        'provider_reported_memory_free_bytes', 'value_semantics', 'admission_validity'
    ) 'Installed GPU observation campaign available GPU provider memory'
    $total = Assert-WindowsLocalFrozenCampaignInteger $Observation.memory_total_bytes 'Installed GPU observation campaign provider memory total' -Positive
    $free = Assert-WindowsLocalFrozenCampaignInteger $Observation.provider_reported_memory_free_bytes 'Installed GPU observation campaign provider memory free'
    if ([string]$Observation.status -cne 'available' -or
        $Observation.backend -isnot [string] -or [string]$Observation.backend -cne [string]$GpuIdentity.backend -or
        $Observation.provider_id -isnot [string] -or [string]$Observation.provider_id -cne [string]$GpuIdentity.provider -or
        $Observation.stable_device -isnot [string] -or [string]$Observation.stable_device -cne [string]$GpuIdentity.stable_device -or
        $total -ne [int64]$GpuIdentity.memory_total_bytes -or $free -gt $total -or
        $Observation.value_semantics -isnot [string] -or [string]$Observation.value_semantics -cne 'native_backend_defined' -or
        $Observation.admission_validity -isnot [string] -or [string]$Observation.admission_validity -cne 'unestablished') {
        throw 'Installed GPU observation campaign available GPU provider memory is inconsistent.'
    }
}

function Assert-WindowsLocalFrozenCampaignProviderMemory(
    [psobject]$ProviderMemory,
    [string]$Target,
    [psobject]$GpuIdentity
) {
    Assert-WindowsLocalFrozenExactProperties $ProviderMemory @('before', 'after') 'Installed GPU observation campaign provider memory pair'
    Assert-WindowsLocalFrozenCampaignProviderMemoryEndpoint $ProviderMemory.before $Target $GpuIdentity
    Assert-WindowsLocalFrozenCampaignProviderMemoryEndpoint $ProviderMemory.after $Target $GpuIdentity
}

function Assert-WindowsLocalFrozenCampaignMemoryAvailabilityEndpoint(
    [psobject]$Observation,
    [string]$Target,
    [psobject]$GpuIdentity
) {
    if ($Target -ceq 'cpu') {
        Assert-WindowsLocalFrozenExactProperties $Observation @('status', 'reason') 'Installed GPU observation campaign CPU memory availability'
        if ($Observation.status -isnot [string] -or [string]$Observation.status -cne 'not_applicable' -or
            $Observation.reason -isnot [string] -or [string]$Observation.reason -cne 'cpu_provider') {
            throw 'Installed GPU observation campaign CPU memory availability is invalid.'
        }
        return
    }
    if ($Observation.status -isnot [string]) {
        throw 'Installed GPU observation campaign GPU memory availability status is invalid.'
    }
    if ([string]$Observation.status -ceq 'unavailable') {
        Assert-WindowsLocalFrozenExactProperties $Observation @('status', 'reason') 'Installed GPU observation campaign unavailable GPU memory availability'
        if ($Observation.reason -isnot [string] -or [string]$Observation.reason -cnotin @(
            'provider_query_failed', 'stable_device_missing', 'stable_device_ambiguous',
            'memory_budget_extension_unavailable', 'memory_budget_invalid',
            'multi_instance_unsupported', 'unsupported_provider'
        )) {
            throw 'Installed GPU observation campaign GPU memory unavailability is invalid.'
        }
        return
    }
    Assert-WindowsLocalFrozenExactProperties $Observation @(
        'status', 'backend', 'provider_id', 'stable_device', 'memory_total_bytes',
        'available_memory_bytes', 'source'
    ) 'Installed GPU observation campaign observed GPU memory availability'
    $total = Assert-WindowsLocalFrozenCampaignInteger $Observation.memory_total_bytes 'Installed GPU observation campaign availability total' -Positive
    $available = Assert-WindowsLocalFrozenCampaignInteger $Observation.available_memory_bytes 'Installed GPU observation campaign available memory'
    if ([string]$Observation.status -cne 'observed' -or
        $Observation.backend -isnot [string] -or [string]$Observation.backend -cne [string]$GpuIdentity.backend -or
        $Observation.provider_id -isnot [string] -or [string]$Observation.provider_id -cne [string]$GpuIdentity.provider -or
        $Observation.stable_device -isnot [string] -or [string]$Observation.stable_device -cne [string]$GpuIdentity.stable_device -or
        $available -gt $total) {
        throw 'Installed GPU observation campaign observed GPU memory availability is inconsistent.'
    }
    if ([string]$GpuIdentity.backend -ceq 'cuda') {
        Assert-WindowsLocalFrozenExactProperties $Observation.source @('method') 'Installed GPU observation campaign CUDA memory source'
        if ($total -ne [int64]$GpuIdentity.memory_total_bytes -or
            $Observation.source.method -isnot [string] -or [string]$Observation.source.method -cne 'cuda_mem_get_info') {
            throw 'Installed GPU observation campaign CUDA memory source is invalid.'
        }
        return
    }
    Assert-WindowsLocalFrozenExactProperties $Observation.source @('method', 'heap_selection', 'heaps') 'Installed GPU observation campaign Vulkan memory source'
    $expectedSelection = switch ([string]$GpuIdentity.device_class) {
        'integrated_gpu' { 'all_heaps_integrated' }
        'discrete_gpu' { 'device_local_heaps' }
        default { throw 'Installed GPU observation campaign Vulkan device class is unsupported.' }
    }
    if ($Observation.source.method -isnot [string] -or [string]$Observation.source.method -cne 'vulkan_memory_budget' -or
        $Observation.source.heap_selection -isnot [string] -or [string]$Observation.source.heap_selection -cne $expectedSelection -or
        $Observation.source.heaps -isnot [object[]] -or @($Observation.source.heaps).Count -lt 1 -or @($Observation.source.heaps).Count -gt 16) {
        throw 'Installed GPU observation campaign Vulkan memory source is invalid.'
    }
    [uint64]$derivedTotal = 0
    [uint64]$derivedAvailable = 0
    $selected = 0
    for ($index = 0; $index -lt @($Observation.source.heaps).Count; $index++) {
        $heap = @($Observation.source.heaps)[$index]
        Assert-WindowsLocalFrozenExactProperties $heap @(
            'heap_index', 'size_bytes', 'flags', 'budget_bytes', 'usage_bytes'
        ) 'Installed GPU observation campaign Vulkan memory heap'
        $heapIndex = Assert-WindowsLocalFrozenCampaignInteger $heap.heap_index 'Installed GPU observation campaign Vulkan heap index'
        $size = Assert-WindowsLocalFrozenCampaignInteger $heap.size_bytes 'Installed GPU observation campaign Vulkan heap size' -Positive
        $flags = Assert-WindowsLocalFrozenCampaignInteger $heap.flags 'Installed GPU observation campaign Vulkan heap flags'
        $budget = Assert-WindowsLocalFrozenCampaignInteger $heap.budget_bytes 'Installed GPU observation campaign Vulkan heap budget' -Positive
        $usage = Assert-WindowsLocalFrozenCampaignInteger $heap.usage_bytes 'Installed GPU observation campaign Vulkan heap usage'
        # MULTI_INSTANCE is a heap capability, not evidence that the pinned
        # singleton logical device allocates across physical devices.
        if ($heapIndex -ne $index -or $flags -gt 3 -or $budget -gt $size) {
            throw 'Installed GPU observation campaign Vulkan heap is noncanonical.'
        }
        $include = $expectedSelection -ceq 'all_heaps_integrated' -or ($flags -band 1) -ne 0
        if ($include) {
            $selected++
            if ([uint64]$size -gt [uint64]::MaxValue - $derivedTotal) {
                throw 'Installed GPU observation campaign Vulkan heap total overflowed.'
            }
            $derivedTotal += [uint64]$size
            $headroom = if ($usage -ge $budget) { [uint64]0 } else { [uint64]($budget - $usage) }
            if ($headroom -gt [uint64]::MaxValue - $derivedAvailable) {
                throw 'Installed GPU observation campaign Vulkan heap headroom overflowed.'
            }
            $derivedAvailable += $headroom
        }
    }
    if ($selected -eq 0 -or $derivedTotal -ne [uint64]$total -or $derivedAvailable -ne [uint64]$available) {
        throw 'Installed GPU observation campaign Vulkan heap inventory is inconsistent.'
    }
}

function Assert-WindowsLocalFrozenCampaignMemoryAvailability(
    [psobject]$MemoryAvailability,
    [string]$Target,
    [psobject]$GpuIdentity
) {
    Assert-WindowsLocalFrozenExactProperties $MemoryAvailability @('before', 'after') 'Installed GPU observation campaign memory availability pair'
    Assert-WindowsLocalFrozenCampaignMemoryAvailabilityEndpoint $MemoryAvailability.before $Target $GpuIdentity
    Assert-WindowsLocalFrozenCampaignMemoryAvailabilityEndpoint $MemoryAvailability.after $Target $GpuIdentity
}

function Assert-WindowsLocalFrozenCampaignAffinityEndpoint(
    [psobject]$Endpoint,
    [string]$Description
) {
    if ($Endpoint.status -isnot [string]) {
        throw "$Description status is invalid."
    }
    if ([string]$Endpoint.status -ceq 'unavailable') {
        Assert-WindowsLocalFrozenExactProperties $Endpoint @('status', 'reason') $Description
        if ($Endpoint.reason -isnot [string] -or [string]$Endpoint.reason -cnotin @(
            'unsupported_processor_group_topology', 'processor_group_query_failed',
            'affinity_mask_query_failed', 'topology_changed_during_query', 'invalid_affinity_masks'
        )) {
            throw "$Description unavailable reason is invalid."
        }
        return 'unavailable'
    }
    Assert-WindowsLocalFrozenExactProperties $Endpoint @(
        'status', 'processor_group', 'process_mask_hex', 'system_mask_hex'
    ) $Description
    $group = Assert-WindowsLocalFrozenCampaignInteger $Endpoint.processor_group "$Description processor group"
    if ([string]$Endpoint.status -cne 'available' -or $group -ne 0 -or
        $Endpoint.process_mask_hex -isnot [string] -or [string]$Endpoint.process_mask_hex -cnotmatch '^[0-9a-f]{16}$' -or
        $Endpoint.system_mask_hex -isnot [string] -or [string]$Endpoint.system_mask_hex -cnotmatch '^[0-9a-f]{16}$') {
        throw "$Description available masks are invalid."
    }
    [uint64]$processMask = [Convert]::ToUInt64([string]$Endpoint.process_mask_hex, 16)
    [uint64]$systemMask = [Convert]::ToUInt64([string]$Endpoint.system_mask_hex, 16)
    if ($processMask -eq 0 -or $systemMask -eq 0 -or ($processMask -band $systemMask) -ne $processMask) {
        throw "$Description available masks are inconsistent."
    }
    return 'available'
}

function Assert-WindowsLocalFrozenCampaignAffinityPair([psobject]$Pair) {
    Assert-WindowsLocalFrozenExactProperties $Pair @('before', 'after', 'changed') 'Installed GPU observation campaign process affinity pair'
    if ($null -eq $Pair.before -or $null -eq $Pair.after) {
        throw 'Installed GPU observation campaign successful process affinity pair omitted an endpoint.'
    }
    $beforeStatus = Assert-WindowsLocalFrozenCampaignAffinityEndpoint $Pair.before 'Installed GPU observation campaign initial process affinity'
    $afterStatus = Assert-WindowsLocalFrozenCampaignAffinityEndpoint $Pair.after 'Installed GPU observation campaign final process affinity'
    if ($beforeStatus -ceq 'available' -and $afterStatus -ceq 'available') {
        $expectedChanged = [string]$Pair.before.process_mask_hex -cne [string]$Pair.after.process_mask_hex -or
            [string]$Pair.before.system_mask_hex -cne [string]$Pair.after.system_mask_hex -or
            [int64]$Pair.before.processor_group -ne [int64]$Pair.after.processor_group
        if ($Pair.changed -isnot [bool] -or [bool]$Pair.changed -ne $expectedChanged) {
            throw 'Installed GPU observation campaign process affinity change result is inconsistent.'
        }
    }
    elseif ($null -ne $Pair.changed) {
        throw 'Installed GPU observation campaign process affinity change must remain unknown.'
    }
}

function Get-WindowsLocalFrozenCampaignPairTarget([int]$PairIndex, [int]$OrderInPair) {
    if (($PairIndex % 2) -eq 1) {
        if ($OrderInPair -eq 1) { return 'cpu' }
        return 'gpu'
    }
    if ($OrderInPair -eq 1) { return 'gpu' }
    return 'cpu'
}

function Read-WindowsLocalFrozenCaptureCampaignReport(
    [string]$ReportPath,
    [psobject]$Expected
) {
    if (-not (Test-Path -LiteralPath $ReportPath -PathType Leaf)) {
        throw 'Installed GPU observation campaign report file is missing.'
    }
    $serialized = Read-WindowsFrozenCpuWorkerBoundedUtf8File $ReportPath (32MB)
    Assert-WindowsLocalFrozenCampaignJson $serialized.Text
    try {
        $report = $serialized.Text | ConvertFrom-Json -Depth 32
    }
    catch {
        throw 'Installed GPU observation campaign report is not valid JSON.'
    }
    Assert-WindowsLocalFrozenExactProperties $report @(
        'schema_version', 'kind', 'unsigned', 'unqualified', 'auto_eligible',
        'release_approved', 'collector_build_revision', 'expected_power', 'inputs',
        'gpu_identity', 'incomplete', 'cleanup_complete', 'captures', 'runs',
        'unavailable', 'environmental_controls'
    ) 'Installed GPU observation campaign report'
    if (($report.schema_version -isnot [int32] -and $report.schema_version -isnot [int64]) -or
        [int]$report.schema_version -ne 2 -or
        $report.kind -isnot [string] -or [string]$report.kind -cne 'windows_gpu_capture_campaign' -or
        $report.unsigned -isnot [bool] -or -not $report.unsigned -or
        $report.unqualified -isnot [bool] -or -not $report.unqualified -or
        $report.auto_eligible -isnot [bool] -or $report.auto_eligible -or
        $report.release_approved -isnot [bool] -or $report.release_approved -or
        $report.incomplete -isnot [bool] -or $report.incomplete -or
        $report.cleanup_complete -isnot [bool] -or -not $report.cleanup_complete -or
        $report.collector_build_revision -isnot [string] -or
        [string]$report.collector_build_revision -cne [string]$Expected.CollectorBuildRevision -or
        $report.expected_power -isnot [string] -or
        [string]$report.expected_power -cne [string]$Expected.CampaignPower) {
        throw 'Installed GPU observation campaign report has invalid local-only completion or identity flags.'
    }
    Assert-WindowsLocalFrozenExactProperties $report.inputs @('model_sha256', 'wav_sha256') 'Installed GPU observation campaign inputs'
    if ($report.inputs.model_sha256 -isnot [string] -or
        [string]$report.inputs.model_sha256 -cne [string]$Expected.ModelSha256 -or
        $report.inputs.wav_sha256 -isnot [string] -or
        [string]$report.inputs.wav_sha256 -cne [string]$Expected.WavSha256) {
        throw 'Installed GPU observation campaign report does not bind the installed model and requested WAV identities.'
    }
    Assert-WindowsLocalFrozenCampaignGpuIdentity $report.gpu_identity $Expected
    Assert-WindowsLocalFrozenExactProperties $report.unavailable @('inference_thread_count', 'thermal_state') 'Installed GPU observation campaign unavailable observations'
    Assert-WindowsLocalFrozenCampaignUnavailableField $report.unavailable.inference_thread_count 'unsupported_by_pinned_runtime_api' 'Installed GPU observation campaign inference thread count'
    Assert-WindowsLocalFrozenCampaignUnavailableField $report.unavailable.thermal_state 'not_observed' 'Installed GPU observation campaign thermal state'
    Assert-WindowsLocalFrozenExactProperties $report.environmental_controls @(
        'background_load', 'host_control', 'affinity_control', 'power_plan'
    ) 'Installed GPU observation campaign environmental controls'
    foreach ($property in $report.environmental_controls.PSObject.Properties.Name) {
        Assert-WindowsLocalFrozenCampaignUnavailableField $report.environmental_controls.$property 'not_observed' "Installed GPU observation campaign $property"
    }
    if ($report.captures -isnot [object[]] -or @($report.captures).Count -ne 14 -or
        $report.runs -isnot [object[]] -or @($report.runs).Count -ne 52) {
        throw 'Installed GPU observation campaign report does not contain the exact capture and request counts.'
    }

    $captureReferences = [Collections.Generic.List[string]]::new()
    $captureDigests = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    for ($index = 0; $index -lt 14; $index++) {
        $capture = @($report.captures)[$index]
        Assert-WindowsLocalFrozenExactProperties $capture @(
            'logical_sequence', 'generation_ref', 'digest_sha256', 'target', 'purpose',
            'hello_frame_hex', 'ready_frame_hex'
        ) 'Installed GPU observation campaign handshake capture'
        $sequence = $index + 1
        if ($index -eq 0) { $expectedTarget = 'cpu'; $expectedPurpose = 'preflight' }
        elseif ($index -eq 1) { $expectedTarget = 'gpu'; $expectedPurpose = 'preflight' }
        elseif ($index -lt 12) {
            $pairIndex = [int][Math]::Floor(($index - 2) / 2) + 1
            $order = (($index - 2) % 2) + 1
            $expectedTarget = Get-WindowsLocalFrozenCampaignPairTarget $pairIndex $order
            $expectedPurpose = 'cold'
        }
        else { $expectedTarget = if ($index -eq 12) { 'cpu' } else { 'gpu' }; $expectedPurpose = 'prime' }
        $digest = [string]$capture.digest_sha256
        $expectedReference = 'generation-{0:D2}-{1}' -f $sequence, $(if ($digest.Length -ge 24) { $digest.Substring(0, 24) } else { '' })
        foreach ($frame in @('hello_frame_hex', 'ready_frame_hex')) {
            $value = $capture.$frame
            if ($value -isnot [string] -or [string]$value -cnotmatch '^53434946[0-9a-f]{44,524332}$' -or
                ([string]$value).Length % 2 -ne 0) {
                throw 'Installed GPU observation campaign handshake frame is invalid.'
            }
        }
        if ((Assert-WindowsLocalFrozenCampaignInteger $capture.logical_sequence 'Installed GPU observation campaign capture sequence' -Positive) -ne $sequence -or
            $capture.target -isnot [string] -or [string]$capture.target -cne $expectedTarget -or
            $capture.purpose -isnot [string] -or [string]$capture.purpose -cne $expectedPurpose -or
            $capture.generation_ref -isnot [string] -or [string]$capture.generation_ref -cne $expectedReference -or
            $capture.digest_sha256 -isnot [string] -or $digest -cnotmatch '^[0-9a-f]{64}$' -or
            -not $captureDigests.Add($digest) -or
            (Get-WindowsLocalFrozenCampaignHandshakeDigest $capture.hello_frame_hex $capture.ready_frame_hex) -cne $digest) {
            throw 'Installed GPU observation campaign captures are not canonical, ordered, unique, and digest-bound.'
        }
        $captureReferences.Add([string]$capture.generation_ref)
    }

    for ($index = 0; $index -lt 52; $index++) {
        $run = @($report.runs)[$index]
        if ($index -lt 10) {
            $phase = 'cold'; $measured = $true
            $pairIndex = [int][Math]::Floor($index / 2) + 1; $order = ($index % 2) + 1
            $target = Get-WindowsLocalFrozenCampaignPairTarget $pairIndex $order
            $generationReference = $captureReferences[$index + 2]
        }
        elseif ($index -lt 12) {
            $phase = 'prime'; $measured = $false; $pairIndex = $null; $order = $index - 9
            $target = if ($index -eq 10) { 'cpu' } else { 'gpu' }
            $generationReference = $captureReferences[$index + 2]
        }
        else {
            $phase = 'warm'; $measured = $true
            $warmIndex = $index - 12; $pairIndex = [int][Math]::Floor($warmIndex / 2) + 1; $order = ($warmIndex % 2) + 1
            $target = Get-WindowsLocalFrozenCampaignPairTarget $pairIndex $order
            $generationReference = if ($target -ceq 'cpu') { $captureReferences[12] } else { $captureReferences[13] }
        }
        $properties = @(
            'measured', 'phase', 'order_in_pair', 'target', 'generation_ref', 'status',
            'power_source_before', 'power_source_after', 'end_to_end_ms', 'backend_ms',
            'model_load_ms', 'warm_reused', 'sampled_max_private_usage_bytes',
            'telemetry_sample_count', 'video_memory', 'raw_provider_memory',
            'memory_availability', 'worker_process_affinity', 'normalized_transcript_sha256'
        )
        if ($null -ne $pairIndex) { $properties += 'pair_index' }
        Assert-WindowsLocalFrozenExactProperties $run $properties 'Installed GPU observation campaign request record'
        if ($run.measured -isnot [bool] -or [bool]$run.measured -ne $measured -or
            $run.phase -isnot [string] -or [string]$run.phase -cne $phase -or
            (Assert-WindowsLocalFrozenCampaignInteger $run.order_in_pair 'Installed GPU observation campaign request order' -Positive) -ne $order -or
            $run.target -isnot [string] -or [string]$run.target -cne $target -or
            $run.generation_ref -isnot [string] -or [string]$run.generation_ref -cne $generationReference -or
            $run.status -isnot [string] -or [string]$run.status -cne 'succeeded' -or
            $run.power_source_before -isnot [string] -or [string]$run.power_source_before -cne [string]$Expected.CampaignPower -or
            $run.power_source_after -isnot [string] -or [string]$run.power_source_after -cne [string]$Expected.CampaignPower -or
            $run.warm_reused -isnot [bool] -or [bool]$run.warm_reused -ne ($phase -ceq 'warm') -or
            $run.normalized_transcript_sha256 -isnot [string] -or [string]$run.normalized_transcript_sha256 -cnotmatch '^[0-9a-f]{64}$') {
            throw 'Installed GPU observation campaign request sequence or completion state is invalid.'
        }
        if ($null -ne $pairIndex -and
            (Assert-WindowsLocalFrozenCampaignInteger $run.pair_index 'Installed GPU observation campaign pair index' -Positive) -ne $pairIndex) {
            throw 'Installed GPU observation campaign pair index is invalid.'
        }
        $null = Assert-WindowsLocalFrozenCampaignInteger $run.end_to_end_ms 'Installed GPU observation campaign end-to-end duration'
        $null = Assert-WindowsLocalFrozenCampaignInteger $run.backend_ms 'Installed GPU observation campaign backend duration'
        $modelLoad = Assert-WindowsLocalFrozenCampaignInteger $run.model_load_ms 'Installed GPU observation campaign model-load duration'
        $null = Assert-WindowsLocalFrozenCampaignInteger $run.sampled_max_private_usage_bytes 'Installed GPU observation campaign private usage'
        $null = Assert-WindowsLocalFrozenCampaignInteger $run.telemetry_sample_count 'Installed GPU observation campaign telemetry sample count' -Positive
        if ($phase -ceq 'warm' -and $modelLoad -ne 0) {
            throw 'Installed GPU observation campaign warm request reloaded its model.'
        }
        Assert-WindowsLocalFrozenCampaignVideoMemory $run.video_memory $target
        Assert-WindowsLocalFrozenCampaignProviderMemory $run.raw_provider_memory $target $report.gpu_identity
        Assert-WindowsLocalFrozenCampaignMemoryAvailability $run.memory_availability $target $report.gpu_identity
        Assert-WindowsLocalFrozenCampaignAffinityPair $run.worker_process_affinity
    }
    return [pscustomobject]@{ Report = $report; Bytes = $serialized.Bytes }
}

function Publish-WindowsLocalFrozenNewReport([string]$OutputPath, [byte[]]$Bytes, [int]$MaximumBytes = 1MB) {
    if (($MaximumBytes -ne 1MB -and $MaximumBytes -ne 32MB) -or
        $null -eq $Bytes -or $Bytes.Length -le 0 -or $Bytes.Length -gt $MaximumBytes) {
        throw 'Installed GPU observation publication bytes are outside the supported bound.'
    }
    $final = Get-WindowsLocalFrozenNormalizedFullPath $OutputPath
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $final
    if (Test-Path -LiteralPath $final) {
        throw 'Installed GPU observation report output already exists; refusing to replace it.'
    }
    $parent = Split-Path -Parent $final
    $null = Assert-WindowsLocalFrozenRegularDirectory $parent 'Installed GPU observation report parent'
    $staging = Join-Path $parent ".$([IO.Path]::GetFileName($final)).staging-$PID-$([guid]::NewGuid().ToString('N'))"
    if (Test-Path -LiteralPath $staging) {
        throw 'Installed GPU observation report staging path unexpectedly exists.'
    }
    $stream = $null
    try {
        $stream = [IO.File]::Open($staging, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        $stream.Write($Bytes, 0, $Bytes.Length)
        $stream.Flush($true)
        $stream.Dispose()
        $stream = $null
        Assert-WindowsFrozenCpuWorkerNoReparseAncestors $staging
        if (Test-Path -LiteralPath $final) {
            throw 'Installed GPU observation report output appeared during publication; refusing to replace it.'
        }
        [IO.File]::Move($staging, $final)
    }
    finally {
        if ($null -ne $stream) { $stream.Dispose() }
        if (Test-Path -LiteralPath $staging) {
            Assert-WindowsFrozenCpuWorkerNoReparseAncestors $staging
            Remove-Item -LiteralPath $staging -Force
        }
    }
    return $final
}

function Stop-WindowsLocalFrozenProcessTree(
    [System.Diagnostics.Process]$Process,
    [string]$Description,
    [int]$TerminationGraceMilliseconds = 5000
) {
    if ($TerminationGraceMilliseconds -lt 1 -or $TerminationGraceMilliseconds -gt 30000) {
        throw 'Local frozen process termination grace period is outside the supported bounds.'
    }
    if (-not $Process.HasExited) {
        try {
            $Process.Kill($true)
        }
        catch {
            try {
                $Process.Kill()
            }
            catch {
                throw "Could not terminate $Description after its bounded timeout."
            }
        }
    }
    if (-not $Process.WaitForExit($TerminationGraceMilliseconds)) {
        throw "$Description did not exit within $TerminationGraceMilliseconds milliseconds after termination."
    }
}

function Invoke-WindowsLocalFrozenBoundedProcess(
    [string]$Executable,
    [string[]]$Arguments,
    [string]$Description,
    [int]$TimeoutMilliseconds = 30000,
    [int]$StreamDrainMilliseconds = 5000
) {
    if ($TimeoutMilliseconds -lt 1 -or $TimeoutMilliseconds -gt 900000 -or
        $StreamDrainMilliseconds -lt 1 -or $StreamDrainMilliseconds -gt 30000) {
        throw 'Local frozen process timeout configuration is outside the supported bounds.'
    }
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $startInfo.ArgumentList.Add($argument) }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            throw "Could not start $Description."
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutMilliseconds)) {
            Stop-WindowsLocalFrozenProcessTree $process $Description
            throw "$Description timed out after $TimeoutMilliseconds milliseconds."
        }
        $streamTasks = [System.Threading.Tasks.Task[]]@($stdout, $stderr)
        if (-not [System.Threading.Tasks.Task]::WaitAll($streamTasks, $StreamDrainMilliseconds)) {
            throw "$Description output streams did not close within $StreamDrainMilliseconds milliseconds after process exit."
        }
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout.GetAwaiter().GetResult()
            Stderr = $stderr.GetAwaiter().GetResult()
        }
    }
    finally {
        $process.Dispose()
    }
}

function Invoke-WindowsLocalFrozenCampaignProcess(
    [string]$Executable,
    [string[]]$Arguments,
    [string]$Description,
    [int]$TimeoutMilliseconds = 900000,
    [int]$StreamDrainMilliseconds = 5000
) {
    if ($TimeoutMilliseconds -ne 900000 -or $StreamDrainMilliseconds -lt 1 -or $StreamDrainMilliseconds -gt 30000) {
        throw 'Local frozen campaign process timeout configuration is outside the fixed supported bounds.'
    }
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $startInfo.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $processStarted = $false
    try {
        if (-not $process.Start()) { throw "Could not start $Description." }
        $processStarted = $true
        $stdoutBuffer = [char[]]::new(8192)
        $stderrBuffer = [char[]]::new(8192)
        $stdoutText = [Text.StringBuilder]::new()
        $stderrText = [Text.StringBuilder]::new()
        $stdoutTask = $process.StandardOutput.ReadAsync($stdoutBuffer, 0, $stdoutBuffer.Length)
        $stderrTask = $process.StandardError.ReadAsync($stderrBuffer, 0, $stderrBuffer.Length)
        $stdoutComplete = $false
        $stderrComplete = $false
        $clock = [Diagnostics.Stopwatch]::StartNew()
        $exitedAt = $null
        $captureFailure = $null
        while ($true) {
            if (-not $stdoutComplete -and $stdoutTask.IsCompleted) {
                if ($stdoutTask.IsFaulted -or $stdoutTask.IsCanceled) {
                    $captureFailure = 'capture'
                }
                else {
                    $count = $stdoutTask.GetAwaiter().GetResult()
                    if ($count -eq 0) {
                        $stdoutComplete = $true
                    }
                    elseif ($stdoutText.Length -gt 262144 - $count) {
                        $captureFailure = 'overflow'
                    }
                    else {
                        $null = $stdoutText.Append($stdoutBuffer, 0, $count)
                        $stdoutTask = $process.StandardOutput.ReadAsync($stdoutBuffer, 0, $stdoutBuffer.Length)
                    }
                }
            }
            if (-not $stderrComplete -and $stderrTask.IsCompleted) {
                if ($stderrTask.IsFaulted -or $stderrTask.IsCanceled) {
                    $captureFailure = 'capture'
                }
                else {
                    $count = $stderrTask.GetAwaiter().GetResult()
                    if ($count -eq 0) {
                        $stderrComplete = $true
                    }
                    elseif ($stderrText.Length -gt 262144 - $count) {
                        $captureFailure = 'overflow'
                    }
                    else {
                        $null = $stderrText.Append($stderrBuffer, 0, $count)
                        $stderrTask = $process.StandardError.ReadAsync($stderrBuffer, 0, $stderrBuffer.Length)
                    }
                }
            }
            if ($null -ne $captureFailure) {
                try { Stop-WindowsLocalFrozenProcessTree $process $Description }
                catch { throw "$Description output capture failed and parent termination could not be confirmed." }
                if ($captureFailure -ceq 'overflow') {
                    throw "$Description exceeded the fixed 262144-character per-stream output bound."
                }
                throw "$Description output capture failed."
            }
            $exited = $process.HasExited
            if ($exited -and $null -eq $exitedAt) {
                $exitedAt = $clock.ElapsedMilliseconds
            }
            if ($exited -and $stdoutComplete -and $stderrComplete) {
                break
            }
            if ($clock.ElapsedMilliseconds -ge $TimeoutMilliseconds) {
                if (-not $exited) {
                    try { Stop-WindowsLocalFrozenProcessTree $process $Description }
                    catch { throw "$Description timed out and parent termination could not be confirmed." }
                    throw "$Description timed out after the fixed campaign deadline."
                }
                throw "$Description output streams did not close within the fixed campaign deadline."
            }
            if ($exited -and ($clock.ElapsedMilliseconds - $exitedAt) -ge $StreamDrainMilliseconds) {
                # A parent that has already exited can leave redirected handles
                # inherited by descendants. The Process API no longer gives us
                # an owned live parent to terminate, so fail closed without
                # claiming that those descendants were retired here.
                throw "$Description output streams did not close within the fixed post-exit drain deadline."
            }
            [Threading.Thread]::Sleep(10)
        }
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdoutText.ToString()
            Stderr = $stderrText.ToString()
        }
    }
    catch {
        $originalError = $_
        if ($processStarted) {
            try {
                if (-not $process.HasExited) {
                    Stop-WindowsLocalFrozenProcessTree $process $Description
                }
            }
            catch {
                # Preserve the primary capture/process failure. This is a
                # best-effort retry after the normal timeout/overflow paths;
                # those paths already report an unconfirmed termination.
            }
        }
        throw $originalError
    }
    finally { $process.Dispose() }
}

function Assert-WindowsLocalFrozenInstallerRecord(
    [string]$Path,
    [psobject]$DesktopContext,
    [psobject]$FrozenCpuWorker,
    [psobject]$Bundle,
    [string]$Installer
) {
    $recordFile = Read-WindowsFrozenCpuWorkerBoundedUtf8File $Path 65536
    try {
        $record = ConvertFrom-Json -InputObject $recordFile.Text -Depth 5 -NoEnumerate
    }
    catch {
        throw "Local frozen installer record is not valid JSON: $($_.Exception.Message)"
    }
    if ($record -is [array] -or $record -isnot [pscustomobject]) {
        throw 'Local frozen installer record must be one JSON object.'
    }
    if ($record.schema_version -isnot [int64] -and $record.schema_version -isnot [int32]) {
        throw 'Local frozen installer record has an unsupported schema.'
    }
    if ($record.kind -isnot [string]) {
        throw 'Local frozen installer record kind must be a string.'
    }
    $schemaVersion = [int]$record.schema_version
    if ($schemaVersion -eq 1) {
        Assert-WindowsLocalFrozenExactProperties $record @(
            'schema_version', 'kind', 'local_only', 'release_approved',
            'source_revision', 'app_version', 'target_triple',
            'frozen_record_sha256', 'bundle_inventory_sha256',
            'installer_filename', 'installer_size_bytes', 'installer_sha256',
            'local_test_token', 'install_relative_path'
        ) 'Local frozen installer record'
        foreach ($field in @(
            'source_revision', 'app_version', 'target_triple',
            'frozen_record_sha256', 'bundle_inventory_sha256'
        )) {
            if ($record.$field -isnot [string]) {
                throw "Local frozen installer schema-1 record field '$field' must be a string."
            }
        }
        if (-not (Test-WindowsFrozenCpuWorkerSameSourceContext $DesktopContext $FrozenCpuWorker.Context)) {
            throw 'Local frozen installer schema-1 record cannot represent distinct desktop and worker sources.'
        }
        if ($record.source_revision -cne $FrozenCpuWorker.Record.source_revision -or
            $record.app_version -cne $FrozenCpuWorker.Record.app_version -or
            $record.target_triple -cne $FrozenCpuWorker.Record.target_triple -or
            $record.frozen_record_sha256 -cne $FrozenCpuWorker.RecordSha256 -or
            $record.bundle_inventory_sha256 -cne $Bundle.InventorySha256) {
            throw 'Local frozen installer schema-1 record does not bind the exact frozen worker and bundle identities.'
        }
    }
    elseif ($schemaVersion -eq 2) {
        Assert-WindowsLocalFrozenExactProperties $record @(
            'schema_version', 'kind', 'local_only', 'release_approved',
            'desktop_source_revision', 'desktop_app_version', 'desktop_build_id', 'target_triple',
            'frozen_record_sha256', 'worker_source_revision', 'worker_app_version',
            'worker_origin_app_build', 'worker_build_id', 'bundled_cpu_worker_sha256',
            'bundle_inventory_sha256', 'installer_filename', 'installer_size_bytes',
            'installer_sha256', 'local_test_token', 'install_relative_path'
        ) 'Local frozen installer mixed-source record'
        foreach ($field in @(
            'desktop_source_revision', 'desktop_app_version', 'desktop_build_id',
            'target_triple', 'frozen_record_sha256', 'worker_source_revision',
            'worker_app_version', 'worker_origin_app_build', 'worker_build_id',
            'bundled_cpu_worker_sha256', 'bundle_inventory_sha256'
        )) {
            if ($record.$field -isnot [string]) {
                throw "Local frozen installer mixed-source record field '$field' must be a string."
            }
        }
        if ((Test-WindowsFrozenCpuWorkerSameSourceContext $DesktopContext $FrozenCpuWorker.Context) -or
            $record.desktop_source_revision -cne $DesktopContext.SourceRevision -or
            $record.desktop_app_version -cne $DesktopContext.AppVersion -or
            $record.desktop_build_id -cne $DesktopContext.DesktopBuildId -or
            $record.target_triple -cne $DesktopContext.TargetTriple -or
            $record.frozen_record_sha256 -cne $FrozenCpuWorker.RecordSha256 -or
            $record.worker_source_revision -cne $FrozenCpuWorker.Record.source_revision -or
            $record.worker_app_version -cne $FrozenCpuWorker.Record.app_version -or
            $record.worker_origin_app_build -cne $FrozenCpuWorker.Context.DesktopBuildId -or
            $record.worker_build_id -cne $FrozenCpuWorker.Context.WorkerBuildId -or
            $record.bundled_cpu_worker_sha256 -cne $FrozenCpuWorker.Record.worker_sha256 -or
            $record.bundle_inventory_sha256 -cne $Bundle.InventorySha256) {
            throw 'Local frozen installer mixed-source record does not bind the exact desktop and frozen worker identities.'
        }
        foreach ($field in @(
            'frozen_record_sha256', 'bundled_cpu_worker_sha256', 'bundle_inventory_sha256'
        )) {
            Assert-WindowsFrozenCpuWorkerDigest $record.$field "Local frozen installer mixed-source $field"
        }
    }
    else {
        throw 'Local frozen installer record has an unsupported schema.'
    }
    if ($record.kind -cne 'windows-local-frozen-test-installer' -or
        $record.local_only -isnot [bool] -or -not $record.local_only -or
        $record.release_approved -isnot [bool] -or $record.release_approved) {
        throw 'Local frozen installer record does not bind the exact local frozen bundle and worker identities.'
    }
    if ($record.installer_filename -isnot [string] -or
        $record.installer_filename -cnotmatch '^Scribe-LOCAL-Frozen-Test-[0-9a-f]{32}\.exe$' -or
        (Split-Path -Leaf $Installer) -cne $record.installer_filename -or
        $record.installer_sha256 -isnot [string] -or $record.installer_sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        $record.local_test_token -isnot [string] -or $record.local_test_token -cnotmatch '^[0-9a-f]{32}$' -or
        $record.installer_filename -cne "Scribe-LOCAL-Frozen-Test-$($record.local_test_token).exe" -or
        $record.install_relative_path -isnot [string] -or
        $record.install_relative_path -cne "Scribe/LOCAL-Frozen-Test/$($record.local_test_token)") {
        throw 'Local frozen installer record has an invalid installer or local-test identity.'
    }
    $installerItem = Assert-WindowsFrozenCpuWorkerRegularFile $Installer
    $installerSize = Assert-WindowsLocalFrozenInt64 $record.installer_size_bytes 'Local frozen installer record installer size'
    if ($installerItem.Length -ne $installerSize -or
        (Get-WindowsFrozenCpuWorkerFileSha256 $Installer) -cne $record.installer_sha256) {
        throw 'Local frozen installer bytes do not match the local installer record.'
    }
    return $record
}

function Get-WindowsLocalFrozenSmokeArguments([string]$InstalledRoot, [psobject]$ModelManifest) {
    return @(
        '--scribe-install-smoke-parent',
        [string]$ModelManifest.model_id,
        (Join-Path -Path $InstalledRoot -ChildPath ([string]$ModelManifest.artifact_filename)),
        'gguf',
        [string]$ModelManifest.size_bytes,
        [string]$ModelManifest.sha256,
        'cpu'
    )
}

function Assert-WindowsLocalFrozenSmokeDiagnostics([psobject]$Smoke) {
    if ($null -eq $Smoke) {
        throw 'Installed local frozen CPU smoke did not verify the expected worker cancellation contract.'
    }
    $smokeProperties = @($Smoke.PSObject.Properties | ForEach-Object { $_.Name })
    if ('cancellation_verified' -notin $smokeProperties -or
        'capabilities' -notin $smokeProperties -or
        'detected_architecture' -notin $smokeProperties) {
        throw 'Installed local frozen CPU smoke did not verify the expected worker cancellation contract.'
    }
    $capabilities = $Smoke.capabilities
    if ($null -eq $capabilities -or
        'cancellation' -notin @($capabilities.PSObject.Properties | ForEach-Object { $_.Name }) -or
        $Smoke.cancellation_verified -isnot [bool] -or $Smoke.cancellation_verified -ne $true -or
        $capabilities.cancellation -isnot [bool] -or $capabilities.cancellation -ne $true -or
        [string]$Smoke.detected_architecture -cne 'whisper') {
        throw 'Installed local frozen CPU smoke did not verify the expected worker cancellation contract.'
    }
}

function Get-WindowsLocalFrozenRemovalPathMetadata([string]$Path) {
    # Keep this as one metadata observation. The fixture suite shadows this
    # private seam to reproduce deletion exactly at the observation boundary.
    return [System.IO.File]::GetAttributes($Path)
}

function Get-WindowsLocalFrozenRemovalPathUnderlyingException([System.Exception]$Exception) {
    $current = $Exception
    while ($null -ne $current.InnerException -and
        ($current -is [System.Management.Automation.MethodInvocationException] -or
            $current -is [System.Management.Automation.RuntimeException])) {
        $current = $current.InnerException
    }
    return $current
}

function Test-WindowsLocalFrozenRemovalPathIsAbsent([System.Exception]$Exception) {
    $underlying = Get-WindowsLocalFrozenRemovalPathUnderlyingException $Exception
    return ($underlying -is [System.IO.FileNotFoundException] -and
        $underlying.HResult -eq -2147024894) -or
        ($underlying -is [System.IO.DirectoryNotFoundException] -and
            $underlying.HResult -eq -2147024893)
}

function Test-WindowsLocalFrozenInstallRootRemovalObserved([string]$Path) {
    $current = Get-WindowsLocalFrozenNormalizedFullPath $Path
    $missingPathObserved = $false
    $survivingRegularAncestorObserved = $false

    while ($true) {
        $attributes = $null
        try {
            $attributes = Get-WindowsLocalFrozenRemovalPathMetadata $current
        }
        catch {
            $metadataException = Get-WindowsLocalFrozenRemovalPathUnderlyingException $_.Exception
            if (Test-WindowsLocalFrozenRemovalPathIsAbsent $metadataException) {
                $missingPathObserved = $true
            }
            else {
                throw "Local frozen installer removal metadata inspection failed at '$current': $($metadataException.Message)"
            }
        }

        if ($null -ne $attributes) {
            if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Local frozen installer removal path cannot cross a symbolic link or reparse point: $current"
            }
            if (($attributes -band [System.IO.FileAttributes]::Directory) -eq 0) {
                throw "Local frozen installer removal path has a non-directory replacement: $current"
            }
            if ($missingPathObserved) {
                $survivingRegularAncestorObserved = $true
            }
        }

        $parent = Split-Path -Parent $current
        if ([string]::IsNullOrEmpty($parent) -or
            [string]::Equals($parent, $current, [System.StringComparison]::OrdinalIgnoreCase)) {
            break
        }
        $current = $parent
    }

    return $missingPathObserved -and $survivingRegularAncestorObserved
}

function Wait-WindowsLocalFrozenInstallRootRemoved(
    [string]$Path,
    [int]$TimeoutMilliseconds = 5000
) {
    if ($TimeoutMilliseconds -lt 1 -or $TimeoutMilliseconds -gt 30000) {
        throw 'Local frozen installer removal timeout is outside the supported bounds.'
    }
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        if (Test-WindowsLocalFrozenInstallRootRemovalObserved $Path) {
            return
        }
        $remaining = $TimeoutMilliseconds - [int]$stopwatch.ElapsedMilliseconds
        if ($remaining -le 0) {
            throw "Local frozen installer uninstaller left its token-bound program directory after $TimeoutMilliseconds milliseconds."
        }
        Start-Sleep -Milliseconds ([Math]::Min(100, $remaining))
    }
}

function Get-WindowsLocalFrozenDeclaredPackFiles(
    [string]$Root,
    [switch]$VerifyCompiledDescriptors
) {
    $catalogPath = Join-Path $Root 'worker-pack-catalog.json'
    $catalogText = Read-WindowsFrozenCpuWorkerBoundedUtf8File $catalogPath (4MB)
    try {
        $catalog = $catalogText.Text | ConvertFrom-Json -Depth 10
    }
    catch {
        throw "Local frozen installer worker-pack catalog is not valid JSON: $($_.Exception.Message)"
    }
    Assert-WindowsLocalFrozenExactProperties $catalog @('schema_version', 'packs') 'Local frozen installer worker-pack catalog'
    if ($catalog.schema_version -isnot [int64] -and $catalog.schema_version -isnot [int32]) {
        throw 'Local frozen installer worker-pack catalog schema must be an integer.'
    }
    if ([int]$catalog.schema_version -ne 1) {
        throw 'Local frozen installer worker-pack catalog has an unsupported schema.'
    }
    $packs = @($catalog.packs)
    if ($packs.Count -gt 8) {
        throw 'Local frozen installer worker-pack catalog exceeds the eight-pack bound.'
    }

    $files = [System.Collections.Generic.List[string]]::new()
    $identities = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($pack in $packs) {
        Assert-WindowsLocalFrozenExactProperties $pack @(
            'pack_id', 'pack_version', 'pack_digest', 'security_epoch',
            'runtime_abi_version', 'backend', 'provider', 'target_os',
            'target_arch', 'worker_relative_path', 'root',
            'installed_size_bytes', 'compressed_size_bytes', 'files'
        ) 'Local frozen installer worker-pack catalog entry'
        foreach ($property in @('pack_id', 'pack_version', 'provider')) {
            if ($pack.$property -isnot [string] -or [string]$pack.$property -cnotmatch '^[A-Za-z0-9._:-]{1,96}$') {
                throw "Local frozen installer worker-pack catalog has an invalid $property."
            }
        }
        if ($pack.pack_digest -isnot [string] -or [string]$pack.pack_digest -cnotmatch '^[0-9a-f]{64}$') {
            throw 'Local frozen installer worker-pack catalog has a non-canonical digest.'
        }
        $expectedRoot = "workers/packs/$($pack.pack_id)/$($pack.pack_version)/$($pack.pack_digest)"
        if ([string]$pack.root -cne $expectedRoot) {
            throw 'Local frozen installer worker-pack root does not match the immutable layout.'
        }
        Assert-WindowsLocalFrozenSafeRelativePath $expectedRoot
        $packFiles = @($pack.files)
        if ($packFiles.Count -lt 3 -or $packFiles.Count -gt 258) {
            throw 'Local frozen installer worker-pack catalog file count is outside the supported bound.'
        }
        $actualSize = [int64]0
        foreach ($file in $packFiles) {
            if ($file -isnot [string]) {
                throw 'Local frozen installer worker-pack catalog paths must be strings.'
            }
            Assert-WindowsLocalFrozenSafeRelativePath $file
            if (-not $file.StartsWith($expectedRoot + '/', [System.StringComparison]::Ordinal)) {
                throw 'Local frozen installer worker-pack catalog file escapes its immutable root.'
            }
            if (-not $identities.Add($file)) {
                throw "Local frozen installer worker-pack catalog contains a duplicate case-insensitive path: $file"
            }
            $item = Assert-WindowsFrozenCpuWorkerRegularFile (Join-Path $Root ($file -replace '/', '\'))
            $actualSize += [int64]$item.Length
            $files.Add($file)
        }
        if ((Assert-WindowsLocalFrozenInt64 $pack.installed_size_bytes 'Local frozen installer worker-pack installed size') -ne $actualSize -or
            (Assert-WindowsLocalFrozenInt64 $pack.compressed_size_bytes 'Local frozen installer worker-pack compressed size') -lt 0) {
            throw 'Local frozen installer worker-pack catalog size evidence is invalid.'
        }

        if ($VerifyCompiledDescriptors) {
            $verifier = Join-Path $Root 'local-transcriber.exe'
            $verification = Invoke-WindowsLocalFrozenBoundedProcess `
                -Executable $verifier `
                -Arguments @('--scribe-verify-worker-pack', (Join-Path $Root ($expectedRoot -replace '/', '\'))) `
                -Description 'Frozen bundle compiled worker-pack verification'
            if ($verification.ExitCode -ne 0) {
                throw "Frozen bundle compiled worker-pack verification failed closed: $($verification.Stderr.Trim())"
            }
            try {
                $descriptor = $verification.Stdout | ConvertFrom-Json -Depth 8
            }
            catch {
                throw "Frozen bundle compiled worker-pack verifier returned invalid JSON: $($_.Exception.Message)"
            }
            foreach ($field in @(
                'pack_id', 'pack_version', 'pack_digest', 'security_epoch',
                'runtime_abi_version', 'backend', 'provider', 'target_os',
                'target_arch', 'worker_relative_path'
            )) {
                if ([string]$descriptor.$field -cne [string]$pack.$field) {
                    throw "Frozen bundle compiled worker-pack verifier differs at '$field'."
                }
            }
        }
    }
    if ($files.Count -gt 1024) {
        throw 'Local frozen installer worker-pack catalog exceeds the 1,024-file bound.'
    }
    return $files.ToArray()
}

function Assert-WindowsLocalFrozenPayloadTree([string]$Root, [string[]]$ExpectedFiles) {
    $null = Assert-WindowsLocalFrozenRegularDirectory $Root 'Local frozen installer payload'
    $expected = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $ExpectedFiles) {
        Assert-WindowsLocalFrozenSafeRelativePath $path
        if (-not $expected.Add($path)) {
            throw "Local frozen installer expected payload contains a case-insensitive collision: $path"
        }
    }
    $actual = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($item in @(Get-ChildItem -LiteralPath $Root -Recurse -File -Force)) {
        Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $item.FullName
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Local frozen installer payload contains a link or reparse point: $($item.FullName)"
        }
        $relative = Get-WindowsLocalFrozenRelativePath $Root $item.FullName
        Assert-WindowsLocalFrozenSafeRelativePath $relative
        if (-not $actual.Add($relative)) {
            throw "Local frozen installer payload contains a case-insensitive file collision: $relative"
        }
    }
    foreach ($item in @(Get-ChildItem -LiteralPath $Root -Recurse -Directory -Force)) {
        Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $item.FullName
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Local frozen installer payload contains a directory link or reparse point: $($item.FullName)"
        }
        Assert-WindowsLocalFrozenSafeRelativePath (Get-WindowsLocalFrozenRelativePath $Root $item.FullName)
    }
    if ($actual.Count -ne $expected.Count -or -not $expected.SetEquals($actual)) {
        throw 'Local frozen installer payload differs from its exact declared inventory.'
    }
}

function Assert-WindowsLocalFrozenBundle(
    [Parameter(Mandatory = $true)]
    [string]$BundlePath,
    [psobject]$FrozenCpuWorker = $null
) {
    $root = Get-WindowsLocalFrozenNormalizedFullPath $BundlePath
    $null = Assert-WindowsLocalFrozenRegularDirectory $root 'Local frozen installer bundle'
    $packFiles = @(Get-WindowsLocalFrozenDeclaredPackFiles $root)
    $expectedFiles = @($script:WindowsLocalFrozenBaseFiles) + $packFiles
    Assert-WindowsLocalFrozenPayloadTree $root (@($expectedFiles) + @('bundle-inventory.json'))

    if ($null -ne $FrozenCpuWorker) {
        $marker = Read-WindowsFrozenCpuWorkerBoundedUtf8File (Join-Path $root (Get-WindowsFrozenCpuWorkerMarkerFileName)) 8192
        $expectedMarker = Get-WindowsFrozenCpuWorkerBundleMarkerText $FrozenCpuWorker.RecordSha256 $FrozenCpuWorker.Record
        if ($marker.Text -cne $expectedMarker) {
            throw 'Local frozen installer bundle marker does not bind the supplied frozen CPU worker record.'
        }
        $workerPath = Join-Path $root (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
        $worker = Assert-WindowsFrozenCpuWorkerRegularFile $workerPath
        if ($worker.Length -ne [int64]$FrozenCpuWorker.Record.worker_size_bytes -or
            (Get-WindowsFrozenCpuWorkerFileSha256 $workerPath) -cne [string]$FrozenCpuWorker.Record.worker_sha256) {
            throw 'Local frozen installer bundle CPU worker does not preserve the supplied frozen worker bytes.'
        }
    }

    $inventoryText = Read-WindowsFrozenCpuWorkerBoundedUtf8File (Join-Path $root 'bundle-inventory.json') (4MB)
    try {
        $inventory = $inventoryText.Text | ConvertFrom-Json -Depth 8
    }
    catch {
        throw "Local frozen installer bundle inventory is not valid JSON: $($_.Exception.Message)"
    }
    Assert-WindowsLocalFrozenExactProperties $inventory @('schema_version', 'platform_triple', 'files') 'Local frozen installer bundle inventory'
    if ((($inventory.schema_version -isnot [int64] -and $inventory.schema_version -isnot [int32]) -or
        [int]$inventory.schema_version -ne 1 -or
        [string]$inventory.platform_triple -cne (Get-WindowsFrozenCpuWorkerTargetTriple))) {
        throw 'Local frozen installer bundle inventory has an unsupported schema or platform.'
    }
    $inventoryEntries = @($inventory.files)
    if ($inventoryEntries.Count -ne $expectedFiles.Count) {
        throw 'Local frozen installer bundle inventory does not contain the exact payload entry count.'
    }
    $paths = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $inventoryEntries) {
        Assert-WindowsLocalFrozenExactProperties $entry @('path', 'size_bytes', 'sha256') 'Local frozen installer bundle inventory entry'
        if ($entry.path -isnot [string] -or $entry.sha256 -isnot [string] -or $entry.sha256 -cnotmatch '^[0-9a-f]{64}$') {
            throw 'Local frozen installer bundle inventory contains non-canonical path or SHA-256 data.'
        }
        Assert-WindowsLocalFrozenSafeRelativePath $entry.path
        if (-not $paths.Add($entry.path)) {
            throw "Local frozen installer bundle inventory contains a case-insensitive collision: $($entry.path)"
        }
        $size = Assert-WindowsLocalFrozenInt64 $entry.size_bytes 'Local frozen installer bundle inventory size'
        if ($size -lt 0) {
            throw 'Local frozen installer bundle inventory contains a negative size.'
        }
        $path = Join-Path $root ($entry.path -replace '/', '\')
        $item = Assert-WindowsFrozenCpuWorkerRegularFile $path
        if ($item.Length -ne $size -or (Get-WindowsFrozenCpuWorkerFileSha256 $path) -cne $entry.sha256) {
            throw "Local frozen installer bundle inventory does not match $($entry.path)."
        }
    }
    $expected = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $expectedFiles) { $null = $expected.Add($path) }
    if ($paths.Count -ne $expected.Count -or -not $expected.SetEquals($paths)) {
        throw 'Local frozen installer bundle inventory paths differ from the exact payload allowlist.'
    }
    $repositoryRoot = Get-WindowsLocalFrozenNormalizedFullPath (Split-Path -Parent $PSScriptRoot)
    $sourceModelManifestPath = Join-Path $repositoryRoot 'runtime-manifests\whisper-base-en-q8_0-windows-x64.json'
    $sourceModelManifest = Read-WindowsFrozenCpuWorkerBoundedUtf8File $sourceModelManifestPath 65536
    $bundledModelManifestPath = Join-Path $root 'bundled-model-manifest.json'
    $bundledModelManifest = Assert-WindowsFrozenCpuWorkerRegularFile $bundledModelManifestPath
    if ($bundledModelManifest.Length -ne $sourceModelManifest.Bytes.Length -or
        (Get-WindowsFrozenCpuWorkerFileSha256 $bundledModelManifestPath) -cne $sourceModelManifest.Sha256) {
        throw 'Local frozen installer bundled model manifest differs from the pinned source manifest.'
    }
    try {
        $model = $sourceModelManifest.Text | ConvertFrom-Json -Depth 5
    }
    catch {
        throw "Pinned local frozen installer model manifest is not valid JSON: $($_.Exception.Message)"
    }
    if ($model.artifact_filename -isnot [string] -or $model.artifact_filename -cne 'whisper-base.en-Q8_0.gguf' -or
        $model.sha256 -isnot [string] -or $model.sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        [string]$model.platform_triple -cne (Get-WindowsFrozenCpuWorkerTargetTriple)) {
        throw 'Pinned local frozen installer model manifest has an unsupported identity.'
    }
    $modelSize = Assert-WindowsLocalFrozenInt64 $model.size_bytes 'Pinned local frozen installer model size'
    $bundleModelPath = Join-Path $root $model.artifact_filename
    $bundleModel = Assert-WindowsFrozenCpuWorkerRegularFile $bundleModelPath
    if ($bundleModel.Length -ne $modelSize -or
        (Get-WindowsFrozenCpuWorkerFileSha256 $bundleModelPath) -cne [string]$model.sha256) {
        throw 'Local frozen installer bundled model differs from the pinned source manifest.'
    }
    foreach ($relativePath in @(
        'licenses/Apache-2.0.txt', 'licenses/OpenAI-Whisper-MIT.txt',
        'licenses/Whisper-Base-En-NOTICE.txt', 'licenses/THIRD-PARTY-NOTICES.txt',
        'licenses/transcribe.cpp-MIT.txt', 'licenses/transcribe.cpp-PROVENANCE.md',
        'licenses/whisper.cpp-MIT.txt', 'licenses/whisper.cpp-PROVENANCE.md',
        'licenses/sherpa-onnx-PROVENANCE.md', 'licenses/Silero-VAD-MIT.txt',
        'licenses/Silero-VAD-PROVENANCE.md'
    )) {
        $sourceRelativePath = switch ($relativePath) {
            'licenses/transcribe.cpp-MIT.txt' { 'native/transcribe-cpp-v0.1.3/LICENSE' }
            'licenses/transcribe.cpp-PROVENANCE.md' { 'native/transcribe-cpp-v0.1.3/PROVENANCE.md' }
            'licenses/whisper.cpp-MIT.txt' { 'native/whisper-f049fff/LICENSE' }
            'licenses/whisper.cpp-PROVENANCE.md' { 'native/whisper-f049fff/PROVENANCE.md' }
            'licenses/sherpa-onnx-PROVENANCE.md' { 'native/sherpa-onnx-v1.13.5/PROVENANCE.md' }
            'licenses/Silero-VAD-MIT.txt' { 'resources/silero-vad/LICENSE' }
            'licenses/Silero-VAD-PROVENANCE.md' { 'resources/silero-vad/PROVENANCE.md' }
            default { 'resources/licenses/' + (Split-Path -Leaf $relativePath) }
        }
        $sourcePath = Join-Path $repositoryRoot ($sourceRelativePath -replace '/', '\')
        $source = Assert-WindowsFrozenCpuWorkerRegularFile $sourcePath
        $candidatePath = Join-Path $root ($relativePath -replace '/', '\')
        $candidate = Assert-WindowsFrozenCpuWorkerRegularFile $candidatePath
        if ($candidate.Length -ne $source.Length -or
            (Get-WindowsFrozenCpuWorkerFileSha256 $candidatePath) -cne (Get-WindowsFrozenCpuWorkerFileSha256 $sourcePath)) {
            throw "Local frozen installer legal payload differs from the pinned source file: $relativePath"
        }
    }
    $null = Assert-ReviewedWindowsPe (Join-Path $root 'local-transcriber.exe') 2
    $null = Assert-ReviewedWindowsPe (Join-Path $root 'scribe-inference-worker.exe') 3
    $null = Get-WindowsLocalFrozenDeclaredPackFiles $root -VerifyCompiledDescriptors
    return [pscustomobject]@{
        Root = $root
        InventorySha256 = $inventoryText.Sha256
        InventoryEntries = @($inventoryEntries)
        Files = @($expectedFiles | Sort-Object)
        PackFiles = $packFiles
    }
}

function Assert-WindowsLocalFrozenPayloadParity(
    [Parameter(Mandatory = $true)]
    [string]$ReferenceBundlePath,
    [Parameter(Mandatory = $true)]
    [string]$InstalledPath,
    [string[]]$AllowedAdditionalRootFiles = @('unins000.exe', 'unins000.dat')
) {
    $reference = Assert-WindowsLocalFrozenBundle $ReferenceBundlePath
    $installed = Get-WindowsLocalFrozenNormalizedFullPath $InstalledPath
    $expected = @($reference.Files) + @('bundle-inventory.json') + @($AllowedAdditionalRootFiles)
    Assert-WindowsLocalFrozenPayloadTree $installed $expected
    foreach ($path in @($reference.Files) + @('bundle-inventory.json')) {
        $referencePath = Join-Path $reference.Root ($path -replace '/', '\')
        $installedPath = Join-Path $installed ($path -replace '/', '\')
        $referenceItem = Assert-WindowsFrozenCpuWorkerRegularFile $referencePath
        $installedItem = Assert-WindowsFrozenCpuWorkerRegularFile $installedPath
        if ($installedItem.Length -ne $referenceItem.Length -or
            (Get-WindowsFrozenCpuWorkerFileSha256 $installedPath) -cne (Get-WindowsFrozenCpuWorkerFileSha256 $referencePath)) {
            throw "Local frozen installer payload parity mismatch for $path."
        }
    }
    # The metadata was parity-verified against the reference bundle, but all
    # subsequent reads must resolve inside the installed payload.  Returning
    # the reference root here would let post-install observation inspect the
    # source catalog instead of the installed catalog it is about to use.
    return [pscustomobject]@{
        Root = $installed
        InventorySha256 = $reference.InventorySha256
        InventoryEntries = $reference.InventoryEntries
        Files = $reference.Files
        PackFiles = $reference.PackFiles
    }
}
