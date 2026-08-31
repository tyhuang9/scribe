Set-StrictMode -Version Latest

function Test-ScribeGpuWorkerKnownCmakeBootstrapFailure([object[]]$Output) {
    $lines = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in @($Output)) {
        foreach ($line in ([string]$entry -split '\r?\n')) {
            if ($lines.Count -ge 2048) { break }
            $lines.Add($(if ($line.Length -gt 1024) { $line.Substring(0, 1024) } else { $line }))
        }
        if ($lines.Count -ge 2048) { break }
    }

    $bounded = $lines -join "`n"
    if ($bounded.Contains('transcribe-cpp-sys') -and
        ($bounded.Contains('The directory name is invalid. (os error 267)') -or
        $bounded.Contains('Could not open file for write in copy operation'))) {
        return $true
    }

    $crateLine = [regex]::new('^error: failed to run custom build command for `transcribe-cpp-sys v0\.1\.3(?: .*)?$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
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
