param(
    [ValidateSet("debug", "release")]
    [string]$Profile = "release",
    [string]$Source,
    [string]$Destination
)

$ErrorActionPreference = "Stop"
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $repositoryRoot "runtime-manifests\whisper-cpp-v1.9.1-windows-x64.json"
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json

if (-not $Source) {
    $Source = Join-Path $repositoryRoot "target\debug\runtimes\whisper_cpp"
}
if (-not $Destination) {
    $Destination = Join-Path $repositoryRoot "target\$Profile\runtimes\whisper_cpp"
}

$sourceRoot = [System.IO.Path]::GetFullPath($Source)
$destinationRoot = [System.IO.Path]::GetFullPath($Destination)
if ($sourceRoot -eq $destinationRoot) {
    throw "Runtime source and destination must be different directories."
}
if (-not (Test-Path -LiteralPath $sourceRoot -PathType Container)) {
    throw "Pinned runtime source does not exist: $sourceRoot"
}
if (Test-Path -LiteralPath $destinationRoot) {
    throw "Runtime destination already exists; remove or archive it explicitly first: $destinationRoot"
}

foreach ($file in $manifest.files) {
    $sourcePath = Join-Path $sourceRoot ($file.path -replace "/", "\")
    if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
        throw "Pinned runtime file is missing: $sourcePath"
    }
    $item = Get-Item -LiteralPath $sourcePath
    if ($item.Length -ne [int64]$file.size_bytes) {
        throw "Pinned runtime file size mismatch for $sourcePath"
    }
    $actualHash = (Get-FileHash -LiteralPath $sourcePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $file.sha256) {
        throw "Pinned runtime file hash mismatch for $sourcePath"
    }
}

$destinationParent = Split-Path -Parent $destinationRoot
New-Item -ItemType Directory -Path $destinationParent -Force | Out-Null
$stagingRoot = "$destinationRoot.staging-$PID"
New-Item -ItemType Directory -Path $stagingRoot | Out-Null

try {
    foreach ($file in $manifest.files) {
        $relativePath = $file.path -replace "/", "\"
        $sourcePath = Join-Path $sourceRoot $relativePath
        $targetPath = Join-Path $stagingRoot $relativePath
        New-Item -ItemType Directory -Path (Split-Path -Parent $targetPath) -Force | Out-Null
        Copy-Item -LiteralPath $sourcePath -Destination $targetPath
    }
    Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $stagingRoot "runtime-manifest.json")
    Move-Item -LiteralPath $stagingRoot -Destination $destinationRoot
}
catch {
    if (Test-Path -LiteralPath $stagingRoot) {
        Remove-Item -LiteralPath $stagingRoot -Recurse -Force
    }
    throw
}

Write-Output "Bundled verified $($manifest.package_id) at $destinationRoot"
