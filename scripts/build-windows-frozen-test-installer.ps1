param(
    [Parameter(Mandatory = $true)]
    [string]$BundlePath,
    [Parameter(Mandatory = $true)]
    [string]$FrozenCpuWorkerRecordPath,
    [Parameter(Mandatory = $true)]
    [string]$InnoCompilerPath,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'windows-frozen-cpu-worker-integrity.ps1')
. (Join-Path $PSScriptRoot 'windows-local-frozen-installer-integrity.ps1')

function Assert-WindowsLocalFrozenInnoCompiler(
    [string]$Path,
    [string]$ProvenancePath
) {
    $provenance = Read-WindowsFrozenCpuWorkerBoundedUtf8File $ProvenancePath 65536
    try {
        $value = $provenance.Text | ConvertFrom-Json -Depth 5
    }
    catch {
        throw "Pinned Inno Setup provenance is not valid JSON: $($_.Exception.Message)"
    }
    Assert-WindowsLocalFrozenExactProperties $value @(
        'schema_version', 'product', 'product_version', 'reviewed_utc_date',
        'package_url', 'package_size_bytes', 'package_sha256',
        'embedded_installer_path', 'embedded_installer_size_bytes',
        'embedded_installer_sha256', 'compiler_relative_path',
        'compiler_size_bytes', 'compiler_sha256',
        'embedded_package_verification_path', 'upstream_installer_url',
        'verification_method', 'trust_scope'
    ) 'Pinned Inno Setup provenance'
    if ((($value.schema_version -isnot [int64] -and $value.schema_version -isnot [int32]) -or
        [int]$value.schema_version -ne 1 -or
        $value.product -cne 'Inno Setup' -or $value.product_version -cne '6.7.1' -or
        $value.compiler_relative_path -cne 'ISCC.exe' -or
        $value.compiler_sha256 -isnot [string] -or $value.compiler_sha256 -cnotmatch '^[0-9a-f]{64}$')) {
        throw 'Pinned Inno Setup provenance does not describe the reviewed 6.7.1 compiler.'
    }
    $compiler = Get-WindowsLocalFrozenNormalizedFullPath $Path
    if ((Split-Path -Leaf $compiler) -cne $value.compiler_relative_path) {
        throw 'Pinned Inno Setup compiler must use the reviewed ISCC.exe filename.'
    }
    $item = Assert-WindowsFrozenCpuWorkerRegularFile $compiler
    $expectedSize = Assert-WindowsLocalFrozenInt64 $value.compiler_size_bytes 'Pinned Inno Setup compiler size'
    if ($item.Length -ne $expectedSize -or
        (Get-WindowsFrozenCpuWorkerFileSha256 $compiler) -cne $value.compiler_sha256) {
        throw 'Pinned Inno Setup compiler does not match its reviewed size and SHA-256.'
    }
    return $compiler
}

function Assert-WindowsLocalFrozenOutputPath(
    [string]$FinalPath,
    [string[]]$ProtectedPaths
) {
    $final = Get-WindowsLocalFrozenNormalizedFullPath $FinalPath
    $parent = Split-Path -Parent $final
    $leaf = Split-Path -Leaf $final
    if (-not $parent -or -not $leaf) {
        throw 'Local frozen installer output must be a named directory below a filesystem root.'
    }
    foreach ($protectedPath in $ProtectedPaths) {
        if ((Test-WindowsLocalFrozenPathIsWithin $final $protectedPath) -or
            (Test-WindowsLocalFrozenPathIsWithin $protectedPath $final)) {
            throw 'Local frozen installer output cannot overlap source, bundle, or frozen-worker inputs.'
        }
    }
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $parent
    if (Test-Path -LiteralPath $final) {
        throw "Local frozen installer output already exists; archive or remove it explicitly first: $final"
    }
    return $final
}

function Assert-WindowsLocalFrozenStagingPath([string]$StagingPath, [string]$FinalPath) {
    $staging = Get-WindowsLocalFrozenNormalizedFullPath $StagingPath
    $final = Get-WindowsLocalFrozenNormalizedFullPath $FinalPath
    if (-not [string]::Equals((Split-Path -Parent $staging), (Split-Path -Parent $final), [System.StringComparison]::OrdinalIgnoreCase)) {
        throw 'Local frozen installer staging must be a direct sibling of its final output.'
    }
    $pattern = '^{0}\.staging-[0-9]+-[0-9a-f]{{32}}$' -f [regex]::Escape((Split-Path -Leaf $final))
    if ((Split-Path -Leaf $staging) -cnotmatch $pattern) {
        throw 'Local frozen installer staging path is outside its bounded transaction naming scheme.'
    }
}

function Remove-WindowsLocalFrozenOwnedStaging([string]$StagingPath, [string]$FinalPath) {
    if (-not (Test-Path -LiteralPath $StagingPath)) {
        return
    }
    Assert-WindowsLocalFrozenStagingPath $StagingPath $FinalPath
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $StagingPath
    $null = Assert-WindowsLocalFrozenRegularDirectory $StagingPath 'Local frozen installer staging'
    foreach ($item in @(Get-ChildItem -LiteralPath $StagingPath -Recurse -Force)) {
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refused local frozen installer staging cleanup through a reparse point: $($item.FullName)"
        }
        Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $item.FullName
    }
    Remove-Item -LiteralPath $StagingPath -Recurse -Force
}

function Copy-WindowsLocalFrozenBundle([string]$SourceRoot, [string]$DestinationRoot) {
    if (Test-Path -LiteralPath $DestinationRoot) {
        throw "Local frozen installer staging payload unexpectedly already exists: $DestinationRoot"
    }
    New-Item -ItemType Directory -Path $DestinationRoot | Out-Null
    foreach ($item in @(Get-ChildItem -LiteralPath $SourceRoot -Force)) {
        Copy-Item -LiteralPath $item.FullName -Destination (Join-Path $DestinationRoot $item.Name) -Recurse
    }
}

function Open-WindowsLocalFrozenPayloadReadHandles([string]$Root, [string[]]$Files) {
    $handles = [System.Collections.Generic.List[System.IO.FileStream]]::new()
    try {
        foreach ($path in @($Files | Sort-Object)) {
            $handles.Add((Open-WindowsFrozenCpuWorkerReadHandle (Join-Path $Root ($path -replace '/', '\'))))
        }
        return $handles
    }
    catch {
        foreach ($handle in $handles) { $handle.Dispose() }
        throw
    }
}

function Assert-WindowsLocalFrozenCompilerOutput([string]$OutputRoot, [string]$ExpectedInstallerName) {
    $null = Assert-WindowsLocalFrozenRegularDirectory $OutputRoot 'Local frozen installer compiler output'
    $items = @(Get-ChildItem -LiteralPath $OutputRoot -Force)
    if ($items.Count -ne 1 -or $items[0].PSIsContainer -or $items[0].Name -cne $ExpectedInstallerName) {
        throw 'Pinned Inno Setup compiler did not produce exactly the expected local frozen installer executable.'
    }
    $installer = Assert-WindowsFrozenCpuWorkerRegularFile $items[0].FullName
    if ($installer.Length -lt 1024 -or $installer.Length -gt (Get-WindowsFrozenCpuWorkerMaximumBytes)) {
        throw 'Pinned Inno Setup compiler produced an installer outside the supported byte bounds.'
    }
    $stream = [System.IO.File]::Open($installer.FullName, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
    try {
        if ($stream.ReadByte() -ne 0x4D -or $stream.ReadByte() -ne 0x5A) {
            throw 'Pinned Inno Setup compiler output is not a Windows executable.'
        }
    }
    finally {
        $stream.Dispose()
    }
    return $installer
}

function New-WindowsLocalFrozenInstallerRecord(
    [psobject]$FrozenCpuWorker,
    [psobject]$Bundle,
    [string]$InstallerName,
    [System.IO.FileInfo]$Installer,
    [string]$Token
) {
    return [ordered]@{
        schema_version = 1
        kind = 'windows-local-frozen-test-installer'
        local_only = $true
        release_approved = $false
        source_revision = [string]$FrozenCpuWorker.Record.source_revision
        app_version = [string]$FrozenCpuWorker.Record.app_version
        target_triple = [string]$FrozenCpuWorker.Record.target_triple
        frozen_record_sha256 = [string]$FrozenCpuWorker.RecordSha256
        bundle_inventory_sha256 = [string]$Bundle.InventorySha256
        installer_filename = $InstallerName
        installer_size_bytes = [int64]$Installer.Length
        installer_sha256 = (Get-WindowsFrozenCpuWorkerFileSha256 $Installer.FullName)
        local_test_token = $Token
        install_relative_path = "Scribe/LOCAL-Frozen-Test/$Token"
    }
}

Assert-WindowsFrozenCpuWorkerLocalOnlyEnvironment
$repositoryRoot = Get-WindowsLocalFrozenNormalizedFullPath (Split-Path -Parent $PSScriptRoot)
$templatePath = Join-Path $repositoryRoot 'installer\scribe-local-frozen.iss'
$provenance = Join-Path $repositoryRoot 'installer\inno-setup-6.7.1-provenance.json'
$null = Assert-WindowsFrozenCpuWorkerRegularFile $templatePath
$compiler = Assert-WindowsLocalFrozenInnoCompiler $InnoCompilerPath $provenance
$frozenCpuWorker = $null
$payloadReadHandles = $null
$staging = $null
try {
    # Record validation also confirms the current source revision and retains the
    # worker read handle. This is local byte integrity, never release authority.
    $frozenCpuWorker = Open-ValidatedWindowsFrozenCpuWorker $FrozenCpuWorkerRecordPath $repositoryRoot
    $bundle = Assert-WindowsLocalFrozenBundle $BundlePath $frozenCpuWorker
    $finalOutput = Assert-WindowsLocalFrozenOutputPath $OutputDirectory @(
        $repositoryRoot, $bundle.Root, $frozenCpuWorker.Root
    )
    $outputParent = Split-Path -Parent $finalOutput
    if (-not (Test-Path -LiteralPath $outputParent)) {
        New-Item -ItemType Directory -Path $outputParent | Out-Null
    }
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $outputParent
    $stale = @(Get-ChildItem -LiteralPath $outputParent -Force | Where-Object {
        $_.Name -like "$(Split-Path -Leaf $finalOutput).staging-*"
    })
    if ($stale.Count -gt 0) {
        throw "A stale local frozen installer staging sibling exists; inspect it explicitly: $($stale[0].FullName)"
    }
    $staging = Join-Path $outputParent "$(Split-Path -Leaf $finalOutput).staging-$PID-$([guid]::NewGuid().ToString('N'))"
    Assert-WindowsLocalFrozenStagingPath $staging $finalOutput
    if (Test-Path -LiteralPath $staging) {
        throw 'Unique local frozen installer staging path unexpectedly exists.'
    }
    New-Item -ItemType Directory -Path $staging | Out-Null
    $payloadStaging = Join-Path $staging 'payload'
    Copy-WindowsLocalFrozenBundle $bundle.Root $payloadStaging
    $copiedBundle = Assert-WindowsLocalFrozenBundle $payloadStaging $frozenCpuWorker
    $sourceAfterCopy = Assert-WindowsLocalFrozenBundle $bundle.Root $frozenCpuWorker
    if ($sourceAfterCopy.InventorySha256 -cne $bundle.InventorySha256 -or
        $copiedBundle.InventorySha256 -cne $bundle.InventorySha256) {
        throw 'Local frozen installer source bundle changed while staging; refusing to compile from drifted bytes.'
    }
    $payloadReadHandles = Open-WindowsLocalFrozenPayloadReadHandles $payloadStaging (@($copiedBundle.Files) + @('bundle-inventory.json'))
    Assert-WindowsFrozenCpuWorkerContextUnchanged $frozenCpuWorker.Context

    $compilerOutput = Join-Path $staging 'compiler-output'
    New-Item -ItemType Directory -Path $compilerOutput | Out-Null
    $token = [guid]::NewGuid().ToString('N')
    $installerName = "Scribe-LOCAL-Frozen-Test-$token.exe"
    & $compiler `
        "/DLocalFrozenBundleRoot=$payloadStaging" `
        "/DLocalFrozenInstallerOutputRoot=$compilerOutput" `
        "/DLocalFrozenTestToken=$token" `
        "/DAppVersion=$($frozenCpuWorker.Record.app_version)" `
        $templatePath
    if ($LASTEXITCODE -ne 0) {
        throw "Pinned Inno Setup compilation failed with exit code $LASTEXITCODE."
    }
    $installer = Assert-WindowsLocalFrozenCompilerOutput $compilerOutput $installerName
    try {
        $stagedAfterCompile = Assert-WindowsLocalFrozenBundle $payloadStaging $frozenCpuWorker
    }
    catch {
        throw "Local frozen installer staging payload revalidation failed after compilation; refusing to publish it. Original validation error: $($_.Exception.Message)"
    }
    if ($stagedAfterCompile.InventorySha256 -cne $bundle.InventorySha256) {
        throw 'Local frozen installer staging payload changed during compilation; refusing to publish it.'
    }
    try {
        $sourceAfterCompile = Assert-WindowsLocalFrozenBundle $bundle.Root $frozenCpuWorker
    }
    catch {
        throw "Local frozen installer source bundle revalidation failed after compilation; refusing to publish the staged installer. Original validation error: $($_.Exception.Message)"
    }
    if ($sourceAfterCompile.InventorySha256 -cne $bundle.InventorySha256) {
        throw 'Local frozen installer source bundle changed during compilation; refusing to publish the staged installer.'
    }
    Assert-WindowsFrozenCpuWorkerContextUnchanged $frozenCpuWorker.Context
    foreach ($handle in $payloadReadHandles) { $handle.Dispose() }
    $payloadReadHandles = $null
    $publish = Join-Path $staging 'publish'
    New-Item -ItemType Directory -Path $publish | Out-Null
    $publishedInstaller = Join-Path $publish $installerName
    Copy-Item -LiteralPath $installer.FullName -Destination $publishedInstaller
    $publishedInstallerItem = Assert-WindowsFrozenCpuWorkerRegularFile $publishedInstaller
    if ($publishedInstallerItem.Length -ne $installer.Length -or
        (Get-WindowsFrozenCpuWorkerFileSha256 $publishedInstaller) -cne (Get-WindowsFrozenCpuWorkerFileSha256 $installer.FullName)) {
        throw 'Local frozen installer output bytes changed while publishing the compiler result.'
    }
    $record = New-WindowsLocalFrozenInstallerRecord $frozenCpuWorker $bundle $installerName $publishedInstallerItem $token
    $recordPath = Join-Path $publish 'windows-local-frozen-test-installer-record.json'
    Write-WindowsFrozenCpuWorkerAtomicUtf8File $recordPath ($record | ConvertTo-Json -Depth 5)
    $publishedItems = @(Get-ChildItem -LiteralPath $publish -Force | ForEach-Object { $_.Name } | Sort-Object)
    $expectedPublishedItems = @($installerName, 'windows-local-frozen-test-installer-record.json' | Sort-Object)
    if ($publishedItems.Count -ne $expectedPublishedItems.Count -or
        (Compare-Object -ReferenceObject $expectedPublishedItems -DifferenceObject $publishedItems -CaseSensitive)) {
        throw 'Local frozen installer publish directory contains unexpected files.'
    }
    if (Test-Path -LiteralPath $finalOutput) {
        throw 'Local frozen installer final output appeared during staging; refusing to replace it.'
    }
    [System.IO.Directory]::Move($publish, $finalOutput)
    Remove-WindowsLocalFrozenOwnedStaging $staging $finalOutput
    $staging = $null
    [pscustomobject]@{
        OutputDirectory = $finalOutput
        InstallerPath = Join-Path $finalOutput $installerName
        RecordPath = Join-Path $finalOutput 'windows-local-frozen-test-installer-record.json'
        InstallerSha256 = [string]$record.installer_sha256
        BundleInventorySha256 = [string]$record.bundle_inventory_sha256
        LocalTestToken = $token
    }
}
catch {
    if ($null -ne $payloadReadHandles) {
        foreach ($handle in $payloadReadHandles) { $handle.Dispose() }
        $payloadReadHandles = $null
    }
    if ($null -ne $staging) {
        try {
            Remove-WindowsLocalFrozenOwnedStaging $staging $OutputDirectory
        }
        catch {
            Write-Warning "Refused local frozen installer staging cleanup because its bounds could not be proven: $($_.Exception.Message)"
        }
    }
    throw
}
finally {
    if ($null -ne $payloadReadHandles) {
        foreach ($handle in $payloadReadHandles) { $handle.Dispose() }
    }
    if ($null -ne $frozenCpuWorker -and $null -ne $frozenCpuWorker.WorkerStream) {
        $frozenCpuWorker.WorkerStream.Dispose()
    }
}
