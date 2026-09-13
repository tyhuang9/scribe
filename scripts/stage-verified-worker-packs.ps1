param(
    [Parameter(Mandatory = $true)]
    [string]$BundleRoot,
    [Parameter(Mandatory = $true)]
    [string]$VerifierExecutable,
    [string[]]$PackRoot = @(),
    [Parameter(Mandatory = $true)]
    [string]$InstallerAllowlistPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'windows-gpu-pack-history.ps1')

function Get-NormalizedFullPath([string]$Path) {
    $full = [System.IO.Path]::GetFullPath($Path)
    $root = [System.IO.Path]::GetPathRoot($full)
    if ([string]::Equals($full, $root, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $root
    }
    return $full.TrimEnd([char[]]@('\', '/'))
}

function Assert-RegularNonReparseFile([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Required worker-pack file is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Worker-pack file must be regular and non-reparse: $Path"
    }
    return $item
}

function Assert-SafeRelativePath([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path) -or
        [System.IO.Path]::IsPathRooted($Path) -or
        $Path.Contains('\') -or
        $Path.Contains(':')) {
        throw "Worker-pack path is unsafe: $Path"
    }
    $segments = @($Path.Split('/'))
    if ($segments.Count -eq 0 -or $segments.Count -gt 16) {
        throw "Worker-pack path depth is invalid: $Path"
    }
    foreach ($segment in $segments) {
        if ([string]::IsNullOrWhiteSpace($segment) -or
            $segment -in @('.', '..') -or
            $segment.Length -gt 128 -or
            $segment.EndsWith('.') -or
            $segment.EndsWith(' ')) {
            throw "Worker-pack path segment is unsafe: $Path"
        }
        $stem = $segment.Split('.')[0].ToUpperInvariant()
        if ($stem -in @('CON', 'PRN', 'AUX', 'NUL', 'CLOCK$') -or
            $stem -match '^(COM|LPT)[1-9]$') {
            throw "Worker-pack path uses a reserved Windows name: $Path"
        }
    }
}

function Assert-TreeIsRegular([string]$Root) {
    $rootItem = Get-Item -LiteralPath $Root -Force
    if (-not $rootItem.PSIsContainer -or
        ($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Worker-pack root must be a regular non-reparse directory: $Root"
    }
    $identities = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($item in @(Get-ChildItem -LiteralPath $Root -Recurse -Force)) {
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Worker-pack tree contains a link or reparse point: $($item.FullName)"
        }
        $relative = [System.IO.Path]::GetRelativePath($Root, $item.FullName).Replace('\', '/')
        Assert-SafeRelativePath $relative
        if (-not $identities.Add($relative)) {
            throw "Worker-pack tree contains a case-insensitive collision: $relative"
        }
    }
}

function Assert-ExactProperties([psobject]$Value, [string[]]$Names, [string]$Label) {
    if ($null -eq $Value) {
        throw "$Label is missing."
    }
    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $expected = @($Names | Sort-Object)
    if ($actual.Count -ne $expected.Count -or
        (Compare-Object -ReferenceObject $expected -DifferenceObject $actual -CaseSensitive)) {
        throw "$Label has unknown or missing fields."
    }
}

function Invoke-PackVerifier([string]$Executable, [string]$Root) {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.ArgumentList.Add('--scribe-verify-worker-pack')
    $startInfo.ArgumentList.Add($Root)
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            throw 'Could not start the compiled worker-pack verifier.'
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $output = $stdout.GetAwaiter().GetResult()
        $errorOutput = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) {
            throw "Compiled worker-pack verification failed closed: $($errorOutput.Trim())"
        }
        try {
            $descriptor = $output | ConvertFrom-Json
        }
        catch {
            throw "Compiled worker-pack verifier returned invalid JSON: $($_.Exception.Message)"
        }
        Assert-ExactProperties $descriptor @(
            'pack_id', 'pack_version', 'pack_digest', 'security_epoch',
            'runtime_abi_version', 'backend', 'provider', 'target_os',
            'target_arch', 'worker_relative_path', 'root'
        ) 'Verified worker-pack descriptor'
        return $descriptor
    }
    finally {
        $process.Dispose()
    }
}

function Assert-SafeIdentity([string]$Value, [string]$Label) {
    if ([string]::IsNullOrWhiteSpace($Value) -or
        $Value.Length -gt 96 -or
        $Value -cnotmatch '^[A-Za-z0-9._:-]+$') {
        throw "Verified worker-pack $Label is unsafe."
    }
}

function Get-CompressedSize([string]$Root) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $temporary = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-pack-size-$([guid]::NewGuid().ToString('N')).zip"
    try {
        [System.IO.Compression.ZipFile]::CreateFromDirectory(
            $Root,
            $temporary,
            [System.IO.Compression.CompressionLevel]::Optimal,
            $false
        )
        return [int64](Assert-RegularNonReparseFile $temporary).Length
    }
    finally {
        if (Test-Path -LiteralPath $temporary -PathType Leaf) {
            Remove-Item -LiteralPath $temporary -Force
        }
    }
}

function Write-WorkerPackCatalog(
    [string]$BundleRoot,
    [object[]]$Packs,
    [string]$SourceRevision
) {
    if ($SourceRevision -cnotmatch '^[0-9a-f]{40}$') {
        throw 'Worker-pack catalog history requires an exact checked-out source revision.'
    }
    # JSON and installer identities must not depend on caller order or host culture.
    $orderedPacks = [System.Collections.Generic.List[object]]::new()
    foreach ($pack in $Packs) {
        $orderedFiles = [string[]]@($pack.files)
        [Array]::Sort($orderedFiles, [System.StringComparer]::Ordinal)
        $pack.files = $orderedFiles
        $orderedPacks.Add($pack)
    }
    $orderedPacks.Sort([System.Comparison[object]]{
        param($left, $right)
        [System.StringComparer]::Ordinal.Compare([string]$left.root, [string]$right.root)
    })
    $catalog = [ordered]@{
        schema_version = 1
        packs = [object[]]$orderedPacks.ToArray()
    }
    $catalogJson = $catalog | ConvertTo-Json -Depth 8
    if ([System.Text.Encoding]::UTF8.GetByteCount($catalogJson) -gt
        $script:WindowsGpuPackHistoryMaximumCatalogSize) {
        throw 'Generated worker-pack catalog exceeds the runtime catalog byte bound.'
    }
    $catalogPath = Join-Path $BundleRoot 'worker-pack-catalog.json'
    [System.IO.File]::WriteAllText(
        $catalogPath,
        $catalogJson,
        [System.Text.UTF8Encoding]::new($false)
    )
    $catalogSize = [int64](Assert-RegularNonReparseFile $catalogPath).Length
    $catalogSha256 = (Get-FileHash -LiteralPath $catalogPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $currentPackHistory = @($orderedPacks | ForEach-Object {
        $pack = $_
        [ordered]@{
            pack_id = $pack.pack_id
            pack_version = $pack.pack_version
            pack_digest = $pack.pack_digest
            security_epoch = $pack.security_epoch
            root = $pack.root
            files = @($pack.files | ForEach-Object {
                $relative = $_
                $filePath = Join-Path $BundleRoot ($relative -replace '/', '\')
                [ordered]@{
                    path = $relative
                    size_bytes = [int64](Assert-RegularNonReparseFile $filePath).Length
                    sha256 = (Get-FileHash -LiteralPath $filePath -Algorithm SHA256).Hash.ToLowerInvariant()
                }
            })
        }
    })
    $currentHistoryJson = [ordered]@{
        schema_version = 1
        history_epoch = 1
        releases = @([ordered]@{
            release_id = "build-$SourceRevision"
            source_revision = $SourceRevision
            catalog_size_bytes = $catalogSize
            catalog_sha256 = $catalogSha256
            catalog_pack_roots = @($orderedPacks | ForEach-Object { $_.root })
            packs = $currentPackHistory
        })
    } | ConvertTo-Json -Depth 12
    $currentHistory = ConvertFrom-WindowsGpuPackHistoryJson `
        -Json $currentHistoryJson -SourceLabel 'verified current staging inventory'
    return [pscustomobject]@{
        Path = $catalogPath
        SizeBytes = $catalogSize
        Sha256 = $catalogSha256
        History = $currentHistory
    }
}

function Get-InstallerFileIdentityClauses([object[]]$Files) {
    return (@($Files | ForEach-Object {
        $native = $_.Path.Replace('/', '\').Replace("'", "''")
        @"
  if SameStr(RelativePath, '$native') then
  begin
    FileSize := $($_.SizeBytes);
    Sha256 := '$($_.Sha256)';
    Result := True;
    Exit;
  end;
"@
    }) -join "`r`n")
}

function Write-InstallerAllowlist(
    [string]$Path,
    [string[]]$Files,
    [object]$RetirementPlan,
    [int64]$CatalogSize,
    [string]$CatalogSha256
) {
    $directories = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($file in $Files) {
        $segments = $file.Split('/')
        for ($index = 1; $index -lt $segments.Count; $index++) {
            $null = $directories.Add(($segments[0..($index - 1)] -join '\'))
        }
    }
    if ($directories.Count -gt 900) {
        throw 'Declared worker packs exceed the installer directory-handle bound.'
    }
    $orderedDirectories = Get-WindowsGpuPackHistoryOrdinalStrings $directories
    $orderedFiles = Get-WindowsGpuPackHistoryOrdinalStrings $Files
    $directoryClauses = @($orderedDirectories | ForEach-Object {
        "    SameStr(RelativePath, '$($_.Replace("'", "''"))')"
    })
    $fileClauses = @($orderedFiles | ForEach-Object {
        $native = $_.Replace('/', '\').Replace("'", "''")
        "    SameStr(RelativePath, '$native')"
    })
    $directoryExpression = if ($directoryClauses.Count -eq 0) { '  Result := False;' } else {
        "  Result :=`r`n" + ($directoryClauses -join " or`r`n") + ';'
    }
    $fileExpression = if ($fileClauses.Count -eq 0) { '  Result := False;' } else {
        "  Result :=`r`n" + ($fileClauses -join " or`r`n") + ';'
    }
    $retiredDirectoryClauses = @($RetirementPlan.RetiredDirectories | ForEach-Object {
        $native = $_.Replace('/', '\').Replace("'", "''")
        "    SameStr(RelativePath, '$native')"
    })
    $retiredDirectoryExpression = if ($retiredDirectoryClauses.Count -eq 0) {
        '  Result := False;'
    } else {
        "  Result :=`r`n" + ($retiredDirectoryClauses -join " or`r`n") + ';'
    }
    $currentFileClauses = Get-InstallerFileIdentityClauses $RetirementPlan.CurrentFiles
    $retiredFileClauses = Get-InstallerFileIdentityClauses $RetirementPlan.RetiredFiles
    $knownCatalogClauses = Get-WindowsGpuPackHistoryOrdinalStrings @(
        "    ((FileSize = $CatalogSize) and SameStr(Sha256, '$CatalogSha256'))"
        $RetirementPlan.HistoricalCatalogs | ForEach-Object {
            "    ((FileSize = $($_.SizeBytes)) and SameStr(Sha256, '$($_.Sha256)'))"
        }
    )
    $knownCatalogClauses = @($knownCatalogClauses | Select-Object -Unique)
    $knownCatalogExpression = "  Result :=`r`n" + ($knownCatalogClauses -join " or`r`n") + ';'
    $text = @"
// Generated from verified current packs and checked-in release history. Do not commit.
function IsGeneratedWorkerPackDirectory(RelativePath: String): Boolean;
begin
$directoryExpression
end;

function IsGeneratedWorkerPackFile(RelativePath: String): Boolean;
begin
$fileExpression
end;

function IsGeneratedRetiredWorkerPackDirectory(RelativePath: String): Boolean;
begin
$retiredDirectoryExpression
end;

function GetGeneratedCurrentWorkerPackFileIdentity(
  RelativePath: String; var FileSize: Int64; var Sha256: String
): Boolean;
begin
  Result := False;
  FileSize := -1;
  Sha256 := '';
$currentFileClauses
end;

function GetGeneratedCurrentWorkerPackFileCount(): Integer;
begin
  Result := $($RetirementPlan.CurrentFiles.Count);
end;

function GetGeneratedRetiredWorkerPackFileIdentity(
  RelativePath: String; var FileSize: Int64; var Sha256: String
): Boolean;
begin
  Result := False;
  FileSize := -1;
  Sha256 := '';
$retiredFileClauses
end;

function IsGeneratedCurrentWorkerCatalog(FileSize: Int64; Sha256: String): Boolean;
begin
  Result := (FileSize = $CatalogSize) and SameStr(Sha256, '$CatalogSha256');
end;

function IsGeneratedKnownWorkerCatalog(FileSize: Int64; Sha256: String): Boolean;
begin
$knownCatalogExpression
end;
"@
    $parent = Split-Path -Parent $Path
    if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    [System.IO.File]::WriteAllText($Path, $text, [System.Text.UTF8Encoding]::new($false))
}

$bundle = Get-NormalizedFullPath $BundleRoot
if (-not (Test-Path -LiteralPath $bundle -PathType Container)) {
    throw "Worker-pack bundle staging root is missing: $bundle"
}
$verifier = Get-NormalizedFullPath $VerifierExecutable
if ($PackRoot.Count -gt 8) {
    throw 'Worker-pack declaration exceeds the eight-pack release bound.'
}
if ($PackRoot.Count -gt 0) {
    $null = Assert-RegularNonReparseFile $verifier
}
$caseFoldedRoots = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
$allPackFiles = [System.Collections.Generic.List[string]]::new()
$catalogPacks = [System.Collections.Generic.List[object]]::new()

foreach ($declaredRoot in @($PackRoot)) {
    $source = Get-NormalizedFullPath $declaredRoot
    if (-not $caseFoldedRoots.Add($source)) {
        throw "Worker-pack roots contain a duplicate path: $source"
    }
    Assert-TreeIsRegular $source
    $sourceDescriptor = Invoke-PackVerifier $verifier $source
    Assert-SafeIdentity ([string]$sourceDescriptor.pack_id) 'pack id'
    Assert-SafeIdentity ([string]$sourceDescriptor.pack_version) 'version'
    if ([string]$sourceDescriptor.pack_digest -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Verified worker-pack digest is not canonical SHA-256.'
    }
    $relativeRoot = "workers/packs/$($sourceDescriptor.pack_id)/$($sourceDescriptor.pack_version)/$($sourceDescriptor.pack_digest)"
    Assert-SafeRelativePath $relativeRoot
    $destination = Join-Path $bundle ($relativeRoot -replace '/', '\')
    if (Test-Path -LiteralPath $destination) {
        throw "Worker-pack immutable digest destination already exists: $destination"
    }
    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    foreach ($item in @(Get-ChildItem -LiteralPath $source -Force)) {
        Copy-Item -LiteralPath $item.FullName -Destination (Join-Path $destination $item.Name) -Recurse
    }
    Assert-TreeIsRegular $destination
    $stagedDescriptor = Invoke-PackVerifier $verifier $destination
    foreach ($field in @(
        'pack_id', 'pack_version', 'pack_digest', 'security_epoch',
        'runtime_abi_version', 'backend', 'provider', 'target_os',
        'target_arch', 'worker_relative_path'
    )) {
        if ([string]$sourceDescriptor.$field -cne [string]$stagedDescriptor.$field) {
            throw "Staged worker-pack descriptor changed field '$field'."
        }
    }
    $packFiles = @(Get-ChildItem -LiteralPath $destination -Recurse -File -Force | ForEach-Object {
        $relative = [System.IO.Path]::GetRelativePath($bundle, $_.FullName).Replace('\', '/')
        Assert-SafeRelativePath $relative
        $relative
    } | Sort-Object)
    $installedSize = [int64](($packFiles | ForEach-Object {
        (Assert-RegularNonReparseFile (Join-Path $bundle ($_ -replace '/', '\'))).Length
    } | Measure-Object -Sum).Sum)
    $compressedSize = Get-CompressedSize $destination
    foreach ($file in $packFiles) {
        $allPackFiles.Add($file)
    }
    $catalogPacks.Add([ordered]@{
        pack_id = [string]$stagedDescriptor.pack_id
        pack_version = [string]$stagedDescriptor.pack_version
        pack_digest = [string]$stagedDescriptor.pack_digest
        security_epoch = [uint64]$stagedDescriptor.security_epoch
        runtime_abi_version = [uint16]$stagedDescriptor.runtime_abi_version
        backend = [string]$stagedDescriptor.backend
        provider = [string]$stagedDescriptor.provider
        target_os = [string]$stagedDescriptor.target_os
        target_arch = [string]$stagedDescriptor.target_arch
        worker_relative_path = [string]$stagedDescriptor.worker_relative_path
        root = $relativeRoot
        installed_size_bytes = $installedSize
        compressed_size_bytes = $compressedSize
        files = $packFiles
    })
    Write-Host "Verified worker pack $($stagedDescriptor.pack_id) $($stagedDescriptor.pack_version): installed=$installedSize compressed=$compressedSize bytes"
}

if ($allPackFiles.Count -gt 1024) {
    throw 'Declared worker packs exceed the 1,024-file release bound.'
}

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$sourceRevision = @(& git -C $repositoryRoot rev-parse --verify HEAD)
if ($LASTEXITCODE -ne 0 -or $sourceRevision.Count -ne 1 -or
    $sourceRevision[0] -cnotmatch '^[0-9a-f]{40}$') {
    throw 'Worker-pack catalog history requires an exact checked-out source revision.'
}
$currentCatalog = Write-WorkerPackCatalog $bundle $catalogPacks.ToArray() $sourceRevision[0]
$history = Read-WindowsGpuPackHistory -LiteralPath (
    Join-Path $repositoryRoot 'runtime-manifests\gpu-worker-pack-history-windows-x64.json'
)
$retirementPlan = Get-WindowsGpuPackRetirementPlan `
    -HistoryDocument $history -CurrentDocument $currentCatalog.History
Write-InstallerAllowlist `
    (Get-NormalizedFullPath $InstallerAllowlistPath) `
    $allPackFiles.ToArray() $retirementPlan $currentCatalog.SizeBytes $currentCatalog.Sha256

[pscustomobject]@{
    PackFiles = $allPackFiles.ToArray()
    PackCount = $catalogPacks.Count
    CatalogPath = $currentCatalog.Path
}
