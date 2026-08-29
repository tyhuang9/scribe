param(
    [Parameter(Mandatory = $true)]
    [string]$ModelSource,
    [string]$BundlePath
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

function Get-NormalizedFullPath([string]$Path) {
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

function Assert-AllowedPayloadFile([string]$RelativePath) {
    Assert-SafeRelativePayloadPath $RelativePath
    $lower = $RelativePath.ToLowerInvariant()
    $segments = @($lower.Split('/'))
    $leaf = $segments[-1]
    $extension = [System.IO.Path]::GetExtension($leaf)

    if ($segments -contains 'runtimes' -or
        $leaf -match '^runtime-manifest(?:\..+)?$' -or
        $lower -match '(^|/)(?:\.?venv|__pycache__|python(?:\d+(?:\.\d+)*)?|runner)(/|$)' -or
        $leaf -match '^(?:python(?:\d+(?:\.\d+)*)?|runner)(?:\..+)?$' -or
        $extension -in @('.pyd', '.py', '.pyc', '.onnx', '.ort')) {
        throw "Release payload contains a forbidden runtime, Python, runner, or loose ONNX artifact: $RelativePath"
    }
    if ($extension -in @('.dll', '.exe') -and
        $RelativePath -cnotin @('local-transcriber.exe', 'scribe-inference-worker.exe')) {
        throw "Release payload contains an unallowlisted executable or DLL: $RelativePath"
    }
}

function Assert-ExactAllowlist([string]$Root, [string[]]$ExpectedPaths) {
    Assert-TreeHasNoReparsePoints $Root
    $expectedCaseFolded = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $ExpectedPaths) {
        Assert-AllowedPayloadFile $path
        if (-not $expectedCaseFolded.Add($path)) {
            throw "Release allowlist contains duplicate case-insensitive paths: $path"
        }
    }
    $actual = @(Get-ChildItem -LiteralPath $Root -Recurse -File -Force | ForEach-Object {
        Get-RelativeBundlePath $Root $_.FullName
    } | Sort-Object)
    $actualCaseFolded = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $actual) {
        Assert-AllowedPayloadFile $path
        if (-not $actualCaseFolded.Add($path)) {
            throw "Release payload contains duplicate case-insensitive paths: $path"
        }
    }
    $expected = @($ExpectedPaths | Sort-Object)
    if ($actual.Count -ne $expected.Count -or
        (Compare-Object -ReferenceObject $expected -DifferenceObject $actual -CaseSensitive)) {
        throw "Release bundle contains files outside the explicit allowlist."
    }

    $expectedDirectories = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($path in $ExpectedPaths) {
        $segments = $path.Split('/')
        for ($index = 1; $index -lt $segments.Count; $index++) {
            $null = $expectedDirectories.Add(($segments[0..($index - 1)] -join '/'))
        }
    }
    $actualDirectories = @(Get-ChildItem -LiteralPath $Root -Recurse -Directory -Force | ForEach-Object {
        Get-RelativeBundlePath $Root $_.FullName
    } | Sort-Object)
    $expectedDirectoryPaths = @($expectedDirectories | Sort-Object)
    if ($actualDirectories.Count -ne $expectedDirectoryPaths.Count -or
        (Compare-Object -ReferenceObject $expectedDirectoryPaths -DifferenceObject $actualDirectories -CaseSensitive)) {
        throw "Release bundle contains directories outside the explicit allowlist."
    }
}

function Assert-ReleaseSmokeDiagnostics([psobject]$Smoke) {
    if ($null -eq $Smoke) {
        throw "Offline staged-bundle smoke diagnostics are missing."
    }
    $smokeProperties = @($Smoke.PSObject.Properties.Name)
    foreach ($requiredProperty in @("cancellation_verified", "capabilities", "detected_architecture")) {
        if ($requiredProperty -notin $smokeProperties) {
            throw "Offline staged-bundle smoke diagnostics are missing '$requiredProperty'."
        }
    }
    if ($null -eq $Smoke.capabilities -or "cancellation" -notin @($Smoke.capabilities.PSObject.Properties.Name)) {
        throw "Offline staged-bundle smoke diagnostics are missing 'capabilities.cancellation'."
    }
    if (-not $Smoke.cancellation_verified -or -not $Smoke.capabilities.cancellation) {
        throw "Offline staged-bundle smoke did not verify cancellation."
    }
    if ([string]$Smoke.detected_architecture -cne "whisper") {
        throw "Offline staged-bundle smoke expected detected architecture 'whisper'; received '$($Smoke.detected_architecture)'."
    }
}

if (-not [Environment]::Is64BitOperatingSystem -or
    [Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "The release bundle is qualified only for Windows x64."
}
if ($modelManifest.platform_triple -ne $targetTriple) {
    throw "The bundled model manifest does not match the qualified Windows x64 target triple."
}
$expectedModelAttribution = @(
    "resources/licenses/Apache-2.0.txt",
    "resources/licenses/OpenAI-Whisper-MIT.txt",
    "resources/licenses/Whisper-Base-En-NOTICE.txt"
)
$actualModelAttribution = @($modelManifest.attribution_files)
if ($actualModelAttribution.Count -ne $expectedModelAttribution.Count -or
    (Compare-Object -ReferenceObject $expectedModelAttribution -DifferenceObject $actualModelAttribution -CaseSensitive)) {
    throw "The bundled model attribution allowlist changed without release review."
}
$legalDestinationNames = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
foreach ($legalFile in $legalFiles) {
    Assert-AllowedPayloadFile $legalFile.Destination
    if (-not $legalDestinationNames.Add($legalFile.Destination)) {
        throw "The release legal inventory contains a duplicate destination: $($legalFile.Destination)"
    }
    $legalSource = Join-Path $repositoryRoot ($legalFile.Source -replace '/', '\')
    Assert-NoReparseAncestors $legalSource
    $null = Assert-RegularFile $legalSource
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
$defaultCargoTargetRoot = Get-NormalizedFullPath (Join-Path $repositoryRoot "target")
$cargoTargetRoot = if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    $defaultCargoTargetRoot
} else {
    $cargoTargetCandidate = if ([System.IO.Path]::IsPathFullyQualified($env:CARGO_TARGET_DIR)) {
        $env:CARGO_TARGET_DIR
    } else {
        Join-Path $repositoryRoot $env:CARGO_TARGET_DIR
    }
    Get-NormalizedFullPath $cargoTargetCandidate
}
foreach ($protectedCargoTargetRoot in @($defaultCargoTargetRoot, $cargoTargetRoot)) {
    if ([string]::Equals($finalBundle, $protectedCargoTargetRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
        $finalBundle.StartsWith($protectedCargoTargetRoot + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Cargo target directories are build inputs and cannot be used as distributable release bundles."
    }
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
Assert-NoReparseAncestors $modelSourcePath
Assert-ExactFile $modelSourcePath ([int64]$modelManifest.size_bytes) $modelManifest.sha256

Push-Location $repositoryRoot
try {
    & cargo build --locked --offline --release --bin local-transcriber --features ui-harness --target $targetTriple --manifest-path (Join-Path $repositoryRoot "Cargo.toml")
    if ($LASTEXITCODE -ne 0) {
        throw "The locked offline Windows x64 desktop release build failed."
    }
    & cargo build --locked --offline --release --bin scribe-inference-worker --features inference-worker --target $targetTriple --manifest-path (Join-Path $repositoryRoot "Cargo.toml")
    if ($LASTEXITCODE -ne 0) {
        throw "The locked offline Windows x64 CPU inference worker release build failed."
    }
}
finally {
    Pop-Location
}

$cargoReleaseRoot = Join-Path $cargoTargetRoot "$targetTriple\release"
$sourceExecutable = Join-Path $cargoReleaseRoot "local-transcriber.exe"
$sourceInferenceWorker = Join-Path $cargoReleaseRoot "scribe-inference-worker.exe"
Assert-Amd64Pe $sourceExecutable
Assert-WindowsGuiSubsystem $sourceExecutable
$null = Assert-ReviewedWindowsPe $sourceExecutable
Assert-Amd64Pe $sourceInferenceWorker
$null = Assert-ReviewedWindowsPe $sourceInferenceWorker 3

try {
    New-Item -ItemType Directory -Path $stagingBundle | Out-Null
    Assert-NoReparseAncestors $stagingBundle
    $stagedExecutable = Join-Path $stagingBundle "local-transcriber.exe"
    $stagedInferenceWorker = Join-Path $stagingBundle "scribe-inference-worker.exe"
    Copy-Item -LiteralPath $sourceExecutable -Destination $stagedExecutable
    Copy-Item -LiteralPath $sourceInferenceWorker -Destination $stagedInferenceWorker

    $stagedModel = Join-Path $stagingBundle $modelManifest.artifact_filename
    Copy-Item -LiteralPath $modelSourcePath -Destination $stagedModel
    foreach ($legalFile in $legalFiles) {
        $sourcePath = Join-Path $repositoryRoot ($legalFile.Source -replace '/', '\')
        $destinationPath = Join-Path $stagingBundle ($legalFile.Destination -replace '/', '\')
        New-Item -ItemType Directory -Path (Split-Path -Parent $destinationPath) -Force | Out-Null
        Copy-Item -LiteralPath $sourcePath -Destination $destinationPath
    }
    $stagedModelManifest = Join-Path $stagingBundle "bundled-model-manifest.json"
    Copy-Item -LiteralPath $modelManifestPath -Destination $stagedModelManifest
    $cargoManifest = Get-Content -LiteralPath (Join-Path $repositoryRoot "Cargo.toml") -Raw
    $versionMatch = [regex]::Match($cargoManifest, '(?m)^version\s*=\s*"([^"]+)"')
    if (-not $versionMatch.Success) {
        throw "Could not read the Scribe version from Cargo.toml."
    }
    $portableReadme = Join-Path $stagingBundle "README.txt"
    $portableReadmeText = @(
        "Scribe $($versionMatch.Groups[1].Value) - Windows x64 self-contained package",
        "",
        "RUN",
        "Portable: extract the entire archive into a new directory, then run local-transcriber.exe. Keep every packaged file together.",
        "Installer: the installer copies this exact portable payload into the per-user Scribe program directory and adds only its uninstaller pair.",
        "",
        "CONTENTS",
        "The package contains the Scribe desktop, its dedicated CPU inference worker, the pinned English Base GGUF, its manifest, a hash inventory, this README, and reviewed license/provenance notices. Native transcribe.cpp, whisper.cpp, sherpa-onnx, and Silero VAD support are statically linked into the appropriate executable; there is no runtime folder, loose DLL, or loose ONNX model.",
        "Moonshine ONNX weights are not packaged. When requested, Scribe downloads them separately as receipt-backed per-user app-data artifacts.",
        "",
        "MANUAL VERIFICATION",
        "1. Confirm the extracted tree contains no files beyond bundle-inventory.json and every exact path listed in its files array.",
        "2. For every listed file, compare its byte length and SHA-256 with bundle-inventory.json.",
        "3. Confirm bundled-model-manifest.json identifies whisper-base.en-Q8_0.gguf as 84,886,208 bytes with SHA-256 3b46ca40bccbf7609c68d88a36d96077a04ca7c87f2060ede06f129fac3e7652.",
        "4. Confirm local-transcriber.exe is an AMD64 Windows GUI PE, scribe-inference-worker.exe is an AMD64 Windows console PE, and no additional EXE, DLL, .onnx, .ort, Python, venv, runner, or runtimes directory is present.",
        "5. For an installer, compare every installed payload file and hash with the portable tree; only unins000.exe and unins000.dat may be additional files in the program directory.",
        "This release workflow does not claim Authenticode signing. Obtain artifacts from a trusted release channel and verify hashes before running them.",
        "",
        "UPGRADES, USER DATA, AND ROLLBACK",
        "Installing, uninstalling, or replacing the portable program directory is not intended to delete Scribe app-data settings, history, downloaded receipt-backed ONNX bundles, or managed model receipts. Imported GGUF files and external sentinel files remain outside the packaged payload and must not be removed by an upgrade.",
        "The installer checks an existing Scribe program directory before copying. If it contains an unexpected, legacy, case-colliding, or reparse-point entry, setup refuses safely and does not delete or change that content. You choose whether to back up the program directory, uninstall the previous version, or remove the unexpected entry before retrying.",
        "To roll back the installer, close Scribe, uninstall the current program payload, and install a previously verified installer. To roll back portable use, close Scribe and launch a previously verified complete portable folder. Do not delete per-user app data or external/imported models as part of rollback.",
        ""
    ) -join "`r`n"
    [System.IO.File]::WriteAllText(
        $portableReadme,
        $portableReadmeText,
        [System.Text.UTF8Encoding]::new($false)
    )

    Assert-NoReparseAncestors $stagingBundle
    Assert-TreeHasNoReparsePoints $stagingBundle
    Assert-CopyMatchesSource $sourceExecutable $stagedExecutable
    Assert-CopyMatchesSource $sourceInferenceWorker $stagedInferenceWorker
    Assert-CopyMatchesSource $modelManifestPath $stagedModelManifest
    foreach ($legalFile in $legalFiles) {
        $sourcePath = Join-Path $repositoryRoot ($legalFile.Source -replace '/', '\')
        $destinationPath = Join-Path $stagingBundle ($legalFile.Destination -replace '/', '\')
        Assert-CopyMatchesSource $sourcePath $destinationPath
    }
    Assert-Amd64Pe $stagedExecutable
    Assert-WindowsGuiSubsystem $stagedExecutable
    $null = Assert-ReviewedWindowsPe $stagedExecutable
    Assert-Amd64Pe $stagedInferenceWorker
    $null = Assert-ReviewedWindowsPe $stagedInferenceWorker 3
    Assert-ExactFile $stagedModel ([int64]$modelManifest.size_bytes) $modelManifest.sha256
    $expectedPaths = [System.Collections.Generic.List[string]]::new()
    $null = $expectedPaths.Add("local-transcriber.exe")
    $null = $expectedPaths.Add("scribe-inference-worker.exe")
    $null = $expectedPaths.Add($modelManifest.artifact_filename)
    foreach ($legalFile in $legalFiles) {
        $null = $expectedPaths.Add($legalFile.Destination)
    }
    $null = $expectedPaths.Add("bundled-model-manifest.json")
    $null = $expectedPaths.Add("README.txt")
    Assert-ExactAllowlist $stagingBundle $expectedPaths.ToArray()

    $previousHubOffline = $env:HF_HUB_OFFLINE
    $previousTransformersOffline = $env:TRANSFORMERS_OFFLINE
    try {
        $env:HF_HUB_OFFLINE = "1"
        $env:TRANSFORMERS_OFFLINE = "1"
        $smokeProcess = Invoke-NativeProcess $stagedExecutable @(
            "--scribe-install-smoke-parent",
            [string]$modelManifest.model_id,
            $stagedModel,
            "gguf",
            [string]$modelManifest.size_bytes,
            [string]$modelManifest.sha256,
            "cpu"
        )
        if ($smokeProcess.ExitCode -ne 0) {
            throw "Offline staged-bundle smoke failed with exit code $($smokeProcess.ExitCode): $($smokeProcess.Stderr.Trim())"
        }
    }
    finally {
        $env:HF_HUB_OFFLINE = $previousHubOffline
        $env:TRANSFORMERS_OFFLINE = $previousTransformersOffline
    }
    if ([string]::IsNullOrWhiteSpace($smokeProcess.Stdout)) {
        throw "Offline staged-bundle smoke returned no diagnostics. Stderr: $($smokeProcess.Stderr.Trim())"
    }
    try {
        $smoke = $smokeProcess.Stdout | ConvertFrom-Json
    }
    catch {
        throw "Offline staged-bundle smoke returned invalid JSON: $($_.Exception.Message). Stderr: $($smokeProcess.Stderr.Trim())"
    }
    Assert-ReleaseSmokeDiagnostics $smoke

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
