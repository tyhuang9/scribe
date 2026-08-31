param(
    [Parameter(Mandatory = $true)]
    [string]$BundlePath,
    [string]$PortableZipPath,
    [string]$InstallerPath,
    [switch]$ExerciseStableUpgrade,
    [string]$EvidenceDirectory
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot "windows-pe-imports.ps1")

$targetTriple = "x86_64-pc-windows-msvc"
$expectedPeMachine = 0x8664
$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$modelManifestPath = Join-Path $repositoryRoot "runtime-manifests\whisper-base-en-q8_0-windows-x64.json"
$modelManifest = Get-Content -LiteralPath $modelManifestPath -Raw | ConvertFrom-Json
$legalFiles = @(
    [pscustomobject]@{ Source = "resources/licenses/Apache-2.0.txt"; Destination = "licenses/Apache-2.0.txt" },
    [pscustomobject]@{ Source = "resources/licenses/OpenAI-Whisper-MIT.txt"; Destination = "licenses/OpenAI-Whisper-MIT.txt" },
    [pscustomobject]@{ Source = "resources/licenses/Whisper-Base-En-NOTICE.txt"; Destination = "licenses/Whisper-Base-En-NOTICE.txt" },
    [pscustomobject]@{ Source = "resources/licenses/THIRD-PARTY-NOTICES.txt"; Destination = "licenses/THIRD-PARTY-NOTICES.txt" },
    [pscustomobject]@{ Source = "native/transcribe-cpp-v0.1.3/LICENSE"; Destination = "licenses/transcribe.cpp-MIT.txt" },
    [pscustomobject]@{ Source = "native/transcribe-cpp-v0.1.3/PROVENANCE.md"; Destination = "licenses/transcribe.cpp-PROVENANCE.md" },
    [pscustomobject]@{ Source = "native/whisper-f049fff/LICENSE"; Destination = "licenses/whisper.cpp-MIT.txt" },
    [pscustomobject]@{ Source = "native/whisper-f049fff/PROVENANCE.md"; Destination = "licenses/whisper.cpp-PROVENANCE.md" },
    [pscustomobject]@{ Source = "native/sherpa-onnx-v1.13.5/PROVENANCE.md"; Destination = "licenses/sherpa-onnx-PROVENANCE.md" },
    [pscustomobject]@{ Source = "resources/silero-vad/LICENSE"; Destination = "licenses/Silero-VAD-MIT.txt" },
    [pscustomobject]@{ Source = "resources/silero-vad/PROVENANCE.md"; Destination = "licenses/Silero-VAD-PROVENANCE.md" }
)
$baseExpectedInventoryPaths = @(
    "bundled-model-manifest.json",
    "licenses/Apache-2.0.txt",
    "licenses/OpenAI-Whisper-MIT.txt",
    "licenses/Silero-VAD-MIT.txt",
    "licenses/Silero-VAD-PROVENANCE.md",
    "licenses/THIRD-PARTY-NOTICES.txt",
    "licenses/Whisper-Base-En-NOTICE.txt",
    "licenses/sherpa-onnx-PROVENANCE.md",
    "licenses/transcribe.cpp-MIT.txt",
    "licenses/transcribe.cpp-PROVENANCE.md",
    "licenses/whisper.cpp-MIT.txt",
    "licenses/whisper.cpp-PROVENANCE.md",
    "local-transcriber.exe",
    "README.txt",
    "scribe-inference-worker.exe",
    "worker-pack-catalog.json",
    "whisper-base.en-Q8_0.gguf"
)
$script:expectedInventoryPaths = @($baseExpectedInventoryPaths)
$script:expectedPortablePayloadPaths = @($script:expectedInventoryPaths) + @("bundle-inventory.json")
$script:allowedPackExecutablePaths = @()

function Get-NormalizedPath([string]$Path) {
    $full = [System.IO.Path]::GetFullPath($Path)
    $root = [System.IO.Path]::GetPathRoot($full)
    if ([string]::Equals($full, $root, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $root
    }
    return $full.TrimEnd([char[]]@('\', '/'))
}

function Invoke-NativeProcess(
    [string]$ExecutablePath,
    [string[]]$Arguments
) {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $ExecutablePath
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $quotedArguments = foreach ($argument in $Arguments) {
        if ($argument.Contains('"')) {
            throw "Native process arguments cannot contain a double quote."
        }
        if ($argument.EndsWith('\')) {
            throw "Native process arguments cannot end with a backslash."
        }
        '"' + $argument + '"'
    }
    $startInfo.Arguments = $quotedArguments -join ' '

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            throw "Could not start native process: $ExecutablePath"
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout.GetAwaiter().GetResult()
            Stderr = $stderr.GetAwaiter().GetResult()
        }
    }
    finally {
        $process.Dispose()
    }
}

function Assert-NoReparseAncestors([string]$Path) {
    $current = Get-NormalizedPath $Path
    while (-not (Test-Path -LiteralPath $current)) {
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            throw "Could not resolve an existing ancestor for release verification path: $Path"
        }
        $current = $parent
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Release verification cannot cross a symbolic link or reparse point: $current"
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

function Assert-TreeHasNoReparsePoints([string]$Root) {
    if (-not (Test-Path -LiteralPath $Root -PathType Container)) {
        throw "Release payload directory is missing: $Root"
    }
    $rootItem = Get-Item -LiteralPath $Root -Force
    if (($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Release payload root cannot be a symbolic link or reparse point: $Root"
    }
    foreach ($item in Get-ChildItem -LiteralPath $Root -Recurse -Force) {
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Release payload cannot contain a symbolic link or reparse point: $($item.FullName)"
        }
    }
}

function Get-RelativeBundlePath([string]$Root, [string]$Path) {
    $rootUri = [System.Uri]::new((Get-NormalizedPath $Root) + [System.IO.Path]::DirectorySeparatorChar)
    $pathUri = [System.Uri]::new((Get-NormalizedPath $Path))
    return [System.Uri]::UnescapeDataString($rootUri.MakeRelativeUri($pathUri).ToString()).Replace('\', '/')
}

function Assert-SafeRelativePayloadPath([string]$RelativePath) {
    if ([string]::IsNullOrWhiteSpace($RelativePath) -or
        [System.IO.Path]::IsPathRooted($RelativePath) -or
        $RelativePath.Contains('\') -or
        $RelativePath.Contains(':')) {
        throw "Release payload contains an unsafe relative path: $RelativePath"
    }
    $segments = @($RelativePath.Split('/'))
    if ($segments.Count -eq 0 -or @($segments | Where-Object { $_ -in @('', '.', '..') }).Count -gt 0) {
        throw "Release payload contains an unsafe path segment: $RelativePath"
    }
}

function Assert-AllowedPayloadFile(
    [string]$RelativePath,
    [string[]]$AllowedExecutablePaths = @("local-transcriber.exe", "scribe-inference-worker.exe")
) {
    Assert-SafeRelativePayloadPath $RelativePath
    $lower = $RelativePath.ToLowerInvariant()
    $segments = @($lower.Split('/'))
    $leaf = $segments[-1]
    $extension = [System.IO.Path]::GetExtension($leaf).ToLowerInvariant()

    if ($segments -contains 'runtimes' -or
        $leaf -match '^runtime-manifest(?:\..+)?$' -or
        $lower -match '(^|/)(?:\.?venv|__pycache__|python(?:\d+(?:\.\d+)*)?|runner)(/|$)' -or
        $leaf -match '^(?:python(?:\d+(?:\.\d+)*)?|runner)(?:\..+)?$' -or
        $extension -in @('.pyd', '.py', '.pyc', '.onnx', '.ort')) {
        throw "Release payload contains a forbidden runtime, Python, runner, or loose ONNX artifact: $RelativePath"
    }
    $allowedExecutable = @($AllowedExecutablePaths | Where-Object {
        [string]::Equals($_, $RelativePath, [System.StringComparison]::OrdinalIgnoreCase)
    })
    if ($extension -in @('.dll', '.exe') -and $allowedExecutable.Count -eq 0) {
        throw "Release payload contains an unallowlisted executable or DLL: $RelativePath"
    }
}

function Assert-ExactFile([string]$Path, [int64]$ExpectedSize, [string]$ExpectedHash) {
    $item = Assert-RegularFile $Path
    if ($item.Length -ne $ExpectedSize) {
        throw "Bundle inventory size mismatch for $Path."
    }
    $hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($hash -cne $ExpectedHash) {
        throw "Bundle inventory SHA-256 mismatch for $Path."
    }
}

function Assert-Amd64GuiPe([string]$Path) {
    $null = Assert-RegularFile $Path
    $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
    try {
        if ($stream.Length -lt 256) {
            throw "PE file is too short: $Path"
        }
        $reader = [System.IO.BinaryReader]::new($stream)
        if ($reader.ReadUInt16() -ne 0x5A4D) {
            throw "PE file is missing the MZ header: $Path"
        }
        $stream.Position = 0x3C
        $peOffset = $reader.ReadUInt32()
        if ($peOffset -gt ($stream.Length - 94)) {
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
        $stream.Position = [int64]$peOffset + 24
        $magic = $reader.ReadUInt16()
        if ($magic -notin 0x10B, 0x20B) {
            throw "PE file has an unsupported optional header: $Path"
        }
        $stream.Position = [int64]$peOffset + 24 + 68
        $subsystem = $reader.ReadUInt16()
        if ($subsystem -ne 2) {
            throw ("PE subsystem mismatch for {0}: expected Windows GUI (2), got {1}" -f $Path, $subsystem)
        }
    }
    finally {
        $stream.Dispose()
    }
}

function Assert-ExactObjectProperties($Object, [string[]]$ExpectedNames, [string]$Description) {
    if ($null -eq $Object) {
        throw "$Description is missing."
    }
    $actualNames = @($Object.PSObject.Properties.Name | Sort-Object)
    $expected = @($ExpectedNames | Sort-Object)
    if ($actualNames.Count -ne $expected.Count -or
        (Compare-Object -ReferenceObject $expected -DifferenceObject $actualNames -CaseSensitive)) {
        throw "$Description has unexpected or missing properties."
    }
}

function Get-ExpectedDirectories([string[]]$FilePaths) {
    $directories = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $FilePaths) {
        $segments = $path.Split('/')
        for ($index = 1; $index -lt $segments.Count; $index++) {
            $null = $directories.Add(($segments[0..($index - 1)] -join '/'))
        }
    }
    return @($directories | Sort-Object)
}

function Assert-ExactPayloadTree(
    [string]$Root,
    [string[]]$ExpectedPaths,
    [string[]]$AllowedExecutablePaths
) {
    Assert-NoReparseAncestors $Root
    Assert-TreeHasNoReparsePoints $Root

    $expectedCaseFolded = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $ExpectedPaths) {
        Assert-AllowedPayloadFile $path $AllowedExecutablePaths
        if (-not $expectedCaseFolded.Add($path)) {
            throw "Expected release payload contains duplicate case-insensitive paths: $path"
        }
    }

    $actualPaths = @(Get-ChildItem -LiteralPath $Root -Recurse -File -Force | ForEach-Object {
        Get-RelativeBundlePath $Root $_.FullName
    } | Sort-Object)
    $actualCaseFolded = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $actualPaths) {
        Assert-AllowedPayloadFile $path $AllowedExecutablePaths
        if (-not $actualCaseFolded.Add($path)) {
            throw "Release payload contains duplicate case-insensitive paths: $path"
        }
    }

    if ($actualPaths.Count -ne $ExpectedPaths.Count -or
        -not $expectedCaseFolded.SetEquals($actualCaseFolded)) {
        throw "Release payload differs from its explicit inventory."
    }

    $actualDirectories = @(Get-ChildItem -LiteralPath $Root -Recurse -Directory -Force | ForEach-Object {
        Get-RelativeBundlePath $Root $_.FullName
    } | Sort-Object)
    $expectedDirectories = @(Get-ExpectedDirectories $ExpectedPaths)
    $expectedDirectorySet = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $expectedDirectories) {
        $null = $expectedDirectorySet.Add($path)
    }
    $actualDirectorySet = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $actualDirectories) {
        if (-not $actualDirectorySet.Add($path)) {
            throw "Release payload contains duplicate case-insensitive directories: $path"
        }
    }
    if ($actualDirectories.Count -ne $expectedDirectorySet.Count -or
        -not $expectedDirectorySet.SetEquals($actualDirectorySet)) {
        throw "Release payload contains directories outside its explicit allowlist."
    }
}

function Get-DeclaredWorkerPackFiles([string]$Root) {
    $catalogPath = Join-Path $Root 'worker-pack-catalog.json'
    $null = Assert-RegularFile $catalogPath
    try {
        $catalog = Get-Content -LiteralPath $catalogPath -Raw | ConvertFrom-Json
    }
    catch {
        throw "Worker-pack catalog is not valid JSON: $($_.Exception.Message)"
    }
    Assert-ExactObjectProperties $catalog @('schema_version', 'packs') 'Worker-pack catalog'
    if ($catalog.schema_version -ne 1) {
        throw 'Worker-pack catalog has an unsupported schema.'
    }
    $packs = @($catalog.packs)
    if ($packs.Count -gt 8) {
        throw 'Worker-pack catalog exceeds its release bound.'
    }
    $files = [System.Collections.Generic.List[string]]::new()
    $identities = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($pack in $packs) {
        Assert-ExactObjectProperties $pack @(
            'pack_id', 'pack_version', 'pack_digest', 'security_epoch',
            'runtime_abi_version', 'backend', 'provider', 'target_os',
            'target_arch', 'worker_relative_path', 'root',
            'installed_size_bytes', 'compressed_size_bytes', 'files'
        ) 'Worker-pack catalog entry'
        foreach ($identityField in @('pack_id', 'pack_version', 'provider')) {
            if ($pack.$identityField -isnot [string] -or
                [string]$pack.$identityField -cnotmatch '^[A-Za-z0-9._:-]{1,96}$') {
                throw "Worker-pack catalog has an invalid $identityField."
            }
        }
        if ($pack.pack_digest -isnot [string] -or
            [string]$pack.pack_digest -cnotmatch '^[0-9a-f]{64}$') {
            throw 'Worker-pack catalog has a non-canonical digest.'
        }
        $expectedRoot = "workers/packs/$($pack.pack_id)/$($pack.pack_version)/$($pack.pack_digest)"
        if ([string]$pack.root -cne $expectedRoot) {
            throw 'Worker-pack catalog root does not match the immutable layout.'
        }
        Assert-SafeRelativePayloadPath $expectedRoot
        $packFiles = @($pack.files)
        if ($packFiles.Count -lt 3 -or $packFiles.Count -gt 258) {
            throw 'Worker-pack catalog file count is outside its release bound.'
        }
        $packInstalledSize = [int64]0
        foreach ($file in $packFiles) {
            if ($file -isnot [string]) {
                throw 'Worker-pack catalog paths must be strings.'
            }
            Assert-SafeRelativePayloadPath $file
            if (-not $file.StartsWith($expectedRoot + '/', [System.StringComparison]::Ordinal)) {
                throw "Worker-pack catalog file escapes its immutable root: $file"
            }
            if (-not $identities.Add($file)) {
                throw "Worker-pack catalog contains a duplicate case-insensitive path: $file"
            }
            $item = Assert-RegularFile (Join-Path $Root ($file -replace '/', '\'))
            $packInstalledSize += [int64]$item.Length
            $files.Add($file)
        }
        if ([int64]$pack.installed_size_bytes -ne $packInstalledSize -or
            [int64]$pack.compressed_size_bytes -lt 0) {
            throw 'Worker-pack catalog size evidence is invalid.'
        }
        $packRoot = Join-Path $Root ($expectedRoot -replace '/', '\')
        $verification = Invoke-NativeProcess (Join-Path $Root 'local-transcriber.exe') @(
            '--scribe-verify-worker-pack', $packRoot
        )
        if ($verification.ExitCode -ne 0) {
            throw "Bundled worker pack failed compiled verification: $($verification.Stderr.Trim())"
        }
        try {
            $descriptor = $verification.Stdout | ConvertFrom-Json
        }
        catch {
            throw "Bundled worker-pack verifier returned invalid JSON: $($_.Exception.Message)"
        }
        foreach ($field in @(
            'pack_id', 'pack_version', 'pack_digest', 'security_epoch',
            'runtime_abi_version', 'backend', 'provider', 'target_os',
            'target_arch', 'worker_relative_path'
        )) {
            if ([string]$descriptor.$field -cne [string]$pack.$field) {
                throw "Bundled worker-pack descriptor differs at '$field'."
            }
        }
    }
    if ($files.Count -gt 1024 -or @(Get-ExpectedDirectories $files.ToArray()).Count -gt 900) {
        throw 'Worker-pack catalog exceeds the installer handle bound.'
    }
    return $files.ToArray()
}

function Assert-Bundle {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,
        [string[]]$AllowedAdditionalFiles = @(),
        $ExpectedModelManifest = $script:modelManifest,
        [string]$ExpectedModelManifestPath = $script:modelManifestPath,
        [object[]]$ExpectedLegalFiles = $script:legalFiles
    )

    $root = Get-NormalizedPath $Root
    $normalizedAllowedAdditionalFiles = @($AllowedAdditionalFiles | Sort-Object)
    $allowedCaseFolded = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $normalizedAllowedAdditionalFiles) {
        if ([string]::IsNullOrWhiteSpace($path) -or $path -match '[\\/]') {
            throw "Allowed additional release files must be root-level filenames."
        }
        if (-not $allowedCaseFolded.Add($path)) {
            throw "Allowed additional release files must not contain duplicate case-insensitive names."
        }
    }

    $declaredPackFiles = @(Get-DeclaredWorkerPackFiles $root)
    $script:expectedInventoryPaths = @($baseExpectedInventoryPaths) + $declaredPackFiles
    $script:expectedPortablePayloadPaths = @($script:expectedInventoryPaths) + @('bundle-inventory.json')
    $script:allowedPackExecutablePaths = @($declaredPackFiles | Where-Object {
        [System.IO.Path]::GetExtension($_) -in @('.exe', '.dll')
    })
    $allowedExecutables = @("local-transcriber.exe", "scribe-inference-worker.exe") + $script:allowedPackExecutablePaths + @($normalizedAllowedAdditionalFiles | Where-Object {
        [System.IO.Path]::GetExtension($_).Equals('.exe', [System.StringComparison]::OrdinalIgnoreCase)
    })
    $expectedPayloadPaths = @($expectedPortablePayloadPaths) + $normalizedAllowedAdditionalFiles
    Assert-ExactPayloadTree $root $expectedPayloadPaths $allowedExecutables

    $inventoryPath = Join-Path $root "bundle-inventory.json"
    $null = Assert-RegularFile $inventoryPath
    try {
        $inventory = Get-Content -LiteralPath $inventoryPath -Raw | ConvertFrom-Json
    }
    catch {
        throw "Bundle inventory is not valid JSON: $($_.Exception.Message)"
    }
    Assert-ExactObjectProperties $inventory @("schema_version", "platform_triple", "files") "Bundle inventory"
    if ($inventory.schema_version -ne 1 -or $inventory.platform_triple -cne $targetTriple) {
        throw "Bundle inventory has an unexpected schema or platform."
    }

    $inventoryEntries = @($inventory.files)
    if ($inventoryEntries.Count -ne $expectedInventoryPaths.Count) {
        throw "Bundle inventory does not contain the exact self-contained payload entry count."
    }
    $inventoryPaths = [System.Collections.Generic.List[string]]::new()
    $inventoryCaseFolded = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $inventoryEntries) {
        Assert-ExactObjectProperties $entry @("path", "size_bytes", "sha256") "Bundle inventory entry"
        if ($entry.path -isnot [string] -or $entry.sha256 -isnot [string]) {
            throw "Bundle inventory path and SHA-256 values must be strings."
        }
        Assert-SafeRelativePayloadPath $entry.path
        if (-not $inventoryCaseFolded.Add($entry.path)) {
            throw "Bundle inventory contains duplicate case-insensitive paths: $($entry.path)"
        }
        if ($entry.sha256 -cnotmatch '^[0-9a-f]{64}$') {
            throw "Bundle inventory contains a non-canonical SHA-256 for $($entry.path)."
        }
        try {
            $expectedSize = [int64]$entry.size_bytes
        }
        catch {
            throw "Bundle inventory contains an invalid size for $($entry.path)."
        }
        if ($expectedSize -lt 0) {
            throw "Bundle inventory contains a negative size for $($entry.path)."
        }
        $inventoryPaths.Add($entry.path)
        Assert-ExactFile (Join-Path $root ($entry.path -replace '/', '\')) $expectedSize $entry.sha256
    }
    $expectedInventoryCaseFolded = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $expectedInventoryPaths) {
        $null = $expectedInventoryCaseFolded.Add($path)
    }
    if ($inventoryPaths.Count -ne $expectedInventoryCaseFolded.Count -or
        -not $expectedInventoryCaseFolded.SetEquals($inventoryCaseFolded)) {
        throw "Bundle inventory paths differ from the canonical self-contained payload allowlist."
    }

    $sourceModelManifest = Assert-RegularFile $ExpectedModelManifestPath
    $sourceModelManifestHash = (Get-FileHash -LiteralPath $ExpectedModelManifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-ExactFile (Join-Path $root "bundled-model-manifest.json") $sourceModelManifest.Length $sourceModelManifestHash
    Assert-ExactFile `
        (Join-Path $root $ExpectedModelManifest.artifact_filename) `
        ([int64]$ExpectedModelManifest.size_bytes) `
        ([string]$ExpectedModelManifest.sha256).ToLowerInvariant()

    foreach ($legalFile in $ExpectedLegalFiles) {
        $sourcePath = Join-Path $repositoryRoot ($legalFile.Source -replace '/', '\')
        $sourceItem = Assert-RegularFile $sourcePath
        $sourceHash = (Get-FileHash -LiteralPath $sourcePath -Algorithm SHA256).Hash.ToLowerInvariant()
        Assert-ExactFile (Join-Path $root ($legalFile.Destination -replace '/', '\')) $sourceItem.Length $sourceHash
    }

    Assert-Amd64GuiPe (Join-Path $root "local-transcriber.exe")
    $null = Assert-ReviewedWindowsPe (Join-Path $root "local-transcriber.exe")
    $null = Assert-ReviewedWindowsPe (Join-Path $root "scribe-inference-worker.exe") 3
}

function Assert-PayloadParity([string]$ReferenceRoot, [string]$CandidateRoot, [string]$Description) {
    foreach ($relativePath in $expectedPortablePayloadPaths) {
        $referencePath = Join-Path $ReferenceRoot ($relativePath -replace '/', '\')
        $candidatePath = Join-Path $CandidateRoot ($relativePath -replace '/', '\')
        $referenceItem = Assert-RegularFile $referencePath
        $referenceHash = (Get-FileHash -LiteralPath $referencePath -Algorithm SHA256).Hash.ToLowerInvariant()
        try {
            Assert-ExactFile $candidatePath $referenceItem.Length $referenceHash
        }
        catch {
            throw "$Description payload parity mismatch for $relativePath`: $($_.Exception.Message)"
        }
    }
}

function Assert-SafePortableZip([string]$Path) {
    $null = Assert-RegularFile $Path
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead((Get-NormalizedPath $Path))
    try {
        $expectedDirectories = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
        foreach ($directory in @(Get-ExpectedDirectories $expectedPortablePayloadPaths)) {
            $null = $expectedDirectories.Add($directory)
        }
        $filePaths = [System.Collections.Generic.List[string]]::new()
        $entryIdentities = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
        foreach ($entry in $archive.Entries) {
            $rawPath = $entry.FullName
            if ($rawPath.Contains('\')) {
                throw "Portable ZIP contains a non-canonical backslash path: $rawPath"
            }
            $isDirectory = $rawPath.EndsWith('/', [System.StringComparison]::Ordinal)
            $path = if ($isDirectory) { $rawPath.TrimEnd('/') } else { $rawPath }
            Assert-SafeRelativePayloadPath $path
            if (-not $entryIdentities.Add($path)) {
                throw "Portable ZIP contains duplicate case-insensitive entries: $path"
            }
            $windowsAttributes = $entry.ExternalAttributes -band 0xFFFF
            $unixType = ($entry.ExternalAttributes -shr 16) -band 0xF000
            if (($windowsAttributes -band [int][System.IO.FileAttributes]::ReparsePoint) -ne 0 -or $unixType -eq 0xA000) {
                throw "Portable ZIP contains a symbolic link or reparse entry: $path"
            }
            if ($isDirectory) {
                if (-not $expectedDirectories.Contains($path)) {
                    throw "Portable ZIP contains an unexpected directory entry: $path"
                }
            }
            else {
                Assert-AllowedPayloadFile $path (@('local-transcriber.exe', 'scribe-inference-worker.exe') + $script:allowedPackExecutablePaths)
                $filePaths.Add($path)
            }
        }
        $expectedFiles = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
        foreach ($expectedFile in $expectedPortablePayloadPaths) {
            $null = $expectedFiles.Add($expectedFile)
        }
        if ($filePaths.Count -ne $expectedFiles.Count -or
            -not $expectedFiles.SetEquals($filePaths)) {
            throw "Portable ZIP entries differ from the canonical self-contained payload allowlist."
        }
    }
    finally {
        $archive.Dispose()
    }
}

function Test-TemporaryCleanupSharingViolation([System.Exception]$Exception) {
    $current = $Exception
    while ($null -ne $current) {
        $nativeErrorCode = ([int64]$current.HResult) -band 0xFFFF
        if ($nativeErrorCode -in @(32, 33)) {
            return $true
        }
        $current = $current.InnerException
    }
    return $false
}

function Remove-ValidatedTemporaryRoot(
    [string]$Path,
    [ValidateRange(1, 20)]
    [int]$MaximumAttempts = 6,
    [ValidateRange(1, 5000)]
    [int]$MaximumRetryMilliseconds = 1000
) {
    $cleanupStopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $attempt = 0
    while ($true) {
        $tempRoot = Get-NormalizedPath ([System.IO.Path]::GetTempPath())
        $resolved = Get-NormalizedPath $Path
        if (-not [string]::Equals((Split-Path -Parent $resolved), $tempRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
            (Split-Path -Leaf $resolved) -cnotmatch '^scribe-release-(?:verification|stable-test|shell-test|package-verifier)-[0-9a-f]{32}$') {
            throw "Refused release verification cleanup outside its bounded temporary directory."
        }
        if (-not (Test-Path -LiteralPath $resolved)) {
            return
        }
        Assert-NoReparseAncestors $resolved
        Assert-TreeHasNoReparsePoints $resolved
        try {
            Remove-Item -LiteralPath $resolved -Recurse -Force
            return
        }
        catch {
            $attempt += 1
            if (-not (Test-TemporaryCleanupSharingViolation $_.Exception) -or
                $attempt -ge $MaximumAttempts -or
                $cleanupStopwatch.ElapsedMilliseconds -ge $MaximumRetryMilliseconds) {
                throw
            }
            $remainingMilliseconds = $MaximumRetryMilliseconds - [int]$cleanupStopwatch.ElapsedMilliseconds
            if ($remainingMilliseconds -le 0) {
                throw
            }
            $backoffMilliseconds = [Math]::Min(250, 50 * [Math]::Pow(2, $attempt - 1))
            Start-Sleep -Milliseconds ([int][Math]::Min($backoffMilliseconds, $remainingMilliseconds))
        }
    }
}

function New-TestShellFixture([string]$Token) {
    if ($Token -cnotmatch '^[0-9a-f]{32}$') {
        throw "Test shell fixture requires an exact lower-case token."
    }
    $root = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-release-shell-test-$Token"
    if (Test-Path -LiteralPath $root) {
        throw "Token-bound test shell root unexpectedly already exists: $root"
    }
    Assert-NoReparseAncestors $root
    $startMenu = Join-Path $root "StartMenu"
    $desktop = Join-Path $root "Desktop"
    New-Item -ItemType Directory -Path $startMenu | Out-Null
    New-Item -ItemType Directory -Path $desktop | Out-Null
    [System.IO.File]::WriteAllBytes((Join-Path $startMenu "Scribe.lnk"), [byte[]](83, 84, 65, 82, 84))
    [System.IO.File]::WriteAllBytes((Join-Path $desktop "Scribe.lnk"), [byte[]](68, 69, 83, 75))
    [System.IO.File]::WriteAllBytes((Join-Path $root "run-sentinel.exe"), [byte[]](82, 85, 78))
    [System.IO.File]::WriteAllBytes((Join-Path $root "task-sentinel.txt"), [byte[]](84, 65, 83, 75))
    return Get-NormalizedPath $root
}

function Get-ExactTreeSnapshot([string]$Root) {
    $resolved = Get-NormalizedPath $Root
    Assert-NoReparseAncestors $resolved
    Assert-TreeHasNoReparsePoints $resolved
    $entries = [System.Collections.Generic.List[object]]::new()
    foreach ($item in @((Get-Item -LiteralPath $resolved -Force)) +
        @(Get-ChildItem -LiteralPath $resolved -Recurse -Force | Sort-Object FullName)) {
        $relativePath = if ($item.FullName -ceq $resolved) {
            "."
        }
        else {
            Get-RelativeBundlePath $resolved $item.FullName
        }
        $isDirectory = $item -is [System.IO.DirectoryInfo]
        $hash = if ($isDirectory) {
            ""
        }
        else {
            (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        $streams = @(Get-Item -LiteralPath $item.FullName -Stream * -ErrorAction Stop | ForEach-Object {
            $streamHash = if ($_.Stream -ceq ':$DATA') {
                $hash
            }
            else {
                $streamBytes = Get-Content -LiteralPath $item.FullName -Stream $_.Stream -AsByteStream -Raw
                [Convert]::ToHexString(
                    [System.Security.Cryptography.SHA256]::HashData([byte[]]$streamBytes)
                ).ToLowerInvariant()
            }
            [pscustomobject]@{
                Name = [string]$_.Stream
                Length = [int64]$_.Length
                Sha256 = $streamHash
            }
        } | Sort-Object Name)
        $entries.Add([pscustomobject]@{
            Path = $relativePath
            Directory = $isDirectory
            Length = if ($isDirectory) { [int64]0 } else { [int64]$item.Length }
            Sha256 = $hash
            Attributes = [int]$item.Attributes
            CreationTimeUtcTicks = $item.CreationTimeUtc.Ticks
            LastWriteTimeUtcTicks = $item.LastWriteTimeUtc.Ticks
            Streams = $streams
        })
    }
    return ($entries | ConvertTo-Json -Depth 6 -Compress)
}

function Assert-ExactTreeSnapshot(
    [string]$Root,
    [string]$ExpectedSnapshot,
    [string]$Description
) {
    $actual = Get-ExactTreeSnapshot $Root
    if ($actual -cne $ExpectedSnapshot) {
        throw "$Description mutated controlled paths, bytes, metadata, entries, or streams."
    }
}

function Assert-IsolatedInstallerLog([string]$Path, [string]$Description) {
    $null = Assert-RegularFile $Path
    $content = Get-Content -LiteralPath $Path -Raw
    foreach ($forbiddenPattern in @(
        '(?im)-- Icon entry --',
        '(?im)-- Run entry --',
        '(?im)Selected tasks:.*desktopicon',
        '(?im)Creating new uninstall key:',
        '(?im)Updating existing uninstall key:'
    )) {
        if ($content -match $forbiddenPattern) {
            throw "$Description performed forbidden shell, task, run, or uninstall-registration integration."
        }
    }
}

function Invoke-IsolatedInstallerProcess(
    [string]$Installer,
    [string[]]$Arguments,
    [string]$LogPath,
    [string]$ShellRoot,
    [string]$ShellSnapshot,
    [string]$Description
) {
    $process = Invoke-NativeProcess $Installer @($Arguments + "/LOG=$LogPath")
    Assert-IsolatedInstallerLog $LogPath $Description
    Assert-ExactTreeSnapshot $ShellRoot $ShellSnapshot $Description
    return $process
}

function Invoke-ExpectedInstallerRefusal(
    [string]$Installer,
    [string[]]$Arguments,
    [string]$LogPath,
    [string]$ShellRoot,
    [string]$ShellSnapshot,
    [string]$InstallRoot,
    [string]$Description
) {
    $before = Get-ExactTreeSnapshot $InstallRoot
    $process = Invoke-IsolatedInstallerProcess `
        $Installer $Arguments $LogPath $ShellRoot $ShellSnapshot $Description
    if ($process.ExitCode -eq 0) {
        throw "$Description was accepted instead of being refused fail-closed."
    }
    Assert-ExactTreeSnapshot $InstallRoot $before $Description
}

function Invoke-ProtectedRenameRace(
    [string]$Installer,
    [string[]]$Arguments,
    [string]$LogPath,
    [string]$ShellRoot,
    [string]$ShellSnapshot,
    [string]$TestContainer,
    [string]$TargetPath,
    [string]$Description
) {
    $readyPath = Join-Path $TestContainer "preflight-ready"
    $continuePath = Join-Path $TestContainer "preflight-continue"
    foreach ($marker in @($readyPath, $continuePath)) {
        if (Test-Path -LiteralPath $marker) {
            Remove-Item -LiteralPath $marker -Force
        }
    }
    $argumentsWithLog = @($Arguments + "/SCRIBETESTPAUSE=1" + "/LOG=$LogPath")
    $process = Start-Process -FilePath $Installer -ArgumentList $argumentsWithLog -PassThru
    $swappedPath = "$TargetPath-swapped"
    $renameSucceeded = $false
    try {
        $deadline = [DateTime]::UtcNow.AddSeconds(60)
        while (-not (Test-Path -LiteralPath $readyPath)) {
            if ($process.HasExited) {
                throw "$Description installer exited before its handle-bound preflight marker."
            }
            if ([DateTime]::UtcNow -ge $deadline) {
                throw "$Description timed out waiting for its handle-bound preflight marker."
            }
            Start-Sleep -Milliseconds 50
        }
        try {
            if (Test-Path -LiteralPath $TargetPath -PathType Container) {
                [System.IO.Directory]::Move($TargetPath, $swappedPath)
            }
            else {
                [System.IO.File]::Move($TargetPath, $swappedPath)
            }
            $renameSucceeded = $true
        }
        catch [System.IO.IOException] {
            $renameSucceeded = $false
        }
        catch [System.UnauthorizedAccessException] {
            $renameSucceeded = $false
        }
        if ($renameSucceeded) {
            if (Test-Path -LiteralPath $swappedPath -PathType Container) {
                [System.IO.Directory]::Move($swappedPath, $TargetPath)
            }
            else {
                [System.IO.File]::Move($swappedPath, $TargetPath)
            }
        }
    }
    finally {
        [System.IO.File]::WriteAllText($continuePath, "continue")
        $process.WaitForExit()
        $exitCode = $process.ExitCode
        $process.Dispose()
    }
    Assert-IsolatedInstallerLog $LogPath $Description
    Assert-ExactTreeSnapshot $ShellRoot $ShellSnapshot $Description
    if ($renameSucceeded) {
        throw "$Description was vulnerable to a rename/swap after preflight."
    }
    if ($exitCode -ne 0) {
        throw "$Description failed after the protected rename was correctly denied (exit $exitCode)."
    }
}

function Invoke-ReparseRefusalFixture(
    [string]$Installer,
    [string]$HarnessRoot,
    [ValidateSet("root", "child")]
    [string]$Kind
) {
    $token = [guid]::NewGuid().ToString('N')
    $container = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-release-stable-test-$token"
    $installRoot = Join-Path $container "installed"
    $targetRoot = Join-Path $container "fixture-target"
    $linkPath = if ($Kind -ceq "root") { $installRoot } else { Join-Path $installRoot "licenses" }
    $shellRoot = $null
    Assert-NoReparseAncestors $container
    New-Item -ItemType Directory -Path $container | Out-Null
    New-Item -ItemType Directory -Path $targetRoot | Out-Null
    [System.IO.File]::WriteAllBytes((Join-Path $targetRoot "sentinel.bin"), [byte[]](82, 69, 80, 65, 82, 83, 69))
    if ($Kind -ceq "child") {
        New-Item -ItemType Directory -Path $installRoot | Out-Null
    }
    try {
        $null = New-Item -ItemType Junction -Path $linkPath -Target $targetRoot
        $linkItem = Get-Item -LiteralPath $linkPath -Force
        if (($linkItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0) {
            throw "Could not create the bounded $Kind reparse fixture."
        }
        $targetSnapshot = Get-ExactTreeSnapshot $targetRoot
        $shellRoot = New-TestShellFixture $token
        $shellSnapshot = Get-ExactTreeSnapshot $shellRoot
        $logPath = Join-Path $HarnessRoot "stable-$Kind-reparse.log"
        $arguments = @(
            "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-",
            "/SCRIBESTABLETEST=$token"
        )
        $process = Invoke-IsolatedInstallerProcess `
            $Installer $arguments $logPath $shellRoot $shellSnapshot `
            "Stable $Kind reparse fixture"
        if ($process.ExitCode -eq 0) {
            throw "Stable $Kind reparse fixture was accepted instead of refused fail-closed."
        }
        Assert-ExactTreeSnapshot $targetRoot $targetSnapshot "Stable $Kind reparse fixture target"
        $postLink = Get-Item -LiteralPath $linkPath -Force
        if (($postLink.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0) {
            throw "Stable $Kind reparse fixture link was replaced or removed."
        }
        $rootEntries = @(Get-ChildItem -LiteralPath $installRoot -Force)
        if ($Kind -ceq "child" -and
            ($rootEntries.Count -ne 1 -or $rootEntries[0].Name -cne "licenses")) {
            throw "Stable child reparse refusal created or removed install-root entries."
        }
    }
    finally {
        if (Test-Path -LiteralPath $linkPath) {
            $linkItem = Get-Item -LiteralPath $linkPath -Force
            if (($linkItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0) {
                throw "Refused to clean a reparse fixture whose exact link identity changed: $linkPath"
            }
            Remove-Item -LiteralPath $linkPath -Force
        }
        if ($null -ne $shellRoot) {
            Remove-ValidatedTemporaryRoot $shellRoot
        }
        Remove-ValidatedTemporaryRoot $container
    }
}

# A fresh Inno Setup installation writes these two default-named uninstaller files
# into {app}. They are installer metadata, not part of the portable release tree.
$InnoSetupUninstallerArtifacts = @("unins000.exe", "unins000.dat")

$bundle = Get-NormalizedPath $BundlePath
$null = Assert-Bundle $bundle

$harnessToken = [guid]::NewGuid().ToString('N')
$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-release-package-verifier-$harnessToken"
$verificationToken = [guid]::NewGuid().ToString('N')
$verificationRoot = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-release-verification-$verificationToken"
$verificationUninstaller = $null
$stableUninstaller = $null
$stableContainer = $null
$caseCollisionContainer = $null
$stableShellRoot = $null
$evidenceRoot = $null
Assert-NoReparseAncestors $temporaryRoot
Assert-NoReparseAncestors $verificationRoot
if (Test-Path -LiteralPath $verificationRoot) {
    throw "Unique token-bound verification root unexpectedly already exists."
}
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
if ($EvidenceDirectory) {
    $evidenceRoot = Get-NormalizedPath $EvidenceDirectory
    $expectedEvidenceRoot = Get-NormalizedPath (Join-Path $repositoryRoot "dist\installer-verification-logs")
    if (-not [string]::Equals($evidenceRoot, $expectedEvidenceRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Installer verification evidence must use the repository's exact dist/installer-verification-logs directory."
    }
    if (Test-Path -LiteralPath $evidenceRoot) {
        throw "Installer verification evidence directory must not already exist."
    }
    Assert-NoReparseAncestors $evidenceRoot
    New-Item -ItemType Directory -Path $evidenceRoot | Out-Null
}
$verificationShellRoot = New-TestShellFixture $verificationToken
$verificationShellSnapshot = Get-ExactTreeSnapshot $verificationShellRoot
try {
    if ($PortableZipPath) {
        $portableZip = Get-NormalizedPath $PortableZipPath
        Assert-SafePortableZip $portableZip
        $zipRoot = Join-Path $temporaryRoot "portable"
        Expand-Archive -LiteralPath $portableZip -DestinationPath $zipRoot
        $null = Assert-Bundle $zipRoot
        Assert-PayloadParity $bundle $zipRoot "Portable ZIP"
    }

    if ($InstallerPath) {
        $installer = Get-NormalizedPath $InstallerPath
        $null = Assert-RegularFile $installer
        $installedRoot = Join-Path $verificationRoot "installed"
        $escapedVerificationRoot = Join-Path $temporaryRoot "escaped"
        $escapedLog = Join-Path $temporaryRoot "verification-escape.log"
        $escapedProcess = Invoke-NativeProcess $installer @(
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/SP-",
            "/DIR=$escapedVerificationRoot",
            "/SCRIBEVERIFY=$verificationToken",
            "/LOG=$escapedLog"
        )
        if ($escapedProcess.ExitCode -eq 0) {
            throw "Installer verification token accepted an override outside its derived temporary destination."
        }
        if (Test-Path -LiteralPath $escapedVerificationRoot) {
            throw "Rejected installer verification destination was created or mutated."
        }
        Assert-IsolatedInstallerLog $escapedLog "Rejected installer verification"
        Assert-ExactTreeSnapshot $verificationShellRoot $verificationShellSnapshot "Rejected installer verification"
        $verificationLog = Join-Path $temporaryRoot "verification-install.log"
        $installerProcess = Invoke-NativeProcess $installer @(
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/SP-",
            "/SCRIBEVERIFY=$verificationToken",
            "/LOG=$verificationLog"
        )
        if ($installerProcess.ExitCode -ne 0) {
            throw "Silent installer verification failed with exit code $($installerProcess.ExitCode): $($installerProcess.Stderr.Trim())"
        }
        Assert-IsolatedInstallerLog $verificationLog "Installer verification"
        Assert-ExactTreeSnapshot $verificationShellRoot $verificationShellSnapshot "Installer verification"
        $verificationUninstaller = Join-Path $installedRoot "unins000.exe"
        $null = Assert-Bundle -Root $installedRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts
        Assert-PayloadParity $bundle $installedRoot "Installed"

        $verificationUninstallLog = Join-Path $temporaryRoot "verification-uninstall.log"
        $uninstallProcess = Invoke-NativeProcess $verificationUninstaller @(
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/LOG=$verificationUninstallLog"
        )
        if ($uninstallProcess.ExitCode -ne 0) {
            throw "Installer verification cleanup failed with exit code $($uninstallProcess.ExitCode): $($uninstallProcess.Stderr.Trim())"
        }
        $verificationUninstaller = $null
        Assert-ExactTreeSnapshot $verificationShellRoot $verificationShellSnapshot "Installer verification uninstaller"

        if ($ExerciseStableUpgrade) {
            $stableToken = [guid]::NewGuid().ToString('N')
            $stableContainer = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-release-stable-test-$stableToken"
            $stableRoot = Join-Path $stableContainer "installed"
            Assert-NoReparseAncestors $stableContainer
            New-Item -ItemType Directory -Path $stableContainer | Out-Null
            New-Item -ItemType Directory -Path $stableRoot | Out-Null
            $stableShellRoot = New-TestShellFixture $stableToken
            $stableShellSnapshot = Get-ExactTreeSnapshot $stableShellRoot

            $stableInstallArguments = @(
                "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-",
                "/SCRIBESTABLETEST=$stableToken"
            )
            $stableInstall = Invoke-IsolatedInstallerProcess `
                $installer $stableInstallArguments `
                (Join-Path $temporaryRoot "stable-install.log") `
                $stableShellRoot $stableShellSnapshot "Initial stable-test install"
            if ($stableInstall.ExitCode -ne 0) {
                throw "Initial isolated stable installation failed with exit code $($stableInstall.ExitCode): $($stableInstall.Stderr.Trim())"
            }
            $stableUninstaller = Join-Path $stableRoot "unins000.exe"
            $null = Assert-Bundle -Root $stableRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts
            Assert-PayloadParity $bundle $stableRoot "Initial stable install"

            $stableUpgrade = Invoke-IsolatedInstallerProcess `
                $installer $stableInstallArguments `
                (Join-Path $temporaryRoot "stable-upgrade.log") `
                $stableShellRoot $stableShellSnapshot "Canonical stable-test upgrade"
            if ($stableUpgrade.ExitCode -ne 0) {
                throw "Canonical stable upgrade failed with exit code $($stableUpgrade.ExitCode): $($stableUpgrade.Stderr.Trim())"
            }
            $null = Assert-Bundle -Root $stableRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts
            Assert-PayloadParity $bundle $stableRoot "Stable upgrade"

            $caseCollisionToken = [guid]::NewGuid().ToString('N')
            $caseCollisionContainer = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-release-stable-test-$caseCollisionToken"
            $caseCollisionRoot = Join-Path $caseCollisionContainer "installed"
            Assert-NoReparseAncestors $caseCollisionContainer
            New-Item -ItemType Directory -Path $caseCollisionRoot | Out-Null
            $fsutil = Join-Path $env:SystemRoot "System32\fsutil.exe"
            $caseSensitiveProcess = Invoke-NativeProcess $fsutil @(
                "file", "setCaseSensitiveInfo", $caseCollisionRoot, "enable"
            )
            if ($caseSensitiveProcess.ExitCode -ne 0) {
                throw "Could not enable the isolated case-collision fixture: $($caseSensitiveProcess.Stderr.Trim())"
            }
            foreach ($item in @(Get-ChildItem -LiteralPath $stableRoot -Force)) {
                Copy-Item -LiteralPath $item.FullName -Destination (Join-Path $caseCollisionRoot $item.Name) -Recurse -Force
            }
            $null = Assert-Bundle -Root $caseCollisionRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts
            Assert-PayloadParity $bundle $caseCollisionRoot "Case-collision fixture"

            $canonicalReadme = Join-Path $caseCollisionRoot "README.txt"
            $caseCollision = Join-Path $caseCollisionRoot "readme.txt"
            Copy-Item -LiteralPath $canonicalReadme -Destination $caseCollision
            if (@(Get-ChildItem -LiteralPath $caseCollisionRoot -File | Where-Object { $_.Name -ieq "README.txt" }).Count -ne 2) {
                throw "Could not create the isolated case-collision fixture."
            }
            $caseCollisionInstallArguments = @(
                "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-",
                "/SCRIBESTABLETEST=$caseCollisionToken"
            )
            Invoke-ExpectedInstallerRefusal `
                $installer $caseCollisionInstallArguments `
                (Join-Path $temporaryRoot "stable-case-collision.log") `
                $stableShellRoot $stableShellSnapshot $caseCollisionRoot `
                "Stable case-insensitive path collision"
            Remove-Item -LiteralPath $caseCollision

            $legacyDirectory = Join-Path $stableRoot "runtimes"
            New-Item -ItemType Directory -Path $legacyDirectory | Out-Null
            $legacyFile = Join-Path $legacyDirectory "whisper.dll"
            [System.IO.File]::WriteAllBytes($legacyFile, [byte[]](1, 3, 3, 7))
            Invoke-ExpectedInstallerRefusal `
                $installer $stableInstallArguments `
                (Join-Path $temporaryRoot "stable-legacy-runtime.log") `
                $stableShellRoot $stableShellSnapshot $stableRoot `
                "Stable unexpected legacy runtime tree"
            Remove-Item -LiteralPath $legacyDirectory -Recurse -Force

            $unexpectedFile = Join-Path $stableRoot "unexpected.txt"
            [System.IO.File]::WriteAllBytes($unexpectedFile, [byte[]](85, 78, 75, 78, 79, 87, 78))
            Invoke-ExpectedInstallerRefusal `
                $installer $stableInstallArguments `
                (Join-Path $temporaryRoot "stable-unexpected-file.log") `
                $stableShellRoot $stableShellSnapshot $stableRoot `
                "Stable unexpected file"
            Remove-Item -LiteralPath $unexpectedFile -Force

            $canonicalReadme = Join-Path $stableRoot "README.txt"
            $originalReadmeAttributes = (Get-Item -LiteralPath $canonicalReadme -Force).Attributes
            Set-ItemProperty -LiteralPath $canonicalReadme -Name IsReadOnly -Value $true
            Invoke-ExpectedInstallerRefusal `
                $installer $stableInstallArguments `
                (Join-Path $temporaryRoot "stable-readonly.log") `
                $stableShellRoot $stableShellSnapshot $stableRoot `
                "Stable read-only payload file"
            (Get-Item -LiteralPath $canonicalReadme -Force).Attributes = $originalReadmeAttributes

            [System.IO.File]::WriteAllBytes("$canonicalReadme`:scribe-test", [byte[]](65, 68, 83))
            Invoke-ExpectedInstallerRefusal `
                $installer $stableInstallArguments `
                (Join-Path $temporaryRoot "stable-file-ads.log") `
                $stableShellRoot $stableShellSnapshot $stableRoot `
                "Stable payload file with alternate data stream"
            [System.IO.File]::Delete("$canonicalReadme`:scribe-test")

            $licensesRoot = Join-Path $stableRoot "licenses"
            [System.IO.File]::WriteAllBytes("$licensesRoot`:scribe-test", [byte[]](65, 68, 83))
            Invoke-ExpectedInstallerRefusal `
                $installer $stableInstallArguments `
                (Join-Path $temporaryRoot "stable-directory-ads.log") `
                $stableShellRoot $stableShellSnapshot $stableRoot `
                "Stable payload directory with alternate data stream"
            [System.IO.File]::Delete("$licensesRoot`:scribe-test")

            $sharingSnapshot = Get-ExactTreeSnapshot $stableRoot
            $exclusiveHandle = [System.IO.File]::Open(
                $canonicalReadme,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::Read,
                [System.IO.FileShare]::None
            )
            try {
                $sharingInstall = Invoke-IsolatedInstallerProcess `
                    $installer $stableInstallArguments `
                    (Join-Path $temporaryRoot "stable-sharing.log") `
                    $stableShellRoot $stableShellSnapshot "Stable incompatible file sharing"
                if ($sharingInstall.ExitCode -eq 0) {
                    throw "Stable incompatible file sharing was accepted instead of refused fail-closed."
                }
            }
            finally {
                $exclusiveHandle.Dispose()
            }
            Assert-ExactTreeSnapshot $stableRoot $sharingSnapshot "Stable incompatible file sharing"

            $currentIdentity = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
            $enumerationSnapshot = Get-ExactTreeSnapshot $stableRoot
            $originalLicensesAcl = Get-Acl -LiteralPath $licensesRoot
            $deniedLicensesAcl = Get-Acl -LiteralPath $licensesRoot
            $denyEnumeration = [System.Security.AccessControl.FileSystemAccessRule]::new(
                $currentIdentity,
                [System.Security.AccessControl.FileSystemRights]::ListDirectory,
                [System.Security.AccessControl.InheritanceFlags]::None,
                [System.Security.AccessControl.PropagationFlags]::None,
                [System.Security.AccessControl.AccessControlType]::Deny
            )
            $null = $deniedLicensesAcl.AddAccessRule($denyEnumeration)
            Set-Acl -LiteralPath $licensesRoot -AclObject $deniedLicensesAcl
            try {
                $enumerationInstall = Invoke-IsolatedInstallerProcess `
                    $installer $stableInstallArguments `
                    (Join-Path $temporaryRoot "stable-enumeration-access-denied.log") `
                    $stableShellRoot $stableShellSnapshot "Stable access-denied enumeration"
                if ($enumerationInstall.ExitCode -eq 0) {
                    throw "Stable access-denied enumeration was accepted instead of refused fail-closed."
                }
            }
            finally {
                Set-Acl -LiteralPath $licensesRoot -AclObject $originalLicensesAcl
            }
            Assert-ExactTreeSnapshot $stableRoot $enumerationSnapshot "Stable access-denied enumeration"

            $updateSnapshot = Get-ExactTreeSnapshot $stableRoot
            $originalReadmeAcl = Get-Acl -LiteralPath $canonicalReadme
            $deniedReadmeAcl = Get-Acl -LiteralPath $canonicalReadme
            $denyUpdate = [System.Security.AccessControl.FileSystemAccessRule]::new(
                $currentIdentity,
                [System.Security.AccessControl.FileSystemRights]::WriteData,
                [System.Security.AccessControl.InheritanceFlags]::None,
                [System.Security.AccessControl.PropagationFlags]::None,
                [System.Security.AccessControl.AccessControlType]::Deny
            )
            $null = $deniedReadmeAcl.AddAccessRule($denyUpdate)
            Set-Acl -LiteralPath $canonicalReadme -AclObject $deniedReadmeAcl
            try {
                $updateInstall = Invoke-IsolatedInstallerProcess `
                    $installer $stableInstallArguments `
                    (Join-Path $temporaryRoot "stable-update-access-denied.log") `
                    $stableShellRoot $stableShellSnapshot "Stable access-denied update"
                if ($updateInstall.ExitCode -eq 0) {
                    throw "Stable access-denied update was accepted instead of refused fail-closed."
                }
            }
            finally {
                Set-Acl -LiteralPath $canonicalReadme -AclObject $originalReadmeAcl
            }
            Assert-ExactTreeSnapshot $stableRoot $updateSnapshot "Stable access-denied update"

            Invoke-ProtectedRenameRace `
                $installer $stableInstallArguments `
                (Join-Path $temporaryRoot "stable-root-rename-race.log") `
                $stableShellRoot $stableShellSnapshot $stableContainer $stableRoot `
                "Stable root rename race"
            $null = Assert-Bundle -Root $stableRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts
            Assert-PayloadParity $bundle $stableRoot "Stable root rename race"

            Invoke-ProtectedRenameRace `
                $installer $stableInstallArguments `
                (Join-Path $temporaryRoot "stable-child-rename-race.log") `
                $stableShellRoot $stableShellSnapshot $stableContainer $licensesRoot `
                "Stable child-directory rename race"
            $null = Assert-Bundle -Root $stableRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts
            Assert-PayloadParity $bundle $stableRoot "Stable child-directory rename race"

            Invoke-ProtectedRenameRace `
                $installer $stableInstallArguments `
                (Join-Path $temporaryRoot "stable-file-rename-race.log") `
                $stableShellRoot $stableShellSnapshot $stableContainer $canonicalReadme `
                "Stable file rename race"
            $null = Assert-Bundle -Root $stableRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts
            Assert-PayloadParity $bundle $stableRoot "Stable file rename race"

            $stableUninstall = Invoke-NativeProcess $stableUninstaller @(
                "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART",
                "/LOG=$(Join-Path $temporaryRoot 'stable-uninstall.log')"
            )
            if ($stableUninstall.ExitCode -ne 0) {
                throw "Stable-upgrade fixture cleanup failed with exit code $($stableUninstall.ExitCode): $($stableUninstall.Stderr.Trim())"
            }
            $stableUninstaller = $null
            Assert-ExactTreeSnapshot $stableShellRoot $stableShellSnapshot "Stable-test uninstaller"

            Invoke-ReparseRefusalFixture $installer $temporaryRoot "root"
            Invoke-ReparseRefusalFixture $installer $temporaryRoot "child"
        }
    }
}
finally {
    if ($null -ne $stableUninstaller -and (Test-Path -LiteralPath $stableUninstaller -PathType Leaf)) {
        try {
            $stableCleanup = Invoke-NativeProcess $stableUninstaller @(
                "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART",
                "/LOG=$(Join-Path $temporaryRoot 'stable-emergency-uninstall.log')"
            )
            if ($stableCleanup.ExitCode -ne 0) {
                Write-Warning "Stable-upgrade fixture uninstaller cleanup returned exit $($stableCleanup.ExitCode)."
            }
        }
        catch {
            Write-Warning "Stable-upgrade fixture uninstaller cleanup failed: $($_.Exception.Message)"
        }
    }
    if ($null -ne $verificationUninstaller -and (Test-Path -LiteralPath $verificationUninstaller -PathType Leaf)) {
        try {
            $cleanupProcess = Invoke-NativeProcess $verificationUninstaller @(
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/LOG=$(Join-Path $temporaryRoot 'verification-emergency-uninstall.log')"
            )
            if ($cleanupProcess.ExitCode -ne 0) {
                Write-Warning "Installer verification uninstaller cleanup returned exit $($cleanupProcess.ExitCode)."
            }
        }
        catch {
            Write-Warning "Installer verification uninstaller cleanup failed: $($_.Exception.Message)"
        }
    }
    if ($null -ne $evidenceRoot -and (Test-Path -LiteralPath $temporaryRoot -PathType Container)) {
        foreach ($log in @(Get-ChildItem -LiteralPath $temporaryRoot -Filter "*.log" -File)) {
            $null = Assert-RegularFile $log.FullName
            Copy-Item -LiteralPath $log.FullName -Destination (Join-Path $evidenceRoot $log.Name)
        }
    }
    if ($null -ne $stableShellRoot) {
        Remove-ValidatedTemporaryRoot $stableShellRoot
    }
    if ($null -ne $caseCollisionContainer) {
        Remove-ValidatedTemporaryRoot $caseCollisionContainer
    }
    if ($null -ne $stableContainer) {
        Remove-ValidatedTemporaryRoot $stableContainer
    }
    Remove-ValidatedTemporaryRoot $verificationShellRoot
    Remove-ValidatedTemporaryRoot $verificationRoot
    Remove-ValidatedTemporaryRoot $temporaryRoot
}

Write-Output "Windows release payload allowlist, inventory, and parity verification passed."
