[CmdletBinding()]
param(
    [string]$BundlePath,
    [string]$OutputDirectory,
    [switch]$FunctionsOnly
)

# Dot-sourced helpers have their own param blocks. Preserve this script's entry
# arguments before loading them so a helper cannot replace the caller's values.
$windowsCpuExportEntryArguments = [ordered]@{
    BundlePath = $BundlePath
    OutputDirectory = $OutputDirectory
}
$windowsCpuExportFunctionsOnly = $FunctionsOnly.IsPresent

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'windows-frozen-cpu-worker-integrity.ps1')
. (Join-Path $PSScriptRoot 'windows-pe-imports.ps1')
. (Join-Path $PSScriptRoot 'invoke-windows-gpu-approved-signing.ps1') -FunctionsOnly

$script:WindowsCpuArtifactRepository = 'tyhuang9/scribe'
$script:WindowsCpuArtifactRef = 'refs/heads/main'
$script:WindowsCpuArtifactWorkflow = '.github/workflows/release.yml'
$script:WindowsCpuArtifactWorkflowName = 'Build Windows installer'
$script:WindowsCpuArtifactRecordName = 'windows-ci-cpu-worker.json'
$script:WindowsCpuArtifactWorkerName = 'scribe-inference-worker.exe'
$script:WindowsCpuArtifactDesktopName = 'local-transcriber.exe'
$script:WindowsCpuArtifactLocalMarker = 'WINDOWS-FROZEN-CPU-WORKER-LOCAL-ONLY.txt'

function Assert-WindowsCpuArtifactExportCondition([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-WindowsCpuArtifactExportExactKeys($Value, [string[]]$Names, [string]$Label) {
    Assert-WindowsCpuArtifactExportCondition ($Value -is [System.Collections.IDictionary]) "$Label must be one JSON object."
    $actual = @($Value.Keys | ForEach-Object { [string]$_ } | Sort-Object -CaseSensitive)
    $expected = @($Names | Sort-Object -CaseSensitive)
    Assert-WindowsCpuArtifactExportCondition `
        ($actual.Count -eq $expected.Count -and -not (Compare-Object $expected $actual -CaseSensitive)) `
        "$Label has unexpected or missing fields."
}

function Assert-WindowsCpuArtifactExportInteger($Value, [int64]$Minimum, [int64]$Maximum, [string]$Label) {
    Assert-WindowsCpuArtifactExportCondition `
        (($Value -is [int32] -or $Value -is [int64]) -and [int64]$Value -ge $Minimum -and [int64]$Value -le $Maximum) `
        "$Label must be a bounded integer."
    return [int64]$Value
}

function Get-WindowsCpuArtifactExportSourceContext([string]$RepositoryRoot) {
    return Get-WindowsFrozenCpuWorkerSourceContext $RepositoryRoot
}

function Assert-WindowsCpuArtifactExportCompiledAdmission(
    [string]$DesktopPath,
    [int64]$DesktopSize,
    [string]$DesktopSha256,
    [psobject]$Context,
    [string]$WorkerSha256
) {
    $frozen = [pscustomobject]@{
        Context = $Context
        Record = [pscustomobject]@{ worker_sha256 = $WorkerSha256 }
    }
    return Assert-WindowsFrozenCpuWorkerCompiledAdmission `
        -Executable $DesktopPath `
        -ExpectedSize $DesktopSize `
        -ExpectedSha256 $DesktopSha256 `
        -DesktopContext $Context `
        -FrozenCpuWorker $frozen
}

function Get-WindowsCpuArtifactExportRecordProperties {
    return @(
        'schema_version',
        'kind',
        'source_repository',
        'source_ref',
        'workflow',
        'run_id',
        'run_attempt',
        'source_revision',
        'app_version',
        'target_triple',
        'protocol_version',
        'worker_abi_version',
        'desktop_build_id',
        'worker_build_id',
        'cargo_lock_sha256',
        'rust_toolchain_sha256',
        'cargo_manifest_sha256',
        'worker_identity_sha256',
        'build_rs_sha256',
        'build_contract_sha256',
        'worker_relative_path',
        'worker_size_bytes',
        'worker_sha256'
    )
}

function New-WindowsCiCpuWorkerRecord(
    [psobject]$Context,
    [string]$RunId,
    [string]$RunAttempt,
    [int64]$WorkerSize,
    [string]$WorkerSha256
) {
    Assert-SigningId $RunId 'CPU worker producer run ID'
    Assert-SigningId $RunAttempt 'CPU worker producer run attempt'
    Assert-SigningHash $WorkerSha256 'CPU worker SHA-256'
    Assert-WindowsCpuArtifactExportCondition `
        ($WorkerSize -gt 0 -and $WorkerSize -le (Get-WindowsFrozenCpuWorkerMaximumBytes)) `
        'CPU worker size is outside the supported artifact bound.'
    return [ordered]@{
        schema_version = 1
        kind = 'windows-ci-cpu-worker'
        source_repository = $script:WindowsCpuArtifactRepository
        source_ref = $script:WindowsCpuArtifactRef
        workflow = $script:WindowsCpuArtifactWorkflow
        run_id = $RunId
        run_attempt = $RunAttempt
        source_revision = $Context.SourceRevision
        app_version = $Context.AppVersion
        target_triple = $Context.TargetTriple
        protocol_version = [int64]$Context.ProtocolVersion
        worker_abi_version = [int64]$Context.WorkerAbiVersion
        desktop_build_id = $Context.DesktopBuildId
        worker_build_id = $Context.WorkerBuildId
        cargo_lock_sha256 = $Context.CargoLockSha256
        rust_toolchain_sha256 = $Context.RustToolchainSha256
        cargo_manifest_sha256 = $Context.CargoManifestSha256
        worker_identity_sha256 = $Context.WorkerIdentitySha256
        build_rs_sha256 = $Context.BuildRsSha256
        build_contract_sha256 = $Context.BuildContractSha256
        worker_relative_path = $script:WindowsCpuArtifactWorkerName
        worker_size_bytes = $WorkerSize
        worker_sha256 = $WorkerSha256
    }
}

function Get-WindowsCpuArtifactExportInventoryEntry($Inventory, [string]$ExpectedPath) {
    Assert-WindowsCpuArtifactExportExactKeys $Inventory @('schema_version', 'platform_triple', 'files') 'Bundle inventory'
    Assert-WindowsCpuArtifactExportCondition `
        (($Inventory.schema_version -is [int64] -or $Inventory.schema_version -is [int32]) -and [int64]$Inventory.schema_version -eq 1) `
        'Bundle inventory schema is unsupported.'
    Assert-WindowsCpuArtifactExportCondition `
        ($Inventory.platform_triple -is [string] -and $Inventory.platform_triple -ceq (Get-WindowsFrozenCpuWorkerTargetTriple)) `
        'Bundle inventory platform is unsupported.'
    $entries = @($Inventory.files)
    Assert-WindowsCpuArtifactExportCondition ($entries.Count -gt 0 -and $entries.Count -le 4096) 'Bundle inventory entry count is invalid.'
    $paths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $match = $null
    foreach ($entry in $entries) {
        Assert-WindowsCpuArtifactExportExactKeys $entry @('path', 'size_bytes', 'sha256') 'Bundle inventory entry'
        Assert-WindowsCpuArtifactExportCondition ($entry.path -is [string]) 'Bundle inventory path must be a string.'
        Assert-WindowsCpuArtifactExportCondition `
            (-not [string]::IsNullOrWhiteSpace($entry.path) -and -not [IO.Path]::IsPathRooted($entry.path) -and
                -not $entry.path.Contains('\') -and -not $entry.path.Contains(':') -and
                @($entry.path.Split('/') | Where-Object { $_ -in @('', '.', '..') }).Count -eq 0) `
            'Bundle inventory contains an unsafe path.'
        Assert-WindowsCpuArtifactExportCondition ($paths.Add([string]$entry.path)) 'Bundle inventory contains duplicate or case-colliding paths.'
        $null = Assert-WindowsCpuArtifactExportInteger $entry.size_bytes 0 ([int64]::MaxValue) 'Bundle inventory entry size'
        Assert-SigningHash ([string]$entry.sha256) 'Bundle inventory entry SHA-256'
        Assert-WindowsCpuArtifactExportCondition `
            (-not [string]::Equals([string]$entry.path, $script:WindowsCpuArtifactLocalMarker, [StringComparison]::OrdinalIgnoreCase)) `
            'A local-only frozen CPU marker cannot be exported as a CI worker artifact.'
        if ([string]$entry.path -ceq $ExpectedPath) {
            Assert-WindowsCpuArtifactExportCondition ($null -eq $match) "Bundle inventory repeats $ExpectedPath."
            $match = $entry
        }
    }
    Assert-WindowsCpuArtifactExportCondition ($null -ne $match) "Bundle inventory does not contain exact entry $ExpectedPath."
    return $match
}

function Assert-WindowsCpuArtifactExportStream(
    [System.IO.FileStream]$Stream,
    $InventoryEntry,
    [string]$Label
) {
    $expectedSize = Assert-WindowsCpuArtifactExportInteger $InventoryEntry.size_bytes 1 ([int64]::MaxValue) "$Label inventory size"
    Assert-WindowsCpuArtifactExportCondition ($Stream.Length -eq $expectedSize) "$Label does not match its bundle inventory size."
    $digest = Get-WindowsFrozenCpuWorkerOpenStreamSha256 $Stream
    Assert-WindowsCpuArtifactExportCondition ($digest -ceq [string]$InventoryEntry.sha256) "$Label does not match its bundle inventory SHA-256."
    return $digest
}

function Assert-WindowsCpuArtifactExportUnlinkedFile([string]$Path, [string]$Label) {
    $item = Assert-WindowsFrozenCpuWorkerRegularFile $Path
    Assert-WindowsCpuArtifactExportCondition `
        ([string]::IsNullOrEmpty([string]$item.LinkType)) `
        "$Label cannot be a symbolic link or hardlink."
    return $item
}

function Copy-WindowsCpuArtifactExportStream([System.IO.FileStream]$Source, [string]$Destination) {
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $Destination
    Assert-WindowsCpuArtifactExportCondition (-not (Test-Path -LiteralPath $Destination)) 'CPU worker artifact output unexpectedly exists.'
    $Source.Position = 0
    $destinationStream = [IO.File]::Open($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $Source.CopyTo($destinationStream) }
    finally { $destinationStream.Dispose(); $Source.Position = 0 }
}

function Remove-WindowsCpuArtifactExportStaging([string]$Staging, [string]$Output) {
    if (-not (Test-Path -LiteralPath $Staging)) { return }
    $stagingFull = Get-WindowsFrozenCpuWorkerNormalizedFullPath $Staging
    $outputFull = Get-WindowsFrozenCpuWorkerNormalizedFullPath $Output
    $parent = Split-Path -Parent $outputFull
    $expectedPrefix = ".$([IO.Path]::GetFileName($outputFull)).staging-"
    Assert-WindowsCpuArtifactExportCondition `
        ((Split-Path -Parent $stagingFull) -ceq $parent -and (Split-Path -Leaf $stagingFull).StartsWith($expectedPrefix, [StringComparison]::Ordinal)) `
        'Refusing to clean an unowned CPU worker artifact staging path.'
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $stagingFull
    $allowed = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $null = $allowed.Add($script:WindowsCpuArtifactRecordName)
    $null = $allowed.Add($script:WindowsCpuArtifactWorkerName)
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $items = @(Get-ChildItem -LiteralPath $stagingFull -Force)
    Assert-WindowsCpuArtifactExportCondition ($items.Count -le 2) 'Refusing to clean a CPU worker staging directory with excess entries.'
    foreach ($item in $items) {
        Assert-WindowsCpuArtifactExportCondition `
            ($allowed.Contains($item.Name) -and $seen.Add($item.Name) -and -not $item.PSIsContainer -and
                ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -and
                [string]::IsNullOrEmpty([string]$item.LinkType)) `
            'Refusing to clean a CPU worker artifact staging directory containing an unknown, linked, or non-file entry.'
        Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $item.FullName
    }
    Remove-Item -LiteralPath $stagingFull -Recurse -Force
}

function Invoke-WindowsCpuWorkerArtifactExport {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$BundlePath,
        [Parameter(Mandatory = $true)][string]$OutputDirectory
    )

    Assert-WindowsCpuArtifactExportCondition (-not [string]::IsNullOrWhiteSpace($BundlePath)) 'BundlePath is required.'
    Assert-WindowsCpuArtifactExportCondition (-not [string]::IsNullOrWhiteSpace($OutputDirectory)) 'OutputDirectory is required.'
    Assert-WindowsCpuArtifactExportCondition `
        ($env:GITHUB_ACTIONS -ceq 'true' -and $env:CI -ceq 'true' -and
            $env:GITHUB_EVENT_NAME -ceq 'workflow_dispatch' -and
            $env:GITHUB_REPOSITORY -ceq $script:WindowsCpuArtifactRepository -and
            $env:GITHUB_REF -ceq $script:WindowsCpuArtifactRef -and
            $env:GITHUB_WORKFLOW -ceq $script:WindowsCpuArtifactWorkflowName) `
        'CPU worker export is restricted to the fixed manual protected-main release workflow.'
    Assert-SigningId $env:GITHUB_RUN_ID 'CPU worker producer run ID'
    Assert-SigningId $env:GITHUB_RUN_ATTEMPT 'CPU worker producer run attempt'
    Assert-WindowsCpuArtifactExportCondition ($env:GITHUB_SHA -cmatch '\A[0-9a-f]{40}\z') 'CPU worker producer source revision is invalid.'

    $repositoryRoot = Get-WindowsFrozenCpuWorkerNormalizedFullPath (Split-Path -Parent $PSScriptRoot)
    $workspace = Get-WindowsFrozenCpuWorkerNormalizedFullPath $env:GITHUB_WORKSPACE
    Assert-WindowsCpuArtifactExportCondition `
        ([string]::Equals($workspace, $repositoryRoot, [StringComparison]::OrdinalIgnoreCase)) `
        'GitHub workspace does not match the physical CPU worker source checkout.'
    $context = Get-WindowsCpuArtifactExportSourceContext $repositoryRoot
    Assert-WindowsCpuArtifactExportCondition ($context.SourceRevision -ceq $env:GITHUB_SHA) 'Physical source revision does not match GITHUB_SHA.'
    $null = Assert-WindowsFrozenCpuWorkerRegularFile (Join-Path $repositoryRoot ($script:WindowsCpuArtifactWorkflow -replace '/', '\'))

    $bundle = Get-WindowsFrozenCpuWorkerNormalizedFullPath $BundlePath
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $bundle
    Assert-WindowsCpuArtifactExportCondition (Test-Path -LiteralPath $bundle -PathType Container) 'Validated portable bundle is missing.'
    Assert-WindowsCpuArtifactExportCondition `
        (-not (Test-Path -LiteralPath (Join-Path $bundle $script:WindowsCpuArtifactLocalMarker))) `
        'A local-only frozen CPU bundle cannot be exported as a CI worker artifact.'

    $output = Get-WindowsFrozenCpuWorkerNormalizedFullPath $OutputDirectory
    Assert-WindowsCpuArtifactExportCondition `
        (-not (Test-WindowsFrozenCpuWorkerPathIsWithin $output $bundle) -and
            -not (Test-WindowsFrozenCpuWorkerPathIsWithin $output $repositoryRoot)) `
        'CPU worker artifact output cannot overlap the verified bundle or trusted source checkout.'

    $inventoryFile = Read-SigningFile (Join-Path $bundle 'bundle-inventory.json') (4MB)
    $inventory = ConvertFrom-SigningJson $inventoryFile
    $desktopEntry = Get-WindowsCpuArtifactExportInventoryEntry $inventory $script:WindowsCpuArtifactDesktopName
    $workerEntry = Get-WindowsCpuArtifactExportInventoryEntry $inventory $script:WindowsCpuArtifactWorkerName
    $desktopPath = Join-Path $bundle $script:WindowsCpuArtifactDesktopName
    $workerPath = Join-Path $bundle $script:WindowsCpuArtifactWorkerName
    $desktopStream = $null
    $workerStream = $null
    $staging = $null
    try {
        $null = Assert-WindowsCpuArtifactExportUnlinkedFile $desktopPath 'Portable desktop'
        $null = Assert-WindowsCpuArtifactExportUnlinkedFile $workerPath 'Portable CPU worker'
        $desktopStream = Open-WindowsFrozenCpuWorkerReadHandle $desktopPath
        $workerStream = Open-WindowsFrozenCpuWorkerReadHandle $workerPath
        $desktopSha256 = Assert-WindowsCpuArtifactExportStream $desktopStream $desktopEntry 'Portable desktop'
        $workerSha256 = Assert-WindowsCpuArtifactExportStream $workerStream $workerEntry 'Portable CPU worker'
        Assert-WindowsCpuArtifactExportCondition `
            ($workerStream.Length -le (Get-WindowsFrozenCpuWorkerMaximumBytes)) `
            'Portable CPU worker exceeds the artifact size bound.'
        $null = Assert-ReviewedWindowsPe $desktopPath 2
        $null = Assert-ReviewedWindowsPe $workerPath 3
        $null = Assert-WindowsCpuArtifactExportCompiledAdmission `
            -DesktopPath $desktopPath `
            -DesktopSize ([int64]$desktopStream.Length) `
            -DesktopSha256 $desktopSha256 `
            -Context $context `
            -WorkerSha256 $workerSha256
        Assert-WindowsFrozenCpuWorkerContextUnchanged $context
        $null = Assert-WindowsCpuArtifactExportStream $desktopStream $desktopEntry 'Portable desktop'
        $null = Assert-WindowsCpuArtifactExportStream $workerStream $workerEntry 'Portable CPU worker'

        $parent = Split-Path -Parent $output
        Assert-WindowsCpuArtifactExportCondition (-not [string]::IsNullOrWhiteSpace($parent)) 'CPU worker artifact output requires a parent directory.'
        Assert-WindowsFrozenCpuWorkerNoReparseAncestors $parent
        Assert-WindowsCpuArtifactExportCondition (Test-Path -LiteralPath $parent -PathType Container) 'CPU worker artifact output parent must already exist.'
        Assert-WindowsCpuArtifactExportCondition (-not (Test-Path -LiteralPath $output)) 'CPU worker artifact output directory must be fresh.'
        $staging = Join-Path $parent (".$([IO.Path]::GetFileName($output)).staging-$PID-$([guid]::NewGuid().ToString('N'))")
        Assert-WindowsCpuArtifactExportCondition (-not (Test-Path -LiteralPath $staging)) 'CPU worker artifact staging directory unexpectedly exists.'
        [IO.Directory]::CreateDirectory($staging) | Out-Null
        Assert-WindowsFrozenCpuWorkerNoReparseAncestors $staging

        $stagedWorker = Join-Path $staging $script:WindowsCpuArtifactWorkerName
        Copy-WindowsCpuArtifactExportStream $workerStream $stagedWorker
        $record = New-WindowsCiCpuWorkerRecord `
            -Context $context `
            -RunId $env:GITHUB_RUN_ID `
            -RunAttempt $env:GITHUB_RUN_ATTEMPT `
            -WorkerSize ([int64]$workerStream.Length) `
            -WorkerSha256 $workerSha256
        $recordText = $record | ConvertTo-Json -Depth 5
        [IO.File]::WriteAllText(
            (Join-Path $staging $script:WindowsCpuArtifactRecordName),
            $recordText,
            [Text.UTF8Encoding]::new($false)
        )
        $stagedItems = @(Get-ChildItem -LiteralPath $staging -Force)
        Assert-WindowsCpuArtifactExportCondition `
            ($stagedItems.Count -eq 2 -and @($stagedItems | Where-Object { $_.PSIsContainer }).Count -eq 0) `
            'CPU worker artifact staging inventory is not exact.'
        $null = Assert-WindowsCpuArtifactExportUnlinkedFile (Join-Path $staging $script:WindowsCpuArtifactRecordName) 'Staged CPU worker record'
        $stagedWorkerItem = Assert-WindowsCpuArtifactExportUnlinkedFile $stagedWorker 'Staged CPU worker'
        Assert-WindowsCpuArtifactExportCondition `
            ($stagedWorkerItem.Length -eq $workerStream.Length -and
                (Get-WindowsFrozenCpuWorkerFileSha256 $stagedWorker) -ceq $workerSha256) `
            'Copied CPU worker artifact bytes changed before publication.'
        Assert-WindowsFrozenCpuWorkerContextUnchanged $context
        $null = Assert-WindowsCpuArtifactExportStream $workerStream $workerEntry 'Portable CPU worker'
        Assert-WindowsCpuArtifactExportCondition (-not (Test-Path -LiteralPath $output)) 'CPU worker artifact output appeared during staging.'
        [IO.Directory]::Move($staging, $output)
        $staging = $null
        return [pscustomobject]@{
            Root = $output
            RecordPath = Join-Path $output $script:WindowsCpuArtifactRecordName
            WorkerPath = Join-Path $output $script:WindowsCpuArtifactWorkerName
            Record = [pscustomobject]$record
        }
    }
    finally {
        if ($null -ne $workerStream) { $workerStream.Dispose() }
        if ($null -ne $desktopStream) { $desktopStream.Dispose() }
        if ($null -ne $staging) {
            Remove-WindowsCpuArtifactExportStaging $staging $OutputDirectory
        }
    }
}

if (-not $windowsCpuExportFunctionsOnly) {
    Invoke-WindowsCpuWorkerArtifactExport @windowsCpuExportEntryArguments
}
