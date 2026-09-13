$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$wrapper = Join-Path $PSScriptRoot 'assert-windows-gpu-pack-history-git-append-only.ps1'
$historyPath = 'runtime-manifests/gpu-worker-pack-history-windows-x64.json'
$script:testCount = 0
$script:fixtureGitReapFailed = $false
$expectedTestCount = 20
$gitPath = (Get-Command git -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
$utf8NoBom = [Text.UTF8Encoding]::new($false)

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-Failure([scriptblock]$Action, [string]$ExpectedText) {
    try { & $Action }
    catch {
        if (-not $_.Exception.Message.Contains($ExpectedText, [StringComparison]::Ordinal)) {
            throw "Expected failure containing '$ExpectedText', got: $($_.Exception.Message)"
        }
        return
    }
    throw "Expected failure containing '$ExpectedText', but the action succeeded."
}

function Invoke-Test([string]$Name, [scriptblock]$Action) {
    & $Action
    $script:testCount++
    Write-Output "PASS: $Name"
}

function Invoke-FixtureGit {
    param(
        [Parameter(Mandatory = $true)][string]$Repository,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [AllowNull()][string]$StandardInput = $null
    )
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $gitPath
    $start.WorkingDirectory = $Repository
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.RedirectStandardInput = $null -ne $StandardInput
    foreach ($key in @($start.Environment.Keys)) {
        if ($key.StartsWith('GIT_', [StringComparison]::OrdinalIgnoreCase)) {
            $null = $start.Environment.Remove($key)
        }
    }
    $start.Environment['GIT_TERMINAL_PROMPT'] = '0'
    $start.Environment['GIT_CONFIG_NOSYSTEM'] = '1'
    $start.Environment['GIT_CONFIG_GLOBAL'] = if ([IO.Path]::DirectorySeparatorChar -eq '\') { 'NUL' } else { '/dev/null' }
    $start.Environment['GIT_AUTHOR_DATE'] = '2000-01-01T00:00:00Z'
    $start.Environment['GIT_COMMITTER_DATE'] = '2000-01-01T00:00:00Z'
    foreach ($argument in @('-C', $Repository) + $Arguments) { $start.ArgumentList.Add($argument) }

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    $started = $false
    try {
        if (-not $process.Start()) { throw 'Could not start fixture Git.' }
        $started = $true
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if ($null -ne $StandardInput) {
            $process.StandardInput.Write($StandardInput)
            $process.StandardInput.Close()
        }
        if (-not $process.WaitForExit(15000)) {
            throw 'Fixture Git exceeded its 15-second deadline.'
        }
        $out = $stdout.GetAwaiter().GetResult()
        $err = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) {
            throw "Fixture Git failed ($($process.ExitCode)): $($err.Trim())"
        }
        return $out.TrimEnd("`r", "`n")
    }
    catch {
        $failure = $_
        if ($started) {
            try {
                if (-not $process.HasExited) {
                    try { $process.Kill($true) }
                    catch { if (-not $process.HasExited) { throw } }
                }
                if (-not $process.WaitForExit(5000)) {
                    throw 'Fixture Git child did not exit within its reap deadline.'
                }
            }
            catch {
                $script:fixtureGitReapFailed = $true
                throw "Fixture Git failed and its exact child could not be reaped: $($failure.Exception.Message)"
            }
        }
        throw $failure
    }
    finally { $process.Dispose() }
}

function Get-Digest([char]$Character) { return ([string]$Character) * 64 }

function New-Pack([string]$Backend, [string]$Version, [char]$Character) {
    $id = "scribe-$Backend-windows-x64"
    $digest = Get-Digest $Character
    $root = "workers/packs/$id/$Version/$digest"
    $relativeFiles = @('bin/worker.exe', 'pack-manifest.json', 'pack-manifest.sig')
    $index = 0
    return [ordered]@{
        pack_id = $id; pack_version = $Version; pack_digest = $digest
        security_epoch = 1; root = $root
        files = [object[]]@($relativeFiles | ForEach-Object {
            $index++
            [ordered]@{ path = "$root/$_"; size_bytes = $index; sha256 = $digest }
        })
    }
}

function New-Release {
    param([string]$Id, [char]$Revision, [char]$Catalog, [object[]]$Packs = @())
    $orderedPacks = @($Packs | Sort-Object { $_.root })
    return [ordered]@{
        release_id = $Id; source_revision = ([string]$Revision) * 40
        catalog_size_bytes = 100 + $orderedPacks.Count
        catalog_sha256 = Get-Digest $Catalog
        catalog_pack_roots = [string[]]@($orderedPacks | ForEach-Object { $_.root })
        packs = [object[]]$orderedPacks
    }
}

function New-Document([object[]]$Releases) {
    return [ordered]@{ schema_version = 1; history_epoch = 1; releases = [object[]]$Releases }
}

function Copy-Value([object]$Value) {
    return (($Value | ConvertTo-Json -Depth 16 -Compress) | ConvertFrom-Json -Depth 16 -AsHashtable)
}

function Write-Document([string]$Repository, [object]$Document) {
    $path = Join-Path $Repository ($historyPath.Replace('/', [IO.Path]::DirectorySeparatorChar))
    $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path)
    [IO.File]::WriteAllText($path, ($Document | ConvertTo-Json -Depth 16 -Compress), $utf8NoBom)
}

function New-HistoryCommit {
    param(
        [string]$Repository, [AllowEmptyString()][string]$Parent,
        [AllowNull()][object]$Document, [string]$Name, [switch]$OmitHistory
    )
    if ($Parent) { $null = Invoke-FixtureGit $Repository @('checkout', '--quiet', '--detach', $Parent) }
    $marker = Join-Path $Repository '.fixture-marker'
    [IO.File]::WriteAllText($marker, $Name, $utf8NoBom)
    $historyFile = Join-Path $Repository ($historyPath.Replace('/', [IO.Path]::DirectorySeparatorChar))
    if ($OmitHistory) {
        if ([IO.File]::Exists($historyFile)) { [IO.File]::Delete($historyFile) }
    }
    else { Write-Document $Repository $Document }
    $null = Invoke-FixtureGit $Repository @('add', '--all')
    $null = Invoke-FixtureGit $Repository @('commit', '--quiet', '--no-gpg-sign', '-m', "fixture $Name")
    return Invoke-FixtureGit $Repository @('rev-parse', 'HEAD')
}

function New-IndexEntryCommit {
    param([string]$Repository, [string]$Parent, [string]$Mode, [string]$Object, [string]$Name)
    $null = Invoke-FixtureGit $Repository @('read-tree', $Parent)
    $null = Invoke-FixtureGit $Repository @('update-index', '--add', '--cacheinfo', "$Mode,$Object,$historyPath")
    $tree = Invoke-FixtureGit $Repository @('write-tree', '--missing-ok')
    return Invoke-FixtureGit $Repository @('commit-tree', $tree, '-p', $Parent, '-m', "fixture $Name")
}

function New-RawBlobCommit {
    param([string]$Repository, [string]$Parent, [byte[]]$Bytes, [string]$Name)
    $rawFile = Join-Path $Repository ".fixture-$Name.bin"
    try {
        [IO.File]::WriteAllBytes($rawFile, $Bytes)
        $blob = Invoke-FixtureGit $Repository @('hash-object', '-w', '--', $rawFile)
        return New-IndexEntryCommit $Repository $Parent '100644' $blob $Name
    }
    finally {
        if ([IO.File]::Exists($rawFile)) { [IO.File]::Delete($rawFile) }
    }
}

function Invoke-Comparison([string]$Repository, [string]$Base, [string]$Candidate) {
    $values = @(& $wrapper -RepositoryRoot $Repository -BaseRevision $Base -CandidateRevision $Candidate)
    Assert-True ($values.Count -eq 1) 'History Git wrapper did not return exactly one result object.'
    return $values[0]
}

function Assert-SuccessResult {
    param([object]$Result, [string]$Base, [string]$Candidate, [string]$BaseBlob,
        [string]$CandidateBlob, [int]$PreviousCount, [int]$CandidateCount)
    $properties = @($Result.PSObject.Properties.Name)
    Assert-True (($properties -join ',') -ceq 'BaseRevision,CandidateRevision,BaseBlob,CandidateBlob,PreviousReleaseCount,CandidateReleaseCount') 'Result contract properties changed.'
    Assert-True ($Result.BaseRevision -ceq $Base) 'Result base revision changed.'
    Assert-True ($Result.CandidateRevision -ceq $Candidate) 'Result candidate revision changed.'
    Assert-True ($Result.BaseBlob -ceq $BaseBlob) 'Result base blob changed.'
    Assert-True ($Result.CandidateBlob -ceq $CandidateBlob) 'Result candidate blob changed.'
    Assert-True ($Result.PreviousReleaseCount -eq $PreviousCount) 'Result previous release count changed.'
    Assert-True ($Result.CandidateReleaseCount -eq $CandidateCount) 'Result candidate release count changed.'
}

$tempParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([IO.Path]::DirectorySeparatorChar)
$fixtureRoot = Join-Path $tempParent ("scribe-gpu-history-git-test-{0}" -f [guid]::NewGuid().ToString('N'))
$repository = Join-Path $fixtureRoot 'repo'
$fixtureCreated = $false

try {
    if (Test-Path -LiteralPath $fixtureRoot) { throw 'History fixture root must be new.' }
    $null = New-Item -ItemType Directory -Path $fixtureRoot
    $fixtureCreated = $true
    $null = New-Item -ItemType Directory -Path $repository
    $null = Invoke-FixtureGit $repository @('init', '--quiet', '--initial-branch=fixture', '--template=')
    $null = Invoke-FixtureGit $repository @('config', '--local', 'user.name', 'Scribe Fixture')
    $null = Invoke-FixtureGit $repository @('config', '--local', 'user.email', 'fixture@example.invalid')
    $null = Invoke-FixtureGit $repository @('config', '--local', 'commit.gpgSign', 'false')
    $null = Invoke-FixtureGit $repository @('config', '--local', 'core.autocrlf', 'false')

    $packA = New-Pack 'cuda' '1.0.0' 'a'
    $packB = New-Pack 'cuda' '2.0.0' 'b'
    $packC = New-Pack 'vulkan' '3.0.0' 'c'
    $releaseA = New-Release '1.0.0' '1' 'a' @($packA)
    $releaseB = New-Release '2.0.0' '2' 'b' @($packB)
    $releaseC = New-Release '3.0.0' '3' 'c' @($packC)
    $cpuRelease = New-Release 'cpu-only' '4' 'd'
    $baseDocument = New-Document @($releaseA, $releaseB)
    $appendDocument = New-Document @($releaseA, $releaseB, $releaseC)
    $cpuDocument = New-Document @($releaseA, $releaseB, $cpuRelease)

    $missingBase = New-HistoryCommit $repository '' $null 'missing-base' -OmitHistory
    $base = New-HistoryCommit $repository $missingBase $baseDocument 'base'
    $unchanged = New-HistoryCommit $repository $base $baseDocument 'unchanged'
    $append = New-HistoryCommit $repository $base $appendDocument 'append-gpu'
    $appendCpu = New-HistoryCommit $repository $base $cpuDocument 'append-cpu'
    $modifiedDocument = Copy-Value $baseDocument
    $modifiedDocument.releases[0].catalog_sha256 = Get-Digest 'f'
    $modified = New-HistoryCommit $repository $base $modifiedDocument 'modified'
    $removed = New-HistoryCommit $repository $base (New-Document @($releaseA)) 'removed'
    $reordered = New-HistoryCommit $repository $base (New-Document @($releaseB, $releaseA)) 'reordered'
    $epochDocument = Copy-Value $baseDocument
    $epochDocument.history_epoch = 2
    $epoch = New-HistoryCommit $repository $base $epochDocument 'epoch'
    $missingCandidate = New-HistoryCommit $repository $base $null 'missing-candidate' -OmitHistory
    $sibling = New-HistoryCommit $repository $missingBase $baseDocument 'sibling'

    $targetBlob = Invoke-FixtureGit $repository @('hash-object', '-w', '--stdin') 'ignored-target'
    $linkCommit = New-IndexEntryCommit $repository $base '120000' $targetBlob 'link-entry'
    $executableCommit = New-IndexEntryCommit $repository $base '100755' (
        Invoke-FixtureGit $repository @('rev-parse', "${base}:$historyPath")
    ) 'executable-entry'
    $missingBlob = 'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee'
    $missingObjectCommit = New-IndexEntryCommit $repository $base '100644' $missingBlob 'missing-object'
    $emptyTree = Invoke-FixtureGit $repository @('mktree') ''
    $runtimeTree = Invoke-FixtureGit $repository @('mktree') "040000 tree $emptyTree`tgpu-worker-pack-history-windows-x64.json`n"
    $rootTree = Invoke-FixtureGit $repository @('mktree') "040000 tree $runtimeTree`truntime-manifests`n"
    $treeCommit = Invoke-FixtureGit $repository @('commit-tree', $rootTree, '-p', $base, '-m', 'fixture tree-entry')
    $baseBytes = $utf8NoBom.GetBytes(($baseDocument | ConvertTo-Json -Depth 16 -Compress))
    $bomBytes = [byte[]]::new($baseBytes.Length + 3)
    $bomBytes[0] = 0xEF; $bomBytes[1] = 0xBB; $bomBytes[2] = 0xBF
    [Array]::Copy($baseBytes, 0, $bomBytes, 3, $baseBytes.Length)
    $bomCommit = New-RawBlobCommit $repository $base $bomBytes 'bom'
    $invalidUtf8Commit = New-RawBlobCommit $repository $base ([byte[]](0xFF, 0xFE)) 'invalid-utf8'
    $oversizedCommit = New-RawBlobCommit $repository $base ([byte[]]::new(4MB + 1)) 'oversized'
    $emptyBlobCommit = New-RawBlobCommit $repository $base ([byte[]]::new(0)) 'empty'

    $baseBlob = Invoke-FixtureGit $repository @('rev-parse', "${base}:$historyPath")
    $unchangedBlob = Invoke-FixtureGit $repository @('rev-parse', "${unchanged}:$historyPath")
    $appendBlob = Invoke-FixtureGit $repository @('rev-parse', "${append}:$historyPath")
    $appendCpuBlob = Invoke-FixtureGit $repository @('rev-parse', "${appendCpu}:$historyPath")

    Invoke-Test 'unchanged committed history succeeds' {
        Assert-SuccessResult (Invoke-Comparison $repository $base $unchanged) $base $unchanged $baseBlob $unchangedBlob 2 2
    }
    Invoke-Test 'appended GPU release row succeeds' {
        Assert-SuccessResult (Invoke-Comparison $repository $base $append) $base $append $baseBlob $appendBlob 2 3
    }
    Invoke-Test 'appended CPU-only empty row succeeds' {
        Assert-SuccessResult (Invoke-Comparison $repository $base $appendCpu) $base $appendCpu $baseBlob $appendCpuBlob 2 3
    }
    Invoke-Test 'modified release row fails' { Assert-Failure { Invoke-Comparison $repository $base $modified } 'modified or reordered' }
    Invoke-Test 'removed release row fails' { Assert-Failure { Invoke-Comparison $repository $base $removed } 'removed a previously published release row' }
    Invoke-Test 'reordered release rows fail' { Assert-Failure { Invoke-Comparison $repository $base $reordered } 'modified or reordered' }
    Invoke-Test 'history epoch change fails' { Assert-Failure { Invoke-Comparison $repository $base $epoch } 'history_epoch' }
    Invoke-Test 'noncanonical revision fails' { Assert-Failure { Invoke-Comparison $repository ('A' * 40) $unchanged } 'exact canonical 40-hex commit IDs' }
    Invoke-Test 'unresolved revision fails' { Assert-Failure { Invoke-Comparison $repository $base ('f' * 40) } 'exact commit objects and blobs must be fetched first' }
    Invoke-Test 'noncommit revision fails' { Assert-Failure { Invoke-Comparison $repository $base $baseBlob } 'must identify commit objects' }
    Invoke-Test 'missing baseline history fails' { Assert-Failure { Invoke-Comparison $repository $missingBase $base } 'exact regular non-executable history blob' }
    Invoke-Test 'missing candidate history fails' { Assert-Failure { Invoke-Comparison $repository $base $missingCandidate } 'exact regular non-executable history blob' }
    Invoke-Test 'nonancestor candidate fails' { Assert-Failure { Invoke-Comparison $repository $base $sibling } 'base must be an ancestor' }
    Invoke-Test 'shallow repository fails' {
        $shallow = Join-Path $repository '.git\shallow'
        try {
            [IO.File]::WriteAllText($shallow, "$unchanged`n", $utf8NoBom)
            Assert-Failure { Invoke-Comparison $repository $base $unchanged } 'refuses a shallow repository'
        }
        finally { if ([IO.File]::Exists($shallow)) { [IO.File]::Delete($shallow) } }
    }
    Invoke-Test 'missing committed blob object fails' { Assert-Failure { Invoke-Comparison $repository $base $missingObjectCommit } 'exact commit objects and blobs must be fetched first' }
    Invoke-Test 'linked or executable history entry fails' {
        Assert-Failure { Invoke-Comparison $repository $base $linkCommit } 'exact regular non-executable history blob'
        Assert-Failure { Invoke-Comparison $repository $base $executableCommit } 'exact regular non-executable history blob'
    }
    Invoke-Test 'tree history entry fails' { Assert-Failure { Invoke-Comparison $repository $base $treeCommit } 'exact regular non-executable history blob' }
    Invoke-Test 'raw committed blob boundaries fail closed' {
        Assert-Failure { Invoke-Comparison $repository $base $bomCommit } 'without a byte-order mark'
        Assert-Failure { Invoke-Comparison $repository $base $invalidUtf8Commit } 'Committed history blob must contain valid UTF-8'
        Assert-Failure { Invoke-Comparison $repository $base $oversizedCommit } 'exceeds the 4 MiB limit'
        Assert-Failure { Invoke-Comparison $repository $base $emptyBlobCommit } 'invalid or oversized byte length'
    }
    Invoke-Test 'dirty malicious worktree history is ignored' {
        $null = Invoke-FixtureGit $repository @('checkout', '--quiet', '--force', '--detach', $unchanged)
        [IO.File]::WriteAllText((Join-Path $repository ($historyPath.Replace('/', [IO.Path]::DirectorySeparatorChar))), '{"malicious":true}', $utf8NoBom)
        Assert-SuccessResult (Invoke-Comparison $repository $base $unchanged) $base $unchanged $baseBlob $unchangedBlob 2 2
    }
    Invoke-Test 'Git replacement refs are ignored' {
        $null = Invoke-FixtureGit $repository @('replace', $unchanged, $modified)
        Assert-SuccessResult (Invoke-Comparison $repository $base $unchanged) $base $unchanged $baseBlob $unchangedBlob 2 2
    }

    Assert-True ($script:testCount -eq $expectedTestCount) "Expected $expectedTestCount tests, ran $script:testCount."
    Write-Output "Windows GPU pack committed-history tests passed: $script:testCount"
}
finally {
    if ($script:fixtureGitReapFailed) {
        throw "Fixture cleanup suppressed because an owned Git child was not reaped: $fixtureRoot"
    }
    if ($fixtureCreated) {
        $resolvedFixture = [IO.Path]::GetFullPath($fixtureRoot)
        $resolvedParent = [IO.Directory]::GetParent($resolvedFixture).FullName
        $leaf = [IO.Path]::GetFileName($resolvedFixture)
        if ($resolvedParent -cne $tempParent -or $leaf -cnotmatch '^scribe-gpu-history-git-test-[0-9a-f]{32}$') {
            throw "Refusing to clean unexpected fixture path: $resolvedFixture"
        }
        if ([IO.Directory]::Exists($resolvedFixture)) {
            $item = Get-Item -LiteralPath $resolvedFixture -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Refusing to clean replaced fixture root: $resolvedFixture"
            }
            Remove-Item -LiteralPath $resolvedFixture -Recurse -Force
        }
        if ([IO.Directory]::Exists($resolvedFixture)) { throw "Fixture cleanup did not remove: $resolvedFixture" }
    }
}
