[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-frozen-cpu-worker-integrity.ps1')
. (Join-Path $PSScriptRoot 'windows-pe-imports.ps1')

function Assert-FrozenCpuWorkerOutputOutside([string]$OutputPath, [string]$ProtectedRoot) {
    if ([string]::Equals($OutputPath, $ProtectedRoot, [StringComparison]::OrdinalIgnoreCase) -or
        $OutputPath.StartsWith($ProtectedRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Frozen CPU worker output must be outside the checkout and Cargo target directories.'
    }
}

function Remove-OwnedFrozenCpuWorkerStaging([string]$StagingPath, [string]$ExpectedParent, [string]$ExpectedName) {
    if (-not (Test-Path -LiteralPath $StagingPath)) { return }
    $resolved = Get-WindowsFrozenCpuWorkerNormalizedFullPath $StagingPath
    if (-not [string]::Equals((Split-Path -Parent $resolved), $ExpectedParent, [StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path -Leaf $resolved) -cne $ExpectedName) {
        throw 'Refused frozen CPU worker cleanup outside the exact owned staging directory.'
    }
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $resolved
    # Do not traverse an unexpected directory or link during failure cleanup.
    $expected = @(
        (Get-WindowsFrozenCpuWorkerExecutableRelativePath),
        (Get-WindowsFrozenCpuWorkerRecordFileName),
        (Get-WindowsFrozenCpuWorkerMarkerFileName)
    )
    foreach ($item in @(Get-ChildItem -LiteralPath $resolved -Force)) {
        if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
            $item.Name -cnotin $expected) {
            throw 'Refused automatic cleanup of unexpected frozen CPU worker staging content.'
        }
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}

if (-not [Environment]::Is64BitOperatingSystem -or
    [Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'Frozen CPU worker construction supports only Windows x64.'
}
if ($env:GITHUB_ACTIONS -eq 'true' -or $env:CI -eq 'true') {
    throw 'Frozen CPU worker construction is local-only and cannot run as a hosted release input.'
}

$repositoryRoot = Get-WindowsFrozenCpuWorkerNormalizedFullPath (Split-Path -Parent $PSScriptRoot)
$context = Get-WindowsFrozenCpuWorkerSourceContext $repositoryRoot
$targetTriple = Get-WindowsFrozenCpuWorkerTargetTriple
$defaultTargetRoot = Get-WindowsFrozenCpuWorkerNormalizedFullPath (Join-Path $repositoryRoot 'target')
$cargoTargetRoot = if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    $defaultTargetRoot
} else {
    $targetCandidate = if ([IO.Path]::IsPathFullyQualified($env:CARGO_TARGET_DIR)) {
        $env:CARGO_TARGET_DIR
    } else {
        Join-Path $repositoryRoot $env:CARGO_TARGET_DIR
    }
    Get-WindowsFrozenCpuWorkerNormalizedFullPath $targetCandidate
}
Assert-WindowsFrozenCpuWorkerNoReparseAncestors $cargoTargetRoot

$finalRoot = Get-WindowsFrozenCpuWorkerNormalizedFullPath $OutputDirectory
$pathRoot = [IO.Path]::GetPathRoot($finalRoot)
if ($finalRoot.Substring($pathRoot.Length).Contains(':')) {
    throw 'Frozen CPU worker output cannot name an alternate data stream.'
}
$outputParent = Split-Path -Parent $finalRoot
$outputName = Split-Path -Leaf $finalRoot
if (-not $outputParent -or -not $outputName -or
    -not (Test-Path -LiteralPath $outputParent -PathType Container)) {
    throw 'Frozen CPU worker output must be a named directory beneath an existing parent.'
}
foreach ($protectedRoot in @($repositoryRoot, $defaultTargetRoot, $cargoTargetRoot)) {
    Assert-FrozenCpuWorkerOutputOutside $finalRoot $protectedRoot
}
Assert-WindowsFrozenCpuWorkerNoReparseAncestors $finalRoot
if (Test-Path -LiteralPath $finalRoot) {
    throw 'Frozen CPU worker output already exists; it will not be overwritten.'
}

$stagingName = "$outputName.staging-$PID-$([guid]::NewGuid().ToString('N'))"
$stagingRoot = Join-Path $outputParent $stagingName
$sourceWorker = Join-Path $cargoTargetRoot "$targetTriple\release\scribe-inference-worker.exe"
$previousRevision = $env:SCRIBE_BUILD_REVISION
$previousWorkerDigest = $env:SCRIBE_BUNDLED_WORKER_SHA256
$previousBuildingWorker = $env:SCRIBE_BUILDING_WORKER
$workerStream = $null
$stagingOwned = $false
$locationPushed = $false
try {
    $env:SCRIBE_BUILD_REVISION = $context.SourceRevision
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $null
    $env:SCRIBE_BUILDING_WORKER = '1'
    Push-Location $repositoryRoot
    $locationPushed = $true
    & cargo build --locked --offline --release --bin scribe-inference-worker --features inference-worker --target $targetTriple --manifest-path (Join-Path $repositoryRoot 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) {
        throw 'The locked offline Windows x64 CPU inference worker release build failed.'
    }

    $workerStream = Open-WindowsFrozenCpuWorkerReadHandle $sourceWorker
    if ($workerStream.Length -le 0 -or $workerStream.Length -gt (Get-WindowsFrozenCpuWorkerMaximumBytes)) {
        throw 'Frozen CPU worker size is outside the supported byte bounds.'
    }
    $null = Assert-ReviewedWindowsPe $sourceWorker 3
    $workerHash = Get-WindowsFrozenCpuWorkerOpenStreamSha256 $workerStream
    Assert-WindowsFrozenCpuWorkerContextUnchanged $context
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $outputParent
    if (Test-Path -LiteralPath $finalRoot) {
        throw 'Frozen CPU worker output appeared during construction; refusing to replace it.'
    }
    $null = New-Item -ItemType Directory -Path $stagingRoot
    $stagingOwned = $true
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $stagingRoot
    $stagedWorker = Join-Path $stagingRoot (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
    Copy-WindowsFrozenCpuWorkerOpenHandle $workerStream $stagedWorker
    $record = New-WindowsFrozenCpuWorkerRecord $context $workerStream.Length $workerHash
    $recordPath = Join-Path $stagingRoot (Get-WindowsFrozenCpuWorkerRecordFileName)
    Write-WindowsFrozenCpuWorkerAtomicUtf8File $recordPath ($record | ConvertTo-Json -Depth 5)
    $recordHash = Get-WindowsFrozenCpuWorkerFileSha256 $recordPath
    Write-WindowsFrozenCpuWorkerAtomicUtf8File `
        (Join-Path $stagingRoot (Get-WindowsFrozenCpuWorkerMarkerFileName)) `
        (Get-WindowsFrozenCpuWorkerMarkerText $recordHash ([pscustomobject]$record))

    # Exercise the same bounded record/inventory/byte checks used by packaging.
    $validated = Open-ValidatedWindowsFrozenCpuWorker $recordPath $repositoryRoot
    try {
        if ($validated.WorkerStream.Length -ne $workerStream.Length -or
            $validated.Record.worker_sha256 -cne $workerHash) {
            throw 'Frozen CPU worker staging did not preserve the built worker bytes.'
        }
    }
    finally {
        $validated.WorkerStream.Dispose()
    }
    Assert-WindowsFrozenCpuWorkerContextUnchanged $context
    Assert-WindowsFrozenCpuWorkerDirectoryInventory $stagingRoot
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $outputParent
    # Unlike Move-Item, Directory.Move never nests staging inside an existing destination.
    [IO.Directory]::Move($stagingRoot, $finalRoot)
    $stagingOwned = $false
    Write-Output "Local-only frozen CPU worker ready: $finalRoot"
    Write-Output "Worker SHA-256: $workerHash"
    Write-Output 'This unsigned integrity record does not authorize release publication.'
}
finally {
    if ($null -ne $workerStream) { $workerStream.Dispose() }
    $env:SCRIBE_BUILD_REVISION = $previousRevision
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $previousWorkerDigest
    $env:SCRIBE_BUILDING_WORKER = $previousBuildingWorker
    if ($locationPushed) { Pop-Location }
    if ($stagingOwned) {
        try {
            Remove-OwnedFrozenCpuWorkerStaging $stagingRoot $outputParent $stagingName
        }
        catch {
            Write-Warning "Owned frozen CPU worker staging was retained for inspection: $($_.Exception.Message)"
        }
    }
}
