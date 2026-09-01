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

function Test-ScribeGpuWorkerKnownCmakeBootstrapFailure([object[]]$Output) {
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
    $successfulJunctionWarningSourceLine = [regex]::new('^\s*CMake Warning in [A-Za-z]:[\\/].*[\\/]tcs[\\/][0-9A-Fa-f]{16}[\\/]build[\\/]e[\\/]src[\\/]vulkan-shaders-gen-build[\\/]CMakeFiles[\\/]CMakeScratch[\\/]TryCompile-[A-Za-z0-9_-]+[\\/]CMakeLists\.txt:\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $objectPathLine = [regex]::new('^.*CMAKE_OBJECT_PATH_MAX.*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $successfulJunctionObjectPathLine = [regex]::new('^\s*characters \(see CMAKE_OBJECT_PATH_MAX\)\.\s+Object file\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $linkLine = [regex]::new('^.*(?:LINK|link) : fatal error LNK1104: cannot open file ''CMakeFiles\\cmTC_[0-9A-Fa-f]+\.dir\\intermediate\.manifest''\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
    $successfulJunctionLinkLine = [regex]::new('^\s*LINK : fatal error LNK1104: cannot open file ''CMakeFiles\\cmTC_[0-9A-Fa-f]+\.dir\\intermediate\.manifest''\s*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)
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

    # A successful transcribe-cpp short OUT_DIR junction is silent. Its use is
    # evidenced by the exact tcs/<hash>/build/e nested CMake warning source.
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
        if ($successfulJunctionState -eq 1 -and $successfulJunctionWarningSourceLine.IsMatch($line)) {
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

function Invoke-ScribeGpuWorkerBoundedNativeProcess(
    [string]$Executable,
    [string[]]$Arguments,
    [string]$FailureMessage
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
