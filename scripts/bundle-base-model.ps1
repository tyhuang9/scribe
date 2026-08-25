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
            throw "Could not resolve an existing ancestor for bundled-model output: $Path"
        }
        $current = $parent
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Bundled-model output cannot cross a symbolic link or reparse point: $current"
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
        throw "Required bundled-model file is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Bundled-model files cannot be symbolic links or reparse points: $Path"
    }
    return $item
}

function Assert-SafeStagingPath([string]$StagingPath, [string]$DestinationPath) {
    $staging = Get-NormalizedFullPath $StagingPath
    $destination = Get-NormalizedFullPath $DestinationPath
    if (-not [string]::Equals((Split-Path -Parent $staging), $destination, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Bundled-model staging must be a direct child of the destination."
    }
    if ((Split-Path -Leaf $staging) -cnotmatch '^\.scribe-base-model-staging-[0-9]+-[0-9a-f]{32}$') {
        throw "Bundled-model staging path does not match the bounded transaction name."
    }
}

function Assert-TreeHasNoReparsePoints([string]$Root) {
    $rootItem = Get-Item -LiteralPath $Root -Force
    if (($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Bundled-model staging root cannot be a reparse point: $Root"
    }
    foreach ($item in Get-ChildItem -LiteralPath $Root -Recurse -Force) {
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Bundled-model staging cannot contain a reparse point: $($item.FullName)"
        }
    }
}

if (-not [Environment]::Is64BitOperatingSystem -or
    [Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "The bundled base model is release-qualified only for Windows x64."
}
if (-not $Destination) {
    $Destination = Join-Path $repositoryRoot "target\$Profile"
}

$sourcePath = Get-NormalizedFullPath $Source
$destinationRoot = Get-NormalizedFullPath $Destination
if (-not $Executable) {
    $Executable = Join-Path $destinationRoot "local-transcriber.exe"
}
$executablePath = Get-NormalizedFullPath $Executable
$destinationModel = Join-Path $destinationRoot $manifest.artifact_filename

if (-not (Test-Path -LiteralPath $destinationRoot -PathType Container)) {
    throw "Bundled-model destination must already exist: $destinationRoot"
}
if ((Split-Path -Leaf $executablePath) -cne "local-transcriber.exe") {
    throw "Bundled-model smoke requires the exact executable name local-transcriber.exe."
}
$executableParent = Get-NormalizedFullPath (Split-Path -Parent $executablePath)
if (-not [string]::Equals($executableParent, $destinationRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "The canonical executable parent must equal the bundled-model destination."
}
Assert-NoReparseAncestors $destinationRoot
Assert-NoReparseAncestors $executablePath
if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
    throw "Pinned model source does not exist: $sourcePath"
}
$sourceItem = Assert-RegularFile $sourcePath
if ($sourceItem.Length -ne [int64]$manifest.size_bytes) {
    throw "Pinned model size mismatch for $sourcePath"
}
$sourceHash = (Get-FileHash -LiteralPath $sourcePath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($sourceHash -ne $manifest.sha256) {
    throw "Pinned model hash mismatch for $sourcePath"
}
$null = Assert-RegularFile $executablePath
if (Test-Path -LiteralPath $destinationModel) {
    throw "Bundled model destination already exists; remove or archive it explicitly first: $destinationModel"
}

$stagingRoot = Join-Path $destinationRoot ".scribe-base-model-staging-$PID-$([guid]::NewGuid().ToString('N'))"
Assert-SafeStagingPath $stagingRoot $destinationRoot
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

    Assert-NoReparseAncestors $destinationRoot
    Assert-SafeStagingPath $stagingRoot $destinationRoot
    Assert-TreeHasNoReparsePoints $stagingRoot

    Move-Item -LiteralPath $stagedModel -Destination $destinationModel
    $null = $createdPaths.Add($destinationModel)
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
            $null = $createdPaths.Add($destinationLicense)
        }
    }

    Assert-NoReparseAncestors $destinationRoot
    $null = Assert-RegularFile $executablePath
    $null = Assert-RegularFile $destinationModel
    foreach ($relativePath in $manifest.attribution_files) {
        $null = Assert-RegularFile (Join-Path $licenseDestination (Split-Path -Leaf $relativePath))
    }

    $previousHubOffline = $env:HF_HUB_OFFLINE
    $previousTransformersOffline = $env:TRANSFORMERS_OFFLINE
    try {
        $env:HF_HUB_OFFLINE = "1"
        $env:TRANSFORMERS_OFFLINE = "1"
        $smokeProcess = Invoke-NativeProcess $executablePath @(
            "--scribe-install-smoke-parent",
            [string]$manifest.model_id,
            $destinationModel,
            "gguf",
            "-",
            [string]$manifest.size_bytes,
            [string]$manifest.sha256,
            "cpu"
        )
        if ($smokeProcess.ExitCode -ne 0) {
            throw "Offline bundled-model smoke failed with exit code $($smokeProcess.ExitCode): $($smokeProcess.Stderr.Trim())"
        }
    }
    finally {
        $env:HF_HUB_OFFLINE = $previousHubOffline
        $env:TRANSFORMERS_OFFLINE = $previousTransformersOffline
    }
    if ([string]::IsNullOrWhiteSpace($smokeProcess.Stdout)) {
        throw "Offline bundled-model smoke returned no diagnostics. Stderr: $($smokeProcess.Stderr.Trim())"
    }
    try {
        $smoke = $smokeProcess.Stdout | ConvertFrom-Json
    }
    catch {
        throw "Offline bundled-model smoke returned invalid JSON: $($_.Exception.Message). Stderr: $($smokeProcess.Stderr.Trim())"
    }
    $smokeProperties = @($smoke.PSObject.Properties.Name)
    foreach ($requiredProperty in @("cancellation_verified", "capabilities", "detected_architecture")) {
        if ($requiredProperty -notin $smokeProperties) {
            throw "Offline bundled-model smoke diagnostics are missing '$requiredProperty'."
        }
    }
    if ($null -eq $smoke.capabilities -or "cancellation" -notin @($smoke.capabilities.PSObject.Properties.Name)) {
        throw "Offline bundled-model smoke diagnostics are missing 'capabilities.cancellation'."
    }
    if (-not $smoke.cancellation_verified) {
        throw "Offline bundled-model smoke did not verify cancellation."
    }
    if (-not $smoke.detected_architecture -or -not $smoke.capabilities.cancellation) {
        throw "Offline bundled-model smoke returned incomplete runtime evidence."
    }

    Write-Output "Bundled and offline-smoke-verified $($manifest.model_id) at $destinationModel"
}
catch {
    Assert-NoReparseAncestors $destinationRoot
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
        Assert-SafeStagingPath $stagingRoot $destinationRoot
        Assert-NoReparseAncestors $stagingRoot
        Assert-TreeHasNoReparsePoints $stagingRoot
        Remove-Item -LiteralPath $stagingRoot -Recurse -Force
    }
}
