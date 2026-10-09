[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# This keeps the CI consumer boundary executable without building native code.
# The copied builder and resolver run as scripts, with real ZIP/PE/hash/handle
# and staging behavior.  Cargo, the smoke executable, compiled admission, and
# GitHub metadata are the only deterministic seams.
$repositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$testRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-windows-ci-cpu-worker-packaging-$([guid]::NewGuid().ToString('N'))"
$fixtureRoot = Join-Path $testRoot 'fixture'
$fixtureModel = Join-Path $testRoot 'fixture-model.gguf'
$fixtureTarget = Join-Path $testRoot 'cargo-target'
$script:Assertions = 0
$script:ScenarioCount = 0
$script:Utf8 = [Text.UTF8Encoding]::new($false)

$environmentNames = @(
    'CARGO_TARGET_DIR', 'SCRIBE_BUILD_REVISION', 'SCRIBE_BUNDLED_WORKER_SHA256', 'SCRIBE_BUILDING_WORKER',
    'GITHUB_ACTIONS', 'CI', 'GITHUB_EVENT_NAME', 'GITHUB_REPOSITORY', 'GITHUB_REF', 'GITHUB_WORKFLOW',
    'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT', 'GITHUB_SHA', 'GITHUB_WORKSPACE',
    'SCRIBE_CI_CPU_FIXTURE_INSTALLER_REVISION', 'SCRIBE_CI_CPU_FIXTURE_WORKER_REVISION',
    'SCRIBE_CI_CPU_FIXTURE_RUN_ID', 'SCRIBE_CI_CPU_FIXTURE_RUN_ATTEMPT',
    'SCRIBE_CI_CPU_FIXTURE_ARTIFACT_ID', 'SCRIBE_CI_CPU_FIXTURE_ARCHIVE_SHA256',
    'SCRIBE_CI_CPU_FIXTURE_ARCHIVE_SIZE', 'SCRIBE_CI_CPU_FIXTURE_POST_CARGO_MUTATION'
)
$savedEnvironment = @{}
foreach ($name in $environmentNames) { $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name) }
$savedGitEnvironment = @{}
foreach ($entry in @(Get-ChildItem Env:GIT_*)) { $savedGitEnvironment[$entry.Name] = $entry.Value }
$savedGlobalCargo = Get-Item -LiteralPath Function:global:cargo -ErrorAction SilentlyContinue
$savedGlobalCargoScriptBlock = if ($null -ne $savedGlobalCargo) { $savedGlobalCargo.ScriptBlock } else { $null }
$fixtureGlobalVariableNames = @(
    'CiCpuWorkerBuilderCargoCalls', 'CiCpuWorkerBuilderNativeCalls', 'CiCpuWorkerBuilderAdmissionCalls',
    'CiCpuWorkerBuilderGitHubCalls', 'CiCpuWorkerBuilderFailDesktop', 'CiCpuWorkerBuilderFailSmoke',
    'CiCpuWorkerBuilderRaceBundle', 'CiCpuWorkerBuilderMutateWorkerPath',
    'CiCpuWorkerBuilderMutationWasBlocked', 'CiCpuWorkerBuilderMutateStagedWorker',
    'CiCpuWorkerBuilderStagedWorkerMutationAttempted', 'CiCpuWorkerBuilderStagedWorkerMutationSucceeded',
    'CiCpuWorkerBuilderSourceDriftPath', 'CiCpuWorkerBuilderAdmissionResponse',
    'CiCpuWorkerBuilderPostCargoMutation', 'CiCpuWorkerBuilderActivationMutation'
)
$savedFixtureGlobals = @{}
foreach ($name in $fixtureGlobalVariableNames) {
    $saved = Get-Variable -Name $name -Scope Global -ErrorAction SilentlyContinue
    $savedFixtureGlobals[$name] = [pscustomobject]@{
        Exists = $null -ne $saved
        Value = if ($null -ne $saved) { $saved.Value } else { $null }
    }
}
$lastExitVariable = Get-Variable -Name LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue
$hadSavedLastExitCode = $null -ne $lastExitVariable
$savedLastExitCode = if ($hadSavedLastExitCode) { [int]$lastExitVariable.Value } else { $null }

function Assert-Test([bool]$Condition, [string]$Message) {
    $script:Assertions++
    if (-not $Condition) { throw "TEST FAILED: $Message" }
}

function Assert-Equal($Actual, $Expected, [string]$Message) {
    $script:Assertions++
    if ($Actual -cne $Expected) { throw "TEST FAILED: $Message Expected '$Expected', got '$Actual'." }
}

function Assert-True([bool]$Value, [string]$Message) {
    $script:Assertions++
    if (-not $Value) { throw "TEST FAILED: $Message" }
}

function Assert-Rejected([string]$Name, [scriptblock]$Action) {
    $script:Assertions++
    try {
        $result = @(& $Action)
        foreach ($item in $result) {
            if ($null -ne $item -and $null -ne $item.PSObject.Properties['WorkerStream'] -and $null -ne $item.WorkerStream) {
                $item.WorkerStream.Dispose()
            }
        }
    }
    catch {
        if ($_.Exception.Message.StartsWith('TEST FAILED:', [StringComparison]::Ordinal)) { throw }
        return
    }
    throw "TEST FAILED: $Name was accepted."
}

function Get-TestHash([byte[]]$Bytes) {
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Bytes)).ToLowerInvariant()
}

function Write-TestBytes([string]$Path, [byte[]]$Bytes) {
    [IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
    [IO.File]::WriteAllBytes($Path, $Bytes)
}

function Copy-FixtureSourceFile([string]$RelativePath) {
    $source = Join-Path $repositoryRoot ($RelativePath -replace '/', '\')
    $destination = Join-Path $fixtureRoot ($RelativePath -replace '/', '\')
    [IO.Directory]::CreateDirectory((Split-Path -Parent $destination)) | Out-Null
    Copy-Item -LiteralPath $source -Destination $destination
}

function Set-TestUInt16([byte[]]$Bytes, [int]$Offset, [uint16]$Value) {
    [BitConverter]::GetBytes($Value).CopyTo($Bytes, $Offset)
}

function Set-TestUInt32([byte[]]$Bytes, [int]$Offset, [uint32]$Value) {
    [BitConverter]::GetBytes($Value).CopyTo($Bytes, $Offset)
}

function New-TestReviewedPe([string]$Path, [uint16]$Subsystem) {
    $bytes = [byte[]]::new(1024)
    $bytes[0] = 0x4D; $bytes[1] = 0x5A
    Set-TestUInt32 $bytes 0x3C 0x80
    Set-TestUInt32 $bytes 0x80 0x00004550
    Set-TestUInt16 $bytes 0x84 0x8664
    Set-TestUInt16 $bytes 0x86 1
    Set-TestUInt16 $bytes 0x94 0x00F0
    Set-TestUInt16 $bytes 0x98 0x020B
    Set-TestUInt16 $bytes 0xDC $Subsystem
    Set-TestUInt32 $bytes 0xD4 0x200
    Set-TestUInt32 $bytes 0x104 16
    Set-TestUInt32 $bytes 0x110 0x1000
    Set-TestUInt32 $bytes 0x114 40
    [System.Array]::Copy([Text.Encoding]::ASCII.GetBytes('.rdata'), 0, $bytes, 0x188, 6)
    Set-TestUInt32 $bytes 0x190 0x200
    Set-TestUInt32 $bytes 0x194 0x1000
    Set-TestUInt32 $bytes 0x198 0x200
    Set-TestUInt32 $bytes 0x19C 0x200
    Set-TestUInt32 $bytes 0x20C 0x1040
    [System.Array]::Copy([Text.Encoding]::ASCII.GetBytes('kernel32.dll'), 0, $bytes, 0x240, 12)
    [IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
    [IO.File]::WriteAllBytes($Path, $bytes)
}

function New-TestArchive([string]$Path, [object[]]$Entries) {
    Add-Type -AssemblyName System.IO.Compression
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $zip = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create, $false)
        try {
            foreach ($entrySpec in $Entries) {
                $entry = $zip.CreateEntry([string]$entrySpec.Name, [IO.Compression.CompressionLevel]::NoCompression)
                $entryStream = $entry.Open()
                try { $entryStream.Write([byte[]]$entrySpec.Bytes, 0, ([byte[]]$entrySpec.Bytes).Length) }
                finally { $entryStream.Dispose() }
            }
        }
        finally { $zip.Dispose() }
    }
    finally { $stream.Dispose() }
}

function Remove-OwnedTestRoot([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $root = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
    Assert-Test ($root.StartsWith($temp + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -and
        (Split-Path -Leaf $root) -cmatch '^scribe-windows-ci-cpu-worker-packaging-[0-9a-f]{32}$') `
        'Refused CI consumer fixture cleanup outside its exact temporary root.'
    $current = $root
    while ($true) {
        $item = Get-Item -LiteralPath $current -Force
        Assert-Test (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Refused CI consumer fixture cleanup through a reparse point.'
        if ([string]::Equals($current, $temp, [StringComparison]::OrdinalIgnoreCase)) { break }
        $parent = Split-Path -Parent $current
        Assert-Test (-not [string]::IsNullOrWhiteSpace($parent) -and $parent -cne $current) 'Could not prove CI consumer fixture cleanup ancestry.'
        $current = $parent
    }
    $ownedItems = @(Get-ChildItem -LiteralPath $root -Recurse -Force)
    foreach ($item in $ownedItems) {
        Assert-Test (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Refused CI consumer fixture cleanup containing a reparse point.'
    }
    # Fixture Git objects can inherit a read-only attribute from the host's
    # object-store settings. Attributes are changed only after exact-root,
    # ancestor, and descendant reparse-point validation has succeeded.
    $rootItem = Get-Item -LiteralPath $root -Force
    if (($rootItem.Attributes -band [IO.FileAttributes]::ReadOnly) -ne 0) {
        $rootItem.Attributes = $rootItem.Attributes -band (-bnot [IO.FileAttributes]::ReadOnly)
    }
    foreach ($item in $ownedItems) {
        if (($item.Attributes -band [IO.FileAttributes]::ReadOnly) -ne 0) {
            $item.Attributes = $item.Attributes -band (-bnot [IO.FileAttributes]::ReadOnly)
        }
    }
    [IO.Directory]::Delete("\\?\$root", $true)
}

function Set-FixtureNativeSmokeSeam([string]$BuilderPath) {
    $source = Get-Content -LiteralPath $BuilderPath -Raw
    $start = $source.IndexOf('function Invoke-NativeProcess')
    $end = $source.IndexOf('function Assert-NoReparseAncestors', $start)
    if ($start -lt 0 -or $end -le $start) { throw 'Could not isolate fixture native-smoke seam.' }
    $fixtureSmoke = @'
function Invoke-NativeProcess([string]$ExecutablePath, [string[]]$Arguments) {
    $global:CiCpuWorkerBuilderNativeCalls.Add([pscustomobject]@{ ExecutablePath = $ExecutablePath; Arguments = @($Arguments) })
    if ($global:CiCpuWorkerBuilderRaceBundle) {
        [IO.Directory]::CreateDirectory($global:CiCpuWorkerBuilderRaceBundle) | Out-Null
    }
    if ($global:CiCpuWorkerBuilderMutateStagedWorker) {
        $global:CiCpuWorkerBuilderStagedWorkerMutationAttempted = $true
        $stagedWorker = Join-Path (Split-Path -Parent $ExecutablePath) 'scribe-inference-worker.exe'
        [IO.File]::WriteAllBytes($stagedWorker, [byte[]](0x66))
        $global:CiCpuWorkerBuilderStagedWorkerMutationSucceeded = $true
    }
    if ($global:CiCpuWorkerBuilderFailSmoke) {
        return [pscustomobject]@{ ExitCode = 1; Stdout = ''; Stderr = 'fixture smoke failure' }
    }
    return [pscustomobject]@{
        ExitCode = 0
        Stdout = '{"cancellation_verified":true,"capabilities":{"cancellation":true},"detected_architecture":"whisper"}'
        Stderr = ''
    }
}

'@
    [IO.File]::WriteAllText($BuilderPath, $source.Substring(0, $start) + $fixtureSmoke + $source.Substring($end), $script:Utf8)
}

function Set-FixtureCompiledAdmissionSeam([string]$IntegrityPath) {
    $source = Get-Content -LiteralPath $IntegrityPath -Raw
    $start = $source.IndexOf('function Invoke-WindowsFrozenCpuWorkerAdmissionProcess')
    $end = $source.IndexOf('function Assert-WindowsFrozenCpuWorkerCompiledAdmission', $start)
    if ($start -lt 0 -or $end -le $start) { throw 'Could not isolate fixture compiled-admission seam.' }
    $fixtureAdmission = @'
function Invoke-WindowsFrozenCpuWorkerAdmissionProcess([string]$Executable) {
    $global:CiCpuWorkerBuilderAdmissionCalls.Add($Executable)
    if ($null -eq $global:CiCpuWorkerBuilderAdmissionResponse) {
        throw 'Fixture compiled admission response was not configured.'
    }
    return $global:CiCpuWorkerBuilderAdmissionResponse
}

'@
    [IO.File]::WriteAllText($IntegrityPath, $source.Substring(0, $start) + $fixtureAdmission + $source.Substring($end), $script:Utf8)
}

function Set-FixtureGitHubSeam([string]$ResolverPath) {
    $source = Get-Content -LiteralPath $ResolverPath -Raw
    $start = $source.IndexOf('function Invoke-WindowsCpuWorkerInputGitHubGet')
    $end = $source.IndexOf('function New-WindowsCpuWorkerInputHttpClient', $start)
    if ($start -lt 0 -or $end -le $start) { throw 'Could not isolate fixture GitHub metadata seam.' }
    $fixtureGitHub = @'
function Invoke-WindowsCpuWorkerInputGitHubGet([string]$Path) {
    $global:CiCpuWorkerBuilderGitHubCalls.Add($Path)
    $source = [string]$env:SCRIBE_CI_CPU_FIXTURE_INSTALLER_REVISION
    $worker = [string]$env:SCRIBE_CI_CPU_FIXTURE_WORKER_REVISION
    $runId = [string]$env:SCRIBE_CI_CPU_FIXTURE_RUN_ID
    $attempt = [int64]$env:SCRIBE_CI_CPU_FIXTURE_RUN_ATTEMPT
    $artifactId = [string]$env:SCRIBE_CI_CPU_FIXTURE_ARTIFACT_ID
    $phase = [string]$env:SCRIBE_CI_CPU_FIXTURE_POST_CARGO_MUTATION
    if (-not [string]::IsNullOrWhiteSpace([string]$global:CiCpuWorkerBuilderActivationMutation) -and
        $global:CiCpuWorkerBuilderGitHubCalls.Count -gt 18) {
        $phase = [string]$global:CiCpuWorkerBuilderActivationMutation
    }
    if ($Path -match '/compare/(?<base>[0-9a-f]{40})\.\.\.(?<head>[0-9a-f]{40})$') {
        return [ordered]@{ merge_base_commit = [ordered]@{ sha = $Matches.base }; status = if ($Matches.base -ceq $Matches.head) { 'identical' } else { 'ahead' } }
    }
    if ($Path -ceq '/repos/tyhuang9/scribe/git/ref/heads/main') {
        return [ordered]@{ object = [ordered]@{ sha = $source } }
    }
    if ($Path -ceq "/repos/tyhuang9/scribe/actions/runs/$runId/attempts/$attempt" -or
        $Path -ceq "/repos/tyhuang9/scribe/actions/runs/$runId") {
        $reportedAttempt = if ($phase -ceq 'attempt') { $attempt + 1 } else { $attempt }
        return [ordered]@{
            id = [int64]$runId; run_attempt = [int64]$reportedAttempt
            repository = [ordered]@{ full_name = 'tyhuang9/scribe' }
            head_repository = [ordered]@{ full_name = 'tyhuang9/scribe' }
            path = '.github/workflows/release.yml'; event = 'workflow_dispatch'; head_branch = 'main'
            status = 'completed'; conclusion = 'success'; head_sha = $worker
        }
    }
    if ($Path -ceq "/repos/tyhuang9/scribe/actions/artifacts/$artifactId") {
        $digest = if ($phase -ceq 'digest') { '0' * 64 } else { [string]$env:SCRIBE_CI_CPU_FIXTURE_ARCHIVE_SHA256 }
        return [ordered]@{
            id = [int64]$artifactId
            workflow_run = [ordered]@{ id = [int64]$runId; head_sha = $worker; head_branch = 'main' }
            name = "windows-cpu-worker-$runId-$attempt"
            expired = ($phase -ceq 'expiry')
            digest = "sha256:$digest"
            size_in_bytes = [int64]$env:SCRIBE_CI_CPU_FIXTURE_ARCHIVE_SIZE
        }
    }
    throw "Unexpected offline CI CPU worker metadata request: $Path"
}

'@
    [IO.File]::WriteAllText($ResolverPath, $source.Substring(0, $start) + $fixtureGitHub + $source.Substring($end), $script:Utf8)
}

function New-WorkerContext([psobject]$DesktopContext, [string]$WorkerRevision) {
    if ($WorkerRevision -ceq $DesktopContext.SourceRevision) { return $DesktopContext }
    return [pscustomobject]@{
        RepositoryRoot = $null
        SourceRevision = $WorkerRevision
        AppVersion = $DesktopContext.AppVersion
        TargetTriple = $DesktopContext.TargetTriple
        ProtocolVersion = $DesktopContext.ProtocolVersion
        WorkerAbiVersion = $DesktopContext.WorkerAbiVersion
        DesktopBuildId = "local-transcriber@$($DesktopContext.AppVersion)#$WorkerRevision"
        WorkerBuildId = "scribe-inference-worker@$($DesktopContext.AppVersion)#$WorkerRevision"
        CargoLockSha256 = $DesktopContext.CargoLockSha256
        RustToolchainSha256 = $DesktopContext.RustToolchainSha256
        CargoManifestSha256 = $DesktopContext.CargoManifestSha256
        WorkerIdentitySha256 = $DesktopContext.WorkerIdentitySha256
        BuildRsSha256 = $DesktopContext.BuildRsSha256
        BuildContractSha256 = $DesktopContext.BuildContractSha256
    }
}

function New-CiWorkerRecord([psobject]$Context, [string]$Revision, [byte[]]$WorkerBytes) {
    return [ordered]@{
        schema_version = 1; kind = 'windows-ci-cpu-worker'; source_repository = 'tyhuang9/scribe'
        source_ref = 'refs/heads/main'; workflow = '.github/workflows/release.yml'
        run_id = '8001'; run_attempt = '2'; source_revision = $Revision
        app_version = $Context.AppVersion; target_triple = $Context.TargetTriple
        protocol_version = [int64]$Context.ProtocolVersion; worker_abi_version = [int64]$Context.WorkerAbiVersion
        desktop_build_id = $Context.DesktopBuildId; worker_build_id = $Context.WorkerBuildId
        cargo_lock_sha256 = $Context.CargoLockSha256; rust_toolchain_sha256 = $Context.RustToolchainSha256
        cargo_manifest_sha256 = $Context.CargoManifestSha256; worker_identity_sha256 = $Context.WorkerIdentitySha256
        build_rs_sha256 = $Context.BuildRsSha256; build_contract_sha256 = $Context.BuildContractSha256
        worker_relative_path = 'scribe-inference-worker.exe'; worker_size_bytes = [int64]$WorkerBytes.Length
        worker_sha256 = Get-TestHash $WorkerBytes
    }
}

function Set-AdmissionResponse([psobject]$DesktopContext, [psobject]$WorkerContext, $Record, [string]$Mutation = '') {
    $report = [ordered]@{
        schema_version = 1
        desktop_build_id = $DesktopContext.DesktopBuildId
        bundled_worker_sha256 = $Record.worker_sha256
        protocol_version = [int64]$DesktopContext.ProtocolVersion
        worker_abi_version = [int64]$DesktopContext.WorkerAbiVersion
        worker_origin_app_build = $WorkerContext.DesktopBuildId
        worker_build_id = $WorkerContext.WorkerBuildId
        admission_kind = if ($DesktopContext.SourceRevision -ceq $WorkerContext.SourceRevision) { 'strict_legacy_same_source' } else { 'compiled_foreign_approval' }
    }
    switch ($Mutation) {
        '' { }
        'anchor' { $report.bundled_worker_sha256 = '0' * 64 }
        'desktop' { $report.desktop_build_id = 'wrong-desktop-build' }
        'origin' { $report.worker_origin_app_build = 'wrong-worker-origin' }
        'protocol' { $report.protocol_version = [int64]($report.protocol_version + 1) }
        'abi' { $report.worker_abi_version = [int64]($report.worker_abi_version + 1) }
        'kind' { $report.admission_kind = 'wrong-kind' }
        'error' {
            $global:CiCpuWorkerBuilderAdmissionResponse = [pscustomobject]@{ ExitCode = 1; Stdout = ''; Stderr = 'fixture admission failure' }
            return
        }
        default { throw "Unknown compiled admission mutation: $Mutation" }
    }
    $global:CiCpuWorkerBuilderAdmissionResponse = [pscustomobject]@{ ExitCode = 0; Stdout = ($report | ConvertTo-Json -Compress); Stderr = '' }
}

function Reset-Scenario([string]$WorkerRevision = '') {
    $script:ScenarioCount++
    $desktop = Get-WindowsFrozenCpuWorkerSourceContext $fixtureRoot
    if ([string]::IsNullOrEmpty($WorkerRevision)) { $WorkerRevision = $desktop.SourceRevision }
    $workerContext = New-WorkerContext $desktop $WorkerRevision
    $workerPath = Join-Path $testRoot "worker-$([guid]::NewGuid().ToString('N')).exe"
    New-TestReviewedPe $workerPath 3
    $workerBytes = [IO.File]::ReadAllBytes($workerPath)
    $record = New-CiWorkerRecord $workerContext $WorkerRevision $workerBytes
    $recordBytes = $script:Utf8.GetBytes(($record | ConvertTo-Json -Depth 5))
    $archivePath = Join-Path $testRoot "worker-$([guid]::NewGuid().ToString('N')).zip"
    New-TestArchive $archivePath @(
        [pscustomobject]@{ Name = 'windows-ci-cpu-worker.json'; Bytes = $recordBytes },
        [pscustomobject]@{ Name = 'scribe-inference-worker.exe'; Bytes = $workerBytes }
    )
    $script:Scenario = [ordered]@{
        DesktopContext = $desktop; WorkerContext = $workerContext; WorkerRevision = $WorkerRevision
        WorkerBytes = $workerBytes; Record = $record; RecordBytes = $recordBytes
        ArchivePath = $archivePath; ArchiveHash = Get-TestHash ([IO.File]::ReadAllBytes($archivePath))
        ResolvedRoot = Join-Path $testRoot "resolved-$([guid]::NewGuid().ToString('N'))"
        BundlePath = Join-Path $testRoot "bundle-$([guid]::NewGuid().ToString('N'))"
    }
    $env:CARGO_TARGET_DIR = $fixtureTarget
    $env:GITHUB_ACTIONS = 'true'; $env:CI = 'true'; $env:GITHUB_EVENT_NAME = 'workflow_dispatch'
    $env:GITHUB_REPOSITORY = 'tyhuang9/scribe'; $env:GITHUB_REF = 'refs/heads/main'
    $env:GITHUB_WORKFLOW = 'Build Windows installer'; $env:GITHUB_SHA = $desktop.SourceRevision
    $env:GITHUB_WORKSPACE = $fixtureRoot
    $env:SCRIBE_CI_CPU_FIXTURE_INSTALLER_REVISION = $desktop.SourceRevision
    $env:SCRIBE_CI_CPU_FIXTURE_WORKER_REVISION = $WorkerRevision
    $env:SCRIBE_CI_CPU_FIXTURE_RUN_ID = '8001'; $env:SCRIBE_CI_CPU_FIXTURE_RUN_ATTEMPT = '2'; $env:SCRIBE_CI_CPU_FIXTURE_ARTIFACT_ID = '8101'
    $env:SCRIBE_CI_CPU_FIXTURE_ARCHIVE_SHA256 = $script:Scenario.ArchiveHash
    $env:SCRIBE_CI_CPU_FIXTURE_ARCHIVE_SIZE = [string](Get-Item -LiteralPath $archivePath -Force).Length
    $env:SCRIBE_CI_CPU_FIXTURE_POST_CARGO_MUTATION = $null
    $global:CiCpuWorkerBuilderCargoCalls.Clear(); $global:CiCpuWorkerBuilderNativeCalls.Clear(); $global:CiCpuWorkerBuilderAdmissionCalls.Clear(); $global:CiCpuWorkerBuilderGitHubCalls.Clear()
    $global:CiCpuWorkerBuilderFailDesktop = $false; $global:CiCpuWorkerBuilderFailSmoke = $false
    $global:CiCpuWorkerBuilderRaceBundle = $null; $global:CiCpuWorkerBuilderMutateWorkerPath = $null
    $global:CiCpuWorkerBuilderMutationWasBlocked = $false; $global:CiCpuWorkerBuilderSourceDriftPath = $null
    $global:CiCpuWorkerBuilderMutateStagedWorker = $false
    $global:CiCpuWorkerBuilderStagedWorkerMutationAttempted = $false
    $global:CiCpuWorkerBuilderStagedWorkerMutationSucceeded = $false
    $global:CiCpuWorkerBuilderPostCargoMutation = $null; $global:CiCpuWorkerBuilderActivationMutation = $null
    Set-AdmissionResponse $desktop $workerContext $record
}

function Get-CiInputParameters {
    return @{
        CiCpuWorkerSourceRevision = $script:Scenario.WorkerRevision
        CiCpuWorkerProducerRunId = '8001'
        CiCpuWorkerProducerRunAttempt = '2'
        CiCpuWorkerArtifactId = '8101'
        CiCpuWorkerExpectedArtifactSha256 = $script:Scenario.ArchiveHash
        CiCpuWorkerArchivePath = $script:Scenario.ArchivePath
        CiCpuWorkerOutputDirectory = $script:Scenario.ResolvedRoot
    }
}

function Invoke-CiBuilder([hashtable]$AdditionalParameters = @{}) {
    $parameters = @{
        ModelSource = $fixtureModel
        BundlePath = $script:Scenario.BundlePath
    }
    foreach ($entry in (Get-CiInputParameters).GetEnumerator()) { $parameters[$entry.Key] = $entry.Value }
    foreach ($entry in $AdditionalParameters.GetEnumerator()) { $parameters[$entry.Key] = $entry.Value }
    return @(& $fixtureBuilder @parameters)
}

function Assert-WorkerReleased([string]$ResolvedRoot, [string]$Description) {
    $worker = Join-Path $ResolvedRoot 'scribe-inference-worker.exe'
    if (-not (Test-Path -LiteralPath $worker)) { return }
    $stream = [IO.File]::Open($worker, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try { Assert-True $true "$Description released the resolver-owned worker handle." }
    finally { $stream.Dispose() }
}

$primaryFailure = $null
try {
    [IO.Directory]::CreateDirectory($fixtureRoot) | Out-Null
    $gitConfigPath = Join-Path $testRoot 'gitconfig'
    $gitEmptyDirectory = Join-Path $testRoot 'git-empty'
    $gitEmptyFile = Join-Path $testRoot 'git-empty-file'
    [IO.Directory]::CreateDirectory($gitEmptyDirectory) | Out-Null
    [IO.File]::WriteAllText($gitConfigPath, '', $script:Utf8)
    [IO.File]::WriteAllText($gitEmptyFile, '', $script:Utf8)
    foreach ($entry in @(Get-ChildItem Env:GIT_*)) { Remove-Item -LiteralPath "Env:$($entry.Name)" }
    $env:GIT_CONFIG_NOSYSTEM = '1'; $env:GIT_ATTR_NOSYSTEM = '1'; $env:GIT_CONFIG_GLOBAL = $gitConfigPath
    $env:GIT_TERMINAL_PROMPT = '0'; $env:GIT_AUTHOR_DATE = '2000-01-01T00:00:00Z'; $env:GIT_COMMITTER_DATE = '2000-01-01T00:00:00Z'
    foreach ($setting in @(
        @('core.hooksPath', $gitEmptyDirectory), @('core.attributesFile', $gitEmptyFile), @('core.excludesFile', $gitEmptyFile),
        @('core.autocrlf', 'false'), @('commit.gpgsign', 'false'), @('tag.gpgsign', 'false'), @('init.templateDir', $gitEmptyDirectory),
        @('init.defaultBranch', 'fixture'), @('user.email', 'fixture@example.invalid'), @('user.name', 'Scribe fixture')
    )) {
        & git config --file $gitConfigPath $setting[0] $setting[1]
        if ($LASTEXITCODE -ne 0) { throw 'Could not configure isolated CI consumer fixture Git settings.' }
    }
    foreach ($relativePath in @(
        '.gitignore', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/config.toml', 'build.rs', 'src/worker_identity.rs',
        '.github/workflows/release.yml', 'scripts/build-windows-release.ps1', 'scripts/resolve-windows-cpu-worker-inputs.ps1',
        'scripts/windows-frozen-cpu-worker-integrity.ps1', 'scripts/windows-cpu-worker-native-baseline.ps1', 'scripts/windows-pe-imports.ps1',
        'scripts/new-windows-frozen-cpu-worker.ps1', 'scripts/invoke-windows-gpu-approved-signing.ps1', 'scripts/stage-verified-worker-packs.ps1',
        'resources/licenses/Apache-2.0.txt', 'resources/licenses/OpenAI-Whisper-MIT.txt', 'resources/licenses/Whisper-Base-En-NOTICE.txt',
        'resources/licenses/THIRD-PARTY-NOTICES.txt', 'native/transcribe-cpp-v0.1.3/LICENSE', 'native/transcribe-cpp-v0.1.3/PROVENANCE.md',
        'native/whisper-f049fff/LICENSE', 'native/whisper-f049fff/PROVENANCE.md', 'native/sherpa-onnx-v1.13.5/PROVENANCE.md',
        'resources/silero-vad/LICENSE', 'resources/silero-vad/PROVENANCE.md'
    )) { Copy-FixtureSourceFile $relativePath }
    [IO.File]::WriteAllBytes($fixtureModel, [byte[]](1, 2, 3, 4))
    $modelHash = Get-TestHash ([IO.File]::ReadAllBytes($fixtureModel))
    $manifestPath = Join-Path $fixtureRoot 'runtime-manifests\whisper-base-en-q8_0-windows-x64.json'
    [IO.Directory]::CreateDirectory((Split-Path -Parent $manifestPath)) | Out-Null
    [IO.File]::WriteAllText($manifestPath, (@{
        model_id = 'fixture-model'; artifact_filename = 'whisper-base.en-Q8_0.gguf'; size_bytes = 4; sha256 = $modelHash
        platform_triple = 'x86_64-pc-windows-msvc'
        attribution_files = @('resources/licenses/Apache-2.0.txt', 'resources/licenses/OpenAI-Whisper-MIT.txt', 'resources/licenses/Whisper-Base-En-NOTICE.txt')
    } | ConvertTo-Json -Depth 4), $script:Utf8)
    Set-FixtureNativeSmokeSeam (Join-Path $fixtureRoot 'scripts\build-windows-release.ps1')
    Set-FixtureCompiledAdmissionSeam (Join-Path $fixtureRoot 'scripts\windows-frozen-cpu-worker-integrity.ps1')
    Set-FixtureGitHubSeam (Join-Path $fixtureRoot 'scripts\resolve-windows-cpu-worker-inputs.ps1')
    & git -C $fixtureRoot init --quiet
    if ($LASTEXITCODE -ne 0) { throw 'Could not initialize CI consumer fixture Git repository.' }
    & git -C $fixtureRoot add --all
    if ($LASTEXITCODE -ne 0) { throw 'Could not stage CI consumer fixture files.' }
    & git -C $fixtureRoot commit --quiet -m fixture
    if ($LASTEXITCODE -ne 0) { throw 'Could not commit CI consumer fixture files.' }

    $fixtureBuilder = Join-Path $fixtureRoot 'scripts\build-windows-release.ps1'
    . (Join-Path $fixtureRoot 'scripts\windows-frozen-cpu-worker-integrity.ps1')
    $global:CiCpuWorkerBuilderCargoCalls = [Collections.Generic.List[object]]::new()
    $global:CiCpuWorkerBuilderNativeCalls = [Collections.Generic.List[object]]::new()
    $global:CiCpuWorkerBuilderAdmissionCalls = [Collections.Generic.List[string]]::new()
    $global:CiCpuWorkerBuilderGitHubCalls = [Collections.Generic.List[string]]::new()

    function global:cargo {
        param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments)
        $binIndex = [array]::IndexOf($Arguments, '--bin')
        if ($binIndex -lt 0 -or $binIndex -ge ($Arguments.Count - 1)) { throw 'Synthetic Cargo seam did not receive an exact --bin argument.' }
        $binary = $Arguments[$binIndex + 1]
        $global:CiCpuWorkerBuilderCargoCalls.Add([pscustomobject]@{
            Binary = $binary; Arguments = @($Arguments); Revision = $env:SCRIBE_BUILD_REVISION
            WorkerDigest = $env:SCRIBE_BUNDLED_WORKER_SHA256; BuildingWorker = $env:SCRIBE_BUILDING_WORKER
        })
        if ($binary -ne 'local-transcriber') { throw "CI consumer rebuilt an unexpected binary: $binary" }
        if ($global:CiCpuWorkerBuilderMutateWorkerPath) {
            try {
                [IO.File]::WriteAllBytes($global:CiCpuWorkerBuilderMutateWorkerPath, [byte[]](0x66))
                $global:CiCpuWorkerBuilderMutationWasBlocked = $false
            }
            catch { $global:CiCpuWorkerBuilderMutationWasBlocked = $true }
        }
        if ($global:CiCpuWorkerBuilderSourceDriftPath) {
            [IO.File]::WriteAllText(
                $global:CiCpuWorkerBuilderSourceDriftPath,
                'untracked source drift',
                [Text.UTF8Encoding]::new($false)
            )
        }
        if (-not [string]::IsNullOrWhiteSpace([string]$global:CiCpuWorkerBuilderPostCargoMutation)) {
            $env:SCRIBE_CI_CPU_FIXTURE_POST_CARGO_MUTATION = $global:CiCpuWorkerBuilderPostCargoMutation
        }
        if ($global:CiCpuWorkerBuilderFailDesktop) { $global:LASTEXITCODE = 1; return }
        New-TestReviewedPe (Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc\release\local-transcriber.exe') 2
        $global:LASTEXITCODE = 0
    }

    # Exact ignored generated areas remain acceptable to the real clean-source
    # validator; no source-context bypass is used here.
    foreach ($path in @('.ci-release-inputs\held.zip', '.ci-tools\resolver.log', 'dist\release-assets\output.txt', 'release-assets\output.txt')) {
        $full = Join-Path $fixtureRoot $path
        [IO.Directory]::CreateDirectory((Split-Path -Parent $full)) | Out-Null
        [IO.File]::WriteAllText($full, 'generated fixture artifact', $script:Utf8)
    }
    Reset-Scenario
    $beforeGenerated = Get-WindowsFrozenCpuWorkerSourceContext $fixtureRoot
    $env:SCRIBE_BUILD_REVISION = 'hostile-caller-revision'
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = 'f' * 64
    $env:SCRIBE_BUILDING_WORKER = 'hostile-caller-worker-flag'
    Invoke-CiBuilder | Out-Null
    $afterGenerated = Get-WindowsFrozenCpuWorkerSourceContext $fixtureRoot
    Assert-Equal $beforeGenerated.SourceRevision $afterGenerated.SourceRevision 'Generated CI-only areas changed the source revision.'
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 1 'CI same-source consumer did not build exactly one desktop.'
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls[0].Binary 'local-transcriber' 'CI same-source consumer rebuilt the worker.'
    Assert-Equal (ConvertTo-Json @($global:CiCpuWorkerBuilderCargoCalls[0].Arguments) -Compress) (ConvertTo-Json @('build', '--locked', '--offline', '--release', '--bin', 'local-transcriber', '--features', 'ui-harness', '--target', 'x86_64-pc-windows-msvc', '--manifest-path', (Join-Path $fixtureRoot 'Cargo.toml')) -Compress) 'CI desktop Cargo argv changed.'
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls[0].Revision $script:Scenario.DesktopContext.SourceRevision 'CI desktop build did not force physical M revision.'
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls[0].WorkerDigest $script:Scenario.Record.worker_sha256 'CI desktop build did not anchor the resolved worker digest.'
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls[0].BuildingWorker $null 'CI desktop build inherited a worker-build marker.'
    Assert-Equal $env:SCRIBE_BUILD_REVISION 'hostile-caller-revision' 'CI builder did not restore caller build revision.'
    Assert-Equal $env:SCRIBE_BUNDLED_WORKER_SHA256 ('f' * 64) 'CI builder did not restore caller worker digest.'
    Assert-Equal $env:SCRIBE_BUILDING_WORKER 'hostile-caller-worker-flag' 'CI builder did not restore caller worker marker.'
    Assert-Equal $global:CiCpuWorkerBuilderAdmissionCalls.Count 1 'CI same-source build did not use compiled admission exactly once.'
    Assert-Equal $global:CiCpuWorkerBuilderNativeCalls.Count 1 'CI same-source build did not run the staged smoke exactly once.'
    Assert-Equal (Get-TestHash ([IO.File]::ReadAllBytes((Join-Path $script:Scenario.BundlePath 'scribe-inference-worker.exe')))) $script:Scenario.Record.worker_sha256 'CI bundle did not copy held worker bytes.'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $script:Scenario.BundlePath 'WINDOWS-FROZEN-CPU-WORKER-LOCAL-ONLY.txt'))) 'CI bundle emitted a local-only frozen marker.'
    Assert-True (Test-Path -LiteralPath (Join-Path $fixtureRoot 'dist\worker-pack-allowlist.iss') -PathType Leaf) 'CI build did not generate the normal dist worker-pack allowlist required by Inno Setup.'
    Assert-True ($global:CiCpuWorkerBuilderGitHubCalls.Count -ge 24) 'CI consumer did not perform initial, post-Cargo, and activation metadata checks.'
    Assert-WorkerReleased $script:Scenario.ResolvedRoot 'Successful CI consumer'

    Reset-Scenario ('a' * 40)
    Invoke-CiBuilder | Out-Null
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 1 'Compiled foreign CI worker did not build exactly one desktop.'
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls[0].Revision $script:Scenario.DesktopContext.SourceRevision 'Compiled foreign CI worker changed M desktop identity.'
    Assert-Equal (Get-TestHash ([IO.File]::ReadAllBytes((Join-Path $script:Scenario.BundlePath 'scribe-inference-worker.exe')))) $script:Scenario.Record.worker_sha256 'Compiled foreign CI worker did not copy R bytes.'
    Assert-WorkerReleased $script:Scenario.ResolvedRoot 'Compiled foreign CI consumer'

    Reset-Scenario ('a' * 40)
    Set-AdmissionResponse $script:Scenario.DesktopContext $script:Scenario.WorkerContext $script:Scenario.Record 'kind'
    Assert-Rejected 'compiled foreign worker refusal' { Invoke-CiBuilder }
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 1 'Compiled foreign worker refusal did not stop after one desktop build.'
    Assert-True (-not (Test-Path -LiteralPath $script:Scenario.BundlePath)) 'Compiled foreign worker refusal published a bundle.'
    Assert-WorkerReleased $script:Scenario.ResolvedRoot 'Compiled foreign worker refusal'

    foreach ($missing in @((Get-CiInputParameters).Keys)) {
        Reset-Scenario
        $partial = Get-CiInputParameters
        $partial.Remove($missing)
        Assert-Rejected "partial CI pins missing $missing" { & $fixtureBuilder -ModelSource $fixtureModel -BundlePath $script:Scenario.BundlePath @partial }
        Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 0 "Partial CI pins missing $missing invoked Cargo."
    }
    foreach ($blank in @((Get-CiInputParameters).Keys)) {
        Reset-Scenario
        $blankPins = Get-CiInputParameters
        $blankPins[$blank] = ' '
        Assert-Rejected "blank CI pin $blank" { Invoke-CiBuilder $blankPins }
        Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 0 "Blank CI pin $blank invoked Cargo."
    }

    Reset-Scenario
    Assert-Rejected 'CI/local frozen input conflict' { Invoke-CiBuilder @{ FrozenCpuWorkerRecordPath = (Join-Path $testRoot 'local-record.json') } }
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 0 'CI/local frozen conflict invoked Cargo.'
    Reset-Scenario
    Assert-Rejected 'CI/local observation conflict' { Invoke-CiBuilder @{ LocalFrozenGpuObservation = $true } }
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 0 'CI/local observation conflict invoked Cargo.'

    Reset-Scenario
    $env:GITHUB_SHA = 'not-a-revision'
    Assert-Rejected 'invalid CI installer context' { Invoke-CiBuilder }
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 0 'Invalid CI installer context invoked Cargo.'

    Reset-Scenario
    Assert-Rejected 'invalid CI worker revision pin' { Invoke-CiBuilder @{ CiCpuWorkerSourceRevision = 'not-a-revision' } }
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 0 'Invalid CI worker revision pin invoked Cargo.'

    Reset-Scenario
    [IO.File]::WriteAllText($script:Scenario.ArchivePath, 'not a ZIP', $script:Utf8)
    $script:Scenario.ArchiveHash = Get-TestHash ([IO.File]::ReadAllBytes($script:Scenario.ArchivePath))
    $env:SCRIBE_CI_CPU_FIXTURE_ARCHIVE_SHA256 = $script:Scenario.ArchiveHash
    $env:SCRIBE_CI_CPU_FIXTURE_ARCHIVE_SIZE = [string](Get-Item -LiteralPath $script:Scenario.ArchivePath).Length
    Assert-Rejected 'raw ZIP rejection' { Invoke-CiBuilder }
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 0 'Raw ZIP rejection invoked Cargo.'

    Reset-Scenario
    $badRecord = [ordered]@{}
    foreach ($entry in $script:Scenario.Record.GetEnumerator()) {
        $badRecord[[string]$entry.Key] = $entry.Value
    }
    $badRecord.kind = 'wrong-kind'
    [IO.File]::Delete($script:Scenario.ArchivePath)
    New-TestArchive $script:Scenario.ArchivePath @(
        [pscustomobject]@{ Name = 'windows-ci-cpu-worker.json'; Bytes = $script:Utf8.GetBytes(($badRecord | ConvertTo-Json -Depth 5)) },
        [pscustomobject]@{ Name = 'scribe-inference-worker.exe'; Bytes = $script:Scenario.WorkerBytes }
    )
    $script:Scenario.ArchiveHash = Get-TestHash ([IO.File]::ReadAllBytes($script:Scenario.ArchivePath))
    $env:SCRIBE_CI_CPU_FIXTURE_ARCHIVE_SHA256 = $script:Scenario.ArchiveHash
    $env:SCRIBE_CI_CPU_FIXTURE_ARCHIVE_SIZE = [string](Get-Item -LiteralPath $script:Scenario.ArchivePath).Length
    Assert-Rejected 'invalid CI worker record' { Invoke-CiBuilder }
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 0 'Invalid CI worker record invoked Cargo.'

    Reset-Scenario
    [IO.Directory]::CreateDirectory($script:Scenario.ResolvedRoot) | Out-Null
    [IO.File]::WriteAllText((Join-Path $script:Scenario.ResolvedRoot 'sentinel'), 'preserve', $script:Utf8)
    Assert-Rejected 'resolved output collision' { Invoke-CiBuilder }
    Assert-Equal ([IO.File]::ReadAllText((Join-Path $script:Scenario.ResolvedRoot 'sentinel'), $script:Utf8)) 'preserve' 'Resolver collision changed existing output.'
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 0 'Resolved output collision invoked Cargo.'

    foreach ($mutation in @('anchor', 'desktop', 'origin', 'protocol', 'abi', 'kind', 'error')) {
        Reset-Scenario
        Set-AdmissionResponse $script:Scenario.DesktopContext $script:Scenario.WorkerContext $script:Scenario.Record $mutation
        Assert-Rejected "compiled admission $mutation" { Invoke-CiBuilder }
        Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 1 "Compiled admission $mutation did not stop after the desktop-only Cargo build."
        Assert-True (-not (Test-Path -LiteralPath $script:Scenario.BundlePath)) "Compiled admission $mutation published a bundle."
        Assert-WorkerReleased $script:Scenario.ResolvedRoot "Compiled admission $mutation"
    }

    Reset-Scenario
    $global:CiCpuWorkerBuilderFailDesktop = $true
    Assert-Rejected 'desktop Cargo failure' { Invoke-CiBuilder }
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 1 'Desktop Cargo failure did not reach exactly one desktop build.'
    Assert-WorkerReleased $script:Scenario.ResolvedRoot 'Desktop Cargo failure'

    Reset-Scenario
    $global:CiCpuWorkerBuilderFailSmoke = $true
    Assert-Rejected 'staged smoke failure' { Invoke-CiBuilder }
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 1 'Staged smoke failure rebuilt or skipped the desktop.'
    Assert-WorkerReleased $script:Scenario.ResolvedRoot 'Staged smoke failure'

    foreach ($drift in @('attempt', 'digest', 'expiry')) {
        Reset-Scenario
        $global:CiCpuWorkerBuilderPostCargoMutation = $drift
        Assert-Rejected "post-Cargo producer $drift drift" { Invoke-CiBuilder }
        Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 1 "Post-Cargo producer $drift drift did not stop after desktop Cargo."
        Assert-True (-not (Test-Path -LiteralPath $script:Scenario.BundlePath)) "Post-Cargo producer $drift drift published a bundle."
        Assert-WorkerReleased $script:Scenario.ResolvedRoot "Post-Cargo producer $drift drift"
    }

    foreach ($drift in @('attempt', 'digest', 'expiry')) {
        Reset-Scenario
        $global:CiCpuWorkerBuilderActivationMutation = $drift
        Assert-Rejected "final activation producer $drift drift" { Invoke-CiBuilder }
        Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 1 "Final activation producer $drift drift did not stop after desktop Cargo."
        Assert-Equal $global:CiCpuWorkerBuilderNativeCalls.Count 1 "Final activation producer $drift drift did not reach the staged smoke boundary."
        Assert-True (-not (Test-Path -LiteralPath $script:Scenario.BundlePath)) "Final activation producer $drift drift published a bundle."
        Assert-WorkerReleased $script:Scenario.ResolvedRoot "final activation producer $drift drift"
    }

    Reset-Scenario
    $global:CiCpuWorkerBuilderSourceDriftPath = Join-Path $fixtureRoot 'untracked-source-drift.txt'
    Assert-Rejected 'M source drift after desktop Cargo' { Invoke-CiBuilder }
    Assert-True (Test-Path -LiteralPath $global:CiCpuWorkerBuilderSourceDriftPath -PathType Leaf) 'M source drift fixture did not write its untracked sentinel after desktop Cargo.'
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 1 'M source drift did not occur after desktop Cargo.'
    Assert-True (-not (Test-Path -LiteralPath $script:Scenario.BundlePath)) 'M source drift published a bundle.'
    Remove-Item -LiteralPath $global:CiCpuWorkerBuilderSourceDriftPath -Force
    Assert-WorkerReleased $script:Scenario.ResolvedRoot 'M source drift'

    Reset-Scenario
    $global:CiCpuWorkerBuilderMutateWorkerPath = Join-Path $script:Scenario.ResolvedRoot 'scribe-inference-worker.exe'
    Invoke-CiBuilder | Out-Null
    Assert-True $global:CiCpuWorkerBuilderMutationWasBlocked 'Held resolved worker did not block desktop-Cargo write mutation.'
    Assert-WorkerReleased $script:Scenario.ResolvedRoot 'Held worker success path'

    Reset-Scenario
    $global:CiCpuWorkerBuilderMutateStagedWorker = $true
    Assert-Rejected 'staged worker mutation after smoke begins' { Invoke-CiBuilder }
    Assert-True $global:CiCpuWorkerBuilderStagedWorkerMutationAttempted 'Staged worker mutation did not reach the smoke boundary.'
    Assert-True $global:CiCpuWorkerBuilderStagedWorkerMutationSucceeded 'Staged worker mutation fixture did not alter the staged worker.'
    Assert-Equal $global:CiCpuWorkerBuilderNativeCalls.Count 1 'Staged worker mutation did not run exactly one smoke invocation.'
    Assert-True (-not (Test-Path -LiteralPath $script:Scenario.BundlePath)) 'Staged worker mutation published a bundle.'
    Assert-WorkerReleased $script:Scenario.ResolvedRoot 'Staged worker mutation'

    Reset-Scenario
    $global:CiCpuWorkerBuilderRaceBundle = $script:Scenario.BundlePath
    Assert-Rejected 'final bundle output race' { Invoke-CiBuilder }
    Assert-True (Test-Path -LiteralPath $script:Scenario.BundlePath) 'Final bundle race fixture did not create the competing output.'
    Assert-WorkerReleased $script:Scenario.ResolvedRoot 'Final bundle output race'

    # A real source-context check—not a mocked Git status—must reject a normal
    # untracked file even though the exact generated directories above remain ignored.
    Reset-Scenario
    $untracked = Join-Path $fixtureRoot 'ordinary-untracked-source.txt'
    [IO.File]::WriteAllText($untracked, 'must make the source dirty', $script:Utf8)
    Assert-Rejected 'ordinary untracked source' { Invoke-CiBuilder }
    Assert-Equal $global:CiCpuWorkerBuilderCargoCalls.Count 0 'Ordinary untracked source bypassed clean-source validation.'
    Remove-Item -LiteralPath $untracked -Force

    # Count the explicit scenarios, not filesystem-dependent cleanup assertions.
    Assert-Equal $script:ScenarioCount 44 'Expected CI CPU worker consumer scenarios were not all discovered.'
}
catch {
    $primaryFailure = $_
    throw
}
finally {
    try {
        if ($null -ne $savedGlobalCargo) { Set-Item -LiteralPath Function:global:cargo -Value $savedGlobalCargoScriptBlock } else { Remove-Item Function:global:cargo -ErrorAction SilentlyContinue }
        foreach ($name in $fixtureGlobalVariableNames) {
            $saved = $savedFixtureGlobals[$name]
            if ($saved.Exists) {
                Set-Variable -Name $name -Scope Global -Value $saved.Value -Force
            }
            else {
                Remove-Variable -Name $name -Scope Global -Force -ErrorAction SilentlyContinue
            }
        }
        foreach ($name in $environmentNames) { [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name]) }
        foreach ($entry in @(Get-ChildItem Env:GIT_*)) { Remove-Item -LiteralPath "Env:$($entry.Name)" }
        foreach ($name in $savedGitEnvironment.Keys) { Set-Item -LiteralPath "Env:$name" -Value $savedGitEnvironment[$name] }
        if ($hadSavedLastExitCode) { $global:LASTEXITCODE = $savedLastExitCode } else { Remove-Variable -Name LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue }
        Remove-OwnedTestRoot $testRoot
    }
    catch {
        if ($null -ne $primaryFailure) {
            Write-Warning "CI CPU worker packaging fixture cleanup also failed after the primary test error: $($_.Exception.Message)"
        }
        else {
            throw
        }
    }
}

if ($null -ne $savedGlobalCargo) {
    $restoredGlobalCargo = Get-Item -LiteralPath Function:global:cargo -ErrorAction SilentlyContinue
    Assert-Test ($null -ne $restoredGlobalCargo -and
        $restoredGlobalCargo.ScriptBlock.ToString() -ceq $savedGlobalCargoScriptBlock.ToString()) `
        'Packaging fixture did not restore the caller global cargo function.'
}
else {
    Assert-Test ($null -eq (Get-Item -LiteralPath Function:global:cargo -ErrorAction SilentlyContinue)) `
        'Packaging fixture leaked a global cargo function into its caller.'
}
foreach ($name in $fixtureGlobalVariableNames) {
    $saved = $savedFixtureGlobals[$name]
    $restored = Get-Variable -Name $name -Scope Global -ErrorAction SilentlyContinue
    Assert-Test (($null -ne $restored) -eq [bool]$saved.Exists) "Packaging fixture did not restore global $name presence."
    if ($saved.Exists) {
        $valuesMatch = if ($null -eq $saved.Value) {
            $null -eq $restored.Value
        }
        elseif ($saved.Value -is [string] -or $saved.Value.GetType().IsValueType) {
            $restored.Value -ceq $saved.Value
        }
        else {
            [object]::ReferenceEquals($restored.Value, $saved.Value)
        }
        Assert-Test $valuesMatch "Packaging fixture did not restore global $name value."
    }
}

Write-Output "Windows CI CPU worker consumer tests passed ($script:ScenarioCount scenarios; $script:Assertions assertions)."
