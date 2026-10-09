[CmdletBinding()]
param(
    [ValidateSet('Preflight', 'Resolve')][string]$Mode,
    [string]$SourceRevision,
    [string]$WorkerSourceRevision,
    [string]$ProducerRunId,
    [string]$ProducerRunAttempt,
    [string]$ArtifactId,
    [string]$ExpectedArtifactSha256,
    [string]$ArchivePath,
    [string]$OutputDirectory,
    [switch]$FunctionsOnly
)

# Dot-sourced helpers have their own param blocks. Preserve both values and
# explicit-presence bits before loading them.
$windowsCpuInputEntryArguments = [ordered]@{
    Mode = $Mode
    SourceRevision = $SourceRevision
    WorkerSourceRevision = $WorkerSourceRevision
    ProducerRunId = $ProducerRunId
    ProducerRunAttempt = $ProducerRunAttempt
    ArtifactId = $ArtifactId
    ExpectedArtifactSha256 = $ExpectedArtifactSha256
    ArchivePath = $ArchivePath
    OutputDirectory = $OutputDirectory
}
$windowsCpuInputExplicit = [ordered]@{
    ExpectedArtifactSha256 = $PSBoundParameters.ContainsKey('ExpectedArtifactSha256')
    ArchivePath = $PSBoundParameters.ContainsKey('ArchivePath')
    OutputDirectory = $PSBoundParameters.ContainsKey('OutputDirectory')
}
$windowsCpuInputFunctionsOnly = $FunctionsOnly.IsPresent

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'windows-frozen-cpu-worker-integrity.ps1')
. (Join-Path $PSScriptRoot 'invoke-windows-gpu-approved-signing.ps1') -FunctionsOnly
Add-Type -AssemblyName System.IO.Compression

$script:WindowsCpuInputRepository = 'tyhuang9/scribe'
$script:WindowsCpuInputRef = 'refs/heads/main'
$script:WindowsCpuInputWorkflow = '.github/workflows/release.yml'
$script:WindowsCpuInputWorkflowName = 'Build Windows installer'
$script:WindowsCpuInputRecordName = 'windows-ci-cpu-worker.json'
$script:WindowsCpuInputWorkerName = 'scribe-inference-worker.exe'
$script:WindowsCpuInputMaximumRecordBytes = [int64]65536
$script:WindowsCpuInputMaximumArchiveBytes = [int64](Get-WindowsFrozenCpuWorkerMaximumBytes) + [int64](4MB)

function Assert-WindowsCpuWorkerInputCondition([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-WindowsCpuWorkerInputExactKeys($Value, [string[]]$Names, [string]$Label) {
    Assert-WindowsCpuWorkerInputCondition ($Value -is [System.Collections.IDictionary]) "$Label must be one JSON object."
    $actual = @($Value.Keys | ForEach-Object { [string]$_ } | Sort-Object -CaseSensitive)
    $expected = @($Names | Sort-Object -CaseSensitive)
    Assert-WindowsCpuWorkerInputCondition `
        ($actual.Count -eq $expected.Count -and -not (Compare-Object $expected $actual -CaseSensitive)) `
        "$Label has unexpected or missing fields."
}

function Assert-WindowsCpuWorkerInputInteger($Value, [int64]$Minimum, [int64]$Maximum, [string]$Label) {
    Assert-WindowsCpuWorkerInputCondition `
        (($Value -is [int32] -or $Value -is [int64]) -and [int64]$Value -ge $Minimum -and [int64]$Value -le $Maximum) `
        "$Label must be a bounded integer."
    return [int64]$Value
}

function Test-WindowsCpuWorkerInputByteEquality([byte[]]$Left, [byte[]]$Right) {
    if ($Left.Length -ne $Right.Length) { return $false }
    for ($index = 0; $index -lt $Left.Length; $index++) {
        if ($Left[$index] -ne $Right[$index]) { return $false }
    }
    return $true
}

function Assert-WindowsCpuWorkerInputUnlinkedFile([string]$Path, [string]$Label) {
    $item = Assert-WindowsFrozenCpuWorkerRegularFile $Path
    Assert-WindowsCpuWorkerInputCondition `
        ([string]::IsNullOrEmpty([string]$item.LinkType)) `
        "$Label cannot be a symbolic link or hardlink."
    return $item
}

function Get-WindowsCpuWorkerInputRecordProperties {
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

function Get-WindowsCpuWorkerInputSourceContext([string]$RepositoryRoot) {
    return Get-WindowsFrozenCpuWorkerSourceContext $RepositoryRoot
}

function Invoke-WindowsCpuWorkerInputGitHubGet([string]$Path) {
    return Invoke-SigningGitHubGet $Path
}

function Assert-WindowsCpuWorkerInputProductionContext([string]$Revision) {
    Assert-WindowsCpuWorkerInputCondition `
        ($env:GITHUB_ACTIONS -ceq 'true' -and $env:CI -ceq 'true' -and
            $env:GITHUB_EVENT_NAME -ceq 'workflow_dispatch' -and
            $env:GITHUB_REPOSITORY -ceq $script:WindowsCpuInputRepository -and
            $env:GITHUB_REF -ceq $script:WindowsCpuInputRef -and
            $env:GITHUB_WORKFLOW -ceq $script:WindowsCpuInputWorkflowName -and
            $env:GITHUB_SHA -ceq $Revision) `
        'CPU worker acquisition is restricted to the fixed manual protected-main release workflow.'
    $repositoryRoot = Get-WindowsFrozenCpuWorkerNormalizedFullPath (Split-Path -Parent $PSScriptRoot)
    $workspace = Get-WindowsFrozenCpuWorkerNormalizedFullPath $env:GITHUB_WORKSPACE
    Assert-WindowsCpuWorkerInputCondition `
        ([string]::Equals($workspace, $repositoryRoot, [StringComparison]::OrdinalIgnoreCase)) `
        'GitHub workspace does not match the physical CPU worker installer checkout.'
    $context = Get-WindowsCpuWorkerInputSourceContext $repositoryRoot
    Assert-WindowsCpuWorkerInputCondition ($context.SourceRevision -ceq $Revision) 'Physical installer source revision does not match its independent pin.'
    $null = Assert-WindowsFrozenCpuWorkerRegularFile (Join-Path $repositoryRoot ($script:WindowsCpuInputWorkflow -replace '/', '\'))
    return [pscustomobject]@{ RepositoryRoot = $repositoryRoot; Context = $context }
}

function Assert-WindowsCpuWorkerInputAncestry([string]$WorkerRevision, [string]$InstallerRevision) {
    $workerComparison = Invoke-WindowsCpuWorkerInputGitHubGet "/repos/$script:WindowsCpuInputRepository/compare/$WorkerRevision...$InstallerRevision"
    Assert-WindowsCpuWorkerInputCondition `
        ($workerComparison.merge_base_commit.sha -ceq $WorkerRevision -and $workerComparison.status -cin @('ahead', 'identical')) `
        'CPU worker source is not an ancestor of the installer source.'
    $branch = Invoke-WindowsCpuWorkerInputGitHubGet "/repos/$script:WindowsCpuInputRepository/git/ref/heads/main"
    $head = [string]$branch.object.sha
    Assert-WindowsCpuWorkerInputCondition ($head -cmatch '\A[0-9a-f]{40}\z') 'Current protected-main revision is invalid.'
    $installerComparison = Invoke-WindowsCpuWorkerInputGitHubGet "/repos/$script:WindowsCpuInputRepository/compare/$InstallerRevision...$head"
    Assert-WindowsCpuWorkerInputCondition `
        ($installerComparison.merge_base_commit.sha -ceq $InstallerRevision -and $installerComparison.status -cin @('ahead', 'identical')) `
        'Installer source is not an ancestor of current protected main.'
    return $head
}

function Invoke-WindowsCpuWorkerInputPreflight {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$SourceRevision,
        [Parameter(Mandatory = $true)][string]$WorkerSourceRevision,
        [Parameter(Mandatory = $true)][string]$ProducerRunId,
        [Parameter(Mandatory = $true)][string]$ProducerRunAttempt,
        [Parameter(Mandatory = $true)][string]$ArtifactId
    )

    Assert-WindowsCpuWorkerInputCondition ($SourceRevision -cmatch '\A[0-9a-f]{40}\z') 'Installer source revision is not canonical.'
    Assert-WindowsCpuWorkerInputCondition ($WorkerSourceRevision -cmatch '\A[0-9a-f]{40}\z') 'CPU worker source revision is not canonical.'
    Assert-SigningId $ProducerRunId 'CPU worker producer run ID'
    Assert-SigningId $ProducerRunAttempt 'CPU worker producer run attempt'
    Assert-SigningId $ArtifactId 'CPU worker artifact ID'
    $production = Assert-WindowsCpuWorkerInputProductionContext $SourceRevision
    $currentMain = Assert-WindowsCpuWorkerInputAncestry $WorkerSourceRevision $SourceRevision

    $run = Invoke-WindowsCpuWorkerInputGitHubGet "/repos/$script:WindowsCpuInputRepository/actions/runs/$ProducerRunId/attempts/$ProducerRunAttempt"
    Assert-SigningRunMetadata $run $ProducerRunId $ProducerRunAttempt $script:WindowsCpuInputWorkflow $WorkerSourceRevision
    $latest = Invoke-WindowsCpuWorkerInputGitHubGet "/repos/$script:WindowsCpuInputRepository/actions/runs/$ProducerRunId"
    Assert-SigningRunMetadata $latest $ProducerRunId $ProducerRunAttempt $script:WindowsCpuInputWorkflow $WorkerSourceRevision
    $artifact = Invoke-WindowsCpuWorkerInputGitHubGet "/repos/$script:WindowsCpuInputRepository/actions/artifacts/$ArtifactId"
    $artifactName = "windows-cpu-worker-$ProducerRunId-$ProducerRunAttempt"
    Assert-SigningArtifactMetadata $artifact $ArtifactId $ProducerRunId $WorkerSourceRevision $artifactName
    $artifactSize = Assert-WindowsCpuWorkerInputInteger $artifact.size_in_bytes 1 $script:WindowsCpuInputMaximumArchiveBytes 'CPU worker archive size'
    $artifactSha256 = ([string]$artifact.digest).Substring(7)
    Assert-SigningHash $artifactSha256 'CPU worker archive SHA-256'
    return [pscustomobject]@{
        SourceRevision = $SourceRevision
        WorkerSourceRevision = $WorkerSourceRevision
        ProducerRunId = $ProducerRunId
        ProducerRunAttempt = $ProducerRunAttempt
        ArtifactId = $ArtifactId
        ArtifactSha256 = $artifactSha256
        ArtifactSizeBytes = $artifactSize
        CurrentMainRevision = $currentMain
        Context = $production.Context
        RepositoryRoot = $production.RepositoryRoot
    }
}

function New-WindowsCpuWorkerInputCanonicalRecord($Record) {
    return [ordered]@{
        schema_version = [int64]$Record.schema_version
        kind = [string]$Record.kind
        source_repository = [string]$Record.source_repository
        source_ref = [string]$Record.source_ref
        workflow = [string]$Record.workflow
        run_id = [string]$Record.run_id
        run_attempt = [string]$Record.run_attempt
        source_revision = [string]$Record.source_revision
        app_version = [string]$Record.app_version
        target_triple = [string]$Record.target_triple
        protocol_version = [int64]$Record.protocol_version
        worker_abi_version = [int64]$Record.worker_abi_version
        desktop_build_id = [string]$Record.desktop_build_id
        worker_build_id = [string]$Record.worker_build_id
        cargo_lock_sha256 = [string]$Record.cargo_lock_sha256
        rust_toolchain_sha256 = [string]$Record.rust_toolchain_sha256
        cargo_manifest_sha256 = [string]$Record.cargo_manifest_sha256
        worker_identity_sha256 = [string]$Record.worker_identity_sha256
        build_rs_sha256 = [string]$Record.build_rs_sha256
        build_contract_sha256 = [string]$Record.build_contract_sha256
        worker_relative_path = [string]$Record.worker_relative_path
        worker_size_bytes = [int64]$Record.worker_size_bytes
        worker_sha256 = [string]$Record.worker_sha256
    }
}

function Assert-WindowsCpuWorkerInputRecord(
    $Record,
    [byte[]]$RawBytes,
    [psobject]$Preflight
) {
    Assert-WindowsCpuWorkerInputExactKeys $Record (Get-WindowsCpuWorkerInputRecordProperties) 'CI CPU worker record'
    foreach ($name in @(
        'kind', 'source_repository', 'source_ref', 'workflow', 'run_id', 'run_attempt', 'source_revision',
        'app_version', 'target_triple', 'desktop_build_id', 'worker_build_id', 'cargo_lock_sha256',
        'rust_toolchain_sha256', 'cargo_manifest_sha256', 'worker_identity_sha256', 'build_rs_sha256',
        'build_contract_sha256', 'worker_relative_path', 'worker_sha256'
    )) {
        Assert-WindowsCpuWorkerInputCondition ($Record[$name] -is [string]) "CI CPU worker record $name must be a string."
    }
    $null = Assert-WindowsCpuWorkerInputInteger $Record.schema_version 1 1 'CI CPU worker schema version'
    $null = Assert-WindowsCpuWorkerInputInteger $Record.protocol_version 0 255 'CI CPU worker protocol version'
    $null = Assert-WindowsCpuWorkerInputInteger $Record.worker_abi_version 0 65535 'CI CPU worker ABI version'
    $null = Assert-WindowsCpuWorkerInputInteger $Record.worker_size_bytes 1 (Get-WindowsFrozenCpuWorkerMaximumBytes) 'CI CPU worker size'
    Assert-WindowsCpuWorkerInputCondition `
        ($Record.kind -ceq 'windows-ci-cpu-worker' -and
            $Record.source_repository -ceq $script:WindowsCpuInputRepository -and
            $Record.source_ref -ceq $script:WindowsCpuInputRef -and
            $Record.workflow -ceq $script:WindowsCpuInputWorkflow -and
            $Record.run_id -ceq $Preflight.ProducerRunId -and
            $Record.run_attempt -ceq $Preflight.ProducerRunAttempt -and
            $Record.source_revision -ceq $Preflight.WorkerSourceRevision -and
            $Record.worker_relative_path -ceq $script:WindowsCpuInputWorkerName) `
        'CI CPU worker record provenance or fixed identity does not match.'
    Assert-WindowsCpuWorkerInputCondition `
        ($Record.app_version -cmatch '\A[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?\z') `
        'CI CPU worker app version is not canonical.'
    Assert-WindowsCpuWorkerInputCondition `
        ($Record.target_triple -ceq (Get-WindowsFrozenCpuWorkerTargetTriple) -and
            $Record.desktop_build_id -ceq "local-transcriber@$($Record.app_version)#$($Record.source_revision)" -and
            $Record.worker_build_id -ceq "scribe-inference-worker@$($Record.app_version)#$($Record.source_revision)") `
        'CI CPU worker build identities are not canonical.'
    foreach ($name in @(
        'cargo_lock_sha256', 'rust_toolchain_sha256', 'cargo_manifest_sha256', 'worker_identity_sha256',
        'build_rs_sha256', 'build_contract_sha256', 'worker_sha256'
    )) {
        Assert-SigningHash ([string]$Record[$name]) "CI CPU worker record $name"
    }

    $installerContext = $Preflight.Context
    foreach ($pair in @(
        @('app_version', 'AppVersion'),
        @('target_triple', 'TargetTriple'),
        @('protocol_version', 'ProtocolVersion'),
        @('worker_abi_version', 'WorkerAbiVersion')
    )) {
        Assert-WindowsCpuWorkerInputCondition `
            ([string]$Record[$pair[0]] -ceq [string]$installerContext.($pair[1])) `
            "CI CPU worker is incompatible with the installer source: $($pair[0])."
    }
    if ($Preflight.WorkerSourceRevision -ceq $Preflight.SourceRevision) {
        foreach ($pair in @(
            @('desktop_build_id', 'DesktopBuildId'),
            @('worker_build_id', 'WorkerBuildId'),
            @('cargo_lock_sha256', 'CargoLockSha256'),
            @('rust_toolchain_sha256', 'RustToolchainSha256'),
            @('cargo_manifest_sha256', 'CargoManifestSha256'),
            @('worker_identity_sha256', 'WorkerIdentitySha256'),
            @('build_rs_sha256', 'BuildRsSha256'),
            @('build_contract_sha256', 'BuildContractSha256')
        )) {
            Assert-WindowsCpuWorkerInputCondition `
                ([string]$Record[$pair[0]] -ceq [string]$installerContext.($pair[1])) `
                "Same-source CI CPU worker contract does not match the installer source: $($pair[0])."
        }
    }

    $canonical = New-WindowsCpuWorkerInputCanonicalRecord $Record
    $canonicalBytes = [Text.UTF8Encoding]::new($false).GetBytes(($canonical | ConvertTo-Json -Depth 5))
    Assert-WindowsCpuWorkerInputCondition `
        (Test-WindowsCpuWorkerInputByteEquality $RawBytes $canonicalBytes) `
        'CI CPU worker record is not the exact canonical JSON encoding.'
}

function ConvertTo-WindowsCpuWorkerInputContext($Record) {
    return [pscustomobject]@{
        RepositoryRoot = $null
        SourceRevision = [string]$Record.source_revision
        AppVersion = [string]$Record.app_version
        TargetTriple = [string]$Record.target_triple
        ProtocolVersion = [int]$Record.protocol_version
        WorkerAbiVersion = [int]$Record.worker_abi_version
        DesktopBuildId = [string]$Record.desktop_build_id
        WorkerBuildId = [string]$Record.worker_build_id
        CargoLockSha256 = [string]$Record.cargo_lock_sha256
        RustToolchainSha256 = [string]$Record.rust_toolchain_sha256
        CargoManifestSha256 = [string]$Record.cargo_manifest_sha256
        WorkerIdentitySha256 = [string]$Record.worker_identity_sha256
        BuildRsSha256 = [string]$Record.build_rs_sha256
        BuildContractSha256 = [string]$Record.build_contract_sha256
    }
}

function Read-WindowsCpuWorkerInputZipRecord([IO.Compression.ZipArchiveEntry]$Entry) {
    Assert-WindowsCpuWorkerInputCondition `
        ($Entry.Length -gt 0 -and $Entry.Length -le $script:WindowsCpuInputMaximumRecordBytes) `
        'CI CPU worker record entry is outside its fixed size bound.'
    $stream = $Entry.Open()
    try {
        $bytes = [byte[]]::new([int]$Entry.Length)
        $offset = 0
        while ($offset -lt $bytes.Length) {
            $count = $stream.Read($bytes, $offset, $bytes.Length - $offset)
            Assert-WindowsCpuWorkerInputCondition ($count -gt 0) 'CI CPU worker record ended before its declared length.'
            $offset += $count
        }
        Assert-WindowsCpuWorkerInputCondition ($stream.ReadByte() -eq -1) 'CI CPU worker record exceeds its declared length.'
        return ,$bytes
    }
    finally { $stream.Dispose() }
}

function Copy-WindowsCpuWorkerInputZipWorker(
    [IO.Compression.ZipArchiveEntry]$Entry,
    [string]$Destination,
    [int64]$ExpectedSize,
    [string]$ExpectedSha256
) {
    Assert-WindowsCpuWorkerInputCondition ($Entry.Length -eq $ExpectedSize) 'CI CPU worker ZIP entry size does not match its record.'
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $Destination
    Assert-WindowsCpuWorkerInputCondition (-not (Test-Path -LiteralPath $Destination)) 'CI CPU worker output unexpectedly exists.'
    $source = $Entry.Open()
    $destinationStream = [IO.File]::Open($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    $hash = [Security.Cryptography.IncrementalHash]::CreateHash([Security.Cryptography.HashAlgorithmName]::SHA256)
    try {
        $buffer = [byte[]]::new(1048576)
        $total = [int64]0
        while (($count = $source.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $total += $count
            Assert-WindowsCpuWorkerInputCondition ($total -le $ExpectedSize) 'CI CPU worker ZIP entry exceeds its declared size.'
            $hash.AppendData($buffer, 0, $count)
            $destinationStream.Write($buffer, 0, $count)
        }
        Assert-WindowsCpuWorkerInputCondition ($total -eq $ExpectedSize) 'CI CPU worker ZIP entry ended before its declared size.'
        $digest = [Convert]::ToHexString($hash.GetHashAndReset()).ToLowerInvariant()
        Assert-WindowsCpuWorkerInputCondition ($digest -ceq $ExpectedSha256) 'CI CPU worker ZIP entry SHA-256 does not match its record.'
    }
    finally {
        $hash.Dispose()
        $destinationStream.Dispose()
        $source.Dispose()
    }
}

function Assert-WindowsCpuWorkerInputZipEntries([IO.Compression.ZipArchive]$Archive) {
    $entries = @($Archive.Entries)
    Assert-WindowsCpuWorkerInputCondition ($entries.Count -eq 2) 'CI CPU worker archive must contain exactly two entries.'
    $expected = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $null = $expected.Add($script:WindowsCpuInputRecordName)
    $null = $expected.Add($script:WindowsCpuInputWorkerName)
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $result = @{}
    $totalLength = [int64]0
    foreach ($entry in $entries) {
        $name = [string]$entry.FullName
        Assert-WindowsCpuWorkerInputCondition `
            ($expected.Contains($name) -and $entry.Name -ceq $name -and
                -not $name.Contains('/') -and -not $name.Contains('\') -and -not $name.Contains(':')) `
            'CI CPU worker archive contains an unexpected or unsafe entry.'
        Assert-WindowsCpuWorkerInputCondition ($seen.Add($name)) 'CI CPU worker archive contains duplicate or case-colliding entries.'
        $windowsAttributes = $entry.ExternalAttributes -band 0xFFFF
        $unixType = ($entry.ExternalAttributes -shr 16) -band 0xF000
        Assert-WindowsCpuWorkerInputCondition `
            (($windowsAttributes -band [int][IO.FileAttributes]::ReparsePoint) -eq 0 -and
                ($windowsAttributes -band [int][IO.FileAttributes]::Directory) -eq 0 -and
                $unixType -in @(0, 0x8000)) `
            'CI CPU worker archive entry is not a regular file.'
        $bound = if ($name -ceq $script:WindowsCpuInputRecordName) {
            $script:WindowsCpuInputMaximumRecordBytes
        }
        else {
            Get-WindowsFrozenCpuWorkerMaximumBytes
        }
        Assert-WindowsCpuWorkerInputCondition ($entry.Length -gt 0 -and $entry.Length -le $bound) 'CI CPU worker archive entry exceeds its fixed expansion bound.'
        $totalLength += [int64]$entry.Length
        Assert-WindowsCpuWorkerInputCondition `
            ($totalLength -le ((Get-WindowsFrozenCpuWorkerMaximumBytes) + $script:WindowsCpuInputMaximumRecordBytes)) `
            'CI CPU worker archive exceeds its total expansion bound.'
        $result[$name] = $entry
    }
    Assert-WindowsCpuWorkerInputCondition `
        ($result.Count -eq 2 -and $result.ContainsKey($script:WindowsCpuInputRecordName) -and $result.ContainsKey($script:WindowsCpuInputWorkerName)) `
        'CI CPU worker archive inventory is incomplete.'
    return $result
}

function Remove-WindowsCpuWorkerInputOwnedDirectory([string]$Path, [string]$Output, [bool]$IsFinal) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $full = Get-WindowsFrozenCpuWorkerNormalizedFullPath $Path
    $outputFull = Get-WindowsFrozenCpuWorkerNormalizedFullPath $Output
    $parent = Split-Path -Parent $outputFull
    if ($IsFinal) {
        Assert-WindowsCpuWorkerInputCondition ($full -ceq $outputFull) 'Refusing to clean an unowned final CPU worker directory.'
    }
    else {
        $expectedPrefix = ".$([IO.Path]::GetFileName($outputFull)).staging-"
        Assert-WindowsCpuWorkerInputCondition `
            ((Split-Path -Parent $full) -ceq $parent -and (Split-Path -Leaf $full).StartsWith($expectedPrefix, [StringComparison]::Ordinal)) `
            'Refusing to clean an unowned CPU worker staging directory.'
    }
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $full
    $allowed = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $null = $allowed.Add($script:WindowsCpuInputRecordName)
    $null = $allowed.Add($script:WindowsCpuInputWorkerName)
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $items = @(Get-ChildItem -LiteralPath $full -Force)
    Assert-WindowsCpuWorkerInputCondition ($items.Count -le 2) 'Refusing to clean a CPU worker directory with excess entries.'
    foreach ($item in $items) {
        Assert-WindowsCpuWorkerInputCondition `
            ($allowed.Contains($item.Name) -and $seen.Add($item.Name) -and -not $item.PSIsContainer -and
                ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -and
                [string]::IsNullOrEmpty([string]$item.LinkType)) `
            'Refusing to clean a CPU worker directory containing an unknown, linked, or non-file entry.'
        Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $item.FullName
    }
    Remove-Item -LiteralPath $full -Recurse -Force
}

function Resolve-WindowsCpuWorkerInputs {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$SourceRevision,
        [Parameter(Mandatory = $true)][string]$WorkerSourceRevision,
        [Parameter(Mandatory = $true)][string]$ProducerRunId,
        [Parameter(Mandatory = $true)][string]$ProducerRunAttempt,
        [Parameter(Mandatory = $true)][string]$ArtifactId,
        [Parameter(Mandatory = $true)][string]$ExpectedArtifactSha256,
        [Parameter(Mandatory = $true)][string]$ArchivePath,
        [Parameter(Mandatory = $true)][string]$OutputDirectory
    )

    Assert-SigningHash $ExpectedArtifactSha256 'Expected CPU worker archive SHA-256'
    Assert-WindowsCpuWorkerInputCondition (-not [string]::IsNullOrWhiteSpace($ArchivePath)) 'ArchivePath is required.'
    Assert-WindowsCpuWorkerInputCondition (-not [string]::IsNullOrWhiteSpace($OutputDirectory)) 'OutputDirectory is required.'
    $preflight = Invoke-WindowsCpuWorkerInputPreflight `
        -SourceRevision $SourceRevision `
        -WorkerSourceRevision $WorkerSourceRevision `
        -ProducerRunId $ProducerRunId `
        -ProducerRunAttempt $ProducerRunAttempt `
        -ArtifactId $ArtifactId
    Assert-WindowsCpuWorkerInputCondition `
        ($preflight.ArtifactSha256 -ceq $ExpectedArtifactSha256) `
        'CPU worker archive digest changed after independent preflight.'

    $archiveFull = Get-WindowsFrozenCpuWorkerNormalizedFullPath $ArchivePath
    $archiveItem = Assert-WindowsCpuWorkerInputUnlinkedFile $archiveFull 'Retained raw CPU worker ZIP'
    Assert-WindowsCpuWorkerInputCondition `
        ($archiveItem.Length -eq [int64]$preflight.ArtifactSizeBytes -and $archiveItem.Length -le $script:WindowsCpuInputMaximumArchiveBytes) `
        'Retained CPU worker archive size does not match GitHub artifact metadata.'
    $archiveStream = Open-WindowsFrozenCpuWorkerReadHandle $archiveFull
    $archive = $null
    $workerStream = $null
    $staging = $null
    $ownedFinal = $false
    $success = $false
    try {
        $archiveSha256 = Get-WindowsFrozenCpuWorkerOpenStreamSha256 $archiveStream
        Assert-WindowsCpuWorkerInputCondition `
            ($archiveSha256 -ceq $ExpectedArtifactSha256) `
            'Retained raw CPU worker ZIP bytes do not match the independently authenticated GitHub artifact digest.'
        $archiveStream.Position = 0
        $archive = [IO.Compression.ZipArchive]::new($archiveStream, [IO.Compression.ZipArchiveMode]::Read, $true)
        $entries = Assert-WindowsCpuWorkerInputZipEntries $archive
        $recordBytes = Read-WindowsCpuWorkerInputZipRecord $entries[$script:WindowsCpuInputRecordName]
        $record = ConvertFrom-SigningJson $recordBytes
        Assert-WindowsCpuWorkerInputRecord $record $recordBytes $preflight

        $output = Get-WindowsFrozenCpuWorkerNormalizedFullPath $OutputDirectory
        Assert-WindowsCpuWorkerInputCondition `
            (-not (Test-WindowsFrozenCpuWorkerPathIsWithin $output $preflight.RepositoryRoot) -and
                -not (Test-WindowsFrozenCpuWorkerPathIsWithin $output $archiveFull) -and
                -not (Test-WindowsFrozenCpuWorkerPathIsWithin $archiveFull $output)) `
            'CPU worker output cannot overlap the trusted source checkout or retained raw archive.'
        $parent = Split-Path -Parent $output
        Assert-WindowsCpuWorkerInputCondition (-not [string]::IsNullOrWhiteSpace($parent)) 'CPU worker output requires a parent directory.'
        Assert-WindowsFrozenCpuWorkerNoReparseAncestors $parent
        Assert-WindowsCpuWorkerInputCondition (Test-Path -LiteralPath $parent -PathType Container) 'CPU worker output parent must already exist.'
        Assert-WindowsCpuWorkerInputCondition (-not (Test-Path -LiteralPath $output)) 'CPU worker output directory must be fresh.'
        $staging = Join-Path $parent (".$([IO.Path]::GetFileName($output)).staging-$PID-$([guid]::NewGuid().ToString('N'))")
        Assert-WindowsCpuWorkerInputCondition (-not (Test-Path -LiteralPath $staging)) 'CPU worker output staging directory unexpectedly exists.'
        [IO.Directory]::CreateDirectory($staging) | Out-Null
        Assert-WindowsFrozenCpuWorkerNoReparseAncestors $staging

        $recordPath = Join-Path $staging $script:WindowsCpuInputRecordName
        $recordOutput = [IO.File]::Open($recordPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try { $recordOutput.Write($recordBytes, 0, $recordBytes.Length) }
        finally { $recordOutput.Dispose() }
        $workerPath = Join-Path $staging $script:WindowsCpuInputWorkerName
        Copy-WindowsCpuWorkerInputZipWorker `
            -Entry $entries[$script:WindowsCpuInputWorkerName] `
            -Destination $workerPath `
            -ExpectedSize ([int64]$record.worker_size_bytes) `
            -ExpectedSha256 ([string]$record.worker_sha256)
        $items = @(Get-ChildItem -LiteralPath $staging -Force)
        Assert-WindowsCpuWorkerInputCondition `
            ($items.Count -eq 2 -and @($items | Where-Object { $_.PSIsContainer }).Count -eq 0) `
            'Resolved CPU worker output inventory is not exact.'
        $null = Assert-WindowsCpuWorkerInputUnlinkedFile $recordPath 'Resolved CPU worker record'
        $null = Assert-WindowsCpuWorkerInputUnlinkedFile $workerPath 'Resolved CPU worker'
        Assert-WindowsCpuWorkerInputCondition `
            ((Get-WindowsFrozenCpuWorkerFileSha256 $recordPath) -ceq (ConvertTo-WindowsFrozenCpuWorkerSha256 $recordBytes)) `
            'Resolved CPU worker record bytes changed before activation.'
        Assert-WindowsCpuWorkerInputCondition `
            ((Get-WindowsFrozenCpuWorkerFileSha256 $workerPath) -ceq [string]$record.worker_sha256) `
            'Resolved CPU worker bytes changed before activation.'
        # Hashing and bounded decompression may be long-running for a maximum-size
        # worker. Re-authenticate the exact producer tuple and clean installer
        # source immediately before the no-replace activation boundary.
        $latePreflight = Invoke-WindowsCpuWorkerInputPreflight `
            -SourceRevision $SourceRevision `
            -WorkerSourceRevision $WorkerSourceRevision `
            -ProducerRunId $ProducerRunId `
            -ProducerRunAttempt $ProducerRunAttempt `
            -ArtifactId $ArtifactId
        Assert-WindowsCpuWorkerInputCondition `
            ($latePreflight.ArtifactSha256 -ceq $ExpectedArtifactSha256 -and
                $latePreflight.ArtifactSha256 -ceq $preflight.ArtifactSha256 -and
                [int64]$latePreflight.ArtifactSizeBytes -eq [int64]$preflight.ArtifactSizeBytes) `
            'CPU worker producer provenance changed before atomic activation.'
        Assert-WindowsFrozenCpuWorkerContextUnchanged $preflight.Context
        Assert-WindowsCpuWorkerInputRecord $record $recordBytes $latePreflight
        Assert-WindowsCpuWorkerInputCondition (-not (Test-Path -LiteralPath $output)) 'CPU worker output appeared during staging.'
        [IO.Directory]::Move($staging, $output)
        $staging = $null
        $ownedFinal = $true
        $finalRecordPath = Join-Path $output $script:WindowsCpuInputRecordName
        $finalWorkerPath = Join-Path $output $script:WindowsCpuInputWorkerName
        $workerStream = Open-WindowsFrozenCpuWorkerReadHandle $finalWorkerPath
        Assert-WindowsCpuWorkerInputCondition `
            ($workerStream.Length -eq [int64]$record.worker_size_bytes -and
                (Get-WindowsFrozenCpuWorkerOpenStreamSha256 $workerStream) -ceq [string]$record.worker_sha256) `
            'Resolved CPU worker changed after atomic activation.'
        $success = $true
        return [pscustomobject]@{
            SourceRevision = $preflight.SourceRevision
            WorkerSourceRevision = $preflight.WorkerSourceRevision
            ProducerRunId = $preflight.ProducerRunId
            ProducerRunAttempt = $preflight.ProducerRunAttempt
            ArtifactId = $preflight.ArtifactId
            ArtifactSha256 = $preflight.ArtifactSha256
            ArtifactSizeBytes = $preflight.ArtifactSizeBytes
            Root = $output
            RecordPath = $finalRecordPath
            Record = $record
            Context = ConvertTo-WindowsCpuWorkerInputContext $record
            WorkerPath = $finalWorkerPath
            WorkerStream = $workerStream
        }
    }
    finally {
        if ($null -ne $archive) { $archive.Dispose() }
        $archiveStream.Dispose()
        if (-not $success -and $null -ne $workerStream) { $workerStream.Dispose() }
        if ($null -ne $staging) {
            Remove-WindowsCpuWorkerInputOwnedDirectory $staging $OutputDirectory $false
        }
        if (-not $success -and $ownedFinal) {
            Remove-WindowsCpuWorkerInputOwnedDirectory $OutputDirectory $OutputDirectory $true
        }
    }
}

if (-not $windowsCpuInputFunctionsOnly) {
    if ($windowsCpuInputEntryArguments.Mode -ceq 'Preflight') {
        Assert-WindowsCpuWorkerInputCondition `
            (-not $windowsCpuInputExplicit.ExpectedArtifactSha256 -and -not $windowsCpuInputExplicit.ArchivePath -and -not $windowsCpuInputExplicit.OutputDirectory) `
            'Preflight does not accept resolve-only archive or output arguments.'
        Invoke-WindowsCpuWorkerInputPreflight `
            -SourceRevision $windowsCpuInputEntryArguments.SourceRevision `
            -WorkerSourceRevision $windowsCpuInputEntryArguments.WorkerSourceRevision `
            -ProducerRunId $windowsCpuInputEntryArguments.ProducerRunId `
            -ProducerRunAttempt $windowsCpuInputEntryArguments.ProducerRunAttempt `
            -ArtifactId $windowsCpuInputEntryArguments.ArtifactId
    }
    elseif ($windowsCpuInputEntryArguments.Mode -ceq 'Resolve') {
        Assert-WindowsCpuWorkerInputCondition `
            ($windowsCpuInputExplicit.ExpectedArtifactSha256 -and $windowsCpuInputExplicit.ArchivePath -and $windowsCpuInputExplicit.OutputDirectory) `
            'Resolve requires the independently pinned archive digest, raw ZIP path, and fresh output directory.'
        Resolve-WindowsCpuWorkerInputs `
            -SourceRevision $windowsCpuInputEntryArguments.SourceRevision `
            -WorkerSourceRevision $windowsCpuInputEntryArguments.WorkerSourceRevision `
            -ProducerRunId $windowsCpuInputEntryArguments.ProducerRunId `
            -ProducerRunAttempt $windowsCpuInputEntryArguments.ProducerRunAttempt `
            -ArtifactId $windowsCpuInputEntryArguments.ArtifactId `
            -ExpectedArtifactSha256 $windowsCpuInputEntryArguments.ExpectedArtifactSha256 `
            -ArchivePath $windowsCpuInputEntryArguments.ArchivePath `
            -OutputDirectory $windowsCpuInputEntryArguments.OutputDirectory
    }
    else {
        throw 'Mode must be Preflight or Resolve.'
    }
}
