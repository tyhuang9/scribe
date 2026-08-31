Set-StrictMode -Version Latest

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

function Test-ScribeEvidenceKnownCmakeBootstrapFailure([object[]]$Output) {
    $lines = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in @($Output)) {
        if ($lines.Count -ge 64) { break }
        $line = [string]$entry
        if ($line.Length -gt 1024) { $line = $line.Substring(0, 1024) }
        $lines.Add($line)
    }

    $crateLine = [regex]::new('^error: failed to run custom build command for `transcribe-cpp-sys v0\.1\.3(?: .*)?$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $commandLine = [regex]::new('^\s*Error: failed to execute command: .+$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $osErrorLine = [regex]::new('^\s*The directory name is invalid\. \(os error 267\)\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $state = 0
    foreach ($line in $lines) {
        if ($state -eq 0 -and $crateLine.IsMatch($line)) { $state = 1; continue }
        if ($state -eq 1 -and $commandLine.IsMatch($line)) { $state = 2; continue }
        if ($state -eq 2 -and $osErrorLine.IsMatch($line)) { return $true }
    }
    return $false
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
