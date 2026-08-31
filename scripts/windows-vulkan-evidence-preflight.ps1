Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-gpu-worker-cmake-bootstrap.ps1')

function Assert-ScribeEvidenceNoReparse([string]$Path) {
    $current = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    while (-not (Test-Path -LiteralPath $current)) {
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) { throw 'Could not find an existing non-reparse ancestor.' }
        $current = $parent
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw 'Evidence path crosses a reparse point.'
        }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) { break }
        $current = $parent
    }
}

function Get-ScribeEvidencePhysicalDirectory([string]$Path, [string]$Label) {
    Assert-ScribeEvidenceNoReparse $Path
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label must be a physical non-reparse directory."
    }
    return $item
}

function Assert-ScribeEvidenceFile([string]$Path, [string]$Label, [UInt64]$MaxBytes) {
    $full = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    Assert-ScribeEvidenceNoReparse $full
    if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { throw "$Label is missing." }
    $item = Get-Item -LiteralPath $full -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or $item.Length -eq 0 -or $item.Length -gt $MaxBytes) {
        throw "$Label is not a bounded regular non-reparse file."
    }
    return $full
}

function Assert-ScribeEvidenceSingleLinkFile([string]$Path, [string]$Label, [UInt64]$MaxBytes, [string]$FsutilPath) {
    $full = Assert-ScribeEvidenceFile $Path $Label $MaxBytes
    $links = @(& $FsutilPath hardlink list $full)
    if ($LASTEXITCODE -ne 0 -or $links.Count -ne 1) { throw "$Label must have exactly one hard link." }
    return $full
}

function Assert-ScribeEvidenceExactProperties([psobject]$Value, [string[]]$Names, [string]$Label) {
    if ($null -eq $Value) { throw "$Label is missing." }
    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $expected = @($Names | Sort-Object)
    if ($actual.Count -ne $expected.Count -or
        (Compare-Object -ReferenceObject $expected -DifferenceObject $actual -CaseSensitive)) {
        throw "$Label has unknown or missing fields."
    }
}

function Assert-ScribeEvidenceDirectChildPath(
    [string]$Path,
    [string]$Root,
    [string]$ExpectedLeaf,
    [string]$Label
) {
    if ($ExpectedLeaf -cnotmatch '^[a-z0-9][a-z0-9._-]{0,127}\.json$') {
        throw "$Label has an unsafe expected leaf."
    }
    $rootItem = Get-ScribeEvidencePhysicalDirectory $Root "$Label root"
    $canonicalRoot = $rootItem.FullName.TrimEnd([char[]]@('\', '/'))
    $full = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    if ((Split-Path -Leaf $full) -cne $ExpectedLeaf -or
        (Split-Path -Parent $full).TrimEnd([char[]]@('\', '/')) -cne $canonicalRoot) {
        throw "$Label must be the exact direct child of its evidence root."
    }
    Assert-ScribeEvidenceNoReparse $full
    return $full
}

function Assert-ScribeEvidenceNoAlternateDataStreams([string]$Path, [string]$Label) {
    $streams = @(Get-Item -LiteralPath $Path -Stream * -ErrorAction Stop)
    if ($streams.Count -ne 1 -or [string]$streams[0].Stream -cne ':$DATA') {
        throw "$Label must not contain alternate data streams."
    }
}

function Assert-ScribeEvidenceUnsignedInteger([object]$Value, [string]$Label) {
    $text = [Convert]::ToString($Value, [Globalization.CultureInfo]::InvariantCulture)
    [UInt64]$parsed = 0
    if ($text -cnotmatch '^[0-9]+$' -or
        -not [UInt64]::TryParse($text, [Globalization.NumberStyles]::None, [Globalization.CultureInfo]::InvariantCulture, [ref]$parsed)) {
        throw "$Label must be an unsigned bounded integer."
    }
}

function Assert-ScribeEvidenceMetadataString([object]$Value, [int]$MaximumLength, [string]$Label) {
    $text = [string]$Value
    if ([string]::IsNullOrWhiteSpace($text) -or
        $text.Length -gt $MaximumLength -or
        $text.IndexOfAny([char[]]@('\', '/', "`r", "`n", [char]0)) -ge 0 -or
        $text.ToCharArray().Where({ [int]$_ -lt 0x20 -or [int]$_ -gt 0x7e }).Count -ne 0) {
        throw "$Label is not bounded printable metadata."
    }
}

function Assert-ScribeEvidenceRunSet([psobject]$Value, [int]$ExpectedCount, [bool]$Cold, [string]$Label) {
    Assert-ScribeEvidenceExactProperties $Value @(
        'end_to_end', 'end_to_end_ms', 'backend_processing',
        'backend_processing_ms', 'model_load', 'model_load_ms'
    ) $Label
    foreach ($statisticsName in @('end_to_end', 'backend_processing')) {
        Assert-ScribeEvidenceExactProperties $Value.$statisticsName @('p50_ms', 'p95_ms') "$Label $statisticsName"
        Assert-ScribeEvidenceUnsignedInteger $Value.$statisticsName.p50_ms "$Label $statisticsName p50_ms"
        Assert-ScribeEvidenceUnsignedInteger $Value.$statisticsName.p95_ms "$Label $statisticsName p95_ms"
    }
    foreach ($samplesName in @('end_to_end_ms', 'backend_processing_ms')) {
        if (@($Value.$samplesName).Count -ne $ExpectedCount) {
            throw "$Label $samplesName has an unexpected sample count."
        }
        foreach ($sample in @($Value.$samplesName)) {
            Assert-ScribeEvidenceUnsignedInteger $sample "$Label $samplesName sample"
        }
    }
    if ($Cold) {
        Assert-ScribeEvidenceExactProperties $Value.model_load @('p50_ms', 'p95_ms') "$Label model_load"
        if (@($Value.model_load_ms).Count -ne $ExpectedCount) {
            throw "$Label model_load_ms has an unexpected sample count."
        }
        Assert-ScribeEvidenceUnsignedInteger $Value.model_load.p50_ms "$Label model_load p50_ms"
        Assert-ScribeEvidenceUnsignedInteger $Value.model_load.p95_ms "$Label model_load p95_ms"
        foreach ($sample in @($Value.model_load_ms)) {
            Assert-ScribeEvidenceUnsignedInteger $sample "$Label model_load_ms sample"
        }
    }
    elseif ($null -ne $Value.model_load -or $null -ne $Value.model_load_ms) {
        throw "$Label contains unexpected warm-run model-load evidence."
    }
}

function Assert-ScribeEvidencePendingReport(
    [string]$PendingPath,
    [string]$EvidenceRoot,
    [string]$PendingLeaf,
    [string]$FsutilPath
) {
    $pending = Assert-ScribeEvidenceDirectChildPath $PendingPath $EvidenceRoot $PendingLeaf 'Pending evidence report'
    $pending = Assert-ScribeEvidenceSingleLinkFile $pending 'Pending evidence report' (1MB) $FsutilPath
    Assert-ScribeEvidenceNoAlternateDataStreams $pending 'Pending evidence report'
    try {
        $report = [IO.File]::ReadAllText($pending, [Text.UTF8Encoding]::new($false, $true)) | ConvertFrom-Json
    }
    catch {
        throw 'Pending evidence report is not strict UTF-8 JSON.'
    }
    Assert-ScribeEvidenceExactProperties $report @(
        'schema_version', 'fixture_only', 'untrusted', 'auto_eligible',
        'source_revision', 'pack', 'model_sha256', 'wav_sha256', 'gpu',
        'nvidia_baseline', 'cold_runs_per_backend', 'warm_runs_per_backend',
        'cpu', 'vulkan', 'expected_phrase_present_every_run',
        'normalized_transcript_parity', 'same_device_internally_verified'
    ) 'Pending evidence report'
    if ($report.schema_version -ne 1 -or
        $report.fixture_only -ne $true -or
        $report.untrusted -ne $true -or
        $report.auto_eligible -ne $false -or
        [string]$report.source_revision -cnotmatch '^[0-9a-f]{40}$' -or
        [string]$report.model_sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        [string]$report.wav_sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        $report.cold_runs_per_backend -ne 5 -or
        $report.warm_runs_per_backend -ne 20 -or
        $report.expected_phrase_present_every_run -ne $true -or
        $report.normalized_transcript_parity -ne $true -or
        $report.same_device_internally_verified -ne $true) {
        throw 'Pending evidence report violates the fixture-only metadata contract.'
    }
    Assert-ScribeEvidenceExactProperties $report.pack @('id', 'version', 'digest', 'security_epoch', 'runtime_abi') 'Pending evidence pack'
    Assert-ScribeEvidenceExactProperties $report.gpu @('backend', 'provider', 'vendor', 'device_class', 'driver', 'memory_total_bytes') 'Pending evidence GPU'
    Assert-ScribeEvidenceExactProperties $report.nvidia_baseline @('product', 'driver', 'memory_total_bytes', 'memory_used_bytes', 'gpu_utilization_percent') 'Pending evidence NVIDIA baseline'
    Assert-ScribeEvidenceMetadataString $report.pack.id 128 'Pending evidence pack id'
    Assert-ScribeEvidenceMetadataString $report.pack.version 128 'Pending evidence pack version'
    if ([string]$report.pack.digest -cnotmatch '^[0-9a-f]{64}$') { throw 'Pending evidence pack digest is not canonical.' }
    Assert-ScribeEvidenceUnsignedInteger $report.pack.security_epoch 'Pending evidence pack security epoch'
    Assert-ScribeEvidenceUnsignedInteger $report.pack.runtime_abi 'Pending evidence pack runtime ABI'
    if ([string]$report.gpu.backend -cne 'vulkan' -or
        [string]$report.gpu.provider -cne 'transcribe-cpp-ggml-vulkan' -or
        [string]$report.gpu.vendor -cne 'nvidia' -or
        [string]$report.gpu.device_class -cne 'discrete_gpu') {
        throw 'Pending evidence GPU identity is outside the exact fixture contract.'
    }
    Assert-ScribeEvidenceMetadataString $report.gpu.driver 128 'Pending evidence GPU driver'
    Assert-ScribeEvidenceUnsignedInteger $report.gpu.memory_total_bytes 'Pending evidence GPU memory'
    Assert-ScribeEvidenceMetadataString $report.nvidia_baseline.product 256 'Pending evidence NVIDIA product'
    Assert-ScribeEvidenceMetadataString $report.nvidia_baseline.driver 128 'Pending evidence NVIDIA driver'
    Assert-ScribeEvidenceUnsignedInteger $report.nvidia_baseline.memory_total_bytes 'Pending evidence NVIDIA total memory'
    Assert-ScribeEvidenceUnsignedInteger $report.nvidia_baseline.memory_used_bytes 'Pending evidence NVIDIA used memory'
    Assert-ScribeEvidenceUnsignedInteger $report.nvidia_baseline.gpu_utilization_percent 'Pending evidence NVIDIA utilization'
    [UInt64]$gpuMemory = $report.gpu.memory_total_bytes
    [UInt64]$baselineTotal = $report.nvidia_baseline.memory_total_bytes
    [UInt64]$baselineUsed = $report.nvidia_baseline.memory_used_bytes
    [UInt64]$baselineUtilization = $report.nvidia_baseline.gpu_utilization_percent
    if ($gpuMemory -eq 0 -or
        $baselineTotal -eq 0 -or
        $baselineUsed -gt $baselineTotal -or
        $baselineUtilization -gt 10 -or
        $baselineUsed -gt ($baselineTotal / 4)) {
        throw 'Pending evidence NVIDIA metadata violates the bounded idle fixture contract.'
    }
    foreach ($backendName in @('cpu', 'vulkan')) {
        Assert-ScribeEvidenceExactProperties $report.$backendName @('cold', 'warm') "Pending evidence $backendName"
        Assert-ScribeEvidenceRunSet $report.$backendName.cold 5 $true "Pending evidence $backendName cold"
        Assert-ScribeEvidenceRunSet $report.$backendName.warm 20 $false "Pending evidence $backendName warm"
    }
    return $pending
}

function Remove-ScribeEvidencePendingReport(
    [string]$PendingPath,
    [string]$EvidenceRoot,
    [string]$PendingLeaf
) {
    $pending = Assert-ScribeEvidenceDirectChildPath $PendingPath $EvidenceRoot $PendingLeaf 'Pending evidence cleanup'
    $item = Get-Item -LiteralPath $pending -Force -ErrorAction SilentlyContinue
    if ($null -eq $item) { return }
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'Pending evidence cleanup refused a non-file or reparse artifact.'
    }
    Remove-Item -LiteralPath $pending -Force
    if (Test-Path -LiteralPath $pending) { throw 'Pending evidence cleanup did not remove the artifact.' }
}

function Add-ScribeEvidenceSecondaryFailures([System.Exception]$Primary, [System.Exception[]]$Secondary) {
    for ($index = 0; $index -lt @($Secondary).Count; $index++) {
        $Primary.Data["ScribeEvidenceSecondaryFailure$index"] = @($Secondary)[$index].Message
    }
}

function Complete-ScribeEvidencePendingReport(
    [string]$PendingPath,
    [string]$FinalPath,
    [string]$EvidenceRoot,
    [string]$PendingLeaf,
    [string]$FinalLeaf,
    [string]$FsutilPath,
    [System.Exception]$PrimaryFailure,
    [System.Exception[]]$SecondaryFailures
) {
    $failures = [System.Collections.Generic.List[System.Exception]]::new()
    foreach ($failure in @($SecondaryFailures)) {
        if ($null -ne $failure) { $failures.Add($failure) }
    }
    if ($null -ne $PrimaryFailure -or $failures.Count -gt 0) {
        try {
            Remove-ScribeEvidencePendingReport $PendingPath $EvidenceRoot $PendingLeaf
        }
        catch {
            $failures.Add($_.Exception)
        }
        if ($null -ne $PrimaryFailure) {
            Add-ScribeEvidenceSecondaryFailures $PrimaryFailure $failures.ToArray()
            throw $PrimaryFailure
        }
        $cleanupPrimary = $failures[0]
        Add-ScribeEvidenceSecondaryFailures $cleanupPrimary @($failures.ToArray() | Select-Object -Skip 1)
        throw $cleanupPrimary
    }

    try {
        $pending = Assert-ScribeEvidencePendingReport $PendingPath $EvidenceRoot $PendingLeaf $FsutilPath
        $final = Assert-ScribeEvidenceDirectChildPath $FinalPath $EvidenceRoot $FinalLeaf 'Final evidence report'
        if ($null -ne (Get-Item -LiteralPath $final -Force -ErrorAction SilentlyContinue) -or
            (Test-Path -LiteralPath $final)) {
            throw 'Final evidence report destination must be fresh.'
        }
        $digest = (Get-FileHash -LiteralPath $pending -Algorithm SHA256).Hash.ToLowerInvariant()
        # Revalidate mutable source/destination topology immediately before the
        # only publication operation. File.Move is an atomic same-directory rename.
        $pending = Assert-ScribeEvidencePendingReport $PendingPath $EvidenceRoot $PendingLeaf $FsutilPath
        $final = Assert-ScribeEvidenceDirectChildPath $FinalPath $EvidenceRoot $FinalLeaf 'Final evidence report'
        if ($null -ne (Get-Item -LiteralPath $final -Force -ErrorAction SilentlyContinue) -or
            (Test-Path -LiteralPath $final)) {
            throw 'Final evidence report destination changed before publication.'
        }
        $result = [pscustomobject]@{ Path = $final; Digest = $digest }
        [IO.File]::Move($pending, $final, $false)
        return $result
    }
    catch {
        $publishFailure = $_.Exception
        try {
            Remove-ScribeEvidencePendingReport $PendingPath $EvidenceRoot $PendingLeaf
        }
        catch {
            Add-ScribeEvidenceSecondaryFailures $publishFailure @($_.Exception)
        }
        throw $publishFailure
    }
}

function Assert-ScribeEvidenceNoReparseDescendants([string]$Path) {
    $root = Get-ScribeEvidencePhysicalDirectory $Path 'CMake bootstrap build directory'
    $pending = [System.Collections.Generic.Stack[IO.DirectoryInfo]]::new()
    $pending.Push($root)
    while ($pending.Count -gt 0) {
        $directory = $pending.Pop()
        foreach ($entry in $directory.EnumerateFileSystemInfos()) {
            if (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw 'CMake bootstrap build directory contains a reparse point.'
            }
            if ($entry -is [IO.DirectoryInfo]) { $pending.Push($entry) }
        }
    }
}

function Set-ScribeEvidenceWorkerBuildMode([bool]$BuildingWorker) {
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $null
    if ($BuildingWorker) {
        $env:SCRIBE_BUILDING_WORKER = '1'
    }
    else {
        $env:SCRIBE_BUILDING_WORKER = $null
    }
}

function Set-ScribeEvidenceProcessEnvironment([System.Collections.IDictionary]$Environment) {
    $previous = [System.Collections.Generic.List[psobject]]::new()
    try {
        foreach ($entry in $Environment.GetEnumerator()) {
            $name = [string]$entry.Key
            $value = [string]$entry.Value
            if ($name -cnotmatch '^[A-Za-z_][A-Za-z0-9_]*$' -or
                [string]::IsNullOrWhiteSpace($value) -or
                $value.Length -gt 32767 -or
                $value.IndexOfAny([char[]]@("`r", "`n", [char]0)) -ge 0) {
                throw 'Pinned toolchain environment export is invalid.'
            }
            $current = Get-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
            $previous.Add([pscustomobject]@{
                    Name = $name
                    Exists = $null -ne $current
                    Value = if ($null -eq $current) { $null } else { [string]$current.Value }
                })
            [Environment]::SetEnvironmentVariable($name, $value, [EnvironmentVariableTarget]::Process)
        }
        return ,$previous.ToArray()
    }
    catch {
        Restore-ScribeEvidenceProcessEnvironment $previous.ToArray()
        throw
    }
}

function Restore-ScribeEvidenceProcessEnvironment([psobject[]]$Previous) {
    foreach ($entry in @($Previous)) {
        if ($entry.Exists) {
            [Environment]::SetEnvironmentVariable([string]$entry.Name, [string]$entry.Value, [EnvironmentVariableTarget]::Process)
        }
        else {
            Remove-Item -LiteralPath "Env:$($entry.Name)" -ErrorAction SilentlyContinue
        }
    }
}

function New-ScribeEvidenceFixturePackVersion([string]$Revision, [string]$Nonce) {
    if ($Revision -cnotmatch '^[0-9a-f]{40}$' -or $Nonce -cnotmatch '^[0-9a-f]{12}$') {
        throw 'Fixture pack version inputs are not canonical.'
    }
    $version = "fixture-$($Revision.Substring(0, 12))-$Nonce"
    $cargoLeaf = "vulkan-$version-cargo"
    if ($version -cnotmatch '^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$' -or $cargoLeaf.Length -gt 48) {
        throw 'Fixture pack version exceeds the bounded builder Cargo target leaf.'
    }
    return $version
}

if (-not ('ScribeEvidenceNative.SystemDirectory' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
namespace ScribeEvidenceNative {
  public static class SystemDirectory {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern uint GetSystemDirectory(StringBuilder buffer, uint size);
  }
}
'@
}

function Get-ScribeVulkanEvidenceActualSystem32 {
    $buffer = [Text.StringBuilder]::new(32768)
    $length = [ScribeEvidenceNative.SystemDirectory]::GetSystemDirectory($buffer, [uint32]$buffer.Capacity)
    if ($length -eq 0 -or $length -ge $buffer.Capacity) { throw 'GetSystemDirectoryW did not return a bounded System32 path.' }
    return $buffer.ToString()
}

function ConvertTo-ScribeVulkanEvidencePci([string]$Value) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        throw 'NVIDIA PCI identity is missing.'
    }
    if ($Value -cne $Value.Trim()) {
        throw 'NVIDIA PCI identity must not contain surrounding whitespace.'
    }
    $normalized = $Value.ToLowerInvariant()
    if ($normalized -match '^native:([0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7])$') {
        return $Matches[1]
    }
    if ($normalized -match '^00000000:([0-9a-f]{2}:[0-9a-f]{2}\.[0-7])$') {
        return "0000:$($Matches[1])"
    }
    if ($normalized -match '^[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$') {
        return $normalized
    }
    throw 'NVIDIA PCI identity is not canonical.'
}

function ConvertTo-ScribeVulkanEvidenceUInt64([string]$Value, [string]$Label) {
    if ($Value -cnotmatch '^[0-9]+$') {
        throw "$Label must be an unsigned decimal integer."
    }
    try {
        return [UInt64]::Parse($Value, [Globalization.CultureInfo]::InvariantCulture)
    }
    catch {
        throw "$Label is outside UInt64 range."
    }
}

function Assert-ScribeVulkanEvidenceTrustedNvidiaSmi([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw 'Required trusted nvidia-smi.exe is missing from System32.'
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'Required trusted nvidia-smi.exe must be a regular non-reparse file.'
    }
    return $item.FullName
}

function Get-ScribeVulkanEvidenceNvidiaBaseline([string]$ExpectedStableDevice, [string]$NvidiaSmiPath) {
    $query = 'pci.bus_id,name,driver_version,memory.total,memory.used,utilization.gpu'
    $rows = @(& $NvidiaSmiPath "--query-gpu=$query" '--format=csv,noheader,nounits')
    if ($LASTEXITCODE -ne 0) {
        throw 'nvidia-smi failed during Vulkan evidence preflight.'
    }
    $expectedPci = ConvertTo-ScribeVulkanEvidencePci $ExpectedStableDevice
    $parsed = @($rows | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
        ConvertFrom-Csv -Header 'pci_bus_id', 'product', 'driver', 'memory_total_mib', 'memory_used_mib', 'gpu_utilization_percent')
    $matching = @($parsed | Where-Object {
        (ConvertTo-ScribeVulkanEvidencePci ([string]$_.pci_bus_id)) -ceq $expectedPci
    })
    if ($matching.Count -ne 1) {
        throw 'nvidia-smi did not provide exactly one row for the expected Vulkan PCI device.'
    }
    $row = $matching[0]
    $totalMib = ConvertTo-ScribeVulkanEvidenceUInt64 ([string]$row.memory_total_mib) 'NVIDIA total memory'
    $usedMib = ConvertTo-ScribeVulkanEvidenceUInt64 ([string]$row.memory_used_mib) 'NVIDIA used memory'
    $utilization = ConvertTo-ScribeVulkanEvidenceUInt64 ([string]$row.gpu_utilization_percent) 'NVIDIA GPU utilization'
    if ([string]::IsNullOrWhiteSpace([string]$row.product) -or
        ([string]$row.product).Length -gt 256 -or
        [string]::IsNullOrWhiteSpace([string]$row.driver) -or
        ([string]$row.driver).Length -gt 128 -or
        $totalMib -eq 0 -or
        $usedMib -gt $totalMib -or
        $utilization -gt 10 -or
        $usedMib -gt ($totalMib / 4)) {
        throw 'NVIDIA Vulkan evidence preflight requires <=10% GPU utilization and <=25% used VRAM.'
    }
    [pscustomobject]@{
        product = ([string]$row.product).Trim()
        driver = ([string]$row.driver).Trim()
        memory_total_bytes = $totalMib * 1MB
        memory_used_bytes = $usedMib * 1MB
        gpu_utilization_percent = [byte]$utilization
    }
}
