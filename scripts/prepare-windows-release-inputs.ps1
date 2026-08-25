param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$runtimeManifestPath = Join-Path $repositoryRoot "runtime-manifests\whisper-cpp-v1.9.1-windows-x64.json"
$modelManifestPath = Join-Path $repositoryRoot "runtime-manifests\whisper-base-en-q8_0-windows-x64.json"
$runtimeManifest = Get-Content -LiteralPath $runtimeManifestPath -Raw | ConvertFrom-Json
$modelManifest = Get-Content -LiteralPath $modelManifestPath -Raw | ConvertFrom-Json

function Assert-ExactFile([string]$Path, [int64]$ExpectedSize, [string]$ExpectedHash) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Required release input is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Release inputs cannot be symbolic links or reparse points: $Path"
    }
    if ($item.Length -ne $ExpectedSize) {
        throw "Release input size mismatch for $Path"
    }
    $actualHash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $ExpectedHash.ToLowerInvariant()) {
        throw "Release input SHA-256 mismatch for $Path"
    }
}

function Get-RequiredValue($Value, [string]$Name) {
    if ([string]::IsNullOrWhiteSpace([string]$Value)) {
        throw "Release manifest is missing $Name."
    }
    return [string]$Value
}

function Download-And-Verify([string]$Url, [string]$Destination, [int64]$Size, [string]$Sha256) {
    curl.exe --fail --location --retry 3 --retry-delay 2 --output $Destination $Url
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to download pinned release input from $Url."
    }
    Assert-ExactFile $Destination $Size $Sha256
}

function Resolve-RuntimeArchiveFile([string]$RuntimeSource, [string]$ManifestPath) {
    $declaredPath = $ManifestPath -replace '/', '\\'
    $nestedPath = Join-Path $RuntimeSource $declaredPath
    if (Test-Path -LiteralPath $nestedPath -PathType Leaf) {
        return $nestedPath
    }

    # whisper.cpp v1.9.1's Windows archive places its release binaries directly
    # under Release/, while Scribe intentionally stages them under bin/ to keep
    # the runtime layout stable. Accept only this explicit flattened equivalent;
    # the size and hash checks below still authenticate every file.
    $flatPath = Join-Path $RuntimeSource (Split-Path -Leaf $declaredPath)
    if (Test-Path -LiteralPath $flatPath -PathType Leaf) {
        return $flatPath
    }

    throw "Pinned runtime archive is missing declared file '$ManifestPath' under '$RuntimeSource'."
}

$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $outputRoot) {
    throw "Release input directory already exists; remove or archive it explicitly first: $outputRoot"
}

$modelRepository = Get-RequiredValue $modelManifest.repository "model repository"
$modelRevision = Get-RequiredValue $modelManifest.revision "model revision"
$modelFilename = Get-RequiredValue $modelManifest.artifact_filename "model artifact filename"
$runtimeUrl = Get-RequiredValue $runtimeManifest.archive.url "runtime archive URL"
$runtimePrefix = Get-RequiredValue $runtimeManifest.archive_prefix "runtime archive prefix"

$modelRoot = Join-Path $outputRoot "model"
$runtimeRoot = Join-Path $outputRoot "runtime"
$downloadRoot = Join-Path $outputRoot "downloads"
$extractRoot = Join-Path $outputRoot "extracted-runtime"
New-Item -ItemType Directory -Path $modelRoot, $runtimeRoot, $downloadRoot, $extractRoot -Force | Out-Null

$modelDestination = Join-Path $modelRoot $modelFilename
$modelUrl = "https://huggingface.co/$modelRepository/resolve/$modelRevision/$modelFilename"
Download-And-Verify $modelUrl $modelDestination ([int64]$modelManifest.size_bytes) $modelManifest.sha256

$runtimeArchive = Join-Path $downloadRoot "whisper-cpp-runtime.zip"
Download-And-Verify $runtimeUrl $runtimeArchive ([int64]$runtimeManifest.archive.size_bytes) $runtimeManifest.archive.sha256
Expand-Archive -LiteralPath $runtimeArchive -DestinationPath $extractRoot
$runtimeSource = Join-Path $extractRoot $runtimePrefix
if (-not (Test-Path -LiteralPath $runtimeSource -PathType Container)) {
    throw "Pinned runtime archive does not contain the declared prefix: $runtimePrefix"
}

foreach ($file in $runtimeManifest.files) {
    $relativePath = $file.path -replace '/', '\\'
    $source = Resolve-RuntimeArchiveFile $runtimeSource $file.path
    Assert-ExactFile $source ([int64]$file.size_bytes) $file.sha256
    $destination = Join-Path $runtimeRoot $relativePath
    New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
    Copy-Item -LiteralPath $source -Destination $destination
    Assert-ExactFile $destination ([int64]$file.size_bytes) $file.sha256
}

Write-Output "Prepared verified Windows release inputs at $outputRoot"
