[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# This is deliberately an offline contract test.  It replaces only the
# protected GitHub/source/native-admission seams; ZIP bytes, filesystem paths,
# hashing, JSON parsing, output activation, and held-handle behavior remain
# real.
$repositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$testRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-windows-cpu-worker-inputs-$([guid]::NewGuid().ToString('N'))"
$script:CpuWorkerInputAssertions = 0
$script:GitHubCalls = [Collections.Generic.List[string]]::new()
$script:ExportAdmissionCalls = [Collections.Generic.List[object]]::new()
$script:PeCalls = [Collections.Generic.List[object]]::new()
$script:TestRoot = $testRoot
$script:Utf8 = [Text.UTF8Encoding]::new($false)

$environmentNames = @(
    'GITHUB_ACTIONS', 'CI', 'GITHUB_EVENT_NAME', 'GITHUB_REPOSITORY',
    'GITHUB_REF', 'GITHUB_WORKFLOW', 'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT',
    'GITHUB_SHA', 'GITHUB_WORKSPACE'
)
$savedEnvironment = @{}
foreach ($name in $environmentNames) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
}
$lastExitVariable = Get-Variable -Name LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue
$hadSavedLastExitCode = $null -ne $lastExitVariable
$savedLastExitCode = if ($hadSavedLastExitCode) { [int]$lastExitVariable.Value } else { $null }

function Assert-Test([bool]$Condition, [string]$Message) {
    $script:CpuWorkerInputAssertions++
    if (-not $Condition) { throw "TEST FAILED: $Message" }
}

function Assert-Rejected([string]$Name, [scriptblock]$Action) {
    $script:CpuWorkerInputAssertions++
    try {
        $result = @(& $Action)
        foreach ($item in $result) {
            if ($null -ne $item -and $null -ne $item.PSObject.Properties['WorkerStream'] -and
                $null -ne $item.WorkerStream) {
                $item.WorkerStream.Dispose()
            }
        }
        throw "TEST FAILED: $Name was accepted."
    }
    catch {
        if ($_.Exception.Message.StartsWith('TEST FAILED:', [StringComparison]::Ordinal)) { throw }
    }
}

function Assert-Equal($Actual, $Expected, [string]$Message) {
    $script:CpuWorkerInputAssertions++
    if ($Actual -cne $Expected) { throw "TEST FAILED: $Message Expected '$Expected', got '$Actual'." }
}

function Write-TestBytes([string]$Path, [byte[]]$Bytes) {
    [IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
    [IO.File]::WriteAllBytes($Path, $Bytes)
}

function Write-TestJson([string]$Path, $Value) {
    Write-TestBytes $Path $script:Utf8.GetBytes(($Value | ConvertTo-Json -Depth 32 -Compress))
}

function Get-TestHash([byte[]]$Bytes) {
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Bytes)).ToLowerInvariant()
}

function Get-TestFileHash([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function New-TestContext([string]$SourceRevision) {
    return [pscustomobject]@{
        RepositoryRoot = $repositoryRoot
        SourceRevision = $SourceRevision
        AppVersion = '0.1.0'
        TargetTriple = 'x86_64-pc-windows-msvc'
        ProtocolVersion = [int64]5
        WorkerAbiVersion = [int64]1
        DesktopBuildId = "local-transcriber@0.1.0#$SourceRevision"
        WorkerBuildId = "scribe-inference-worker@0.1.0#$SourceRevision"
        CargoLockSha256 = '3' * 64
        RustToolchainSha256 = '4' * 64
        CargoManifestSha256 = '5' * 64
        WorkerIdentitySha256 = '6' * 64
        BuildRsSha256 = '7' * 64
        BuildContractSha256 = '8' * 64
    }
}

function New-TestCpuWorkerRecord(
    [psobject]$Context,
    [string]$SourceRevision,
    [string]$RunId,
    [string]$RunAttempt,
    [byte[]]$WorkerBytes
) {
    return [ordered]@{
        schema_version = 1
        kind = 'windows-ci-cpu-worker'
        source_repository = 'tyhuang9/scribe'
        source_ref = 'refs/heads/main'
        workflow = '.github/workflows/release.yml'
        run_id = $RunId
        run_attempt = $RunAttempt
        source_revision = $SourceRevision
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
        worker_relative_path = 'scribe-inference-worker.exe'
        worker_size_bytes = [int64]$WorkerBytes.Length
        worker_sha256 = Get-TestHash $WorkerBytes
    }
}

function New-TestArchive([string]$Path, [object[]]$Entries) {
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $zip = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create, $false)
        try {
            foreach ($entrySpec in $Entries) {
                $entry = $zip.CreateEntry([string]$entrySpec.Name, [IO.Compression.CompressionLevel]::NoCompression)
                $attributes = $entrySpec.PSObject.Properties['ExternalAttributes']
                if ($null -ne $attributes -and $null -ne $attributes.Value) { $entry.ExternalAttributes = [int]$attributes.Value }
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
        (Split-Path -Leaf $root) -cmatch '^scribe-windows-cpu-worker-inputs-[0-9a-f]{32}$') `
        'Refused CPU worker input fixture cleanup outside the exact temporary root.'
    $current = $root
    while ($true) {
        $item = Get-Item -LiteralPath $current -Force
        Assert-Test (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Refused test cleanup through a reparse point.'
        if ([string]::Equals($current, $temp, [StringComparison]::OrdinalIgnoreCase)) { break }
        $parent = Split-Path -Parent $current
        Assert-Test (-not [string]::IsNullOrWhiteSpace($parent) -and $parent -cne $current) 'Could not prove test cleanup ancestry.'
        $current = $parent
    }
    foreach ($item in @(Get-ChildItem -LiteralPath $root -Recurse -Force)) {
        Assert-Test (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Refused test cleanup containing a reparse point.'
    }
    [IO.Directory]::Delete("\\?\$root", $true)
}

. (Join-Path $PSScriptRoot 'export-windows-cpu-worker-artifact.ps1') -FunctionsOnly

$savedExportSourceContext = ${function:Get-WindowsCpuArtifactExportSourceContext}
$savedExportAdmission = ${function:Assert-WindowsCpuArtifactExportCompiledAdmission}
$savedPeVerifier = ${function:Assert-ReviewedWindowsPe}
$savedFrozenSourceContext = ${function:Get-WindowsFrozenCpuWorkerSourceContext}

function Get-WindowsCpuArtifactExportSourceContext([string]$RepositoryRoot) {
    return $script:Fixture.ExportContext
}

function Assert-WindowsCpuArtifactExportCompiledAdmission(
    [string]$DesktopPath,
    [int64]$DesktopSize,
    [string]$DesktopSha256,
    [psobject]$Context,
    [string]$WorkerSha256
) {
    $script:ExportAdmissionCalls.Add([pscustomobject]@{
        DesktopPath = $DesktopPath; DesktopSize = $DesktopSize; DesktopSha256 = $DesktopSha256
        Context = $Context; WorkerSha256 = $WorkerSha256
    })
    if ($script:Fixture.ExportAdmissionFails) { throw 'Deterministic compiled CPU worker admission rejection.' }
    return $true
}

function Assert-ReviewedWindowsPe([string]$Path, [uint16]$ExpectedSubsystem = 2) {
    $script:PeCalls.Add([pscustomobject]@{ Path = $Path; Subsystem = $ExpectedSubsystem })
    if ($script:Fixture.PeFails) { throw 'Deterministic PE rejection.' }
    return [pscustomobject]@{ Machine = 0x8664; Subsystem = $ExpectedSubsystem }
}

. (Join-Path $PSScriptRoot 'resolve-windows-cpu-worker-inputs.ps1') -FunctionsOnly

$savedInputSourceContext = ${function:Get-WindowsCpuWorkerInputSourceContext}
$savedInputGitHubGet = ${function:Invoke-WindowsCpuWorkerInputGitHubGet}

# Loading the resolver re-dots the common integrity helpers, so this source
# context seam must be installed after both implementation scripts are loaded.
function Get-WindowsFrozenCpuWorkerSourceContext([string]$RepositoryRoot) {
    if ($null -ne $script:Fixture -and $script:Fixture.Contains('ExportContext')) {
        return $script:Fixture.ExportContext
    }
    return $script:Fixture.InstallerContext
}

function Get-WindowsCpuWorkerInputSourceContext([string]$RepositoryRoot) {
    return $script:Fixture.InstallerContext
}

function New-TestRun([string]$RunId, [string]$Attempt, [string]$Revision) {
    return [ordered]@{
        id = [int64]$RunId
        run_attempt = [int64]$Attempt
        repository = [ordered]@{ full_name = 'tyhuang9/scribe' }
        head_repository = [ordered]@{ full_name = 'tyhuang9/scribe' }
        path = '.github/workflows/release.yml'
        event = 'workflow_dispatch'
        head_branch = 'main'
        status = 'completed'
        conclusion = 'success'
        head_sha = $Revision
    }
}

function New-TestArtifact([string]$ArtifactId, [string]$RunId, [string]$Revision, [string]$Name, [string]$Digest, [int64]$Size) {
    return [ordered]@{
        id = [int64]$ArtifactId
        workflow_run = [ordered]@{ id = [int64]$RunId; head_sha = $Revision; head_branch = 'main' }
        name = $Name
        expired = $false
        digest = "sha256:$Digest"
        size_in_bytes = $Size
    }
}

function Invoke-WindowsCpuWorkerInputGitHubGet([string]$Path) {
    $script:GitHubCalls.Add($Path)
    $prefix = '/repos/tyhuang9/scribe'
    switch -CaseSensitive ($Path) {
        "$prefix/compare/$($script:Fixture.WorkerRevision)...$($script:Fixture.SourceRevision)" { return $script:Fixture.WorkerComparison }
        "$prefix/git/ref/heads/main" { return [ordered]@{ object = [ordered]@{ sha = $script:Fixture.MainRevision } } }
        "$prefix/compare/$($script:Fixture.SourceRevision)...$($script:Fixture.MainRevision)" { return $script:Fixture.InstallerComparison }
        "$prefix/actions/runs/$($script:Fixture.ProducerRunId)/attempts/$($script:Fixture.ProducerRunAttempt)" { return $script:Fixture.Run }
        "$prefix/actions/runs/$($script:Fixture.ProducerRunId)" { return $script:Fixture.Latest }
        "$prefix/actions/artifacts/$($script:Fixture.ArtifactId)" {
            $script:Fixture.ArtifactReadCount = [int]$script:Fixture.ArtifactReadCount + 1
            if ($script:Fixture.ArtifactReadCount -eq 2 -and -not [string]::IsNullOrEmpty($script:Fixture.LateArtifactDigest)) {
                $script:Fixture.Artifact.digest = $script:Fixture.LateArtifactDigest
            }
            if ($script:Fixture.ArtifactReadCount -eq 2 -and $null -ne $script:Fixture.LateArtifactSizeBytes) {
                $script:Fixture.Artifact.size_in_bytes = [int64]$script:Fixture.LateArtifactSizeBytes
            }
            return $script:Fixture.Artifact
        }
        default { throw "Unexpected offline CPU worker GitHub request: $Path" }
    }
}

function Get-TestRecordBytes($Record) {
    $bytes = $script:Utf8.GetBytes(($Record | ConvertTo-Json -Depth 5))
    return ,$bytes
}

function Copy-TestRecord($Record) {
    $copy = [ordered]@{}
    foreach ($key in $Record.Keys) { $copy[[string]$key] = $Record[$key] }
    return $copy
}

function Get-ValidResolverEntries {
    return @(
        [pscustomobject]@{ Name = 'windows-ci-cpu-worker.json'; Bytes = $script:Fixture.RecordBytes },
        [pscustomobject]@{ Name = 'scribe-inference-worker.exe'; Bytes = $script:Fixture.WorkerBytes }
    )
}

function Set-ResolverArchive([object[]]$Entries) {
    $archive = Join-Path $script:TestRoot "cpu-worker-$([guid]::NewGuid().ToString('N')).zip"
    New-TestArchive $archive $Entries
    $script:Fixture.ArchivePath = $archive
    $script:Fixture.Artifact.digest = "sha256:$(Get-TestFileHash $archive)"
    $script:Fixture.Artifact.size_in_bytes = [int64](Get-Item -LiteralPath $archive -Force).Length
}

function Reset-ResolverFixture([switch]$ForeignWorkerSource) {
    $sourceRevision = 'b' * 40
    $workerRevision = if ($ForeignWorkerSource) { 'a' * 40 } else { $sourceRevision }
    $mainRevision = 'c' * 40
    $installerContext = New-TestContext $sourceRevision
    $workerContext = New-TestContext $workerRevision
    $workerBytes = $script:Utf8.GetBytes("fixture worker bytes for $workerRevision")
    $record = New-TestCpuWorkerRecord $workerContext $workerRevision '8001' '1' $workerBytes
    $recordBytes = Get-TestRecordBytes $record
    $outputParent = Join-Path $script:TestRoot "resolve-output-$([guid]::NewGuid().ToString('N'))"
    [IO.Directory]::CreateDirectory($outputParent) | Out-Null
    $script:Fixture = [ordered]@{
        SourceRevision = $sourceRevision
        WorkerRevision = $workerRevision
        MainRevision = $mainRevision
        InstallerContext = $installerContext
        WorkerContext = $workerContext
        ProducerRunId = '8001'
        ProducerRunAttempt = '1'
        ArtifactId = '8101'
        Run = New-TestRun '8001' '1' $workerRevision
        Latest = New-TestRun '8001' '1' $workerRevision
        Artifact = $null
        WorkerComparison = [ordered]@{ merge_base_commit = [ordered]@{ sha = $workerRevision }; status = if ($ForeignWorkerSource) { 'ahead' } else { 'identical' } }
        InstallerComparison = [ordered]@{ merge_base_commit = [ordered]@{ sha = $sourceRevision }; status = 'ahead' }
        WorkerBytes = $workerBytes
        Record = $record
        RecordBytes = $recordBytes
        ArchivePath = $null
        OutputParent = $outputParent
        ArtifactReadCount = 0
        LateArtifactDigest = $null
        LateArtifactSizeBytes = $null
    }
    $script:Fixture.Artifact = New-TestArtifact '8101' '8001' $workerRevision 'windows-cpu-worker-8001-1' ('0' * 64) 1
    Set-ResolverArchive (Get-ValidResolverEntries)
    $script:GitHubCalls.Clear()
    $env:GITHUB_ACTIONS = 'true'; $env:CI = 'true'; $env:GITHUB_EVENT_NAME = 'workflow_dispatch'
    $env:GITHUB_REPOSITORY = 'tyhuang9/scribe'; $env:GITHUB_REF = 'refs/heads/main'
    $env:GITHUB_WORKFLOW = 'Build Windows installer'; $env:GITHUB_SHA = $sourceRevision; $env:GITHUB_WORKSPACE = $repositoryRoot
}

function Invoke-TestPreflight {
    return Invoke-WindowsCpuWorkerInputPreflight `
        -SourceRevision $script:Fixture.SourceRevision `
        -WorkerSourceRevision $script:Fixture.WorkerRevision `
        -ProducerRunId $script:Fixture.ProducerRunId `
        -ProducerRunAttempt $script:Fixture.ProducerRunAttempt `
        -ArtifactId $script:Fixture.ArtifactId
}

function Invoke-TestResolve(
    [string]$OutputName = 'resolved-worker',
    [string]$ExpectedArtifactSha256 = '',
    [string]$OutputDirectory = ''
) {
    if ([string]::IsNullOrEmpty($ExpectedArtifactSha256)) {
        $ExpectedArtifactSha256 = ([string]$script:Fixture.Artifact.digest).Substring(7)
    }
    if ([string]::IsNullOrEmpty($OutputDirectory)) {
        $OutputDirectory = Join-Path $script:Fixture.OutputParent $OutputName
    }
    return Resolve-WindowsCpuWorkerInputs `
        -SourceRevision $script:Fixture.SourceRevision `
        -WorkerSourceRevision $script:Fixture.WorkerRevision `
        -ProducerRunId $script:Fixture.ProducerRunId `
        -ProducerRunAttempt $script:Fixture.ProducerRunAttempt `
        -ArtifactId $script:Fixture.ArtifactId `
        -ExpectedArtifactSha256 $ExpectedArtifactSha256 `
        -ArchivePath $script:Fixture.ArchivePath `
        -OutputDirectory $OutputDirectory
}

function Reset-ExportFixture {
    $revision = 'a' * 40
    $context = New-TestContext $revision
    $fixtureId = [guid]::NewGuid().ToString('N')
    $bundle = Join-Path $script:TestRoot "export-bundle-$fixtureId"
    $outputParent = Join-Path $script:TestRoot "export-output-$fixtureId"
    $desktopBytes = $script:Utf8.GetBytes('fixture desktop executable bytes')
    $workerBytes = $script:Utf8.GetBytes('fixture inference worker executable bytes')
    [IO.Directory]::CreateDirectory($bundle) | Out-Null
    [IO.Directory]::CreateDirectory($outputParent) | Out-Null
    Write-TestBytes (Join-Path $bundle 'local-transcriber.exe') $desktopBytes
    Write-TestBytes (Join-Path $bundle 'scribe-inference-worker.exe') $workerBytes
    Write-TestJson (Join-Path $bundle 'bundle-inventory.json') ([ordered]@{
        schema_version = 1
        platform_triple = 'x86_64-pc-windows-msvc'
        files = @(
            [ordered]@{ path = 'local-transcriber.exe'; size_bytes = [int64]$desktopBytes.Length; sha256 = Get-TestHash $desktopBytes },
            [ordered]@{ path = 'scribe-inference-worker.exe'; size_bytes = [int64]$workerBytes.Length; sha256 = Get-TestHash $workerBytes }
        )
    })
    $script:Fixture = [ordered]@{
        ExportContext = $context; Revision = $revision; Bundle = $bundle; OutputParent = $outputParent
        DesktopBytes = $desktopBytes; WorkerBytes = $workerBytes; ExportAdmissionFails = $false; PeFails = $false
    }
    $script:ExportAdmissionCalls.Clear()
    $script:PeCalls.Clear()
    $env:GITHUB_ACTIONS = 'true'; $env:CI = 'true'; $env:GITHUB_EVENT_NAME = 'workflow_dispatch'
    $env:GITHUB_REPOSITORY = 'tyhuang9/scribe'; $env:GITHUB_REF = 'refs/heads/main'
    $env:GITHUB_WORKFLOW = 'Build Windows installer'; $env:GITHUB_RUN_ID = '7001'; $env:GITHUB_RUN_ATTEMPT = '2'
    $env:GITHUB_SHA = $revision; $env:GITHUB_WORKSPACE = $repositoryRoot
}

function Invoke-TestExport([string]$OutputName = 'worker-artifact') {
    return Invoke-WindowsCpuWorkerArtifactExport `
        -BundlePath $script:Fixture.Bundle `
        -OutputDirectory (Join-Path $script:Fixture.OutputParent $OutputName)
}

try {
    [IO.Directory]::CreateDirectory($testRoot) | Out-Null

    Reset-ExportFixture
    $exported = Invoke-TestExport
    Assert-Test (Test-Path -LiteralPath $exported.WorkerPath -PathType Leaf) 'Exporter did not create the worker payload.'
    Assert-Test (Test-Path -LiteralPath $exported.RecordPath -PathType Leaf) 'Exporter did not create the provenance record.'
    Assert-Test (@(Get-ChildItem -LiteralPath $exported.Root -Force).Count -eq 2) 'Exporter output must contain exactly worker and record.'
    Assert-Equal (Get-TestFileHash $exported.WorkerPath) (Get-TestHash $script:Fixture.WorkerBytes) 'Exporter changed worker bytes.'
    Assert-Equal $exported.Record.kind 'windows-ci-cpu-worker' 'Exporter record kind changed.'
    Assert-Equal $exported.Record.source_repository 'tyhuang9/scribe' 'Exporter record repository binding changed.'
    Assert-Equal $exported.Record.source_ref 'refs/heads/main' 'Exporter record ref binding changed.'
    Assert-Equal $exported.Record.workflow '.github/workflows/release.yml' 'Exporter record workflow binding changed.'
    Assert-Equal $exported.Record.run_id '7001' 'Exporter record run ID changed.'
    Assert-Equal $exported.Record.run_attempt '2' 'Exporter record attempt changed.'
    Assert-Equal $exported.Record.source_revision $script:Fixture.Revision 'Exporter record source revision changed.'
    Assert-Equal $exported.Record.worker_sha256 (Get-TestHash $script:Fixture.WorkerBytes) 'Exporter record worker hash changed.'
    Assert-Test ($script:ExportAdmissionCalls.Count -eq 1) 'Exporter did not require compiled admission exactly once.'
    Assert-Test ($script:PeCalls.Count -eq 2 -and $script:PeCalls[0].Subsystem -eq 2 -and $script:PeCalls[1].Subsystem -eq 3) 'Exporter did not validate desktop and worker PE contracts.'

    Reset-ExportFixture
    Write-TestBytes (Join-Path $script:Fixture.Bundle 'WINDOWS-FROZEN-CPU-WORKER-LOCAL-ONLY.txt') $script:Utf8.GetBytes('local only')
    Assert-Rejected 'local-only bundle marker' { $null = Invoke-TestExport }
    Assert-Test (-not (Test-Path -LiteralPath (Join-Path $script:Fixture.OutputParent 'worker-artifact'))) 'Rejected local marker created output.'

    Reset-ExportFixture
    $insideBundleOutput = Join-Path $script:Fixture.Bundle 'must-not-export-here'
    Assert-Rejected 'export output overlapping bundle' {
        $null = Invoke-WindowsCpuWorkerArtifactExport -BundlePath $script:Fixture.Bundle -OutputDirectory $insideBundleOutput
    }
    Assert-Test (-not (Test-Path -LiteralPath $insideBundleOutput)) 'Rejected bundle-overlap export created output.'

    Reset-ExportFixture
    $insideSourceOutput = Join-Path $repositoryRoot ".must-not-export-$([guid]::NewGuid().ToString('N'))"
    Assert-Rejected 'export output overlapping trusted source' {
        $null = Invoke-WindowsCpuWorkerArtifactExport -BundlePath $script:Fixture.Bundle -OutputDirectory $insideSourceOutput
    }
    Assert-Test (-not (Test-Path -LiteralPath $insideSourceOutput)) 'Rejected source-overlap export created output.'

    foreach ($name in @('local-transcriber.exe', 'scribe-inference-worker.exe')) {
        Reset-ExportFixture
        $bundleFile = Join-Path $script:Fixture.Bundle $name
        $hardlinkTarget = Join-Path $script:TestRoot "export-hardlink-target-$name-$([guid]::NewGuid().ToString('N'))"
        Write-TestBytes $hardlinkTarget ([IO.File]::ReadAllBytes($bundleFile))
        Remove-Item -LiteralPath $bundleFile -Force
        New-Item -ItemType HardLink -Path $bundleFile -Target $hardlinkTarget | Out-Null
        Assert-Rejected "export hardlinked $name" { $null = Invoke-TestExport }
        Assert-Test (-not (Test-Path -LiteralPath (Join-Path $script:Fixture.OutputParent 'worker-artifact'))) "Rejected hardlinked $name created output."
    }

    Reset-ExportFixture
    $script:Fixture.ExportContext = New-TestContext ('b' * 40)
    Assert-Rejected 'physical source/context mismatch' { $null = Invoke-TestExport }

    Reset-ExportFixture
    $script:Fixture.ExportAdmissionFails = $true
    Assert-Rejected 'compiled admission failure' { $null = Invoke-TestExport }
    Assert-Test (-not (Test-Path -LiteralPath (Join-Path $script:Fixture.OutputParent 'worker-artifact'))) 'Rejected compiled admission created output.'

    Reset-ExportFixture
    $inventoryPath = Join-Path $script:Fixture.Bundle 'bundle-inventory.json'
    $inventory = Get-Content -LiteralPath $inventoryPath -Raw | ConvertFrom-Json -AsHashtable -Depth 8
    $inventory.files[1].sha256 = '0' * 64
    Write-TestJson $inventoryPath $inventory
    Assert-Rejected 'worker inventory digest mismatch' { $null = Invoke-TestExport }

    Reset-ExportFixture
    $collision = Join-Path $script:Fixture.OutputParent 'worker-artifact'
    [IO.Directory]::CreateDirectory($collision) | Out-Null
    Write-TestBytes (Join-Path $collision 'sentinel') $script:Utf8.GetBytes('preserve me')
    Assert-Rejected 'preexisting export output' { $null = Invoke-TestExport }
    Assert-Equal ([Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes((Join-Path $collision 'sentinel')))) 'preserve me' 'Rejected output collision changed existing bytes.'

    # Provenance metadata is authenticated independently before retained ZIP
    # bytes are considered.  Exercise same-source and ancestor-worker paths.
    Reset-ResolverFixture
    $preflight = Invoke-TestPreflight
    Assert-Equal $preflight.SourceRevision $script:Fixture.SourceRevision 'Preflight did not pin the installer revision.'
    Assert-Equal $preflight.WorkerSourceRevision $script:Fixture.WorkerRevision 'Preflight did not pin the worker revision.'
    Assert-Equal $preflight.ProducerRunId $script:Fixture.ProducerRunId 'Preflight did not pin the producer run.'
    Assert-Equal $preflight.ProducerRunAttempt $script:Fixture.ProducerRunAttempt 'Preflight did not pin the producer attempt.'
    Assert-Equal $preflight.ArtifactId $script:Fixture.ArtifactId 'Preflight did not pin the artifact ID.'
    Assert-Equal $preflight.ArtifactSha256 ([string]$script:Fixture.Artifact.digest).Substring(7) 'Preflight did not return the authenticated archive digest.'
    Assert-Equal $preflight.ArtifactSizeBytes ([int64]$script:Fixture.Artifact.size_in_bytes) 'Preflight did not return the authenticated archive size.'
    Assert-Test ($script:GitHubCalls.Count -eq 6) 'Preflight did not perform the fixed ancestry and producer metadata reads.'

    $resolved = Invoke-TestResolve
    try {
        Assert-Equal $resolved.ArtifactSha256 $preflight.ArtifactSha256 'Resolve did not preserve its preflight archive digest pin.'
        Assert-Equal $resolved.Root (Join-Path $script:Fixture.OutputParent 'resolved-worker') 'Resolve returned an unexpected output root.'
        Assert-Test (Test-Path -LiteralPath $resolved.WorkerPath -PathType Leaf) 'Resolve did not activate the worker.'
        Assert-Test (Test-Path -LiteralPath $resolved.RecordPath -PathType Leaf) 'Resolve did not activate the record.'
        Assert-Test (@(Get-ChildItem -LiteralPath $resolved.Root -Force).Count -eq 2) 'Resolve output inventory is not exact.'
        Assert-Equal (Get-TestFileHash $resolved.WorkerPath) (Get-TestHash $script:Fixture.WorkerBytes) 'Resolve changed worker bytes.'
        Assert-Equal $resolved.Record.source_revision $script:Fixture.SourceRevision 'Same-source resolve returned a record with the wrong origin.'
        Assert-Rejected 'held resolved worker write-open' {
            $writeHandle = [IO.File]::Open($resolved.WorkerPath, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try { } finally { $writeHandle.Dispose() }
        }
    }
    finally { $resolved.WorkerStream.Dispose() }

    Reset-ResolverFixture -ForeignWorkerSource
    $foreign = Invoke-TestResolve
    try {
        Assert-Equal $foreign.SourceRevision $script:Fixture.SourceRevision 'Foreign-source resolve changed the installer pin.'
        Assert-Equal $foreign.WorkerSourceRevision $script:Fixture.WorkerRevision 'Foreign-source resolve did not retain the ancestor worker pin.'
        Assert-Equal $foreign.Record.source_revision $script:Fixture.WorkerRevision 'Foreign-source record origin was not retained.'
    }
    finally { $foreign.WorkerStream.Dispose() }

    Reset-ResolverFixture
    $insideResolverSource = Join-Path $repositoryRoot ".must-not-resolve-$([guid]::NewGuid().ToString('N'))"
    Assert-Rejected 'resolve output overlapping trusted source' {
        Invoke-TestResolve -OutputDirectory $insideResolverSource
    }
    Assert-Test (-not (Test-Path -LiteralPath $insideResolverSource)) 'Rejected source-overlap resolve created output.'

    Reset-ResolverFixture
    $archiveOverlapOutput = Join-Path $script:Fixture.ArchivePath 'nested'
    Assert-Rejected 'resolve output overlapping retained archive' {
        Invoke-TestResolve -OutputDirectory $archiveOverlapOutput
    }
    Assert-Test (-not (Test-Path -LiteralPath $archiveOverlapOutput)) 'Rejected archive-overlap resolve created output.'

    Reset-ResolverFixture
    $archiveSiblingOutput = "$($script:Fixture.ArchivePath).output"
    $archiveSibling = Invoke-TestResolve -OutputDirectory $archiveSiblingOutput
    try {
        Assert-Equal $archiveSibling.Root $archiveSiblingOutput 'Resolve rejected a sibling of the retained archive.'
    }
    finally { $archiveSibling.WorkerStream.Dispose() }

    foreach ($case in @(
        [pscustomobject]@{ Name = 'wrong producer run'; Change = { $script:Fixture.Run.id = [int64]8002 } },
        [pscustomobject]@{ Name = 'foreign producer repository'; Change = { $script:Fixture.Run.repository.full_name = 'other/scribe' } },
        [pscustomobject]@{ Name = 'wrong producer event'; Change = { $script:Fixture.Run.event = 'push' } },
        [pscustomobject]@{ Name = 'wrong producer workflow'; Change = { $script:Fixture.Run.path = '.github/workflows/other.yml' } },
        [pscustomobject]@{ Name = 'wrong producer source revision'; Change = { $script:Fixture.Run.head_sha = 'c' * 40 } },
        [pscustomobject]@{ Name = 'incomplete producer'; Change = { $script:Fixture.Run.status = 'in_progress' } },
        [pscustomobject]@{ Name = 'failed producer'; Change = { $script:Fixture.Run.conclusion = 'failure' } },
        [pscustomobject]@{ Name = 'stale producer attempt'; Change = { $script:Fixture.Latest.run_attempt = [int64]2 } },
        [pscustomobject]@{ Name = 'wrong artifact name'; Change = { $script:Fixture.Artifact.name = 'wrong-name' } },
        [pscustomobject]@{ Name = 'expired artifact'; Change = { $script:Fixture.Artifact.expired = $true } },
        [pscustomobject]@{ Name = 'invalid artifact digest'; Change = { $script:Fixture.Artifact.digest = 'sha1:' + ('0' * 64) } },
        [pscustomobject]@{ Name = 'nonancestor worker source'; Change = { $script:Fixture.WorkerComparison.status = 'behind' } },
        [pscustomobject]@{ Name = 'installer not on protected main'; Change = { $script:Fixture.InstallerComparison.status = 'behind' } }
    )) {
        Reset-ResolverFixture
        & $case.Change
        Assert-Rejected $case.Name { $null = Invoke-TestPreflight }
    }

    Reset-ResolverFixture
    $approvedPreflight = Invoke-TestPreflight
    $script:Fixture.Artifact.digest = 'sha256:' + ('0' * 64)
    Assert-Rejected 'preflight-to-resolve artifact metadata drift' { Invoke-TestResolve -ExpectedArtifactSha256 $approvedPreflight.ArtifactSha256 }
    Assert-Test (-not (Test-Path -LiteralPath (Join-Path $script:Fixture.OutputParent 'resolved-worker'))) 'Metadata drift created an output directory.'

    Reset-ResolverFixture
    $script:Fixture.LateArtifactDigest = 'sha256:' + ('0' * 64)
    Assert-Rejected 'late producer provenance drift before activation' { Invoke-TestResolve }
    Assert-Test (-not (Test-Path -LiteralPath (Join-Path $script:Fixture.OutputParent 'resolved-worker'))) 'Late provenance drift activated output.'

    Reset-ResolverFixture
    $script:Fixture.Artifact.digest = 'sha256:' + ('0' * 64)
    Assert-Rejected 'raw ZIP digest mismatch before parsing' { Invoke-TestResolve -ExpectedArtifactSha256 ('0' * 64) }
    Assert-Test (-not (Test-Path -LiteralPath (Join-Path $script:Fixture.OutputParent 'resolved-worker'))) 'Raw digest mismatch created an output directory.'

    Reset-ResolverFixture
    $archiveHardlink = Join-Path $script:TestRoot "cpu-worker-hardlink-$([guid]::NewGuid().ToString('N')).zip"
    New-Item -ItemType HardLink -Path $archiveHardlink -Target $script:Fixture.ArchivePath | Out-Null
    $script:Fixture.ArchivePath = $archiveHardlink
    Assert-Rejected 'raw ZIP hardlink' { Invoke-TestResolve }

    Reset-ResolverFixture
    $notZip = Join-Path $script:TestRoot "not-a-zip-$([guid]::NewGuid().ToString('N')).zip"
    Write-TestBytes $notZip $script:Utf8.GetBytes('not a ZIP')
    $script:Fixture.ArchivePath = $notZip
    $script:Fixture.Artifact.digest = "sha256:$(Get-TestFileHash $notZip)"
    $script:Fixture.Artifact.size_in_bytes = [int64](Get-Item -LiteralPath $notZip -Force).Length
    Assert-Rejected 'malformed ZIP with matching raw digest' { Invoke-TestResolve }

    Reset-ResolverFixture
    $badRecord = Copy-TestRecord $script:Fixture.Record
    $badRecord.kind = 'windows-frozen-cpu-worker'
    $script:Fixture.RecordBytes = Get-TestRecordBytes $badRecord
    Set-ResolverArchive (Get-ValidResolverEntries)
    Assert-Rejected 'local-only record kind' { Invoke-TestResolve }

    Reset-ResolverFixture
    $badRecord = Copy-TestRecord $script:Fixture.Record
    $badRecord.protocol_version = '5'
    $script:Fixture.RecordBytes = Get-TestRecordBytes $badRecord
    Set-ResolverArchive (Get-ValidResolverEntries)
    Assert-Rejected 'coerced record integer' { Invoke-TestResolve }

    Reset-ResolverFixture
    $badRecord = Copy-TestRecord $script:Fixture.Record
    $badRecord.unexpected = 'field'
    $script:Fixture.RecordBytes = Get-TestRecordBytes $badRecord
    Set-ResolverArchive (Get-ValidResolverEntries)
    Assert-Rejected 'record unknown field' { Invoke-TestResolve }

    Reset-ResolverFixture
    $recordText = $script:Utf8.GetString($script:Fixture.RecordBytes)
    $duplicateRecordText = $recordText.Replace('"schema_version": 1', '"schema_version": 1,"schema_version": 1')
    Assert-Test ($duplicateRecordText -cne $recordText) 'Duplicate-field fixture did not modify canonical JSON.'
    $script:Fixture.RecordBytes = $script:Utf8.GetBytes($duplicateRecordText)
    Set-ResolverArchive (Get-ValidResolverEntries)
    Assert-Rejected 'duplicate record field' { Invoke-TestResolve }

    Reset-ResolverFixture
    $foreignContext = New-TestContext ('c' * 40)
    $script:Fixture.RecordBytes = Get-TestRecordBytes (New-TestCpuWorkerRecord $foreignContext $foreignContext.SourceRevision '8001' '1' $script:Fixture.WorkerBytes)
    Set-ResolverArchive (Get-ValidResolverEntries)
    Assert-Rejected 'record source revision mismatch' { Invoke-TestResolve }

    Reset-ResolverFixture
    $badRecord = Copy-TestRecord $script:Fixture.Record
    $badRecord.worker_sha256 = '0' * 64
    $script:Fixture.RecordBytes = Get-TestRecordBytes $badRecord
    Set-ResolverArchive (Get-ValidResolverEntries)
    Assert-Rejected 'record worker hash mismatch' { Invoke-TestResolve }

    Reset-ResolverFixture
    $badRecord = Copy-TestRecord $script:Fixture.Record
    $badRecord.worker_size_bytes = [int64]($script:Fixture.WorkerBytes.Length + 1)
    $script:Fixture.RecordBytes = Get-TestRecordBytes $badRecord
    Set-ResolverArchive (Get-ValidResolverEntries)
    Assert-Rejected 'record worker size mismatch' { Invoke-TestResolve }

    Reset-ResolverFixture
    $badRecord = Copy-TestRecord $script:Fixture.Record
    $badRecord.worker_abi_version = [int64]2
    $script:Fixture.RecordBytes = Get-TestRecordBytes $badRecord
    Set-ResolverArchive (Get-ValidResolverEntries)
    Assert-Rejected 'record ABI mismatch' { Invoke-TestResolve }

    Reset-ResolverFixture
    $traversalEntries = @(
        [pscustomobject]@{ Name = 'windows-ci-cpu-worker.json'; Bytes = $script:Fixture.RecordBytes },
        [pscustomobject]@{ Name = '../scribe-inference-worker.exe'; Bytes = $script:Fixture.WorkerBytes }
    )
    Set-ResolverArchive $traversalEntries
    Assert-Rejected 'ZIP traversal entry' { Invoke-TestResolve }

    Reset-ResolverFixture
    $duplicateEntries = @(
        [pscustomobject]@{ Name = 'windows-ci-cpu-worker.json'; Bytes = $script:Fixture.RecordBytes },
        [pscustomobject]@{ Name = 'windows-ci-cpu-worker.json'; Bytes = $script:Fixture.RecordBytes }
    )
    Set-ResolverArchive $duplicateEntries
    Assert-Rejected 'ZIP duplicate entry' { Invoke-TestResolve }

    Reset-ResolverFixture
    $caseCollisionEntries = @(
        [pscustomobject]@{ Name = 'windows-ci-cpu-worker.json'; Bytes = $script:Fixture.RecordBytes },
        [pscustomobject]@{ Name = 'WINDOWS-CI-CPU-WORKER.JSON'; Bytes = $script:Fixture.RecordBytes }
    )
    Set-ResolverArchive $caseCollisionEntries
    Assert-Rejected 'ZIP case-colliding entry' { Invoke-TestResolve }

    Reset-ResolverFixture
    $extraEntries = @(Get-ValidResolverEntries) + @([pscustomobject]@{ Name = 'extra.txt'; Bytes = $script:Utf8.GetBytes('extra') })
    Set-ResolverArchive $extraEntries
    Assert-Rejected 'ZIP extra entry' { Invoke-TestResolve }

    Reset-ResolverFixture
    $symlinkEntries = @(
        [pscustomobject]@{ Name = 'windows-ci-cpu-worker.json'; Bytes = $script:Fixture.RecordBytes },
        [pscustomobject]@{ Name = 'scribe-inference-worker.exe'; Bytes = $script:Fixture.WorkerBytes; ExternalAttributes = [int]0xA1FF0000 }
    )
    Set-ResolverArchive $symlinkEntries
    Assert-Rejected 'ZIP symbolic link entry' { Invoke-TestResolve }

    Reset-ResolverFixture
    $fifoEntries = @(
        [pscustomobject]@{ Name = 'windows-ci-cpu-worker.json'; Bytes = $script:Fixture.RecordBytes },
        [pscustomobject]@{ Name = 'scribe-inference-worker.exe'; Bytes = $script:Fixture.WorkerBytes; ExternalAttributes = [int][uint32]0x11FF0000 }
    )
    Set-ResolverArchive $fifoEntries
    Assert-Rejected 'ZIP FIFO entry' { Invoke-TestResolve }

    Reset-ResolverFixture
    $oversizeEntries = @(
        [pscustomobject]@{ Name = 'windows-ci-cpu-worker.json'; Bytes = [byte[]]::new(65537) },
        [pscustomobject]@{ Name = 'scribe-inference-worker.exe'; Bytes = $script:Fixture.WorkerBytes }
    )
    Set-ResolverArchive $oversizeEntries
    Assert-Rejected 'ZIP oversized record entry' { Invoke-TestResolve }

    Reset-ResolverFixture
    $output = Join-Path $script:Fixture.OutputParent 'resolved-worker'
    [IO.Directory]::CreateDirectory($output) | Out-Null
    Write-TestBytes (Join-Path $output 'sentinel') $script:Utf8.GetBytes('preserve me')
    Assert-Rejected 'resolve output collision' { Invoke-TestResolve }
    Assert-Equal ([Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes((Join-Path $output 'sentinel')))) 'preserve me' 'Rejected resolve output collision changed existing bytes.'

    # Keep this suite fail-closed if a future edit accidentally removes a
    # meaningful block of fixtures without updating the expected coverage.
    Assert-Test ($script:CpuWorkerInputAssertions -ge 65) 'CPU worker input fixture discovery coverage unexpectedly shrank.'
}
finally {
    Set-Item -Path Function:Get-WindowsCpuArtifactExportSourceContext -Value $savedExportSourceContext
    Set-Item -Path Function:Assert-WindowsCpuArtifactExportCompiledAdmission -Value $savedExportAdmission
    Set-Item -Path Function:Assert-ReviewedWindowsPe -Value $savedPeVerifier
    Set-Item -Path Function:Get-WindowsFrozenCpuWorkerSourceContext -Value $savedFrozenSourceContext
    Set-Item -Path Function:Get-WindowsCpuWorkerInputSourceContext -Value $savedInputSourceContext
    Set-Item -Path Function:Invoke-WindowsCpuWorkerInputGitHubGet -Value $savedInputGitHubGet
    foreach ($name in $environmentNames) { [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name]) }
    if ($hadSavedLastExitCode) { $global:LASTEXITCODE = $savedLastExitCode } else { Remove-Variable -Name LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue }
    Remove-OwnedTestRoot $testRoot
}

Write-Output "Windows CPU worker artifact-input tests passed ($script:CpuWorkerInputAssertions assertions)."
