[CmdletBinding()]
param(
    [ValidateSet('Preflight', 'Download', 'Resolve')][string]$Mode,
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
    SourceRevision = $PSBoundParameters.ContainsKey('SourceRevision')
    WorkerSourceRevision = $PSBoundParameters.ContainsKey('WorkerSourceRevision')
    ProducerRunId = $PSBoundParameters.ContainsKey('ProducerRunId')
    ProducerRunAttempt = $PSBoundParameters.ContainsKey('ProducerRunAttempt')
    ArtifactId = $PSBoundParameters.ContainsKey('ArtifactId')
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
$script:WindowsCpuInputMaximumResponseHeaderBytes = [int64]65536
$script:WindowsCpuInputDownloadDeadline = [TimeSpan]::FromSeconds(120)

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

function New-WindowsCpuWorkerInputHttpClient([bool]$Authenticated) {
    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $false
    $handler.MaxResponseHeadersLength = [int]($script:WindowsCpuInputMaximumResponseHeaderBytes / 1024)
    $client = [Net.Http.HttpClient]::new($handler, $true)
    $client.Timeout = $script:WindowsCpuInputDownloadDeadline
    if ($Authenticated) {
        Assert-WindowsCpuWorkerInputCondition `
            (-not [string]::IsNullOrWhiteSpace($env:GH_TOKEN)) `
            'Read-only GitHub token is missing.'
        $client.DefaultRequestHeaders.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $env:GH_TOKEN)
        $client.DefaultRequestHeaders.Add('Accept', 'application/vnd.github+json')
        $client.DefaultRequestHeaders.Add('X-GitHub-Api-Version', '2022-11-28')
        $client.DefaultRequestHeaders.Add('User-Agent', 'scribe-ci-cpu-worker-input')
    }
    return $client
}

function Assert-WindowsCpuWorkerInputResponseHeaders([Net.Http.HttpResponseMessage]$Response) {
    $total = [int64]0
    $encoding = [Text.UTF8Encoding]::new($false, $true)
    $headerCollections = @($Response.Headers)
    if ($null -ne $Response.Content) {
        $headerCollections += @($Response.Content.Headers)
    }
    foreach ($headers in $headerCollections) {
        foreach ($header in $headers) {
            $total += [int64]$encoding.GetByteCount([string]$header.Key)
            foreach ($value in $header.Value) {
                $total += [int64]$encoding.GetByteCount([string]$value)
            }
            Assert-WindowsCpuWorkerInputCondition `
                ($total -le $script:WindowsCpuInputMaximumResponseHeaderBytes) `
                'CPU worker download response headers exceed the fixed bound.'
        }
    }
}

function Remove-WindowsCpuWorkerInputOwnedDownload([string]$Path, [string]$Destination) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $full = Get-WindowsFrozenCpuWorkerNormalizedFullPath $Path
    $destinationFull = Get-WindowsFrozenCpuWorkerNormalizedFullPath $Destination
    $parent = Split-Path -Parent $destinationFull
    $expectedPrefix = ".$(Split-Path -Leaf $destinationFull).download-"
    Assert-WindowsCpuWorkerInputCondition `
        ((Split-Path -Parent $full) -ceq $parent -and
            (Split-Path -Leaf $full).StartsWith($expectedPrefix, [StringComparison]::Ordinal)) `
        'Refusing to clean an unowned CPU worker download staging file.'
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $full
    $item = Assert-WindowsCpuWorkerInputUnlinkedFile $full 'CPU worker download staging file'
    Assert-WindowsCpuWorkerInputCondition `
        ($item.Length -ge 0 -and $item.Length -le $script:WindowsCpuInputMaximumArchiveBytes) `
        'Refusing to clean an oversized CPU worker download staging file.'
    Remove-Item -LiteralPath $full -Force
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

function Assert-WindowsCpuWorkerInputPreflightMatchesRequest(
    [psobject]$Preflight,
    [string]$SourceRevision,
    [string]$WorkerSourceRevision,
    [string]$ProducerRunId,
    [string]$ProducerRunAttempt,
    [string]$ArtifactId,
    [string]$ExpectedArtifactSha256 = ''
) {
    Assert-WindowsCpuWorkerInputCondition ($null -ne $Preflight) 'CPU worker provenance preflight returned no result.'
    foreach ($pair in @(
        @('SourceRevision', $SourceRevision),
        @('WorkerSourceRevision', $WorkerSourceRevision),
        @('ProducerRunId', $ProducerRunId),
        @('ProducerRunAttempt', $ProducerRunAttempt),
        @('ArtifactId', $ArtifactId)
    )) {
        Assert-WindowsCpuWorkerInputCondition `
            ([string]$Preflight.($pair[0]) -ceq [string]$pair[1]) `
            "CPU worker provenance preflight changed the requested $($pair[0])."
    }
    Assert-SigningHash ([string]$Preflight.ArtifactSha256) 'CPU worker provenance preflight archive SHA-256'
    Assert-WindowsCpuWorkerInputCondition `
        ([int64]$Preflight.ArtifactSizeBytes -gt 0 -and [int64]$Preflight.ArtifactSizeBytes -le $script:WindowsCpuInputMaximumArchiveBytes) `
        'CPU worker provenance preflight archive size is outside the fixed bound.'
    if (-not [string]::IsNullOrEmpty($ExpectedArtifactSha256)) {
        Assert-SigningHash $ExpectedArtifactSha256 'Expected CPU worker archive SHA-256'
        Assert-WindowsCpuWorkerInputCondition `
            ([string]$Preflight.ArtifactSha256 -ceq $ExpectedArtifactSha256) `
            'CPU worker archive digest changed after independent preflight.'
    }
}

function Invoke-WindowsCpuWorkerInputDownload {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$SourceRevision,
        [Parameter(Mandatory = $true)][string]$WorkerSourceRevision,
        [Parameter(Mandatory = $true)][string]$ProducerRunId,
        [Parameter(Mandatory = $true)][string]$ProducerRunAttempt,
        [Parameter(Mandatory = $true)][string]$ArtifactId,
        [Parameter(Mandatory = $true)][string]$ExpectedArtifactSha256,
        [Parameter(Mandatory = $true)][string]$ArchivePath
    )

    Assert-SigningHash $ExpectedArtifactSha256 'Expected CPU worker archive SHA-256'
    Assert-WindowsCpuWorkerInputCondition (-not [string]::IsNullOrWhiteSpace($ArchivePath)) 'ArchivePath is required.'
    $preflight = Invoke-WindowsCpuWorkerInputPreflight `
        -SourceRevision $SourceRevision `
        -WorkerSourceRevision $WorkerSourceRevision `
        -ProducerRunId $ProducerRunId `
        -ProducerRunAttempt $ProducerRunAttempt `
        -ArtifactId $ArtifactId
    Assert-WindowsCpuWorkerInputPreflightMatchesRequest `
        -Preflight $preflight `
        -SourceRevision $SourceRevision `
        -WorkerSourceRevision $WorkerSourceRevision `
        -ProducerRunId $ProducerRunId `
        -ProducerRunAttempt $ProducerRunAttempt `
        -ArtifactId $ArtifactId `
        -ExpectedArtifactSha256 $ExpectedArtifactSha256

    $archiveFull = Get-WindowsFrozenCpuWorkerNormalizedFullPath $ArchivePath
    $parent = Split-Path -Parent $archiveFull
    Assert-WindowsCpuWorkerInputCondition (-not [string]::IsNullOrWhiteSpace($parent)) 'CPU worker archive destination requires a parent directory.'
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $parent
    Assert-WindowsCpuWorkerInputCondition (Test-Path -LiteralPath $parent -PathType Container) 'CPU worker archive destination parent must already exist.'
    Assert-WindowsCpuWorkerInputCondition (-not (Test-Path -LiteralPath $archiveFull)) 'CPU worker archive destination must be fresh.'
    $staging = Join-Path $parent ".$(Split-Path -Leaf $archiveFull).download-$PID-$([guid]::NewGuid().ToString('N'))"
    Assert-WindowsCpuWorkerInputCondition (-not (Test-Path -LiteralPath $staging)) 'CPU worker archive download staging file unexpectedly exists.'

    $apiClient = $null
    $downloadClient = $null
    $apiRequest = $null
    $apiResponse = $null
    $downloadRequest = $null
    $downloadResponse = $null
    $downloadStream = $null
    $outputStream = $null
    $hash = $null
    $deadline = [Threading.CancellationTokenSource]::new($script:WindowsCpuInputDownloadDeadline)
    $success = $false
    try {
        $apiClient = New-WindowsCpuWorkerInputHttpClient $true
        $apiRequest = [Net.Http.HttpRequestMessage]::new(
            [Net.Http.HttpMethod]::Get,
            "https://api.github.com/repos/$script:WindowsCpuInputRepository/actions/artifacts/$ArtifactId/zip"
        )
        $apiResponse = $apiClient.SendAsync(
            $apiRequest,
            [Net.Http.HttpCompletionOption]::ResponseHeadersRead,
            $deadline.Token
        ).GetAwaiter().GetResult()
        Assert-WindowsCpuWorkerInputResponseHeaders $apiResponse
        Assert-WindowsCpuWorkerInputCondition `
            ($apiResponse.StatusCode -eq [Net.HttpStatusCode]::Found) `
            'CPU worker artifact endpoint did not return the required redirect.'
        $location = $apiResponse.Headers.Location
        Assert-WindowsCpuWorkerInputCondition `
            ($null -ne $location -and $location.IsAbsoluteUri -and
                $location.Scheme -ceq 'https' -and
                -not [string]::IsNullOrWhiteSpace($location.Host) -and
                [string]::IsNullOrEmpty($location.UserInfo)) `
            'CPU worker artifact redirect is not a credential-free HTTPS URL.'

        $downloadClient = New-WindowsCpuWorkerInputHttpClient $false
        Assert-WindowsCpuWorkerInputCondition `
            ($null -eq $downloadClient.DefaultRequestHeaders.Authorization) `
            'CPU worker redirect client unexpectedly contains credentials.'
        $downloadRequest = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Get, $location)
        $downloadResponse = $downloadClient.SendAsync(
            $downloadRequest,
            [Net.Http.HttpCompletionOption]::ResponseHeadersRead,
            $deadline.Token
        ).GetAwaiter().GetResult()
        Assert-WindowsCpuWorkerInputResponseHeaders $downloadResponse
        Assert-WindowsCpuWorkerInputCondition `
            ($downloadResponse.StatusCode -eq [Net.HttpStatusCode]::OK -and $null -ne $downloadResponse.Content) `
            'CPU worker artifact redirect did not return a successful archive response.'
        # PowerShell unboxes Nullable<Int64> response headers: an absent length
        # is `$null`, while a present length is already an Int64 (so HasValue /
        # Value are not reliable member accesses here).
        $reportedContentLength = $downloadResponse.Content.Headers.ContentLength
        if ($null -ne $reportedContentLength) {
            Assert-WindowsCpuWorkerInputCondition `
                ([int64]$reportedContentLength -eq [int64]$preflight.ArtifactSizeBytes) `
                'CPU worker archive response length does not match authenticated artifact metadata.'
        }

        $downloadStream = $downloadResponse.Content.ReadAsStreamAsync($deadline.Token).GetAwaiter().GetResult()
        $outputStream = [IO.File]::Open($staging, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        $hash = [Security.Cryptography.IncrementalHash]::CreateHash([Security.Cryptography.HashAlgorithmName]::SHA256)
        $buffer = [byte[]]::new(1048576)
        $total = [int64]0
        while ($true) {
            $deadline.Token.ThrowIfCancellationRequested()
            $count = $downloadStream.ReadAsync($buffer, 0, $buffer.Length, $deadline.Token).GetAwaiter().GetResult()
            if ($count -eq 0) { break }
            $total += [int64]$count
            Assert-WindowsCpuWorkerInputCondition `
                ($total -le [int64]$preflight.ArtifactSizeBytes -and $total -le $script:WindowsCpuInputMaximumArchiveBytes) `
                'CPU worker archive download exceeds its authenticated size bound.'
            $hash.AppendData($buffer, 0, $count)
            $outputStream.Write($buffer, 0, $count)
        }
        $deadline.Token.ThrowIfCancellationRequested()
        Assert-WindowsCpuWorkerInputCondition `
            ($total -eq [int64]$preflight.ArtifactSizeBytes) `
            'CPU worker archive download ended before its authenticated size.'
        $digest = [Convert]::ToHexString($hash.GetHashAndReset()).ToLowerInvariant()
        Assert-WindowsCpuWorkerInputCondition `
            ($digest -ceq $ExpectedArtifactSha256 -and $digest -ceq $preflight.ArtifactSha256) `
            'CPU worker archive download digest does not match authenticated metadata.'
        $outputStream.Flush($true)
        $outputStream.Dispose()
        $outputStream = $null
        Assert-WindowsCpuWorkerInputCondition (-not (Test-Path -LiteralPath $archiveFull)) 'CPU worker archive destination appeared during download.'
        [IO.File]::Move($staging, $archiveFull)
        $staging = $null
        $success = $true
        return [pscustomobject]@{
            SourceRevision = $preflight.SourceRevision
            WorkerSourceRevision = $preflight.WorkerSourceRevision
            ProducerRunId = $preflight.ProducerRunId
            ProducerRunAttempt = $preflight.ProducerRunAttempt
            ArtifactId = $preflight.ArtifactId
            ArchivePath = $archiveFull
            ArtifactSha256 = $preflight.ArtifactSha256
            ArtifactSizeBytes = $preflight.ArtifactSizeBytes
        }
    }
    finally {
        if ($null -ne $hash) { $hash.Dispose() }
        if ($null -ne $outputStream) { $outputStream.Dispose() }
        if ($null -ne $downloadStream) { $downloadStream.Dispose() }
        if ($null -ne $downloadResponse) { $downloadResponse.Dispose() }
        if ($null -ne $downloadRequest) { $downloadRequest.Dispose() }
        if ($null -ne $apiResponse) { $apiResponse.Dispose() }
        if ($null -ne $apiRequest) { $apiRequest.Dispose() }
        if ($null -ne $downloadClient) { $downloadClient.Dispose() }
        if ($null -ne $apiClient) { $apiClient.Dispose() }
        $deadline.Dispose()
        if (-not $success -and $null -ne $staging) {
            Remove-WindowsCpuWorkerInputOwnedDownload $staging $archiveFull
        }
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
    Assert-WindowsCpuWorkerInputPreflightMatchesRequest `
        -Preflight $preflight `
        -SourceRevision $SourceRevision `
        -WorkerSourceRevision $WorkerSourceRevision `
        -ProducerRunId $ProducerRunId `
        -ProducerRunAttempt $ProducerRunAttempt `
        -ArtifactId $ArtifactId `
        -ExpectedArtifactSha256 $ExpectedArtifactSha256

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
        $recordSha256 = ConvertTo-WindowsFrozenCpuWorkerSha256 $recordBytes
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
        Assert-WindowsCpuWorkerInputPreflightMatchesRequest `
            -Preflight $latePreflight `
            -SourceRevision $SourceRevision `
            -WorkerSourceRevision $WorkerSourceRevision `
            -ProducerRunId $ProducerRunId `
            -ProducerRunAttempt $ProducerRunAttempt `
            -ArtifactId $ArtifactId `
            -ExpectedArtifactSha256 $ExpectedArtifactSha256
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
            RecordBytes = $recordBytes
            RecordSha256 = $recordSha256
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
    elseif ($windowsCpuInputEntryArguments.Mode -ceq 'Download') {
        Assert-WindowsCpuWorkerInputCondition `
            ($windowsCpuInputExplicit.SourceRevision -and
                $windowsCpuInputExplicit.WorkerSourceRevision -and
                $windowsCpuInputExplicit.ProducerRunId -and
                $windowsCpuInputExplicit.ProducerRunAttempt -and
                $windowsCpuInputExplicit.ArtifactId -and
                $windowsCpuInputExplicit.ExpectedArtifactSha256 -and
                $windowsCpuInputExplicit.ArchivePath -and
                -not $windowsCpuInputExplicit.OutputDirectory) `
            'Download requires the exact producer tuple, independently pinned archive digest, and fresh raw ZIP destination only.'
        Invoke-WindowsCpuWorkerInputDownload `
            -SourceRevision $windowsCpuInputEntryArguments.SourceRevision `
            -WorkerSourceRevision $windowsCpuInputEntryArguments.WorkerSourceRevision `
            -ProducerRunId $windowsCpuInputEntryArguments.ProducerRunId `
            -ProducerRunAttempt $windowsCpuInputEntryArguments.ProducerRunAttempt `
            -ArtifactId $windowsCpuInputEntryArguments.ArtifactId `
            -ExpectedArtifactSha256 $windowsCpuInputEntryArguments.ExpectedArtifactSha256 `
            -ArchivePath $windowsCpuInputEntryArguments.ArchivePath
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
        throw 'Mode must be Preflight, Download, or Resolve.'
    }
}
