Set-StrictMode -Version Latest

if (-not ('ScribeGpuWorkerNativeProcessFailure' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Text;

public sealed class ScribeGpuWorkerNativeProcessFailure : Exception
{
    public int ExitCode { get; }
    public string Stdout { get; }
    public string Stderr { get; }
    public bool CaptureExceeded { get; }

    public ScribeGpuWorkerNativeProcessFailure(
        string message,
        int exitCode,
        string stdout,
        string stderr,
        bool captureExceeded) : base(message)
    {
        ExitCode = exitCode;
        Stdout = stdout ?? string.Empty;
        Stderr = stderr ?? string.Empty;
        CaptureExceeded = captureExceeded;
    }
}

public sealed class ScribeGpuWorkerNativeProcessStreamCapture
{
    private readonly long maximumLines;
    private readonly long maximumLineLength;
    private readonly long maximumCharacters;
    private readonly List<string> lines = new List<string>();
    private readonly StringBuilder currentLine = new StringBuilder();
    private long lineCount;
    private long characterCount;
    private bool currentLineStarted;
    private bool previousWasCarriageReturn;

    public bool Exceeded { get; private set; }

    public ScribeGpuWorkerNativeProcessStreamCapture(
        long maximumLines,
        long maximumLineLength,
        long maximumCharacters)
    {
        if (maximumLines <= 0 || maximumLineLength <= 0 || maximumCharacters <= 0)
        {
            throw new ArgumentOutOfRangeException(nameof(maximumLines));
        }
        this.maximumLines = maximumLines;
        this.maximumLineLength = maximumLineLength;
        this.maximumCharacters = maximumCharacters;
    }

    public void Add(char[] buffer, int count)
    {
        if (buffer == null) throw new ArgumentNullException(nameof(buffer));
        if (count < 0 || count > buffer.Length) throw new ArgumentOutOfRangeException(nameof(count));
        if (Exceeded) return;

        for (var index = 0; index < count; index++)
        {
            if (characterCount >= maximumCharacters)
            {
                Exceeded = true;
                return;
            }
            characterCount = checked(characterCount + 1L);

            var character = buffer[index];
            if (character == '\r')
            {
                CompleteLine();
                previousWasCarriageReturn = true;
                if (Exceeded) return;
                continue;
            }
            if (character == '\n')
            {
                if (previousWasCarriageReturn)
                {
                    previousWasCarriageReturn = false;
                    continue;
                }
                CompleteLine();
                if (Exceeded) return;
                continue;
            }

            previousWasCarriageReturn = false;
            currentLineStarted = true;
            if ((long)currentLine.Length >= maximumLineLength)
            {
                Exceeded = true;
                return;
            }
            currentLine.Append(character);
        }
    }

    public void Complete()
    {
        if (!Exceeded && currentLineStarted)
        {
            CompleteLine();
        }
    }

    public string GetText()
    {
        return string.Join("\n", lines);
    }

    private void CompleteLine()
    {
        if (lineCount >= maximumLines)
        {
            Exceeded = true;
            return;
        }
        lineCount = checked(lineCount + 1L);
        lines.Add(currentLine.ToString());
        currentLine.Clear();
        currentLineStarted = false;
    }
}
'@
}

function Get-ScribeGpuWorkerBoundedDiagnosticLines([object[]]$Output) {
    $lines = [System.Collections.Generic.List[string]]::new()
    $exceeded = $false
    foreach ($entry in @($Output)) {
        foreach ($line in ([string]$entry -split '\r?\n')) {
            if ($lines.Count -ge 2048 -or $line.Length -gt 1024) {
                $exceeded = $true
                break
            }
            $lines.Add($line)
        }
        if ($exceeded) { break }
    }
    return [pscustomobject]@{
        Lines = $lines.ToArray()
        Exceeded = $exceeded
    }
}

function Test-ScribeGpuWorkerCanonicalCargoTargetWarningSource(
    [string]$Line,
    [string]$CargoTarget,
    [string]$BuildEnvironment
) {
    if ([string]::IsNullOrWhiteSpace($CargoTarget) -or
        -not [IO.Path]::IsPathFullyQualified($CargoTarget) -or
        [string]::IsNullOrWhiteSpace($BuildEnvironment) -or
        -not [IO.Path]::IsPathFullyQualified($BuildEnvironment)) {
        return $false
    }
    $warningSource = [regex]::new(
        '^\s*CMake Warning in (?<source>[A-Za-z]:[\\/][^:\r\n]{1,768}):\s*$',
        [Text.RegularExpressions.RegexOptions]::CultureInvariant
    ).Match($Line)
    if (-not $warningSource.Success) { return $false }

    try {
        $cargoTargetItem = Get-ScribeGpuWorkerPhysicalDirectory $CargoTarget 'The retry classifier Cargo target'
        $buildEnvironmentItem = Get-ScribeGpuWorkerPhysicalDirectory $BuildEnvironment 'The retry classifier build environment'
        $canonicalCargoTarget = $cargoTargetItem.FullName.TrimEnd([char[]]@('\', '/'))
        $canonicalBuildEnvironment = $buildEnvironmentItem.FullName.TrimEnd([char[]]@('\', '/'))
        $sourceText = $warningSource.Groups['source'].Value.Replace('/', '\')
        if (-not [IO.Path]::IsPathFullyQualified($sourceText)) { return $false }
        $canonicalSource = [IO.Path]::GetFullPath($sourceText).TrimEnd([char[]]@('\', '/'))
        if (-not [string]::Equals($sourceText, $canonicalSource, [StringComparison]::OrdinalIgnoreCase) -or
            -not (Test-ScribeGpuWorkerPathWithin $canonicalSource $canonicalCargoTarget)) {
            return $false
        }
        $relativeSource = [IO.Path]::GetRelativePath($canonicalCargoTarget, $canonicalSource).Replace('\', '/')
        $relativeSourceMatch = [regex]::Match(
            $relativeSource,
            '^release/build/transcribe-cpp-sys-(?<crateHash>[0-9a-f]{16})/out/build/e/src/vulkan-shaders-gen-build/CMakeFiles/CMakeScratch/TryCompile-[A-Za-z0-9][A-Za-z0-9_-]{0,63}/CMakeLists\.txt$',
            [Text.RegularExpressions.RegexOptions]::CultureInvariant
        )
        if (-not $relativeSourceMatch.Success) {
            return $false
        }
        $crateHash = $relativeSourceMatch.Groups['crateHash'].Value
        $outDirectory = Join-Path $canonicalCargoTarget "release\build\transcribe-cpp-sys-$crateHash\out"
        $outItem = Get-ScribeGpuWorkerPhysicalDirectory $outDirectory 'The retry classifier transcribe-cpp OUT_DIR'
        if (-not (Test-ScribeGpuWorkerPathWithin $outItem.FullName $canonicalCargoTarget) -or
            [IO.Path]::GetRelativePath($canonicalCargoTarget, $outItem.FullName).Replace('\', '/') -cne "release/build/transcribe-cpp-sys-$crateHash/out") {
            return $false
        }

        $tcsRoot = Join-Path $canonicalBuildEnvironment 'tcs'
        $tcsItem = Get-ScribeGpuWorkerPhysicalDirectory $tcsRoot 'The retry classifier tcs inventory'
        if ((Split-Path -Parent $tcsItem.FullName) -cne $canonicalBuildEnvironment) { return $false }
        $entries = @(Get-ChildItem -LiteralPath $tcsItem.FullName -Force)
        if ($entries.Count -ne 1) { return $false }
        $shortOut = $entries[0]
        if (-not $shortOut.PSIsContainer -or
            ($shortOut.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -or
            $shortOut.LinkType -cne 'Junction' -or
            @($shortOut.Target).Count -ne 1 -or
            -not [regex]::IsMatch($shortOut.Name, '^[0-9a-f]{16}$', [Text.RegularExpressions.RegexOptions]::CultureInvariant) -or
            (Split-Path -Parent $shortOut.FullName) -cne $tcsItem.FullName -or
            (Get-ScribeGpuWorkerPhysicalDirectory ([string]@($shortOut.Target)[0]) 'The retry classifier transcribe-cpp OUT_DIR').FullName -cne $outItem.FullName) {
            return $false
        }
        return $true
    }
    catch {
        return $false
    }
}

function Test-ScribeGpuWorkerKnownCmakeBootstrapFailure(
    [object[]]$Output,
    [string]$CargoTarget,
    [string]$BuildEnvironment,
    [switch]$AllowSuccessfulJunctionMixedSeparatorLink
) {
    $bounded = Get-ScribeGpuWorkerBoundedDiagnosticLines $Output
    if ($bounded.Exceeded) { return $false }
    $lines = @($bounded.Lines)

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
    $successfulJunctionObjectPathLine = [regex]::new('^\s*characters \(see CMAKE_OBJECT_PATH_MAX\)\.\s+Object file\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $linkLine = [regex]::new('^.*(?:LINK|link) : fatal error LNK1104: cannot open file ''CMakeFiles\\cmTC_[0-9A-Fa-f]+\.dir\\intermediate\.manifest''\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $successfulJunctionLinkLine = [regex]::new('^\s*LINK : fatal error LNK1104: cannot open file ''CMakeFiles\\cmTC_[0-9A-Fa-f]{1,64}\.dir/intermediate\.manifest''\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $state = 0
    foreach ($line in $lines) {
        if ($state -eq 0 -and $junctionLine.IsMatch($line)) { $state = 1; continue }
        if ($state -eq 1 -and $crateLine.IsMatch($line)) { $state = 2; continue }
        if ($state -eq 2 -and $warningSourceLine.IsMatch($line)) {
            $state = if ($objectPathLine.IsMatch($line)) { 4 } else { 3 }
            continue
        }
        if ($state -eq 3 -and $objectPathLine.IsMatch($line)) { $state = 4; continue }
        if ($state -eq 4 -and
            ($linkLine.IsMatch($line) -or
             ($AllowSuccessfulJunctionMixedSeparatorLink -and $successfulJunctionLinkLine.IsMatch($line)))) {
            return $true
        }
    }

    # A successful transcribe-cpp short OUT_DIR junction is silent. Accept its
    # warning source only when it is bound to the caller's exact active Cargo
    # target and isolated short-build topology.
    # Reject this signature if the bounded diagnostic contains any fallback
    # warning: that case is accepted only by the separately ordered signature
    # above, where the warning must precede the crate failure.
    foreach ($line in $lines) {
        if ($junctionLine.IsMatch($line)) { return $false }
    }
    $successfulJunctionState = 0
    foreach ($line in $lines) {
        if ($successfulJunctionState -eq 0 -and $crateLine.IsMatch($line)) {
            $successfulJunctionState = 1
            continue
        }
        if ($successfulJunctionState -eq 1 -and
            (Test-ScribeGpuWorkerCanonicalCargoTargetWarningSource $line $CargoTarget $BuildEnvironment)) {
            $successfulJunctionState = 2
            continue
        }
        if ($successfulJunctionState -eq 2 -and $successfulJunctionObjectPathLine.IsMatch($line)) {
            $successfulJunctionState = 3
            continue
        }
        if ($successfulJunctionState -eq 3 -and $successfulJunctionLinkLine.IsMatch($line)) { return $true }
    }
    return $false
}

function Get-ScribeGpuWorkerCmakeRetryTopologyObservation(
    [string]$CargoTarget,
    [string]$BuildEnvironment,
    [string]$CrateHash
) {
    # This observer is intentionally separate from the retry predicate.  Its
    # result is diagnostic metadata only and must never decide whether a retry
    # is permitted.
    if ([string]::IsNullOrWhiteSpace($CargoTarget) -or
        -not [IO.Path]::IsPathFullyQualified($CargoTarget) -or
        [string]::IsNullOrWhiteSpace($BuildEnvironment) -or
        -not [IO.Path]::IsPathFullyQualified($BuildEnvironment) -or
        $CrateHash -cnotmatch '^[0-9a-f]{16}$') {
        return 'invalid_input'
    }
    try {
        try { $cargoTargetItem = Get-ScribeGpuWorkerPhysicalDirectory $CargoTarget 'Retry assessment Cargo target' }
        catch { return 'cargo_target_not_physical' }
        try { $buildEnvironmentItem = Get-ScribeGpuWorkerPhysicalDirectory $BuildEnvironment 'Retry assessment build environment' }
        catch { return 'build_environment_not_physical' }
        $canonicalCargoTarget = $cargoTargetItem.FullName.TrimEnd([char[]]@('\', '/'))
        $canonicalBuildEnvironment = $buildEnvironmentItem.FullName.TrimEnd([char[]]@('\', '/'))
        $outDirectory = Join-Path $canonicalCargoTarget "release\build\transcribe-cpp-sys-$CrateHash\out"
        try { $outItem = Get-ScribeGpuWorkerPhysicalDirectory $outDirectory 'Retry assessment transcribe-cpp OUT_DIR' }
        catch { return 'out_directory_not_physical' }
        if (-not (Test-ScribeGpuWorkerPathWithin $outItem.FullName $canonicalCargoTarget) -or
            [IO.Path]::GetRelativePath($canonicalCargoTarget, $outItem.FullName).Replace('\', '/') -cne "release/build/transcribe-cpp-sys-$CrateHash/out") {
            return 'out_directory_mismatch'
        }
        $tcsRoot = Join-Path $canonicalBuildEnvironment 'tcs'
        try { $tcsItem = Get-ScribeGpuWorkerPhysicalDirectory $tcsRoot 'Retry assessment tcs inventory' }
        catch { return 'tcs_root_not_physical' }
        if ((Split-Path -Parent $tcsItem.FullName) -cne $canonicalBuildEnvironment) { return 'tcs_root_mismatch' }
        try { $entries = @(Get-ChildItem -LiteralPath $tcsItem.FullName -Force) }
        catch { return 'observer_failure' }
        if ($entries.Count -ne 1) { return 'tcs_entry_count_invalid' }
        $shortOut = $entries[0]
        if (-not $shortOut.PSIsContainer -or
            ($shortOut.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -or
            $shortOut.LinkType -cne 'Junction' -or
            @($shortOut.Target).Count -ne 1) {
            return 'tcs_entry_not_junction'
        }
        if (-not [regex]::IsMatch($shortOut.Name, '^[0-9a-f]{16}$', [Text.RegularExpressions.RegexOptions]::CultureInvariant) -or
            (Split-Path -Parent $shortOut.FullName) -cne $tcsItem.FullName) {
            return 'tcs_leaf_invalid'
        }
        try { $targetItem = Get-ScribeGpuWorkerPhysicalDirectory ([string]@($shortOut.Target)[0]) 'Retry assessment junction target' }
        catch { return 'junction_target_not_physical' }
        if ($targetItem.FullName -cne $outItem.FullName) { return 'junction_target_mismatch' }
        return 'accepted'
    }
    catch {
        return 'observer_failure'
    }
}

function Get-ScribeGpuWorkerCmakeRetryDiagnosticLines(
    [string]$Text
) {
    if ([string]::IsNullOrEmpty($Text)) {
        return [pscustomobject]@{ Lines = @(); Exceeded = $false }
    }
    $lines = [System.Collections.Generic.List[string]]::new()
    [long]$characters = 0
    foreach ($line in ($Text -split '\n')) {
        $normalized = $line.TrimEnd([char[]]@("`r"))
        $characters += $normalized.Length + 1
        if ($lines.Count -ge 1024 -or $normalized.Length -gt 1024 -or $characters -gt 1048576) {
            return [pscustomobject]@{ Lines = @(); Exceeded = $true }
        }
        $lines.Add($normalized)
    }
    return [pscustomobject]@{ Lines = $lines.ToArray(); Exceeded = $false }
}

function New-ScribeGpuWorkerCmakeRetryMarkerSummary([string[]]$Lines) {
    # Keep these diagnostic-only expressions in lockstep with the existing
    # predicate. They are not used to grant retry authority.
    $patterns = [ordered]@{
        crate = [regex]::new('^error: failed to run custom build command for `transcribe-cpp-sys v0\.1\.3`\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
        command = [regex]::new('^\s*Error: failed to execute command: .+$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
        os_error = [regex]::new('^\s*The directory name is invalid\. \(os error 267\)\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
        copy_failure = [regex]::new('^\s*(?:Error: )?Could not open file for write in copy operation\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
        junction = [regex]::new('^.*transcribe-cpp-sys: could not create short build junction .+; building in OUT_DIR \(may exceed Windows MAX_PATH in deep checkouts\)\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
        warning_source = [regex]::new('^.*vulkan-shaders-gen.*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
        object_path = [regex]::new('^.*CMAKE_OBJECT_PATH_MAX.*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
        successful_object_path = [regex]::new('^\s*characters \(see CMAKE_OBJECT_PATH_MAX\)\.\s+Object file\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
        link = [regex]::new('^.*(?:LINK|link) : fatal error LNK1104: cannot open file ''CMakeFiles\\cmTC_[0-9A-Fa-f]+\.dir\\intermediate\.manifest''\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
        successful_link = [regex]::new('^\s*LINK : fatal error LNK1104: cannot open file ''CMakeFiles\\cmTC_[0-9A-Fa-f]{1,64}\.dir/intermediate\.manifest''\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    }
    $summary = [ordered]@{ line_count = [int]$Lines.Count }
    foreach ($entry in $patterns.GetEnumerator()) {
        [int]$count = 0
        $first = $null
        $last = $null
        for ($index = 0; $index -lt $Lines.Count; $index++) {
            if ($entry.Value.IsMatch($Lines[$index])) {
                $count++
                if ($null -eq $first) { $first = [int]($index + 1) }
                $last = [int]($index + 1)
            }
        }
        $summary["$($entry.Key)_count"] = $count
        $summary["$($entry.Key)_first"] = $first
        $summary["$($entry.Key)_last"] = $last
    }
    return [pscustomobject]$summary
}

function Get-ScribeGpuWorkerCmakeRetryAssessment(
    [object]$Failure,
    [bool]$RetryEligible,
    [string]$CargoTarget,
    [string]$BuildEnvironment
) {
    $assessment = [ordered]@{
        schema_version = 1
        assessment_status = 'not_evaluated'
        failure_kind = 'not_native_process_failure'
        exit_code = $null
        capture_overflow = $null
        retry_eligible = [bool]$RetryEligible
        diagnostic_order = 'not_evaluated'
        stdout = $null
        stderr = $null
        combined = $null
        topology_rejection_stage = 'not_evaluated'
    }
    if ($Failure -isnot [ScribeGpuWorkerNativeProcessFailure]) { return [pscustomobject]$assessment }
    $assessment.failure_kind = 'native_process_failure'
    $assessment.exit_code = [int]$Failure.ExitCode
    $assessment.capture_overflow = [bool]$Failure.CaptureExceeded
    if ($Failure.CaptureExceeded) {
        $assessment.failure_kind = 'native_process_capture_overflow'
        return [pscustomobject]$assessment
    }
    if ($null -eq $Failure.Stdout -or $null -eq $Failure.Stderr) { return [pscustomobject]$assessment }
    $stdout = Get-ScribeGpuWorkerCmakeRetryDiagnosticLines $Failure.Stdout
    $stderr = Get-ScribeGpuWorkerCmakeRetryDiagnosticLines $Failure.Stderr
    if ($stdout.Exceeded -or $stderr.Exceeded) { return [pscustomobject]$assessment }
    $combinedLines = @($stdout.Lines) + @($stderr.Lines)
    if ($combinedLines.Count -gt 2048) { return [pscustomobject]$assessment }
    $assessment.assessment_status = 'evaluated'
    $assessment.diagnostic_order = 'stdout_then_stderr'
    $assessment.stdout = New-ScribeGpuWorkerCmakeRetryMarkerSummary @($stdout.Lines)
    $assessment.stderr = New-ScribeGpuWorkerCmakeRetryMarkerSummary @($stderr.Lines)
    $assessment.combined = New-ScribeGpuWorkerCmakeRetryMarkerSummary $combinedLines

    $sourceMatch = [regex]::Match(
        ($combinedLines -join "`n"),
        'CMake Warning in [A-Za-z]:[\\/][^:\r\n]*release[\\/]build[\\/]transcribe-cpp-sys-(?<crateHash>[0-9a-f]{16})[\\/]out[\\/]build[\\/]e[\\/]src[\\/]vulkan-shaders-gen-build[\\/]CMakeFiles[\\/]CMakeScratch[\\/]TryCompile-[A-Za-z0-9][A-Za-z0-9_-]{0,63}[\\/]CMakeLists\.txt:',
        [Text.RegularExpressions.RegexOptions]::CultureInvariant
    )
    if ($sourceMatch.Success) {
        $assessment.topology_rejection_stage = Get-ScribeGpuWorkerCmakeRetryTopologyObservation `
            $CargoTarget $BuildEnvironment $sourceMatch.Groups['crateHash'].Value
    }
    elseif ($assessment.combined.warning_source_count -gt 0) {
        $assessment.topology_rejection_stage = 'invalid_input'
    }
    else {
        $assessment.topology_rejection_stage = 'not_required'
    }
    return [pscustomobject]$assessment
}

function ConvertTo-ScribeGpuWorkerFixtureCmakeRetryAssessmentJson([object]$Assessment) {
    $unavailable = '{"schema_version":1,"assessment_status":"unavailable","failure_kind":"not_evaluated","exit_code":null,"capture_overflow":null,"retry_eligible":false,"diagnostic_order":"not_evaluated","stdout":null,"stderr":null,"combined":null,"topology_rejection_stage":"not_evaluated"}'
    try {
        $rootProperties = @('schema_version', 'assessment_status', 'failure_kind', 'exit_code', 'capture_overflow', 'retry_eligible', 'diagnostic_order', 'stdout', 'stderr', 'combined', 'topology_rejection_stage')
        if ($null -eq $Assessment -or
            (@($Assessment.PSObject.Properties.Name | Sort-Object) -join ',') -cne (@($rootProperties | Sort-Object) -join ',') -or
            $Assessment.schema_version -isnot [int] -or $Assessment.schema_version -ne 1 -or
            $Assessment.assessment_status -cnotin @('evaluated', 'not_evaluated') -or
            $Assessment.failure_kind -cnotin @('native_process_failure', 'native_process_capture_overflow', 'not_native_process_failure') -or
            $Assessment.diagnostic_order -cnotin @('stdout_then_stderr', 'not_evaluated') -or
            $Assessment.topology_rejection_stage -cnotin @('accepted', 'not_required', 'not_evaluated', 'invalid_input', 'cargo_target_not_physical', 'build_environment_not_physical', 'out_directory_not_physical', 'out_directory_mismatch', 'tcs_root_not_physical', 'tcs_root_mismatch', 'tcs_entry_count_invalid', 'tcs_entry_not_junction', 'tcs_leaf_invalid', 'junction_target_not_physical', 'junction_target_mismatch', 'observer_failure') -or
            $Assessment.retry_eligible -isnot [bool] -or
            ($null -ne $Assessment.capture_overflow -and $Assessment.capture_overflow -isnot [bool]) -or
            ($null -ne $Assessment.exit_code -and $Assessment.exit_code -isnot [int])) {
            return $unavailable
        }
        $markerProperties = @('line_count')
        foreach ($marker in @('crate', 'command', 'os_error', 'copy_failure', 'junction', 'warning_source', 'object_path', 'successful_object_path', 'link', 'successful_link')) {
            $markerProperties += @("${marker}_count", "${marker}_first", "${marker}_last")
        }
        function Convert-ScribeGpuWorkerCmakeRetryAssessmentStream {
            param([object]$Stream, [string[]]$ExpectedProperties)
            if ($null -eq $Stream) { return $null }
            if ((@($Stream.PSObject.Properties.Name | Sort-Object) -join ',') -cne (@($ExpectedProperties | Sort-Object) -join ',')) {
                throw 'Invalid assessment stream schema.'
            }
            $normalized = [ordered]@{}
            foreach ($property in $ExpectedProperties) {
                $value = $Stream.$property
                if ($property -eq 'line_count' -or $property.EndsWith('_count')) {
                    if ($value -isnot [int] -or $value -lt 0 -or $value -gt 2048) { throw 'Invalid assessment count.' }
                }
                elseif ($null -ne $value -and ($value -isnot [int] -or $value -lt 1 -or $value -gt 2048)) {
                    throw 'Invalid assessment position.'
                }
                $normalized[$property] = $value
            }
            return [pscustomobject]$normalized
        }
        $stdout = Convert-ScribeGpuWorkerCmakeRetryAssessmentStream $Assessment.stdout $markerProperties
        $stderr = Convert-ScribeGpuWorkerCmakeRetryAssessmentStream $Assessment.stderr $markerProperties
        $combined = Convert-ScribeGpuWorkerCmakeRetryAssessmentStream $Assessment.combined $markerProperties
        if ($Assessment.assessment_status -ceq 'evaluated' -and ($null -eq $stdout -or $null -eq $stderr -or $null -eq $combined)) {
            return $unavailable
        }
        $normalizedAssessment = [pscustomobject][ordered]@{
            schema_version = 1
            assessment_status = [string]$Assessment.assessment_status
            failure_kind = [string]$Assessment.failure_kind
            exit_code = $Assessment.exit_code
            capture_overflow = $Assessment.capture_overflow
            retry_eligible = [bool]$Assessment.retry_eligible
            diagnostic_order = [string]$Assessment.diagnostic_order
            stdout = $stdout
            stderr = $stderr
            combined = $combined
            topology_rejection_stage = [string]$Assessment.topology_rejection_stage
        }
        $json = $normalizedAssessment | ConvertTo-Json -Depth 4 -Compress
        if ([string]::IsNullOrEmpty($json) -or $json.Length -gt 16384) { return $unavailable }
        return $json
    }
    catch {
        return $unavailable
    }
}

function Invoke-ScribeGpuWorkerBoundedNativeProcess(
    [string]$Executable,
    [string[]]$Arguments,
    [string]$FailureMessage,
    [switch]$AllowDiagnosticCaptureOverflowOnSuccessWithUnusedOutput
) {
    # Each stream is independently bounded. Both pipes continue to be drained
    # after overflow so a noisy child cannot deadlock while exiting.
    [long]$maximumStreamLines = 1024
    [long]$maximumStreamLineLength = 1024
    [long]$maximumStreamCharacters = 1048576
    $readBufferCharacters = 256
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) {
        $startInfo.ArgumentList.Add($argument)
    }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $stdoutCapture = [ScribeGpuWorkerNativeProcessStreamCapture]::new(
        $maximumStreamLines,
        $maximumStreamLineLength,
        $maximumStreamCharacters
    )
    $stderrCapture = [ScribeGpuWorkerNativeProcessStreamCapture]::new(
        $maximumStreamLines,
        $maximumStreamLineLength,
        $maximumStreamCharacters
    )
    try {
        if (-not $process.Start()) {
            throw $FailureMessage
        }
        $stdoutBuffer = [char[]]::new($readBufferCharacters)
        $stderrBuffer = [char[]]::new($readBufferCharacters)
        $stdoutRead = $process.StandardOutput.ReadAsync($stdoutBuffer, 0, $stdoutBuffer.Length)
        $stderrRead = $process.StandardError.ReadAsync($stderrBuffer, 0, $stderrBuffer.Length)
        $stdoutComplete = $false
        $stderrComplete = $false
        while (-not ($stdoutComplete -and $stderrComplete)) {
            $pendingReads = [System.Collections.Generic.List[System.Threading.Tasks.Task]]::new()
            if (-not $stdoutComplete) { $pendingReads.Add($stdoutRead) }
            if (-not $stderrComplete) { $pendingReads.Add($stderrRead) }
            [System.Threading.Tasks.Task]::WaitAny($pendingReads.ToArray()) | Out-Null
            if (-not $stdoutComplete -and $stdoutRead.IsCompleted) {
                $stdoutCount = $stdoutRead.GetAwaiter().GetResult()
                if ($stdoutCount -eq 0) {
                    $stdoutCapture.Complete()
                    $stdoutComplete = $true
                }
                else {
                    $stdoutCapture.Add($stdoutBuffer, $stdoutCount)
                    $stdoutRead = $process.StandardOutput.ReadAsync($stdoutBuffer, 0, $stdoutBuffer.Length)
                }
            }
            if (-not $stderrComplete -and $stderrRead.IsCompleted) {
                $stderrCount = $stderrRead.GetAwaiter().GetResult()
                if ($stderrCount -eq 0) {
                    $stderrCapture.Complete()
                    $stderrComplete = $true
                }
                else {
                    $stderrCapture.Add($stderrBuffer, $stderrCount)
                    $stderrRead = $process.StandardError.ReadAsync($stderrBuffer, 0, $stderrBuffer.Length)
                }
            }
        }
        $process.WaitForExit()
        $output = $stdoutCapture.GetText()
        $errorOutput = $stderrCapture.GetText()
        $captureExceeded = $stdoutCapture.Exceeded -or $stderrCapture.Exceeded
        if ($captureExceeded) {
            if ($process.ExitCode -eq 0 -and $AllowDiagnosticCaptureOverflowOnSuccessWithUnusedOutput) {
                # This opt-in is only for callers that treat exit code zero as
                # authoritative and intentionally discard both output streams.
                # Never expose a truncated prefix as if it were complete output.
                return [pscustomobject]@{
                    Stdout = ''
                    Stderr = ''
                }
            }
            throw [ScribeGpuWorkerNativeProcessFailure]::new(
                "$FailureMessage Child process output exceeded the bounded in-memory diagnostic capture.",
                $process.ExitCode,
                $output,
                $errorOutput,
                $true
            )
        }
        if ($process.ExitCode -ne 0) {
            $detail = $errorOutput.Trim()
            if (-not $detail) { $detail = $output.Trim() }
            $detail = [regex]::Replace($detail, '[\x00-\x1F\x7F]+', ' ').Trim()
            if ($detail.Length -gt 512) { $detail = $detail.Substring(0, 512) + '...' }
            $message = "$FailureMessage Exit code $($process.ExitCode)."
            if ($detail) { $message += " $detail" }
            throw [ScribeGpuWorkerNativeProcessFailure]::new(
                $message,
                $process.ExitCode,
                $output,
                $errorOutput,
                $false
            )
        }
        return [pscustomobject]@{
            Stdout = $output
            Stderr = $errorOutput
        }
    }
    finally {
        $process.Dispose()
    }
}

function Get-ScribeGpuWorkerNativeProcessRetryDiagnostic([System.Exception]$Failure) {
    if ($Failure -isnot [ScribeGpuWorkerNativeProcessFailure] -or $Failure.CaptureExceeded) {
        return @()
    }
    # Cross-stream event timing is unstable; classifier order is deliberately
    # fixed as stdout followed by stderr for both builder and evidence runner.
    return @($Failure.Stdout, $Failure.Stderr)
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
