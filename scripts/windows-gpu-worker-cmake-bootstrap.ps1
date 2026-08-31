Set-StrictMode -Version Latest

function Get-ScribeGpuWorkerBoundedDiagnosticLines([object[]]$Output) {
    $lines = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in @($Output)) {
        foreach ($line in ([string]$entry -split '\r?\n')) {
            if ($lines.Count -ge 2048) { break }
            $lines.Add($(if ($line.Length -gt 1024) { $line.Substring(0, 1024) } else { $line }))
        }
        if ($lines.Count -ge 2048) { break }
    }
    return $lines.ToArray()
}

function Test-ScribeGpuWorkerKnownCmakeBootstrapFailure([object[]]$Output) {
    $lines = @(Get-ScribeGpuWorkerBoundedDiagnosticLines $Output)

    $crateLine = [regex]::new('^error: failed to run custom build command for `transcribe-cpp-sys v0\.1\.3`\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $commandLine = [regex]::new('^\s*Error: failed to execute command: .+$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $osErrorLine = [regex]::new('^\s*The directory name is invalid\. \(os error 267\)\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $copyFailureLine = [regex]::new('^\s*(?:Error: )?Could not open file for write in copy operation\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $legacyState = 0
    foreach ($line in $lines) {
        if ($legacyState -eq 0 -and $crateLine.IsMatch($line)) { $legacyState = 1; continue }
        if ($legacyState -eq 1 -and $copyFailureLine.IsMatch($line)) { return $true }
        if ($legacyState -eq 1 -and $commandLine.IsMatch($line)) { $legacyState = 2; continue }
        if ($legacyState -eq 2 -and $osErrorLine.IsMatch($line)) { return $true }
    }

    $junctionLine = [regex]::new('^.*transcribe-cpp-sys: could not create short build junction .+; building in OUT_DIR \(may exceed Windows MAX_PATH in deep checkouts\)\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $warningSourceLine = [regex]::new('^.*vulkan-shaders-gen.*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $objectPathLine = [regex]::new('^.*CMAKE_OBJECT_PATH_MAX.*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $linkLine = [regex]::new('^.*(?:LINK|link) : fatal error LNK1104: cannot open file ''CMakeFiles\\cmTC_[0-9A-Fa-f]+\.dir\\intermediate\.manifest''\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $state = 0
    foreach ($line in $lines) {
        if ($state -eq 0 -and $junctionLine.IsMatch($line)) { $state = 1; continue }
        if ($state -eq 1 -and $crateLine.IsMatch($line)) { $state = 2; continue }
        if ($state -eq 2 -and $warningSourceLine.IsMatch($line)) {
            $state = if ($objectPathLine.IsMatch($line)) { 4 } else { 3 }
            continue
        }
        if ($state -eq 3 -and $objectPathLine.IsMatch($line)) { $state = 4; continue }
        if ($state -eq 4 -and $linkLine.IsMatch($line)) { return $true }
    }
    return $false
}

function Assert-ScribeGpuWorkerNoReparse([string]$Path) {
    $current = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    while (-not (Test-Path -LiteralPath $current)) {
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) { throw 'Could not find an existing non-reparse ancestor.' }
        $current = $parent
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw 'GPU worker CMake bootstrap path crosses a reparse point.'
        }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) { break }
        $current = $parent
    }
}

function Get-ScribeGpuWorkerPhysicalDirectory([string]$Path, [string]$Label) {
    Assert-ScribeGpuWorkerNoReparse $Path
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label must be a physical non-reparse directory."
    }
    return $item
}

function Test-ScribeGpuWorkerPathWithin([string]$Path, [string]$Root) {
    $pathValue = $Path.TrimEnd([char[]]@('\', '/'))
    $rootValue = $Root.TrimEnd([char[]]@('\', '/'))
    return $pathValue -ieq $rootValue -or $pathValue.StartsWith("$rootValue\", [StringComparison]::OrdinalIgnoreCase)
}

function Assert-ScribeGpuWorkerNoReparseDescendants([string]$Path) {
    $root = Get-ScribeGpuWorkerPhysicalDirectory $Path 'CMake bootstrap build directory'
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
