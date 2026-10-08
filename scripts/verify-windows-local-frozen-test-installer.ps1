param(
    [Parameter(Mandatory = $true)]
    [string]$BundlePath,
    [Parameter(Mandatory = $true)]
    [string]$FrozenCpuWorkerRecordPath,
    [string]$FrozenCpuWorkerSourceRoot,
    [Parameter(Mandatory = $true)]
    [string]$InstallerPath,
    [Parameter(Mandatory = $true)]
    [string]$InstallerRecordPath,
    [string]$ObservationWavPath,
    [string]$ObservationWavSha256,
    [string]$ObservationGpuPackId,
    [string]$ObservationGpuBackend,
    [string]$ObservationGpuDevice,
    [string]$ObservationReportPath,
    [ValidateSet('ac', 'battery')]
    [string]$ObservationCampaignPower
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'windows-frozen-cpu-worker-integrity.ps1')
. (Join-Path $PSScriptRoot 'windows-local-frozen-installer-integrity.ps1')

function Invoke-WindowsLocalFrozenInstallerProcess([string]$Executable, [string[]]$Arguments, [string]$Description) {
    return Invoke-WindowsLocalFrozenBoundedProcess `
        -Executable $Executable `
        -Arguments $Arguments `
        -Description $Description `
        -TimeoutMilliseconds 60000 `
        -StreamDrainMilliseconds 5000
}

function Assert-WindowsLocalFrozenObservationOutputDestination(
    [psobject]$Request,
    [string[]]$ForbiddenRoots
) {
    $output = Get-WindowsLocalFrozenNormalizedFullPath $Request.ReportPath
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $output
    if (Test-Path -LiteralPath $output) {
        throw 'Installed GPU observation report output already exists; refusing to replace it.'
    }
    $parent = Split-Path -Parent $output
    $null = Assert-WindowsLocalFrozenRegularDirectory $parent 'Installed GPU observation report parent'
    foreach ($root in $ForbiddenRoots) {
        if ([string]::IsNullOrWhiteSpace($root)) { continue }
        if (Test-WindowsLocalFrozenPathIsWithin $output $root) {
            throw 'Installed GPU observation report output cannot overlap local source, input, bundle, installation, or verifier paths.'
        }
    }
    return $output
}

function Remove-WindowsLocalFrozenVerifierTemporaryRoot([string]$Path, [string]$TemporaryBaseRoot) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $root = Get-WindowsLocalFrozenNormalizedFullPath $Path
    $temp = Get-WindowsLocalFrozenNormalizedFullPath $TemporaryBaseRoot
    if ((Split-Path -Parent $root) -cne $temp -or
        (Split-Path -Leaf $root) -cnotmatch '^scribe-local-frozen-installer-verification-[0-9a-f]{32}$') {
        throw 'Refused local frozen installer verifier cleanup outside its exact temporary root.'
    }
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $root
    $null = Assert-WindowsLocalFrozenRegularDirectory $root 'Local frozen installer verifier temporary'
    foreach ($item in @(Get-ChildItem -LiteralPath $root -Recurse -Force)) {
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refused local frozen installer verifier cleanup through a reparse point: $($item.FullName)"
        }
        Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $item.FullName
    }
    Remove-Item -LiteralPath $root -Recurse -Force
}

Assert-WindowsFrozenCpuWorkerLocalOnlyEnvironment
$repositoryRoot = Get-WindowsLocalFrozenNormalizedFullPath (Split-Path -Parent $PSScriptRoot)
if ($PSBoundParameters.ContainsKey('FrozenCpuWorkerSourceRoot') -and [string]::IsNullOrWhiteSpace($FrozenCpuWorkerSourceRoot)) {
    throw 'FrozenCpuWorkerSourceRoot was explicitly supplied but is empty or whitespace.'
}
$resolvedFrozenWorkerSourceRoot = if ($PSBoundParameters.ContainsKey('FrozenCpuWorkerSourceRoot')) {
    Get-WindowsLocalFrozenNormalizedFullPath $FrozenCpuWorkerSourceRoot
}
else {
    $repositoryRoot
}
$frozenCpuWorker = $null
$desktopSourceContext = $null
$temporaryRoot = $null
$temporaryBaseRoot = $null
$installedRoot = $null
$uninstaller = $null
$payloadParityConfirmed = $false
$observationRequest = Get-WindowsLocalFrozenCaptureObservationRequest $PSBoundParameters
$observationReport = $null
try {
    $desktopSourceContext = Get-WindowsFrozenCpuWorkerSourceContext $repositoryRoot
    $frozenCpuWorker = Open-ValidatedWindowsFrozenCpuWorker $FrozenCpuWorkerRecordPath $resolvedFrozenWorkerSourceRoot
    Assert-WindowsFrozenCpuWorkerCompatibleSourceContexts $desktopSourceContext $frozenCpuWorker.Context
    $temporaryBaseRoot = Get-WindowsFrozenCpuWorkerSafeTemporaryRoot $desktopSourceContext $frozenCpuWorker
    $bundle = Assert-WindowsLocalFrozenBundle $BundlePath $frozenCpuWorker
    Assert-WindowsLocalFrozenBundleCompiledAdmission $bundle $desktopSourceContext $frozenCpuWorker | Out-Null
    $installer = Get-WindowsLocalFrozenNormalizedFullPath $InstallerPath
    $recordPath = Get-WindowsLocalFrozenNormalizedFullPath $InstallerRecordPath
    if ((Split-Path -Parent $installer) -cne (Split-Path -Parent $recordPath)) {
        throw 'Local frozen installer and its record must be sibling files.'
    }
    $record = Assert-WindowsLocalFrozenInstallerRecord $recordPath $desktopSourceContext $frozenCpuWorker $bundle $installer
    $expectedInstallerDirectory = Get-WindowsLocalFrozenNormalizedFullPath (Split-Path -Parent $installer)
    $expectedRecordPath = Join-Path $expectedInstallerDirectory 'windows-local-frozen-test-installer-record.json'
    if ($recordPath -cne $expectedRecordPath) {
        throw 'Local frozen installer record must use its exact supported filename.'
    }
    $installedRoot = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)) ($record.install_relative_path -replace '/', '\')
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $installedRoot
    if (Test-Path -LiteralPath $installedRoot) {
        throw 'Local frozen installer verifier refuses to use or overwrite an existing local test installation.'
    }
    $observationPack = $null
    $observationOutput = $null
    if ($null -ne $observationRequest) {
        $wav = Assert-WindowsFrozenCpuWorkerRegularFile $observationRequest.WavPath
        if ($wav.Length -le 0 -or $wav.Length -gt 256MB) {
            throw 'Installed GPU observation WAV is outside the supported bounded input size.'
        }
        $observationPack = Get-WindowsLocalFrozenCapturePackBinding `
            -Bundle $bundle `
            -PackId $observationRequest.GpuPackId `
            -Backend $observationRequest.GpuBackend
        $observationOutput = Assert-WindowsLocalFrozenObservationOutputDestination `
            -Request $observationRequest `
            -ForbiddenRoots @(
                $repositoryRoot, $bundle.Root, $installedRoot,
                $resolvedFrozenWorkerSourceRoot, $frozenCpuWorker.Root,
                (Split-Path -Parent $installer),
                (Split-Path -Parent $recordPath),
                $observationRequest.WavPath
            )
    }
    $token = [guid]::NewGuid().ToString('N')
    $temporaryRoot = Join-Path $temporaryBaseRoot "scribe-local-frozen-installer-verification-$token"
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $temporaryRoot
    New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
    if ($null -ne $observationRequest) {
        $observationOutput = Assert-WindowsLocalFrozenObservationOutputDestination `
            -Request $observationRequest `
            -ForbiddenRoots @($repositoryRoot, $resolvedFrozenWorkerSourceRoot, $bundle.Root, $installedRoot, $temporaryRoot)
    }
    $installLog = Join-Path $temporaryRoot 'install.log'
    $install = Invoke-WindowsLocalFrozenInstallerProcess $installer @(
        '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', "/LOG=$installLog"
    ) 'local frozen installer'
    if ($install.ExitCode -ne 0) {
        throw "Local frozen installer exited with $($install.ExitCode): $($install.Stderr.Trim())"
    }
    $installedBundle = Assert-WindowsLocalFrozenPayloadParity $bundle.Root $installedRoot
    Assert-WindowsLocalFrozenBundleCompiledAdmission $installedBundle $desktopSourceContext $frozenCpuWorker | Out-Null
    $payloadParityConfirmed = $true
    $uninstaller = Join-Path $installedRoot 'unins000.exe'
    $modelManifest = Get-Content -LiteralPath (Join-Path $repositoryRoot 'runtime-manifests\whisper-base-en-q8_0-windows-x64.json') -Raw | ConvertFrom-Json
    $previousHubOffline = $env:HF_HUB_OFFLINE
    $previousTransformersOffline = $env:TRANSFORMERS_OFFLINE
    try {
        $env:HF_HUB_OFFLINE = '1'
        $env:TRANSFORMERS_OFFLINE = '1'
        $smokeArguments = Get-WindowsLocalFrozenSmokeArguments $installedRoot $modelManifest
        $smoke = Invoke-WindowsLocalFrozenInstallerProcess (Join-Path $installedRoot 'local-transcriber.exe') $smokeArguments 'installed local frozen CPU smoke'
    }
    finally {
        $env:HF_HUB_OFFLINE = $previousHubOffline
        $env:TRANSFORMERS_OFFLINE = $previousTransformersOffline
    }
    if ($smoke.ExitCode -ne 0 -or [string]::IsNullOrWhiteSpace($smoke.Stdout)) {
        throw "Installed local frozen CPU smoke failed with exit code $($smoke.ExitCode): $($smoke.Stderr.Trim())"
    }
    try {
        Assert-WindowsLocalFrozenSmokeDiagnostics ($smoke.Stdout | ConvertFrom-Json)
    }
    catch {
        throw "Installed local frozen CPU smoke returned invalid diagnostics: $($_.Exception.Message)"
    }
    if ($null -ne $observationRequest) {
        $collector = Get-WindowsLocalFrozenVerifiedInventoryFile $installedBundle 'local-transcriber.exe'
        $model = Get-WindowsLocalFrozenVerifiedInventoryFile $installedBundle 'whisper-base.en-Q8_0.gguf'
        $collectorPath = Join-Path $installedRoot ($collector.RelativePath -replace '/', '\')
        $modelPath = Join-Path $installedRoot ($model.RelativePath -replace '/', '\')
        foreach ($identity in @(
            [pscustomobject]@{ Path = $collectorPath; Size = $collector.SizeBytes; Sha256 = $collector.Sha256; Description = 'collector' },
            [pscustomobject]@{ Path = $modelPath; Size = $model.SizeBytes; Sha256 = $model.Sha256; Description = 'model' }
        )) {
            $item = Assert-WindowsFrozenCpuWorkerRegularFile $identity.Path
            if ($item.Length -ne [int64]$identity.Size -or
                (Get-WindowsFrozenCpuWorkerFileSha256 $identity.Path) -cne [string]$identity.Sha256) {
                throw "Installed GPU observation $($identity.Description) does not match the parity-verified inventory."
            }
        }
        $installedPack = Get-WindowsLocalFrozenCapturePackBinding `
            -Bundle $installedBundle `
            -PackId $observationRequest.GpuPackId `
            -Backend $observationRequest.GpuBackend
        foreach ($field in @('PackId', 'PackVersion', 'PackSha256', 'PackSecurityEpoch', 'RuntimeAbi', 'Backend', 'Provider')) {
            if ([string]$installedPack.$field -cne [string]$observationPack.$field) {
                throw "Installed GPU observation pack binding changed at $field after payload parity."
            }
        }
        $campaignMode = $null -ne $observationRequest.CampaignPower
        $temporaryObservationReport = Join-Path $temporaryRoot $(if ($campaignMode) { 'gpu-campaign.json' } else { 'gpu-observation.json' })
        $observationArguments = @(
            '--scribe-windows-gpu-capture-observation',
            '--model', $modelPath, '--model-sha256', $model.Sha256,
            '--wav', $observationRequest.WavPath, '--wav-sha256', $observationRequest.WavSha256,
            '--gpu-pack-id', $observationRequest.GpuPackId,
            '--gpu-backend', $observationRequest.GpuBackend,
            '--gpu-device', $observationRequest.GpuDevice
        )
        if ($campaignMode) {
            $observationArguments += @('--campaign-power', $observationRequest.CampaignPower)
        }
        $observationArguments += @('--output', $temporaryObservationReport)
        if ($campaignMode) {
            $observation = Invoke-WindowsLocalFrozenCampaignProcess `
                -Executable $collectorPath `
                -Arguments $observationArguments `
                -Description 'installed local frozen GPU observation campaign' `
                -TimeoutMilliseconds 900000 `
                -StreamDrainMilliseconds 5000
        }
        else {
            $observation = Invoke-WindowsLocalFrozenBoundedProcess `
                -Executable $collectorPath `
                -Arguments $observationArguments `
                -Description 'installed local frozen GPU observation' `
                -TimeoutMilliseconds 900000 `
                -StreamDrainMilliseconds 5000
        }
        if ($observation.ExitCode -ne 0) {
            if ($campaignMode) {
                throw "Installed local frozen GPU observation campaign failed with exit code $($observation.ExitCode)."
            }
            throw "Installed local frozen GPU observation failed with exit code $($observation.ExitCode): $($observation.Stderr.Trim())"
        }
        $expectedObservation = [pscustomobject]@{
            CollectorBuildRevision = $desktopSourceContext.SourceRevision
            ModelSha256 = $model.Sha256
            WavSha256 = $observationRequest.WavSha256
            PackId = $installedPack.PackId
            PackVersion = $installedPack.PackVersion
            PackSha256 = $installedPack.PackSha256
            PackSecurityEpoch = $installedPack.PackSecurityEpoch
            RuntimeAbi = $installedPack.RuntimeAbi
            Backend = $installedPack.Backend
            Provider = $installedPack.Provider
            StableDevice = $observationRequest.GpuDevice
        }
        if ($campaignMode) {
            $expectedObservation | Add-Member -NotePropertyName CampaignPower -NotePropertyValue $observationRequest.CampaignPower
            $observationReport = Read-WindowsLocalFrozenCaptureCampaignReport `
                -ReportPath $temporaryObservationReport `
                -Expected $expectedObservation
        }
        else {
            $observationReport = Read-WindowsLocalFrozenCaptureObservationReport `
                -ReportPath $temporaryObservationReport `
                -Expected $expectedObservation
        }
    }
    $uninstallLog = Join-Path $temporaryRoot 'uninstall.log'
    $uninstall = Invoke-WindowsLocalFrozenInstallerProcess $uninstaller @(
        '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=$uninstallLog"
    ) 'local frozen installer uninstaller'
    if ($uninstall.ExitCode -ne 0) {
        throw "Local frozen installer uninstaller exited with $($uninstall.ExitCode): $($uninstall.Stderr.Trim())"
    }
    Wait-WindowsLocalFrozenInstallRootRemoved $installedRoot
    $uninstaller = $null
    Assert-WindowsFrozenCpuWorkerContextUnchanged $desktopSourceContext
    Assert-WindowsFrozenCpuWorkerContextUnchanged $frozenCpuWorker.Context
    if ($null -ne $observationReport) {
        # The capture bytes are already retained in memory.  Remove every
        # verifier-owned scratch file before making a caller-visible report so
        # a cleanup failure cannot look like a successful observation.
        Remove-WindowsLocalFrozenVerifierTemporaryRoot $temporaryRoot $temporaryBaseRoot
        $temporaryRoot = $null
        $publishedReport = Publish-WindowsLocalFrozenNewReport `
            $observationOutput `
            $observationReport.Bytes `
            $(if ($campaignMode) { 32MB } else { 1MB })
        if ($campaignMode) {
            Write-Output "Local frozen installer payload parity, unsigned GPU campaign diagnostic validation, and uninstall cleanup completed: $($record.local_test_token) ($publishedReport)"
        }
        else {
            Write-Output "Local frozen installer payload parity, GPU observation, and uninstall cleanup passed: $($record.local_test_token) ($publishedReport)"
        }
    }
    else {
        Write-Output "Local frozen installer payload parity and uninstall cleanup passed: $($record.local_test_token)"
    }
}
finally {
    if ($payloadParityConfirmed -and $null -ne $uninstaller -and (Test-Path -LiteralPath $uninstaller -PathType Leaf)) {
        try {
            $cleanup = Invoke-WindowsLocalFrozenInstallerProcess $uninstaller @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') 'local frozen installer cleanup'
            if ($cleanup.ExitCode -ne 0) {
                Write-Warning "Local frozen installer cleanup exited with $($cleanup.ExitCode)."
            }
            else {
                Wait-WindowsLocalFrozenInstallRootRemoved $installedRoot
            }
        }
        catch {
            Write-Warning "Local frozen installer cleanup failed: $($_.Exception.Message)"
        }
    }
    elseif (-not $payloadParityConfirmed -and $null -ne $installedRoot -and (Test-Path -LiteralPath $installedRoot)) {
        Write-Warning "Local frozen installer verification retained the untrusted token-bound installation for inspection: $installedRoot"
    }
    if ($null -ne $temporaryRoot) {
        Remove-WindowsLocalFrozenVerifierTemporaryRoot $temporaryRoot $temporaryBaseRoot
    }
    if ($null -ne $frozenCpuWorker -and $null -ne $frozenCpuWorker.WorkerStream) {
        $frozenCpuWorker.WorkerStream.Dispose()
    }
}
