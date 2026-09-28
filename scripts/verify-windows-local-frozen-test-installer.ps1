param(
    [Parameter(Mandatory = $true)]
    [string]$BundlePath,
    [Parameter(Mandatory = $true)]
    [string]$FrozenCpuWorkerRecordPath,
    [Parameter(Mandatory = $true)]
    [string]$InstallerPath,
    [Parameter(Mandatory = $true)]
    [string]$InstallerRecordPath
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

function Remove-WindowsLocalFrozenVerifierTemporaryRoot([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $root = Get-WindowsLocalFrozenNormalizedFullPath $Path
    $temp = Get-WindowsLocalFrozenNormalizedFullPath ([System.IO.Path]::GetTempPath())
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
$frozenCpuWorker = $null
$temporaryRoot = $null
$installedRoot = $null
$uninstaller = $null
$payloadParityConfirmed = $false
try {
    $frozenCpuWorker = Open-ValidatedWindowsFrozenCpuWorker $FrozenCpuWorkerRecordPath $repositoryRoot
    $bundle = Assert-WindowsLocalFrozenBundle $BundlePath $frozenCpuWorker
    $installer = Get-WindowsLocalFrozenNormalizedFullPath $InstallerPath
    $recordPath = Get-WindowsLocalFrozenNormalizedFullPath $InstallerRecordPath
    if ((Split-Path -Parent $installer) -cne (Split-Path -Parent $recordPath)) {
        throw 'Local frozen installer and its record must be sibling files.'
    }
    $record = Assert-WindowsLocalFrozenInstallerRecord $recordPath $frozenCpuWorker $bundle $installer
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
    $token = [guid]::NewGuid().ToString('N')
    $temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-local-frozen-installer-verification-$token"
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $temporaryRoot
    New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
    $installLog = Join-Path $temporaryRoot 'install.log'
    $install = Invoke-WindowsLocalFrozenInstallerProcess $installer @(
        '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', "/LOG=$installLog"
    ) 'local frozen installer'
    if ($install.ExitCode -ne 0) {
        throw "Local frozen installer exited with $($install.ExitCode): $($install.Stderr.Trim())"
    }
    Assert-WindowsLocalFrozenPayloadParity $bundle.Root $installedRoot
    $payloadParityConfirmed = $true
    $uninstaller = Join-Path $installedRoot 'unins000.exe'
    $modelManifest = Get-Content -LiteralPath (Join-Path $repositoryRoot 'runtime-manifests\whisper-base-en-q8_0-windows-x64.json') -Raw | ConvertFrom-Json
    $previousHubOffline = $env:HF_HUB_OFFLINE
    $previousTransformersOffline = $env:TRANSFORMERS_OFFLINE
    try {
        $env:HF_HUB_OFFLINE = '1'
        $env:TRANSFORMERS_OFFLINE = '1'
        $smoke = Invoke-WindowsLocalFrozenInstallerProcess (Join-Path $installedRoot 'local-transcriber.exe') @(
            '--scribe-install-smoke-parent',
            [string]$modelManifest.model_id,
            (Join-Path $installedRoot [string]$modelManifest.artifact_filename),
            'gguf',
            [string]$modelManifest.size_bytes,
            [string]$modelManifest.sha256,
            'cpu'
        ) 'installed local frozen CPU smoke'
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
    $uninstallLog = Join-Path $temporaryRoot 'uninstall.log'
    $uninstall = Invoke-WindowsLocalFrozenInstallerProcess $uninstaller @(
        '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=$uninstallLog"
    ) 'local frozen installer uninstaller'
    if ($uninstall.ExitCode -ne 0) {
        throw "Local frozen installer uninstaller exited with $($uninstall.ExitCode): $($uninstall.Stderr.Trim())"
    }
    Wait-WindowsLocalFrozenInstallRootRemoved $installedRoot
    $uninstaller = $null
    Write-Output "Local frozen installer payload parity and uninstall cleanup passed: $($record.local_test_token)"
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
        Remove-WindowsLocalFrozenVerifierTemporaryRoot $temporaryRoot
    }
    if ($null -ne $frozenCpuWorker -and $null -ne $frozenCpuWorker.WorkerStream) {
        $frozenCpuWorker.WorkerStream.Dispose()
    }
}
