param(
    [Parameter(Mandatory = $true)]
    [string]$Source,
    [ValidateSet("debug", "release")]
    [string]$Profile = "release",
    [string]$Destination,
    [string]$Executable
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $repositoryRoot "runtime-manifests\whisper-base-en-q8_0-windows-x64.json"
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json

if (-not [Environment]::Is64BitOperatingSystem -or
    [Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "The bundled base model is release-qualified only for Windows x64."
}
if (-not $Destination) {
    $Destination = Join-Path $repositoryRoot "target\$Profile"
}

$sourcePath = [System.IO.Path]::GetFullPath($Source)
$destinationRoot = [System.IO.Path]::GetFullPath($Destination)
if (-not $Executable) {
    $Executable = Join-Path $destinationRoot "local-transcriber.exe"
}
$executablePath = [System.IO.Path]::GetFullPath($Executable)
$destinationModel = Join-Path $destinationRoot $manifest.artifact_filename

if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
    throw "Pinned model source does not exist: $sourcePath"
}
$sourceItem = Get-Item -LiteralPath $sourcePath -Force
if (($sourceItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "Pinned model source cannot be a symbolic link or reparse point: $sourcePath"
}
if ($sourceItem.Length -ne [int64]$manifest.size_bytes) {
    throw "Pinned model size mismatch for $sourcePath"
}
$sourceHash = (Get-FileHash -LiteralPath $sourcePath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($sourceHash -ne $manifest.sha256) {
    throw "Pinned model hash mismatch for $sourcePath"
}
if (-not (Test-Path -LiteralPath $executablePath -PathType Leaf)) {
    throw "Release executable does not exist: $executablePath"
}
if (Test-Path -LiteralPath $destinationModel) {
    throw "Bundled model destination already exists; remove or archive it explicitly first: $destinationModel"
}

New-Item -ItemType Directory -Path $destinationRoot -Force | Out-Null
$stagingRoot = Join-Path $destinationRoot ".scribe-base-model-staging-$PID"
if (Test-Path -LiteralPath $stagingRoot) {
    throw "Bundled model staging directory already exists: $stagingRoot"
}
$createdPaths = [System.Collections.Generic.List[string]]::new()
$createdLicenseDirectory = $false
$licenseDestination = Join-Path $destinationRoot "licenses"

try {
    New-Item -ItemType Directory -Path $stagingRoot | Out-Null
    $stagedModel = Join-Path $stagingRoot $manifest.artifact_filename
    Copy-Item -LiteralPath $sourcePath -Destination $stagedModel

    $stagedItem = Get-Item -LiteralPath $stagedModel
    $stagedHash = (Get-FileHash -LiteralPath $stagedModel -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($stagedItem.Length -ne [int64]$manifest.size_bytes -or $stagedHash -ne $manifest.sha256) {
        throw "Staged bundled model failed exact size/SHA-256 verification."
    }

    $stagedLicenses = Join-Path $stagingRoot "licenses"
    New-Item -ItemType Directory -Path $stagedLicenses | Out-Null
    foreach ($relativePath in $manifest.attribution_files) {
        $licenseSource = Join-Path $repositoryRoot ($relativePath -replace "/", "\")
        if (-not (Test-Path -LiteralPath $licenseSource -PathType Leaf)) {
            throw "Required bundled-model attribution is missing: $licenseSource"
        }
        Copy-Item -LiteralPath $licenseSource -Destination $stagedLicenses
    }

    Move-Item -LiteralPath $stagedModel -Destination $destinationModel
    $createdPaths.Add($destinationModel)
    if (-not (Test-Path -LiteralPath $licenseDestination)) {
        New-Item -ItemType Directory -Path $licenseDestination | Out-Null
        $createdLicenseDirectory = $true
    }
    foreach ($relativePath in $manifest.attribution_files) {
        $fileName = Split-Path -Leaf $relativePath
        $stagedLicense = Join-Path $stagedLicenses $fileName
        $destinationLicense = Join-Path $licenseDestination $fileName
        if (Test-Path -LiteralPath $destinationLicense) {
            $expectedLicenseHash = (Get-FileHash -LiteralPath $stagedLicense -Algorithm SHA256).Hash
            $existingLicenseHash = (Get-FileHash -LiteralPath $destinationLicense -Algorithm SHA256).Hash
            if ($existingLicenseHash -ne $expectedLicenseHash) {
                throw "Existing attribution file differs from the reviewed copy: $destinationLicense"
            }
        }
        else {
            Move-Item -LiteralPath $stagedLicense -Destination $destinationLicense
            $createdPaths.Add($destinationLicense)
        }
    }

    $previousHubOffline = $env:HF_HUB_OFFLINE
    $previousTransformersOffline = $env:TRANSFORMERS_OFFLINE
    try {
        $env:HF_HUB_OFFLINE = "1"
        $env:TRANSFORMERS_OFFLINE = "1"
        $smokeJson = & $executablePath `
            --scribe-install-smoke-parent `
            $manifest.model_id `
            $destinationModel `
            gguf `
            - `
            $manifest.size_bytes `
            $manifest.sha256 `
            cpu
        if ($LASTEXITCODE -ne 0) {
            throw "Offline bundled-model smoke failed with exit code $LASTEXITCODE."
        }
    }
    finally {
        $env:HF_HUB_OFFLINE = $previousHubOffline
        $env:TRANSFORMERS_OFFLINE = $previousTransformersOffline
    }
    $smoke = ($smokeJson | Out-String) | ConvertFrom-Json
    if (-not $smoke.cancellation_verified) {
        throw "Offline bundled-model smoke did not verify cancellation."
    }
    if (-not $smoke.detected_architecture -or -not $smoke.capabilities.cancellation) {
        throw "Offline bundled-model smoke returned incomplete runtime evidence."
    }

    Write-Output "Bundled and offline-smoke-verified $($manifest.model_id) at $destinationModel"
}
catch {
    foreach ($createdPath in $createdPaths) {
        if (Test-Path -LiteralPath $createdPath -PathType Leaf) {
            Remove-Item -LiteralPath $createdPath -Force
        }
    }
    if ($createdLicenseDirectory -and (Test-Path -LiteralPath $licenseDestination)) {
        Remove-Item -LiteralPath $licenseDestination -Force -ErrorAction SilentlyContinue
    }
    throw
}
finally {
    if (Test-Path -LiteralPath $stagingRoot) {
        Remove-Item -LiteralPath $stagingRoot -Recurse -Force
    }
}
