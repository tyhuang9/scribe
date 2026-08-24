param(
    [Parameter(Mandatory = $true)]
    [string]$ModelSource,
    [Parameter(Mandatory = $true)]
    [string]$RuntimeSource,
    [string]$BundlePath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$targetTriple = "x86_64-pc-windows-msvc"
$expectedPeMachine = 0x8664
$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$runtimeManifestPath = Join-Path $repositoryRoot "runtime-manifests\whisper-cpp-v1.9.1-windows-x64.json"
$modelManifestPath = Join-Path $repositoryRoot "runtime-manifests\whisper-base-en-q8_0-windows-x64.json"
$runtimeManifest = Get-Content -LiteralPath $runtimeManifestPath -Raw | ConvertFrom-Json
$modelManifest = Get-Content -LiteralPath $modelManifestPath -Raw | ConvertFrom-Json

function Get-NormalizedFullPath([string]$Path) {
    $full = [System.IO.Path]::GetFullPath($Path)
    $root = [System.IO.Path]::GetPathRoot($full)
    if ([string]::Equals($full, $root, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $root
    }
    return $full.TrimEnd([char[]]@('\', '/'))
}

function Assert-NoReparseAncestors([string]$Path) {
    $current = Get-NormalizedFullPath $Path
    while (-not (Test-Path -LiteralPath $current)) {
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            throw "Could not resolve an existing ancestor for output path: $Path"
        }
        $current = $parent
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Release output cannot cross a symbolic link or reparse point: $current"
        }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            break
        }
        $current = $parent
    }
}

function Assert-RegularFile([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Required release file is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Release files cannot be symbolic links or reparse points: $Path"
    }
    return $item
}

function Assert-Amd64Pe([string]$Path) {
    $null = Assert-RegularFile $Path
    $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
    try {
        if ($stream.Length -lt 64) {
            throw "PE file is too short: $Path"
        }
        $reader = [System.IO.BinaryReader]::new($stream)
        if ($reader.ReadUInt16() -ne 0x5A4D) {
            throw "PE file is missing the MZ header: $Path"
        }
        $stream.Position = 0x3C
        $peOffset = $reader.ReadUInt32()
        if ($peOffset -gt ($stream.Length - 6)) {
            throw "PE header offset is outside the file: $Path"
        }
        $stream.Position = $peOffset
        if ($reader.ReadUInt32() -ne 0x00004550) {
            throw "PE file is missing the PE signature: $Path"
        }
        $machine = $reader.ReadUInt16()
        if ($machine -ne $expectedPeMachine) {
            throw ("PE Machine mismatch for {0}: expected AMD64 0x8664, got 0x{1:X4}" -f $Path, $machine)
        }
    }
    finally {
        $stream.Dispose()
    }
}

function Get-PeSubsystem([string]$Path) {
    $null = Assert-RegularFile $Path
    $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
    try {
        if ($stream.Length -lt 256) {
            throw "PE file is too short for an optional header: $Path"
        }
        $reader = [System.IO.BinaryReader]::new($stream)
        $stream.Position = 0x3C
        $peOffset = $reader.ReadUInt32()
        $optionalHeader = [int64]$peOffset + 24
        $subsystemOffset = $optionalHeader + 68
        if ($subsystemOffset -gt ($stream.Length - 2)) {
            throw "PE subsystem field is outside the file: $Path"
        }
        $stream.Position = $optionalHeader
        $magic = $reader.ReadUInt16()
        if ($magic -notin 0x10B, 0x20B) {
            throw "PE file has an unsupported optional header: $Path"
        }
        $stream.Position = $subsystemOffset
        return $reader.ReadUInt16()
    }
    finally {
        $stream.Dispose()
    }
}

function Assert-WindowsGuiSubsystem([string]$Path) {
    $subsystem = Get-PeSubsystem $Path
    if ($subsystem -ne 2) {
        throw ("PE subsystem mismatch for {0}: expected Windows GUI (2), got {1}" -f $Path, $subsystem)
    }
}

function Assert-ExactFile([string]$Path, [int64]$ExpectedSize, [string]$ExpectedHash) {
    $item = Assert-RegularFile $Path
    if ($item.Length -ne $ExpectedSize) {
        throw "Release file size mismatch for $Path"
    }
    $actualHash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $ExpectedHash.ToLowerInvariant()) {
        throw "Release file SHA-256 mismatch for $Path"
    }
}

function Assert-CopyMatchesSource([string]$Source, [string]$Destination) {
    $sourceItem = Assert-RegularFile $Source
    $sourceHash = (Get-FileHash -LiteralPath $Source -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-ExactFile $Destination ([int64]$sourceItem.Length) $sourceHash
}

function Assert-SafeStagingPath([string]$StagingPath, [string]$FinalPath) {
    $staging = Get-NormalizedFullPath $StagingPath
    $final = Get-NormalizedFullPath $FinalPath
    $expectedParent = Split-Path -Parent $final
    if (-not [string]::Equals((Split-Path -Parent $staging), $expectedParent, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Release staging must be a direct sibling of the final bundle."
    }
    $finalName = Split-Path -Leaf $final
    $stagingName = Split-Path -Leaf $staging
    $pattern = '^{0}\.staging-[0-9]+-[0-9a-f]{{32}}$' -f [regex]::Escape($finalName)
    if ($stagingName -cnotmatch $pattern) {
        throw "Release staging path does not match the bounded transaction name."
    }
}

function Assert-TreeHasNoReparsePoints([string]$Root) {
    $rootItem = Get-Item -LiteralPath $Root -Force
    if (($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Release staging root cannot be a symbolic link or reparse point: $Root"
    }
    foreach ($item in Get-ChildItem -LiteralPath $Root -Recurse -Force) {
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Release staging cannot contain a symbolic link or reparse point: $($item.FullName)"
        }
    }
}

function Remove-ValidatedStaging([string]$StagingPath, [string]$FinalPath) {
    if (-not (Test-Path -LiteralPath $StagingPath)) {
        return
    }
    Assert-SafeStagingPath $StagingPath $FinalPath
    Assert-NoReparseAncestors $StagingPath
    Assert-TreeHasNoReparsePoints $StagingPath
    Remove-Item -LiteralPath $StagingPath -Recurse -Force
}

function Get-RelativeBundlePath([string]$Root, [string]$Path) {
    $rootUri = [System.Uri]::new((Get-NormalizedFullPath $Root) + [System.IO.Path]::DirectorySeparatorChar)
    $pathUri = [System.Uri]::new((Get-NormalizedFullPath $Path))
    return [System.Uri]::UnescapeDataString($rootUri.MakeRelativeUri($pathUri).ToString()).Replace('\', '/')
}

function Assert-ExactAllowlist([string]$Root, [string[]]$ExpectedPaths) {
    Assert-TreeHasNoReparsePoints $Root
    $actual = @(Get-ChildItem -LiteralPath $Root -Recurse -File -Force | ForEach-Object {
        Get-RelativeBundlePath $Root $_.FullName
    } | Sort-Object)
    $expected = @($ExpectedPaths | Sort-Object)
    if ($actual.Count -ne $expected.Count -or (Compare-Object -ReferenceObject $expected -DifferenceObject $actual)) {
        throw "Release bundle contains files outside the explicit allowlist."
    }
}

if (-not [Environment]::Is64BitOperatingSystem -or
    [Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "The release bundle is qualified only for Windows x64."
}
if ($runtimeManifest.platform_triple -ne $targetTriple -or $modelManifest.platform_triple -ne $targetTriple) {
    throw "Release manifests do not match the qualified Windows x64 target triple."
}
if (-not $BundlePath) {
    $BundlePath = Join-Path $repositoryRoot "artifacts\Scribe-windows-x64"
}

$finalBundle = Get-NormalizedFullPath $BundlePath
$bundleParent = Split-Path -Parent $finalBundle
$finalName = Split-Path -Leaf $finalBundle
if (-not $bundleParent -or -not $finalName) {
    throw "Final release bundle must be a named directory below an existing filesystem root."
}
$cargoTargetRoot = Get-NormalizedFullPath (Join-Path $repositoryRoot "target")
if ([string]::Equals($finalBundle, $cargoTargetRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
    $finalBundle.StartsWith($cargoTargetRoot + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Cargo target directories are build inputs and cannot be used as distributable release bundles."
}
Assert-NoReparseAncestors $bundleParent
if (Test-Path -LiteralPath $finalBundle) {
    throw "Final release bundle already exists; remove or archive it explicitly first: $finalBundle"
}
if (-not (Test-Path -LiteralPath $bundleParent)) {
    New-Item -ItemType Directory -Path $bundleParent | Out-Null
}
Assert-NoReparseAncestors $bundleParent
$staleStaging = @(Get-ChildItem -LiteralPath $bundleParent -Force | Where-Object {
    $_.Name -like "$finalName.staging-*"
})
if ($staleStaging.Count -gt 0) {
    throw "A stale release staging sibling exists; inspect and remove it explicitly: $($staleStaging[0].FullName)"
}

$stagingName = "$finalName.staging-$PID-$([guid]::NewGuid().ToString('N'))"
$stagingBundle = Join-Path $bundleParent $stagingName
Assert-SafeStagingPath $stagingBundle $finalBundle
if (Test-Path -LiteralPath $stagingBundle) {
    throw "Unique release staging path unexpectedly exists: $stagingBundle"
}

$modelSourcePath = Get-NormalizedFullPath $ModelSource
$runtimeSourceRoot = Get-NormalizedFullPath $RuntimeSource
Assert-NoReparseAncestors $modelSourcePath
Assert-NoReparseAncestors $runtimeSourceRoot
Assert-ExactFile $modelSourcePath ([int64]$modelManifest.size_bytes) $modelManifest.sha256
if (-not (Test-Path -LiteralPath $runtimeSourceRoot -PathType Container)) {
    throw "Pinned runtime source does not exist: $runtimeSourceRoot"
}
foreach ($file in $runtimeManifest.files) {
    $runtimeSourcePath = Join-Path $runtimeSourceRoot ($file.path -replace '/', '\')
    Assert-ExactFile $runtimeSourcePath ([int64]$file.size_bytes) $file.sha256
    if ([System.IO.Path]::GetExtension($runtimeSourcePath) -in @('.dll', '.exe')) {
        Assert-Amd64Pe $runtimeSourcePath
    }
}

Push-Location $repositoryRoot
try {
    & cargo build --locked --offline --release --all-features --target $targetTriple --manifest-path (Join-Path $repositoryRoot "Cargo.toml")
    if ($LASTEXITCODE -ne 0) {
        throw "The locked offline Windows x64 release build failed."
    }
}
finally {
    Pop-Location
}

$cargoReleaseRoot = Join-Path $repositoryRoot "target\$targetTriple\release"
$sourceExecutable = Join-Path $cargoReleaseRoot "local-transcriber.exe"
Assert-Amd64Pe $sourceExecutable
Assert-WindowsGuiSubsystem $sourceExecutable

try {
    New-Item -ItemType Directory -Path $stagingBundle | Out-Null
    Assert-NoReparseAncestors $stagingBundle
    $stagedExecutable = Join-Path $stagingBundle "local-transcriber.exe"
    Copy-Item -LiteralPath $sourceExecutable -Destination $stagedExecutable

    $stagedRuntimeRoot = Join-Path $stagingBundle "runtimes\whisper_cpp"
    foreach ($file in $runtimeManifest.files) {
        $relative = $file.path -replace '/', '\'
        $sourcePath = Join-Path $runtimeSourceRoot $relative
        $destinationPath = Join-Path $stagedRuntimeRoot $relative
        New-Item -ItemType Directory -Path (Split-Path -Parent $destinationPath) -Force | Out-Null
        Copy-Item -LiteralPath $sourcePath -Destination $destinationPath
    }
    Copy-Item -LiteralPath $runtimeManifestPath -Destination (Join-Path $stagedRuntimeRoot "runtime-manifest.json")

    $stagedModel = Join-Path $stagingBundle $modelManifest.artifact_filename
    Copy-Item -LiteralPath $modelSourcePath -Destination $stagedModel
    $stagedLicenses = Join-Path $stagingBundle "licenses"
    New-Item -ItemType Directory -Path $stagedLicenses | Out-Null
    foreach ($relativePath in $modelManifest.attribution_files) {
        $sourcePath = Join-Path $repositoryRoot ($relativePath -replace '/', '\')
        $null = Assert-RegularFile $sourcePath
        Copy-Item -LiteralPath $sourcePath -Destination $stagedLicenses
    }
    $stagedModelManifest = Join-Path $stagingBundle "bundled-model-manifest.json"
    Copy-Item -LiteralPath $modelManifestPath -Destination $stagedModelManifest
    $cargoManifest = Get-Content -LiteralPath (Join-Path $repositoryRoot "Cargo.toml") -Raw
    $versionMatch = [regex]::Match($cargoManifest, '(?m)^version\s*=\s*"([^"]+)"')
    if (-not $versionMatch.Success) {
        throw "Could not read the Scribe version from Cargo.toml."
    }
    $portableReadme = Join-Path $stagingBundle "README.txt"
    [System.IO.File]::WriteAllText(
        $portableReadme,
        "Scribe $($versionMatch.Groups[1].Value)`r`n`r`nExtract this entire folder before running local-transcriber.exe. This portable Windows x64 package includes the verified English Base model and compatibility runtime; do not distribute the executable by itself.`r`n",
        [System.Text.UTF8Encoding]::new($false)
    )

    Assert-NoReparseAncestors $stagingBundle
    Assert-TreeHasNoReparsePoints $stagingBundle
    Assert-CopyMatchesSource $sourceExecutable $stagedExecutable
    Assert-CopyMatchesSource $runtimeManifestPath (Join-Path $stagedRuntimeRoot "runtime-manifest.json")
    Assert-CopyMatchesSource $modelManifestPath $stagedModelManifest
    foreach ($relativePath in $modelManifest.attribution_files) {
        $sourcePath = Join-Path $repositoryRoot ($relativePath -replace '/', '\')
        Assert-CopyMatchesSource $sourcePath (Join-Path $stagedLicenses (Split-Path -Leaf $relativePath))
    }
    Assert-Amd64Pe $stagedExecutable
    Assert-WindowsGuiSubsystem $stagedExecutable
    Assert-ExactFile $stagedModel ([int64]$modelManifest.size_bytes) $modelManifest.sha256
    foreach ($file in $runtimeManifest.files) {
        $path = Join-Path $stagedRuntimeRoot ($file.path -replace '/', '\')
        Assert-ExactFile $path ([int64]$file.size_bytes) $file.sha256
        if ([System.IO.Path]::GetExtension($path) -in @('.dll', '.exe')) {
            Assert-Amd64Pe $path
        }
    }

    $expectedPaths = [System.Collections.Generic.List[string]]::new()
    $null = $expectedPaths.Add("local-transcriber.exe")
    foreach ($file in $runtimeManifest.files) {
        $null = $expectedPaths.Add("runtimes/whisper_cpp/$($file.path)")
    }
    $null = $expectedPaths.Add("runtimes/whisper_cpp/runtime-manifest.json")
    $null = $expectedPaths.Add($modelManifest.artifact_filename)
    foreach ($relativePath in $modelManifest.attribution_files) {
        $null = $expectedPaths.Add("licenses/$(Split-Path -Leaf $relativePath)")
    }
    $null = $expectedPaths.Add("bundled-model-manifest.json")
    $null = $expectedPaths.Add("README.txt")
    Assert-ExactAllowlist $stagingBundle $expectedPaths.ToArray()

    $previousHubOffline = $env:HF_HUB_OFFLINE
    $previousTransformersOffline = $env:TRANSFORMERS_OFFLINE
    try {
        $env:HF_HUB_OFFLINE = "1"
        $env:TRANSFORMERS_OFFLINE = "1"
        $smokeJson = & $stagedExecutable `
            --scribe-install-smoke-parent `
            $modelManifest.model_id `
            $stagedModel `
            gguf `
            - `
            $modelManifest.size_bytes `
            $modelManifest.sha256 `
            cpu
        if ($LASTEXITCODE -ne 0) {
            throw "Offline staged-bundle smoke failed with exit code $LASTEXITCODE."
        }
    }
    finally {
        $env:HF_HUB_OFFLINE = $previousHubOffline
        $env:TRANSFORMERS_OFFLINE = $previousTransformersOffline
    }
    $smoke = ($smokeJson | Out-String) | ConvertFrom-Json
    if (-not $smoke.cancellation_verified -or -not $smoke.capabilities.cancellation) {
        throw "Offline staged-bundle smoke did not verify cancellation."
    }

    $inventoryEntries = @($expectedPaths.ToArray() | Sort-Object | ForEach-Object {
        $path = Join-Path $stagingBundle ($_ -replace '/', '\')
        $item = Assert-RegularFile $path
        [ordered]@{
            path = $_
            size_bytes = [int64]$item.Length
            sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    })
    $inventory = [ordered]@{
        schema_version = 1
        platform_triple = $targetTriple
        files = $inventoryEntries
    }
    $inventoryPath = Join-Path $stagingBundle "bundle-inventory.json"
    $inventoryJson = $inventory | ConvertTo-Json -Depth 5
    [System.IO.File]::WriteAllText($inventoryPath, $inventoryJson, [System.Text.UTF8Encoding]::new($false))
    $expectedWithInventory = @($expectedPaths.ToArray()) + @("bundle-inventory.json")
    Assert-ExactAllowlist $stagingBundle $expectedWithInventory
    foreach ($entry in $inventoryEntries) {
        Assert-ExactFile (Join-Path $stagingBundle ($entry.path -replace '/', '\')) $entry.size_bytes $entry.sha256
    }
    $inventoryHash = (Get-FileHash -LiteralPath $inventoryPath -Algorithm SHA256).Hash.ToLowerInvariant()

    Assert-SafeStagingPath $stagingBundle $finalBundle
    Assert-NoReparseAncestors $stagingBundle
    Assert-TreeHasNoReparsePoints $stagingBundle
    if (Test-Path -LiteralPath $finalBundle) {
        throw "Final release bundle appeared during staging; refusing to replace it: $finalBundle"
    }
    Move-Item -LiteralPath $stagingBundle -Destination $finalBundle

    Write-Output "Windows x64 release bundle ready: $finalBundle"
    Write-Output "Bundle inventory SHA-256: $inventoryHash"
}
catch {
    try {
        Remove-ValidatedStaging $stagingBundle $finalBundle
    }
    catch {
        Write-Warning "Refused automatic staging cleanup because its bounds could not be proven: $($_.Exception.Message)"
    }
    throw
}
