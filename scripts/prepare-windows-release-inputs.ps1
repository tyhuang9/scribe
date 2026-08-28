param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$targetTriple = "x86_64-pc-windows-msvc"
$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$modelManifestPath = Join-Path $repositoryRoot "runtime-manifests\whisper-base-en-q8_0-windows-x64.json"
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
            throw "Could not resolve an existing ancestor for release input path: $Path"
        }
        $current = $parent
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Release input preparation cannot cross a symbolic link or reparse point: $current"
        }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            break
        }
        $current = $parent
    }
}

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

if ($modelManifest.schema_version -ne 1 -or $modelManifest.platform_triple -cne $targetTriple) {
    throw "The bundled model manifest has an unexpected schema or platform triple."
}
$modelRepository = Get-RequiredValue $modelManifest.repository "model repository"
$modelRevision = Get-RequiredValue $modelManifest.revision "model revision"
$modelFilename = Get-RequiredValue $modelManifest.artifact_filename "model artifact filename"
if ($modelFilename -cne [System.IO.Path]::GetFileName($modelFilename) -or
    $modelFilename -match '[\\/]') {
    throw "The pinned model artifact filename must be one root-level filename."
}

$outputRoot = Get-NormalizedFullPath $OutputDirectory
Assert-NoReparseAncestors $outputRoot
if (Test-Path -LiteralPath $outputRoot) {
    throw "Release input directory already exists; remove or archive it explicitly first: $outputRoot"
}

$modelRoot = Join-Path $outputRoot "model"
New-Item -ItemType Directory -Path $modelRoot -Force | Out-Null
Assert-NoReparseAncestors $modelRoot

$modelDestination = Join-Path $modelRoot $modelFilename
$modelUrl = "https://huggingface.co/$modelRepository/resolve/$modelRevision/$modelFilename"
Download-And-Verify $modelUrl $modelDestination ([int64]$modelManifest.size_bytes) $modelManifest.sha256

$actualFiles = @(Get-ChildItem -LiteralPath $outputRoot -Recurse -File -Force)
if ($actualFiles.Count -ne 1 -or $actualFiles[0].FullName -cne $modelDestination) {
    throw "Prepared release inputs must contain only the pinned Base GGUF."
}

Write-Output "Prepared verified Base GGUF release input at $modelDestination"
