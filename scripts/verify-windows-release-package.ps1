param(
    [Parameter(Mandatory = $true)]
    [string]$BundlePath,
    [string]$PortableZipPath,
    [string]$InstallerPath,
    [switch]$ExerciseStableUpgrade
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
$expectedInventoryPaths = @(
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
    "whisper-base.en-Q8_0.gguf"
)
$expectedPortablePayloadPaths = @($expectedInventoryPaths) + @("bundle-inventory.json")

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
    [string[]]$AllowedExecutablePaths = @("local-transcriber.exe")
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
    if ($extension -in @('.dll', '.exe') -and $RelativePath -cnotin $AllowedExecutablePaths) {
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
    $directories = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
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

    $expectedSorted = @($ExpectedPaths | Sort-Object)
    if ($actualPaths.Count -ne $expectedSorted.Count -or
        (Compare-Object -ReferenceObject $expectedSorted -DifferenceObject $actualPaths -CaseSensitive)) {
        throw "Release payload differs from its explicit inventory."
    }

    $actualDirectories = @(Get-ChildItem -LiteralPath $Root -Recurse -Directory -Force | ForEach-Object {
        Get-RelativeBundlePath $Root $_.FullName
    } | Sort-Object)
    $expectedDirectories = @(Get-ExpectedDirectories $ExpectedPaths)
    if ($actualDirectories.Count -ne $expectedDirectories.Count -or
        (Compare-Object -ReferenceObject $expectedDirectories -DifferenceObject $actualDirectories -CaseSensitive)) {
        throw "Release payload contains directories outside its explicit allowlist."
    }
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

    $allowedExecutables = @("local-transcriber.exe") + @($normalizedAllowedAdditionalFiles | Where-Object {
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
    $expectedInventorySorted = @($expectedInventoryPaths | Sort-Object)
    $actualInventorySorted = @($inventoryPaths | Sort-Object)
    if ($actualInventorySorted.Count -ne $expectedInventorySorted.Count -or
        (Compare-Object -ReferenceObject $expectedInventorySorted -DifferenceObject $actualInventorySorted -CaseSensitive)) {
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
        $expectedDirectories = @(Get-ExpectedDirectories $expectedPortablePayloadPaths)
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
                if ($path -cnotin $expectedDirectories) {
                    throw "Portable ZIP contains an unexpected directory entry: $path"
                }
            }
            else {
                Assert-AllowedPayloadFile $path
                $filePaths.Add($path)
            }
        }
        $actualFiles = @($filePaths | Sort-Object)
        $expectedFiles = @($expectedPortablePayloadPaths | Sort-Object)
        if ($actualFiles.Count -ne $expectedFiles.Count -or
            (Compare-Object -ReferenceObject $expectedFiles -DifferenceObject $actualFiles -CaseSensitive)) {
            throw "Portable ZIP entries differ from the canonical self-contained payload allowlist."
        }
    }
    finally {
        $archive.Dispose()
    }
}

function Test-VerificationUninstallRegistration([string]$AppId) {
    $subkey = "Software\Microsoft\Windows\CurrentVersion\Uninstall\$AppId`_is1"
    foreach ($view in @([Microsoft.Win32.RegistryView]::Registry64, [Microsoft.Win32.RegistryView]::Registry32)) {
        $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, $view)
        try {
            $key = $base.OpenSubKey($subkey, $false)
            if ($null -ne $key) {
                $key.Dispose()
                return $true
            }
        }
        finally {
            $base.Dispose()
        }
    }
    return $false
}

function Remove-VerificationUninstallRegistration([string]$AppId) {
    $subkey = "Software\Microsoft\Windows\CurrentVersion\Uninstall\$AppId`_is1"
    foreach ($view in @([Microsoft.Win32.RegistryView]::Registry64, [Microsoft.Win32.RegistryView]::Registry32)) {
        $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, $view)
        try {
            $base.DeleteSubKeyTree($subkey, $false)
        }
        finally {
            $base.Dispose()
        }
    }
}

function Remove-ValidatedTemporaryRoot([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }
    $tempRoot = Get-NormalizedPath ([System.IO.Path]::GetTempPath())
    $resolved = Get-NormalizedPath $Path
    if (-not [string]::Equals((Split-Path -Parent $resolved), $tempRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path -Leaf $resolved) -cnotmatch '^scribe-release-verification-[0-9a-f]{32}$') {
        throw "Refused release verification cleanup outside its bounded temporary directory."
    }
    Assert-NoReparseAncestors $resolved
    Assert-TreeHasNoReparsePoints $resolved
    Remove-Item -LiteralPath $resolved -Recurse -Force
}

# A fresh Inno Setup installation writes these two default-named uninstaller files
# into {app}. They are installer metadata, not part of the portable release tree.
$InnoSetupUninstallerArtifacts = @("unins000.exe", "unins000.dat")

$bundle = Get-NormalizedPath $BundlePath
$null = Assert-Bundle $bundle

$verificationToken = [guid]::NewGuid().ToString('N')
$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-release-verification-$verificationToken"
$verificationAppId = "{8E0F1935-8E3D-4B1D-9A42-7C7D7C3D5E7A}.verification.$verificationToken"
$verificationUninstaller = $null
$stableAppId = "{8E0F1935-8E3D-4B1D-9A42-7C7D7C3D5E7A}"
$stableUninstaller = $null
Assert-NoReparseAncestors $temporaryRoot
if (Test-VerificationUninstallRegistration $verificationAppId) {
    throw "Unique installer verification registration unexpectedly already exists."
}
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
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
        $installedRoot = Join-Path $temporaryRoot "installed"
        $escapedVerificationRoot = Join-Path $temporaryRoot "escaped"
        $escapedProcess = Invoke-NativeProcess $installer @(
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/SP-",
            "/NOICONS",
            "/DIR=$escapedVerificationRoot",
            "/SCRIBEVERIFY=$verificationToken"
        )
        if ($escapedProcess.ExitCode -eq 0) {
            throw "Installer verification token accepted an override outside its derived temporary destination."
        }
        if (Test-Path -LiteralPath $escapedVerificationRoot) {
            throw "Rejected installer verification destination was created or mutated."
        }
        if (Test-VerificationUninstallRegistration $verificationAppId) {
            throw "Rejected installer verification destination created an uninstall registration."
        }
        $installerProcess = Invoke-NativeProcess $installer @(
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/SP-",
            "/NOICONS",
            "/SCRIBEVERIFY=$verificationToken"
        )
        if ($installerProcess.ExitCode -ne 0) {
            throw "Silent installer verification failed with exit code $($installerProcess.ExitCode): $($installerProcess.Stderr.Trim())"
        }
        if (-not (Test-VerificationUninstallRegistration $verificationAppId)) {
            throw "Installer verification mode did not create its isolated uninstall registration."
        }
        $verificationUninstaller = Join-Path $installedRoot "unins000.exe"
        $null = Assert-Bundle -Root $installedRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts
        Assert-PayloadParity $bundle $installedRoot "Installed"

        $uninstallProcess = Invoke-NativeProcess $verificationUninstaller @(
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART"
        )
        if ($uninstallProcess.ExitCode -ne 0) {
            throw "Installer verification cleanup failed with exit code $($uninstallProcess.ExitCode): $($uninstallProcess.Stderr.Trim())"
        }
        $verificationUninstaller = $null
        if (Test-VerificationUninstallRegistration $verificationAppId) {
            throw "Installer verification cleanup left its isolated uninstall registration behind."
        }

        if ($ExerciseStableUpgrade) {
            if (Test-VerificationUninstallRegistration $stableAppId) {
                throw "Stable Scribe uninstall registration already exists; refusing the isolated stable-upgrade exercise."
            }
            $stableRoot = Join-Path $temporaryRoot "stable"
            New-Item -ItemType Directory -Path $stableRoot | Out-Null
            $fsutil = Join-Path $env:SystemRoot "System32\fsutil.exe"
            $caseSensitiveProcess = Invoke-NativeProcess $fsutil @(
                "file", "setCaseSensitiveInfo", $stableRoot, "enable"
            )
            if ($caseSensitiveProcess.ExitCode -ne 0) {
                throw "Could not enable the isolated stable-upgrade case-collision fixture: $($caseSensitiveProcess.Stderr.Trim())"
            }

            $stableInstallArguments = @(
                "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-", "/NOICONS", "/DIR=$stableRoot"
            )
            $stableInstall = Invoke-NativeProcess $installer $stableInstallArguments
            if ($stableInstall.ExitCode -ne 0) {
                throw "Initial isolated stable installation failed with exit code $($stableInstall.ExitCode): $($stableInstall.Stderr.Trim())"
            }
            if (-not (Test-VerificationUninstallRegistration $stableAppId)) {
                throw "Initial isolated stable installation did not create the stable uninstall registration."
            }
            $stableUninstaller = Join-Path $stableRoot "unins000.exe"
            $null = Assert-Bundle -Root $stableRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts
            Assert-PayloadParity $bundle $stableRoot "Initial stable install"

            $stableUpgrade = Invoke-NativeProcess $installer $stableInstallArguments
            if ($stableUpgrade.ExitCode -ne 0) {
                throw "Canonical stable upgrade failed with exit code $($stableUpgrade.ExitCode): $($stableUpgrade.Stderr.Trim())"
            }
            $null = Assert-Bundle -Root $stableRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts
            Assert-PayloadParity $bundle $stableRoot "Stable upgrade"

            $canonicalReadme = Join-Path $stableRoot "README.txt"
            $caseCollision = Join-Path $stableRoot "readme.txt"
            Copy-Item -LiteralPath $canonicalReadme -Destination $caseCollision
            if (@(Get-ChildItem -LiteralPath $stableRoot -File | Where-Object { $_.Name -ieq "README.txt" }).Count -ne 2) {
                throw "Could not create the isolated case-collision fixture."
            }
            $caseCollisionHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $caseCollision).Hash
            $caseCollisionInstall = Invoke-NativeProcess $installer $stableInstallArguments
            if ($caseCollisionInstall.ExitCode -eq 0) {
                throw "Stable installer accepted a case-insensitive path collision."
            }
            if (-not (Test-Path -LiteralPath $caseCollision -PathType Leaf) -or
                (Get-FileHash -Algorithm SHA256 -LiteralPath $caseCollision).Hash -cne $caseCollisionHash) {
                throw "Stable installer mutated the refused case-collision fixture."
            }
            Remove-Item -LiteralPath $caseCollision

            $legacyDirectory = Join-Path $stableRoot "runtimes"
            New-Item -ItemType Directory -Path $legacyDirectory | Out-Null
            $legacyFile = Join-Path $legacyDirectory "whisper.dll"
            [System.IO.File]::WriteAllBytes($legacyFile, [byte[]](1, 3, 3, 7))
            $legacyHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $legacyFile).Hash
            $legacyInstall = Invoke-NativeProcess $installer $stableInstallArguments
            if ($legacyInstall.ExitCode -eq 0) {
                throw "Stable installer accepted an unexpected legacy runtime tree."
            }
            if (-not (Test-Path -LiteralPath $legacyFile -PathType Leaf) -or
                (Get-FileHash -Algorithm SHA256 -LiteralPath $legacyFile).Hash -cne $legacyHash) {
                throw "Stable installer deleted or mutated refused legacy content."
            }

            $stableUninstall = Invoke-NativeProcess $stableUninstaller @(
                "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"
            )
            if ($stableUninstall.ExitCode -ne 0) {
                throw "Stable-upgrade fixture cleanup failed with exit code $($stableUninstall.ExitCode): $($stableUninstall.Stderr.Trim())"
            }
            $stableUninstaller = $null
            if (Test-VerificationUninstallRegistration $stableAppId) {
                throw "Stable-upgrade fixture cleanup left the stable uninstall registration behind."
            }
            if (-not (Test-Path -LiteralPath $legacyFile -PathType Leaf) -or
                (Get-FileHash -Algorithm SHA256 -LiteralPath $legacyFile).Hash -cne $legacyHash) {
                throw "Stable uninstaller unexpectedly deleted or mutated refused legacy content."
            }
        }
    }
}
finally {
    if ($null -ne $stableUninstaller -and (Test-Path -LiteralPath $stableUninstaller -PathType Leaf)) {
        try {
            $stableCleanup = Invoke-NativeProcess $stableUninstaller @(
                "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"
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
                "/NORESTART"
            )
            if ($cleanupProcess.ExitCode -ne 0) {
                Write-Warning "Installer verification uninstaller cleanup returned exit $($cleanupProcess.ExitCode)."
            }
        }
        catch {
            Write-Warning "Installer verification uninstaller cleanup failed: $($_.Exception.Message)"
        }
    }
    if (Test-VerificationUninstallRegistration $verificationAppId) {
        Remove-VerificationUninstallRegistration $verificationAppId
    }
    Remove-ValidatedTemporaryRoot $temporaryRoot
}

Write-Output "Windows release payload allowlist, inventory, and parity verification passed."
