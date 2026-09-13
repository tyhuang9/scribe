[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$BaseRevision,
    [Parameter(Mandatory = $true)]
    [string]$CandidateRevision,
    [string]$RepositoryRoot = (Split-Path -Parent $PSScriptRoot)
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# This is an unprivileged repository regression check, not released-history
# authority. Neither the candidate checkout nor its scripts authorize deletion.
. (Join-Path $PSScriptRoot 'windows-gpu-pack-history.ps1')

foreach ($revision in @($BaseRevision, $CandidateRevision)) {
    if ($revision -cnotmatch '^[0-9a-f]{40}$') {
        throw 'History comparison requires exact canonical 40-hex commit IDs.'
    }
}
$repository = [IO.Path]::GetFullPath($RepositoryRoot)
if (-not (Test-Path -LiteralPath $repository -PathType Container)) {
    throw 'History comparison requires an existing local repository.'
}
$gitCommand = Get-Command git -CommandType Application -ErrorAction Stop | Select-Object -First 1
$gitPath = [IO.Path]::GetFullPath($gitCommand.Source)
$historyPath = 'runtime-manifests/gpu-worker-pack-history-windows-x64.json'
$strictUtf8 = [Text.UTF8Encoding]::new($false, $true)

function Invoke-HistoryGit {
    param(
        [string[]]$Arguments,
        [switch]$AllowNotAncestor
    )

    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $gitPath
    $start.WorkingDirectory = $repository
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    # Bind to the supplied repository and raw objects, independent of ambient
    # Git directory/config overrides, replacement refs, or lazy network fetches.
    foreach ($key in @($start.Environment.Keys)) {
        if ($key.StartsWith('GIT_', [StringComparison]::OrdinalIgnoreCase)) {
            $null = $start.Environment.Remove($key)
        }
    }
    $start.Environment['GIT_TERMINAL_PROMPT'] = '0'
    $start.Environment['GIT_NO_LAZY_FETCH'] = '1'
    $start.Environment['GIT_OPTIONAL_LOCKS'] = '0'
    $start.Environment['GIT_CONFIG_NOSYSTEM'] = '1'
    $start.Environment['GIT_CONFIG_GLOBAL'] = if ($IsWindows) { 'NUL' } else { '/dev/null' }
    foreach ($argument in @('--no-replace-objects', '--no-lazy-fetch', '-c', 'core.commitGraph=false', '-C', $repository) + $Arguments) {
        $start.ArgumentList.Add($argument)
    }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    $stdout = [IO.MemoryStream]::new()
    $started = $false
    try {
        if (-not $process.Start()) { throw 'Could not start the local Git history reader.' }
        $started = $true
        # Only fixed metadata commands and a size-checked immutable blob are read.
        # Keep raw bytes: StreamReader would silently consume a UTF-8 BOM.
        $copy = $process.StandardOutput.BaseStream.CopyToAsync($stdout)
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) {
            $process.Kill($true)
            if (-not $process.WaitForExit(5000)) {
                throw 'Timed-out Git history reader could not be reaped.'
            }
            throw 'Local Git history read exceeded its 30-second deadline.'
        }
        $null = $copy.GetAwaiter().GetResult()
        $null = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0 -and -not ($AllowNotAncestor -and $process.ExitCode -eq 1)) {
            throw 'Local Git history read failed; exact commit objects and blobs must be fetched first.'
        }
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Bytes = $stdout.ToArray()
        }
    }
    finally {
        if ($started -and -not $process.HasExited) {
            $process.Kill($true)
            if (-not $process.WaitForExit(5000)) {
                throw 'Git history reader could not be reaped after failure.'
            }
        }
        $stdout.Dispose()
        $process.Dispose()
    }
}

function Get-HistoryGitText([string[]]$Arguments) {
    $result = Invoke-HistoryGit -Arguments $Arguments
    return $strictUtf8.GetString($result.Bytes).TrimEnd([char[]]@("`r", "`n"))
}

if ((Get-HistoryGitText @('rev-parse', '--is-shallow-repository')) -cne 'false') {
    throw 'History comparison refuses a shallow repository; fetch complete ancestry first.'
}
$graftPath = Get-HistoryGitText @('rev-parse', '--path-format=absolute', '--git-path', 'info/grafts')
if (Test-Path -LiteralPath $graftPath) {
    throw 'History comparison refuses local Git grafts that can rewrite commit ancestry.'
}
foreach ($revision in @($BaseRevision, $CandidateRevision)) {
    if ((Get-HistoryGitText @('cat-file', '-t', $revision)) -cne 'commit') {
        throw 'History comparison revisions must identify commit objects, not tags, trees, or blobs.'
    }
}
$ancestry = Invoke-HistoryGit -Arguments @('merge-base', '--is-ancestor', $BaseRevision, $CandidateRevision) -AllowNotAncestor
if ($ancestry.ExitCode -ne 0) {
    throw 'History comparison base must be an ancestor of the exact candidate commit.'
}

function Read-CommittedHistory([string]$Revision) {
    $entry = Invoke-HistoryGit -Arguments @('ls-tree', '-z', '--full-tree', $Revision, '--', $historyPath)
    $entryText = $strictUtf8.GetString($entry.Bytes)
    $pattern = '\A100644 blob ([0-9a-f]{40})\t' + [regex]::Escape($historyPath) + '\x00\z'
    $match = [regex]::Match($entryText, $pattern)
    if (-not $match.Success) {
        throw 'Each commit must contain the exact regular non-executable history blob; missing, linked, or directory entries are refused.'
    }
    $blob = $match.Groups[1].Value
    $sizeText = Get-HistoryGitText @('cat-file', '-s', $blob)
    if ($sizeText -cnotmatch '^[1-9][0-9]{0,6}$') {
        throw 'Committed history blob has an invalid or oversized byte length.'
    }
    $size = [int]$sizeText
    if ($size -gt $script:WindowsGpuPackHistoryMaximumJsonBytes) {
        throw 'Committed history blob exceeds the 4 MiB limit.'
    }
    $content = Invoke-HistoryGit -Arguments @('cat-file', 'blob', $blob)
    if ($content.Bytes.Length -ne $size) {
        throw 'Committed history blob did not match its declared byte length.'
    }
    if ($size -ge 3 -and $content.Bytes[0] -eq 0xEF -and
        $content.Bytes[1] -eq 0xBB -and $content.Bytes[2] -eq 0xBF) {
        throw 'Committed history must be UTF-8 without a byte-order mark.'
    }
    try { $json = $strictUtf8.GetString($content.Bytes) }
    catch { throw 'Committed history blob must contain valid UTF-8.' }
    return [pscustomobject]@{
        Blob = $blob
        Document = ConvertFrom-WindowsGpuPackHistoryJson -Json $json -SourceLabel "Committed history $Revision"
    }
}

$previous = Read-CommittedHistory $BaseRevision
$candidate = Read-CommittedHistory $CandidateRevision
$null = Assert-WindowsGpuPackHistoryAppendOnly -PreviousDocument $previous.Document -NewDocument $candidate.Document
[pscustomobject]@{
    BaseRevision = $BaseRevision
    CandidateRevision = $CandidateRevision
    BaseBlob = $previous.Blob
    CandidateBlob = $candidate.Blob
    PreviousReleaseCount = $previous.Document.releases.Count
    CandidateReleaseCount = $candidate.Document.releases.Count
}
