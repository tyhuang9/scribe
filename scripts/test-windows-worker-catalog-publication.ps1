param(
    [string] $InnoCompiler = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$expectedCompilerSha256 = 'eb6f4410c8db367a5f74127e8025ad2ccacc0afabbe783959d237df3050f97fb'
$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$testRunToken = [guid]::NewGuid().ToString('N')
$temporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$fixtureRoot = Join-Path $temporaryRoot "scribe-worker-catalog-publication-test-$testRunToken"
$fixtureInstallerRoot = Join-Path $fixtureRoot 'installer'
$fixtureDistRoot = Join-Path $fixtureRoot 'dist'
$fixturePortableRoot = Join-Path $fixtureDistRoot 'portable'
$maximumFixtureRoot = Join-Path $fixtureRoot 'maximum'
$maximumInstallerRoot = Join-Path $maximumFixtureRoot 'installer'
$maximumDistRoot = Join-Path $maximumFixtureRoot 'dist'
$maximumPortableRoot = Join-Path $maximumDistRoot 'portable'
$evidenceRoot = Join-Path $fixtureRoot 'evidence'
$fixtureVersion = '9.8.7'
$installerPath = Join-Path $fixtureDistRoot "Scribe-Setup-$fixtureVersion.exe"
$maximumInstallerPath = Join-Path $maximumDistRoot "Scribe-Setup-$fixtureVersion.exe"
$catalogLiveName = 'worker-pack-catalog.json'
$catalogNextName = 'worker-pack-catalog.next.json'
$catalogPreviousName = 'worker-pack-catalog.previous.json'
$workerRelativePath = 'workers\packs\fixture\1.0.0\aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\pack.json'
$currentCatalogBytes = [System.Text.UTF8Encoding]::new($false).GetBytes('{"schema_version":1,"packs":[]}')
$historicalCatalogBytes = [System.Text.UTF8Encoding]::new($false).GetBytes('{"schema_version":1,"packs":[],"release":"historical"}')
$unknownCatalogBytes = [System.Text.UTF8Encoding]::new($false).GetBytes('{"schema_version":1,"packs":[],"release":"unknown"}')
$currentWorkerBytes = [System.Text.UTF8Encoding]::new($false).GetBytes('{"fixture":"current-worker-pack"}')
$maximumWorkerLeafDirectories = @(0..897 | ForEach-Object {
    "workers\packs\maximum-$($_.ToString('D3'))"
})
$maximumWorkerDirectories = @('workers', 'workers\packs') + $maximumWorkerLeafDirectories
$maximumWorkerRelativePaths = @(
    foreach ($directory in $maximumWorkerLeafDirectories) {
        "$directory\pack.bin"
    }
    for ($index = 0; $index -lt 126; $index++) {
        "$($maximumWorkerLeafDirectories[0])\extra-$($index.ToString('D3')).bin"
    }
)
$ownedCaseRoots = [System.Collections.Generic.List[string]]::new()
$ownedShellRoots = [System.Collections.Generic.List[string]]::new()
$ownedProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$cleanupMayProceed = $true
$lastInstallerLog = ''

function Get-LowerSha256([byte[]] $Bytes) {
    return [Convert]::ToHexString(
        [System.Security.Cryptography.SHA256]::HashData($Bytes)
    ).ToLowerInvariant()
}

function Assert-OwnedTemporaryPath {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [string] $LeafPattern
    )

    $fullPath = [System.IO.Path]::GetFullPath($Path)
    $parent = [System.IO.Path]::GetFullPath((Split-Path -Parent $fullPath))
    $leaf = Split-Path -Leaf $fullPath
    if ($parent -cne $temporaryRoot.TrimEnd('\') -or $leaf -cnotmatch $LeafPattern) {
        throw "Refusing cleanup outside an exact owned temporary root: $fullPath"
    }
    return $fullPath
}

function Remove-OwnedTemporaryPath {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [string] $LeafPattern
    )

    $validated = Assert-OwnedTemporaryPath $Path $LeafPattern
    if (Test-Path -LiteralPath $validated) {
        Remove-Item -LiteralPath $validated -Recurse -Force
    }
}

function Write-Bytes([string] $Path, [byte[]] $Bytes) {
    $parent = Split-Path -Parent $Path
    if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    [System.IO.File]::WriteAllBytes($Path, $Bytes)
}

function Stop-AndReapOwnedProcess {
    param(
        [Parameter(Mandatory)] [System.Diagnostics.Process] $Process,
        [Parameter(Mandatory)] [string] $Description
    )

    try {
        if (-not $Process.HasExited) {
            $Process.Kill($true)
        }
        if (-not $Process.WaitForExit(10000)) {
            $script:cleanupMayProceed = $false
            throw "$Description did not exit after exact-process termination."
        }
    }
    catch {
        $script:cleanupMayProceed = $false
        throw "Could not terminate and reap the exact owned process for ${Description}: $($_.Exception.Message)"
    }
}

function Wait-OwnedProcess {
    param(
        [Parameter(Mandatory)] [System.Diagnostics.Process] $Process,
        [Parameter(Mandatory)] [int] $TimeoutSeconds,
        [Parameter(Mandatory)] [string] $Description
    )

    if (-not $Process.WaitForExit($TimeoutSeconds * 1000)) {
        Stop-AndReapOwnedProcess $Process $Description
        throw "$Description exceeded its $TimeoutSeconds second bound and was terminated by exact process identity."
    }
    return $Process.ExitCode
}

function Invoke-BoundedOwnedProcess {
    param(
        [Parameter(Mandatory)] [string] $FilePath,
        [Parameter(Mandatory)] [string[]] $ArgumentList,
        [Parameter(Mandatory)] [int] $TimeoutSeconds,
        [Parameter(Mandatory)] [string] $Description,
        [string] $StandardOutputPath,
        [string] $StandardErrorPath
    )

    $startParameters = @{
        FilePath = $FilePath
        ArgumentList = $ArgumentList
        PassThru = $true
        WindowStyle = 'Hidden'
    }
    if ($StandardOutputPath) { $startParameters.RedirectStandardOutput = $StandardOutputPath }
    if ($StandardErrorPath) { $startParameters.RedirectStandardError = $StandardErrorPath }
    $process = Start-Process @startParameters
    $ownedProcesses.Add($process)
    return Wait-OwnedProcess $process $TimeoutSeconds $Description
}

function Assert-Bytes([string] $Path, [byte[]] $Expected, [string] $Description) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Description is missing: $Path"
    }
    $actual = [System.IO.File]::ReadAllBytes($Path)
    if ($actual.Length -ne $Expected.Length -or
        [Convert]::ToHexString($actual) -cne [Convert]::ToHexString($Expected)) {
        throw "$Description bytes changed unexpectedly: $Path"
    }
}

function Assert-Absent([string] $Path, [string] $Description) {
    if (Test-Path -LiteralPath $Path) {
        throw "$Description must be absent: $Path"
    }
}

function New-CaseState {
    param(
        [byte[]] $Live,
        [byte[]] $Next,
        [byte[]] $Previous,
        [switch] $WithCurrentWorker
    )

    $token = [guid]::NewGuid().ToString('N')
    $container = Join-Path $temporaryRoot "scribe-release-stable-test-$token"
    $installRoot = Join-Path $container 'installed'
    $shellRoot = Join-Path $temporaryRoot "scribe-release-shell-test-$token"
    $null = Assert-OwnedTemporaryPath $container '^scribe-release-stable-test-[0-9a-f]{32}$'
    $null = Assert-OwnedTemporaryPath $shellRoot '^scribe-release-shell-test-[0-9a-f]{32}$'
    $ownedCaseRoots.Add($container)
    $ownedShellRoots.Add($shellRoot)

    if ($null -ne $Live -or $null -ne $Next -or $null -ne $Previous -or $WithCurrentWorker) {
        New-Item -ItemType Directory -Path (Join-Path $installRoot 'licenses') -Force | Out-Null
    }
    if ($null -ne $Live) { Write-Bytes (Join-Path $installRoot $catalogLiveName) $Live }
    if ($null -ne $Next) { Write-Bytes (Join-Path $installRoot $catalogNextName) $Next }
    if ($null -ne $Previous) { Write-Bytes (Join-Path $installRoot $catalogPreviousName) $Previous }
    if ($WithCurrentWorker) {
        Write-Bytes (Join-Path $installRoot $workerRelativePath) $currentWorkerBytes
    }

    return [pscustomobject]@{
        Token = $token
        Container = $container
        InstallRoot = $installRoot
        ShellRoot = $shellRoot
        Live = Join-Path $installRoot $catalogLiveName
        Next = Join-Path $installRoot $catalogNextName
        Previous = Join-Path $installRoot $catalogPreviousName
        Worker = Join-Path $installRoot $workerRelativePath
        LaunchMarker = Join-Path $shellRoot 'launch-marker.txt'
    }
}

function Add-MaximumCurrentWorkerPayload([object] $Case) {
    foreach ($relativePath in $maximumWorkerRelativePaths) {
        Write-Bytes (Join-Path $Case.InstallRoot $relativePath) $currentWorkerBytes
    }
}

function Get-MaximumWorkerAllowlistSource {
    param(
        [Parameter(Mandatory)] [string] $CurrentWorkerHash,
        [Parameter(Mandatory)] [string] $CurrentCatalogHash,
        [Parameter(Mandatory)] [string] $HistoricalCatalogHash
    )

    $directoryExpression = ($maximumWorkerDirectories | ForEach-Object {
        "    SameStr(RelativePath, '$($_.Replace("'", "''"))')"
    }) -join " or`r`n"
    $fileExpression = ($maximumWorkerRelativePaths | ForEach-Object {
        "    SameStr(RelativePath, '$($_.Replace("'", "''"))')"
    }) -join " or`r`n"
    return @"
function IsGeneratedWorkerPackDirectory(RelativePath: String): Boolean;
begin
  Result :=
$directoryExpression;
end;

function IsGeneratedWorkerPackFile(RelativePath: String): Boolean;
begin
  Result :=
$fileExpression;
end;

function GetGeneratedCurrentWorkerPackFileIdentity(
  RelativePath: String; var FileSize: Int64; var Sha256: String
): Boolean;
begin
  FileSize := -1;
  Sha256 := '';
  Result := IsGeneratedWorkerPackFile(RelativePath);
  if Result then
  begin
    FileSize := $($currentWorkerBytes.Length);
    Sha256 := '$CurrentWorkerHash';
  end;
end;

function GetGeneratedCurrentWorkerPackFileCount(): Integer;
begin
  Result := $($maximumWorkerRelativePaths.Count);
end;

function IsGeneratedCurrentWorkerCatalog(FileSize: Int64; Sha256: String): Boolean;
begin
  Result := (FileSize = $($currentCatalogBytes.Length)) and
    SameStr(Sha256, '$CurrentCatalogHash');
end;

function IsGeneratedKnownWorkerCatalog(FileSize: Int64; Sha256: String): Boolean;
begin
  Result := IsGeneratedCurrentWorkerCatalog(FileSize, Sha256) or
    ((FileSize = $($historicalCatalogBytes.Length)) and
     SameStr(Sha256, '$HistoricalCatalogHash'));
end;
"@
}

function Enable-LaunchSentinel([object] $Case) {
    New-Item -ItemType Directory -Path $Case.ShellRoot -Force | Out-Null
    $command = "@echo off`r`n> `"$($Case.LaunchMarker)`" echo launched`r`n"
    [System.IO.File]::WriteAllText(
        (Join-Path $Case.ShellRoot 'run-sentinel.cmd'),
        $command,
        [System.Text.Encoding]::ASCII
    )
}

function Invoke-FixtureInstaller {
    param(
        [Parameter(Mandatory)] [object] $Case,
        [Parameter(Mandatory)] [string] $Name,
        [string] $Fault,
        [switch] $Launch,
        [switch] $RestartApplications
    )

    $logPath = Join-Path $evidenceRoot "$Name.log"
    $script:lastInstallerLog = $logPath
    $arguments = [System.Collections.Generic.List[string]]::new()
    foreach ($argument in @(
        '/VERYSILENT',
        '/SUPPRESSMSGBOXES',
        '/NORESTART',
        '/SP-',
        '/NOICONS',
        "/SCRIBESTABLETEST=$($Case.Token)",
        "/LOG=$logPath"
    )) {
        $arguments.Add($argument)
    }
    if ($Fault) { $arguments.Add("/SCRIBECATALOGFAULT=$Fault") }
    if ($Launch) { $arguments.Add('/SCRIBECATALOGTESTLAUNCH=1') }
    if ($RestartApplications) { $arguments.Add('/RESTARTAPPLICATIONS') }
    return Invoke-BoundedOwnedProcess -FilePath $installerPath `
        -ArgumentList $arguments.ToArray() -TimeoutSeconds 60 `
        -Description "fixture installer $Name"
}

function Invoke-MaximumFixtureInstaller {
    param(
        [Parameter(Mandatory)] [object] $Case,
        [Parameter(Mandatory)] [string] $Name
    )

    $logPath = Join-Path $evidenceRoot "$Name.log"
    $script:lastInstallerLog = $logPath
    return Invoke-BoundedOwnedProcess -FilePath $maximumInstallerPath `
        -ArgumentList @(
            '/VERYSILENT',
            '/SUPPRESSMSGBOXES',
            '/NORESTART',
            '/SP-',
            '/NOICONS',
            "/SCRIBESTABLETEST=$($Case.Token)",
            "/LOG=$logPath"
        ) -TimeoutSeconds 120 -Description "maximum fixture installer $Name"
}

function Invoke-FixtureInstallerWithStagingConflict {
    param(
        [Parameter(Mandatory)] [object] $Case,
        [Parameter(Mandatory)] [string] $Name
    )

    $logPath = Join-Path $evidenceRoot "$Name.log"
    $script:lastInstallerLog = $logPath
    $arguments = @(
        '/VERYSILENT',
        '/SUPPRESSMSGBOXES',
        '/NORESTART',
        '/SP-',
        '/NOICONS',
        "/SCRIBESTABLETEST=$($Case.Token)",
        '/SCRIBETESTPAUSE=1',
        "/LOG=$logPath"
    )
    $process = Start-Process -FilePath $installerPath -ArgumentList $arguments `
        -PassThru -WindowStyle Hidden
    $ownedProcesses.Add($process)
    $readyPath = Join-Path $Case.Container 'preflight-ready'
    $continuePath = Join-Path $Case.Container 'preflight-continue'
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while (-not (Test-Path -LiteralPath $readyPath -PathType Leaf)) {
        if ($process.HasExited) {
            throw "Staging-conflict installer exited before its preflight boundary with code $($process.ExitCode)."
        }
        if ([DateTime]::UtcNow -ge $deadline) {
            Stop-AndReapOwnedProcess $process "staging-conflict installer $Name"
            throw 'Timed out waiting for the bounded staging-conflict preflight boundary.'
        }
        Start-Sleep -Milliseconds 50
    }

    # The exact next name is occupied only after preflight. onlyifdoesntexist
    # must preserve these bytes, and post-file authentication must fail closed.
    Write-Bytes $Case.Next $unknownCatalogBytes
    [System.IO.File]::WriteAllText(
        $continuePath,
        'continue',
        [System.Text.UTF8Encoding]::new($false)
    )
    return Wait-OwnedProcess $process 60 "staging-conflict installer $Name"
}

function Assert-Success([int] $ExitCode, [string] $Description) {
    if ($ExitCode -ne 0) {
        $logTail = if (Test-Path -LiteralPath $script:lastInstallerLog -PathType Leaf) {
            (Get-Content -LiteralPath $script:lastInstallerLog -Tail 25) -join "`n"
        } else { '<installer log unavailable>' }
        throw "$Description failed with exit code $ExitCode.`n$logTail"
    }
}

function Assert-Failure([int] $ExitCode, [string] $Description) {
    if ($ExitCode -eq 0) {
        $logTail = if (Test-Path -LiteralPath $script:lastInstallerLog -PathType Leaf) {
            (Get-Content -LiteralPath $script:lastInstallerLog -Tail 30) -join "`n"
        } else { '<installer log unavailable>' }
        throw "$Description unexpectedly succeeded.`n$logTail"
    }
}

function Assert-CatalogState {
    param(
        [Parameter(Mandatory)] [object] $Case,
        [ValidateSet('absent', 'current', 'historical', 'unknown', 'occupied')] [string] $Live,
        [ValidateSet('absent', 'current', 'historical', 'unknown', 'occupied')] [string] $Next,
        [ValidateSet('absent', 'current', 'historical', 'unknown', 'occupied')] [string] $Previous
    )

    $expected = @{
        current = $currentCatalogBytes
        historical = $historicalCatalogBytes
        unknown = $unknownCatalogBytes
        occupied = [System.Text.UTF8Encoding]::new($false).GetBytes('occupied')
    }
    foreach ($entry in @(
        @{ Path = $Case.Live; State = $Live; Name = 'live catalog' },
        @{ Path = $Case.Next; State = $Next; Name = 'next catalog' },
        @{ Path = $Case.Previous; State = $Previous; Name = 'previous catalog' }
    )) {
        if ($entry.State -ceq 'absent') {
            Assert-Absent $entry.Path $entry.Name
        }
        else {
            Assert-Bytes $entry.Path $expected[$entry.State] $entry.Name
        }
    }
}

try {
    $null = Assert-OwnedTemporaryPath $fixtureRoot '^scribe-worker-catalog-publication-test-[0-9a-f]{32}$'
    if ($maximumWorkerDirectories.Count -ne 900 -or
        $maximumWorkerRelativePaths.Count -ne 1024) {
        throw 'Maximum worker fixture does not exercise the admitted 900-directory/1,024-file bounds.'
    }
    if ([string]::IsNullOrWhiteSpace($InnoCompiler)) {
        throw 'Pass -InnoCompiler with the explicitly acquired Inno Setup 6.7.1 ISCC.exe path.'
    }
    if (-not (Test-Path -LiteralPath $InnoCompiler -PathType Leaf)) {
        throw "Pinned Inno compiler is missing: $InnoCompiler"
    }
    $compilerHash = (Get-FileHash -LiteralPath $InnoCompiler -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($compilerHash -cne $expectedCompilerSha256) {
        throw "Pinned Inno compiler hash mismatch: $compilerHash"
    }

    New-Item -ItemType Directory -Path $fixtureInstallerRoot -Force | Out-Null
    New-Item -ItemType Directory -Path $fixturePortableRoot -Force | Out-Null
    New-Item -ItemType Directory -Path $maximumInstallerRoot -Force | Out-Null
    New-Item -ItemType Directory -Path $maximumPortableRoot -Force | Out-Null
    New-Item -ItemType Directory -Path $evidenceRoot -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'installer\scribe.iss') -Destination $fixtureInstallerRoot
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'installer\worker-catalog-publication.iss') -Destination $fixtureInstallerRoot
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'installer\scribe.iss') -Destination $maximumInstallerRoot
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'installer\worker-catalog-publication.iss') -Destination $maximumInstallerRoot
    Write-Bytes (Join-Path $fixturePortableRoot $catalogLiveName) $currentCatalogBytes
    Write-Bytes (Join-Path $fixturePortableRoot $workerRelativePath) $currentWorkerBytes
    Write-Bytes (Join-Path $maximumPortableRoot $catalogLiveName) $currentCatalogBytes
    foreach ($relativePath in $maximumWorkerRelativePaths) {
        Write-Bytes (Join-Path $maximumPortableRoot $relativePath) $currentWorkerBytes
    }

    $currentCatalogHash = Get-LowerSha256 $currentCatalogBytes
    $historicalCatalogHash = Get-LowerSha256 $historicalCatalogBytes
    $currentWorkerHash = Get-LowerSha256 $currentWorkerBytes
    $nativeWorkerPath = $workerRelativePath.Replace("'", "''")
    $workerDirectories = @(
        'workers',
        'workers\packs',
        'workers\packs\fixture',
        'workers\packs\fixture\1.0.0',
        'workers\packs\fixture\1.0.0\aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
    )
    $directoryExpression = ($workerDirectories | ForEach-Object {
        "    SameStr(RelativePath, '$($_.Replace("'", "''"))')"
    }) -join " or`r`n"
    $allowlist = @"
function IsGeneratedWorkerPackDirectory(RelativePath: String): Boolean;
begin
  Result :=
$directoryExpression;
end;

function IsGeneratedWorkerPackFile(RelativePath: String): Boolean;
begin
  Result := SameStr(RelativePath, '$nativeWorkerPath');
end;

function GetGeneratedCurrentWorkerPackFileIdentity(
  RelativePath: String; var FileSize: Int64; var Sha256: String
): Boolean;
begin
  FileSize := -1;
  Sha256 := '';
  Result := SameStr(RelativePath, '$nativeWorkerPath');
  if Result then
  begin
    FileSize := $($currentWorkerBytes.Length);
    Sha256 := '$currentWorkerHash';
  end;
end;

function GetGeneratedCurrentWorkerPackFileCount(): Integer;
begin
  Result := 1;
end;

function IsGeneratedCurrentWorkerCatalog(FileSize: Int64; Sha256: String): Boolean;
begin
  Result := (FileSize = $($currentCatalogBytes.Length)) and
    SameStr(Sha256, '$currentCatalogHash');
end;

function IsGeneratedKnownWorkerCatalog(FileSize: Int64; Sha256: String): Boolean;
begin
  Result := IsGeneratedCurrentWorkerCatalog(FileSize, Sha256) or
    ((FileSize = $($historicalCatalogBytes.Length)) and
     SameStr(Sha256, '$historicalCatalogHash'));
end;
"@
    [System.IO.File]::WriteAllText(
        (Join-Path $fixtureInstallerRoot 'worker-pack-allowlist.fixture.iss'),
        $allowlist,
        [System.Text.UTF8Encoding]::new($false)
    )
    [System.IO.File]::WriteAllText(
        (Join-Path $maximumInstallerRoot 'worker-pack-allowlist.maximum.iss'),
        (Get-MaximumWorkerAllowlistSource `
            -CurrentWorkerHash $currentWorkerHash `
            -CurrentCatalogHash $currentCatalogHash `
            -HistoricalCatalogHash $historicalCatalogHash),
        [System.Text.UTF8Encoding]::new($false)
    )

    $productionCompileExit = Invoke-BoundedOwnedProcess -FilePath $InnoCompiler `
        -ArgumentList @(
            "/DAppVersion=$fixtureVersion",
            '/DWorkerPackAllowlist=worker-pack-allowlist.fixture.iss',
            (Join-Path $fixtureInstallerRoot 'scribe.iss')
        ) -TimeoutSeconds 60 -Description 'production fixture compiler' `
        -StandardOutputPath (Join-Path $evidenceRoot 'production-compile.log') `
        -StandardErrorPath (Join-Path $evidenceRoot 'production-compile.err.log')
    if ($productionCompileExit -ne 0) {
        throw "Pinned Inno compiler production build failed with exit code $productionCompileExit."
    }

    $testCompileExit = Invoke-BoundedOwnedProcess -FilePath $InnoCompiler `
        -ArgumentList @(
            "/DAppVersion=$fixtureVersion",
            '/DWorkerPackAllowlist=worker-pack-allowlist.fixture.iss',
            '/DWorkerCatalogPublicationTests=1',
            (Join-Path $fixtureInstallerRoot 'scribe.iss')
        ) -TimeoutSeconds 60 -Description 'test fixture compiler' `
        -StandardOutputPath (Join-Path $evidenceRoot 'compile.log') `
        -StandardErrorPath (Join-Path $evidenceRoot 'compile.err.log')
    if ($testCompileExit -ne 0 -or -not (Test-Path -LiteralPath $installerPath -PathType Leaf)) {
        throw "Pinned Inno compiler failed with exit code $testCompileExit."
    }

    $maximumCompileExit = Invoke-BoundedOwnedProcess -FilePath $InnoCompiler `
        -ArgumentList @(
            "/DAppVersion=$fixtureVersion",
            '/DWorkerPackAllowlist=worker-pack-allowlist.maximum.iss',
            '/DWorkerCatalogPublicationTests=1',
            (Join-Path $maximumInstallerRoot 'scribe.iss')
        ) -TimeoutSeconds 180 -Description 'maximum fixture compiler' `
        -StandardOutputPath (Join-Path $evidenceRoot 'maximum-compile.log') `
        -StandardErrorPath (Join-Path $evidenceRoot 'maximum-compile.err.log')
    if ($maximumCompileExit -ne 0 -or
        -not (Test-Path -LiteralPath $maximumInstallerPath -PathType Leaf)) {
        throw "Pinned Inno compiler maximum build failed with exit code $maximumCompileExit."
    }

    $fresh = New-CaseState
    Enable-LaunchSentinel $fresh
    Assert-Success (Invoke-FixtureInstaller $fresh 'fresh' -Launch) 'fresh publication'
    Assert-CatalogState $fresh current absent absent
    Assert-Bytes $fresh.Worker $currentWorkerBytes 'fresh current worker payload'
    if (-not (Test-Path -LiteralPath $fresh.LaunchMarker -PathType Leaf)) {
        throw 'Successful catalog publication did not run the bounded launch sentinel.'
    }

    $update = New-CaseState -Live $historicalCatalogBytes
    Assert-Success (Invoke-FixtureInstaller $update 'update') 'historical-to-current update'
    Assert-CatalogState $update current absent historical

    $currentRepair = New-CaseState -Live $currentCatalogBytes
    Assert-Success (Invoke-FixtureInstaller $currentRepair 'current-repair') 'current catalog repair'
    Assert-CatalogState $currentRepair current absent absent

    $currentRepairPrevious = New-CaseState -Live $currentCatalogBytes -Previous $historicalCatalogBytes
    Assert-Success (Invoke-FixtureInstaller $currentRepairPrevious 'current-repair-previous') 'current catalog repair with previous'
    Assert-CatalogState $currentRepairPrevious current absent historical

    $redundantNext = New-CaseState -Live $currentCatalogBytes -Next $currentCatalogBytes `
        -Previous $historicalCatalogBytes -WithCurrentWorker
    Assert-Success (Invoke-FixtureInstaller $redundantNext 'redundant-next') 'redundant next repair'
    Assert-CatalogState $redundantNext current absent historical

    $freshBoundary = New-CaseState
    $freshBoundaryExit = Invoke-FixtureInstaller $freshBoundary 'fresh-boundary' -Fault 'before-next-to-live'
    if ($freshBoundaryExit -ne 73) { throw "Fresh boundary fault returned $freshBoundaryExit instead of 73." }
    Assert-CatalogState $freshBoundary absent current absent
    Assert-Success (Invoke-FixtureInstaller $freshBoundary 'fresh-boundary-restart') 'fresh boundary restart'
    Assert-CatalogState $freshBoundary current absent absent

    $firstBoundary = New-CaseState -Live $historicalCatalogBytes
    $firstBoundaryExit = Invoke-FixtureInstaller $firstBoundary 'first-boundary' -Fault 'before-live-to-previous'
    if ($firstBoundaryExit -ne 73) { throw "First rename-boundary fault returned $firstBoundaryExit instead of 73." }
    Assert-CatalogState $firstBoundary historical current absent
    Assert-Success (Invoke-FixtureInstaller $firstBoundary 'first-boundary-restart') 'first rename-boundary restart'
    Assert-CatalogState $firstBoundary current absent historical

    $secondBoundary = New-CaseState -Live $historicalCatalogBytes
    $secondBoundaryExit = Invoke-FixtureInstaller $secondBoundary 'second-boundary' -Fault 'before-next-to-live'
    if ($secondBoundaryExit -ne 73) { throw "Second rename-boundary fault returned $secondBoundaryExit instead of 73." }
    Assert-CatalogState $secondBoundary absent current historical
    Assert-Success (Invoke-FixtureInstaller $secondBoundary 'second-boundary-restart') 'second rename-boundary restart'
    Assert-CatalogState $secondBoundary current absent historical

    $occupiedPrevious = New-CaseState -Live $historicalCatalogBytes
    Assert-Failure (
        Invoke-FixtureInstaller $occupiedPrevious 'occupied-previous' -Fault 'occupy-previous'
    ) 'occupied previous no-replace boundary'
    Assert-CatalogState $occupiedPrevious historical current occupied

    $occupiedLive = New-CaseState -Live $historicalCatalogBytes
    Assert-Failure (
        Invoke-FixtureInstaller $occupiedLive 'occupied-live' -Fault 'occupy-live'
    ) 'occupied live no-replace boundary'
    Assert-CatalogState $occupiedLive occupied current historical

    $corruptLive = New-CaseState -Live $unknownCatalogBytes
    Assert-Failure (Invoke-FixtureInstaller $corruptLive 'corrupt-live') 'unknown live catalog preflight'
    Assert-CatalogState $corruptLive unknown absent absent
    Assert-Absent $corruptLive.Worker 'worker payload after corrupt-catalog refusal'

    $corruptNext = New-CaseState -Next $unknownCatalogBytes -WithCurrentWorker
    Assert-Failure (Invoke-FixtureInstaller $corruptNext 'corrupt-next') 'unknown next catalog preflight'
    Assert-CatalogState $corruptNext absent unknown absent

    $stagingFailure = New-CaseState -Live $historicalCatalogBytes
    Assert-Failure (
        Invoke-FixtureInstallerWithStagingConflict $stagingFailure 'staging-conflict'
    ) 'staging occupied-name refusal'
    Assert-CatalogState $stagingFailure historical unknown absent

    $lockRefusal = New-CaseState -Live $historicalCatalogBytes
    $lock = [System.IO.File]::Open(
        $lockRefusal.Live,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::None
    )
    try {
        Assert-Failure (Invoke-FixtureInstaller $lockRefusal 'lock-refusal') 'catalog lease conflict'
    }
    finally {
        $lock.Dispose()
    }
    Assert-CatalogState $lockRefusal historical absent absent

    $catalogHardlink = New-CaseState -Live $historicalCatalogBytes
    $catalogHardlinkPeer = Join-Path $catalogHardlink.Container 'catalog-hardlink-peer'
    New-Item -ItemType HardLink -Path $catalogHardlinkPeer -Target $catalogHardlink.Live | Out-Null
    Assert-Failure (
        Invoke-FixtureInstaller $catalogHardlink 'catalog-hardlink-refusal'
    ) 'catalog hard-link refusal'
    Assert-CatalogState $catalogHardlink historical absent absent
    Assert-Bytes $catalogHardlinkPeer $historicalCatalogBytes 'catalog hard-link peer'

    $catalogAds = New-CaseState -Live $historicalCatalogBytes
    Set-Content -LiteralPath $catalogAds.Live -Stream 'scribe-test' -Value 'blocked' -NoNewline
    Assert-Failure (
        Invoke-FixtureInstaller $catalogAds 'catalog-ads-refusal'
    ) 'catalog alternate-stream refusal'
    Assert-CatalogState $catalogAds historical absent absent

    $missingLive = New-CaseState -Previous $historicalCatalogBytes
    Enable-LaunchSentinel $missingLive
    $missingLiveExit = Invoke-FixtureInstaller $missingLive 'missing-live-publication-boundary' `
        -Fault 'before-next-to-live' -Launch
    if ($missingLiveExit -ne 73) { throw "Missing-live restage boundary returned $missingLiveExit instead of 73." }
    Assert-CatalogState $missingLive absent current historical
    Assert-Absent $missingLive.LaunchMarker 'launch after incomplete missing-live restage'
    Assert-Success (Invoke-FixtureInstaller $missingLive 'missing-live-restage-restart') 'missing-live restage restart'
    Assert-CatalogState $missingLive current absent historical

    $ambiguous = New-CaseState -Live $historicalCatalogBytes -Next $currentCatalogBytes `
        -Previous $historicalCatalogBytes -WithCurrentWorker
    Assert-Failure (Invoke-FixtureInstaller $ambiguous 'ambiguous-a-b-a') 'ambiguous A,B,A state'
    Assert-CatalogState $ambiguous historical current historical

    $incompleteRecovery = New-CaseState -Live $historicalCatalogBytes -Next $currentCatalogBytes
    Assert-Failure (
        Invoke-FixtureInstaller $incompleteRecovery 'incomplete-recovery'
    ) 'incomplete current worker payload recovery'
    Assert-CatalogState $incompleteRecovery historical current absent

    $tamperedWorkerRecovery = New-CaseState -Live $historicalCatalogBytes `
        -Next $currentCatalogBytes -WithCurrentWorker
    Write-Bytes $tamperedWorkerRecovery.Worker ([System.Text.UTF8Encoding]::new($false).GetBytes('tampered'))
    Assert-Failure (
        Invoke-FixtureInstaller $tamperedWorkerRecovery 'tampered-worker-recovery'
    ) 'tampered current worker payload recovery'
    Assert-CatalogState $tamperedWorkerRecovery historical current absent

    $workerHardlinkRecovery = New-CaseState -Live $historicalCatalogBytes `
        -Next $currentCatalogBytes -WithCurrentWorker
    $workerHardlinkPeer = Join-Path $workerHardlinkRecovery.Container 'worker-hardlink-peer'
    New-Item -ItemType HardLink -Path $workerHardlinkPeer -Target $workerHardlinkRecovery.Worker | Out-Null
    Assert-Failure (
        Invoke-FixtureInstaller $workerHardlinkRecovery 'worker-hardlink-recovery'
    ) 'current worker hard-link refusal during recovery'
    Assert-CatalogState $workerHardlinkRecovery historical current absent
    Assert-Bytes $workerHardlinkPeer $currentWorkerBytes 'worker hard-link peer'

    $failedLaunch = New-CaseState -Live $historicalCatalogBytes
    Enable-LaunchSentinel $failedLaunch
    $failedLaunchExit = Invoke-FixtureInstaller $failedLaunch 'failed-launch' `
        -Fault 'before-live-to-previous' -Launch
    if ($failedLaunchExit -ne 73) { throw "Failed launch case returned $failedLaunchExit instead of 73." }
    Assert-Absent $failedLaunch.LaunchMarker 'launch sentinel after catalog publication failure'

    $restartOverride = New-CaseState
    Assert-Failure (
        Invoke-FixtureInstaller $restartOverride 'restart-applications-override' -RestartApplications
    ) '/RESTARTAPPLICATIONS override'
    Assert-CatalogState $restartOverride absent absent absent

    $maximumUpgrade = New-CaseState -Live $historicalCatalogBytes
    Add-MaximumCurrentWorkerPayload $maximumUpgrade
    Assert-Success (
        Invoke-MaximumFixtureInstaller $maximumUpgrade 'maximum-populated-upgrade'
    ) 'maximum populated historical-to-current update'
    Assert-CatalogState $maximumUpgrade current absent historical
    $installedMaximumDirectories = @(
        Get-ChildItem -LiteralPath $maximumUpgrade.InstallRoot -Directory -Recurse -Force |
            Where-Object { $_.FullName -like "$(Join-Path $maximumUpgrade.InstallRoot 'workers')*" }
    )
    $installedMaximumFiles = @(
        Get-ChildItem -LiteralPath (Join-Path $maximumUpgrade.InstallRoot 'workers') -File -Recurse -Force
    )
    if ($installedMaximumDirectories.Count -ne 900 -or $installedMaximumFiles.Count -ne 1024) {
        throw "Maximum populated update produced $($installedMaximumDirectories.Count) worker directories and $($installedMaximumFiles.Count) worker files."
    }
    foreach ($relativePath in $maximumWorkerRelativePaths) {
        Assert-Bytes (Join-Path $maximumUpgrade.InstallRoot $relativePath) `
            $currentWorkerBytes "maximum current worker payload $relativePath"
    }

    Write-Output 'Windows worker catalog publication tests passed.'
}
finally {
    foreach ($process in $ownedProcesses) {
        if (-not $process.HasExited) {
            try {
                Stop-AndReapOwnedProcess $process 'fixture cleanup'
            }
            catch {
                Write-Warning $_.Exception.Message
            }
        }
    }
    if ($cleanupMayProceed) {
        foreach ($path in $ownedShellRoots) {
            Remove-OwnedTemporaryPath $path '^scribe-release-shell-test-[0-9a-f]{32}$'
        }
        foreach ($path in $ownedCaseRoots) {
            Remove-OwnedTemporaryPath $path '^scribe-release-stable-test-[0-9a-f]{32}$'
        }
        Remove-OwnedTemporaryPath $fixtureRoot '^scribe-worker-catalog-publication-test-[0-9a-f]{32}$'
    }
    else {
        Write-Warning "Preserved fixture roots because an exact owned process could not be reaped: $fixtureRoot"
    }
}
