[CmdletBinding()]
param(
    [string]$ProducerSourceRevision,
    [string]$ProducerRunId,
    [string]$ProducerRunAttempt,
    [string]$ArtifactId,
    [string]$ExpectedArtifactSha256,
    [string]$ExpectedPerformanceContractSha256,
    [string]$ExpectedCampaignNonce,
    [string]$ExpectedAuthorizationSha256,
    [switch]$FunctionsOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:QualificationCiRepository = 'tyhuang9/scribe'
$script:QualificationCiRepositoryId = [int64]1273587431
$script:QualificationCiRef = 'refs/heads/main'
$script:QualificationCiWorkflow = '.github/workflows/windows-gpu-qualification-preflight.yml'
$script:QualificationCiProducer = '.github/workflows/windows-gpu-performance-capture.yml'
$script:QualificationCiPolicyPath = 'runtime-manifests/windows-gpu-qualification-ci-policy.json'
$script:QualificationCiUtf8 = [Text.UTF8Encoding]::new($false, $true)
$script:QualificationCiResponseLimit = 4MB

function Assert-QualificationCi([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "GPU qualification preflight rejected: $Message" }
}

function Assert-QualificationCiHash($Value, [int]$Length = 64) {
    Assert-QualificationCi ($Value -is [string] -and $Value -cmatch "\A[0-9a-f]{$Length}\z" -and $Value -cne ('0' * $Length)) 'A required digest or nonce is not canonical.'
}

function Assert-QualificationCiId($Value) {
    $parsed = [int64]0
    Assert-QualificationCi ($Value -is [string] -and $Value -cmatch '\A[1-9][0-9]{0,18}\z' -and [int64]::TryParse($Value, [ref]$parsed)) 'A required GitHub ID is not canonical.'
}

function Assert-QualificationCiInteger($Value, [int64]$Minimum, [int64]$Maximum) {
    Assert-QualificationCi (($Value -is [int32] -or $Value -is [int64]) -and $Value -ge $Minimum -and $Value -le $Maximum) 'A required JSON integer is invalid.'
}

function Assert-QualificationCiKeys($Value, [string[]]$Expected) {
    Assert-QualificationCi ($Value -is [Collections.IDictionary] -and $Value.Count -eq $Expected.Count) 'Policy fields are missing or unexpected.'
    foreach ($key in $Expected) {
        Assert-QualificationCi (@($Value.Keys | Where-Object { $_ -ceq $key }).Count -eq 1) 'Policy fields are missing or unexpected.'
    }
}

function ConvertFrom-QualificationCiJson([byte[]]$Bytes) {
    Assert-QualificationCi ($Bytes.Length -gt 0 -and $Bytes.Length -le $script:QualificationCiResponseLimit) 'JSON exceeds its size bound.'
    $null = $script:QualificationCiUtf8.GetString($Bytes)
    $options = [Text.Json.JsonDocumentOptions]::new()
    $options.MaxDepth = 32
    $document = [Text.Json.JsonDocument]::Parse([ReadOnlyMemory[byte]]::new($Bytes), $options)
    function ConvertFrom-QualificationCiJsonNode([Text.Json.JsonElement]$Node) {
        if ($Node.ValueKind -eq [Text.Json.JsonValueKind]::Object) {
            $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
            $result = @{}
            foreach ($property in $Node.EnumerateObject()) {
                Assert-QualificationCi ($names.Add($property.Name)) 'JSON contains duplicate or case-colliding fields.'
                $result[$property.Name] = ConvertFrom-QualificationCiJsonNode $property.Value
            }
            return $result
        }
        elseif ($Node.ValueKind -eq [Text.Json.JsonValueKind]::Array) {
            return ,([object[]]@($Node.EnumerateArray() | ForEach-Object { ConvertFrom-QualificationCiJsonNode $_ }))
        }
        # Preserve JSON string and integer types explicitly. ConvertFrom-Json
        # can coerce timestamp strings to DateTime on newer PowerShell hosts.
        switch ($Node.ValueKind) {
            String { return $Node.GetString() }
            Number {
                $integer = [int64]0
                if ($Node.TryGetInt64([ref]$integer)) { return $integer }
                return $Node.GetDouble()
            }
            True { return $true }
            False { return $false }
            Null { return $null }
            default { throw 'GPU qualification preflight rejected: Unsupported JSON value.' }
        }
    }
    try {
        Assert-QualificationCi ($document.RootElement.ValueKind -eq [Text.Json.JsonValueKind]::Object) 'JSON must be one object.'
        return ConvertFrom-QualificationCiJsonNode $document.RootElement
    }
    finally { $document.Dispose() }
}

function ConvertTo-QualificationCiSortedNode($Value) {
    if ($Value -is [Collections.IDictionary]) {
        $result = [ordered]@{}
        [string[]]$keys = @($Value.Keys)
        [Array]::Sort($keys, [StringComparer]::Ordinal)
        foreach ($key in $keys) { $result[$key] = ConvertTo-QualificationCiSortedNode $Value[$key] }
        return $result
    }
    if ($Value -is [Collections.IList] -and $Value -isnot [string]) {
        return ,([object[]]@($Value | ForEach-Object { ConvertTo-QualificationCiSortedNode $_ }))
    }
    return $Value
}

function Get-QualificationCiCanonicalJson($Value) {
    return ((ConvertTo-QualificationCiSortedNode $Value) | ConvertTo-Json -Compress -Depth 32) + "`n"
}

function Get-QualificationCiDigest([byte[]]$Bytes) {
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Bytes)).ToLowerInvariant()
}

function Assert-QualificationCiApiPath([string]$Path) {
    $prefix = [regex]::Escape("/repos/$script:QualificationCiRepository/")
    $policy = [regex]::Escape("contents/$script:QualificationCiPolicyPath")
    $producer = [regex]::Escape("contents/$script:QualificationCiProducer")
    $id = '[1-9][0-9]{0,18}'
    Assert-QualificationCi ($Path -cmatch "\A$prefix(?:git/ref/heads/main|compare/[0-9a-f]{40}\.\.\.[0-9a-f]{40}|(?:$policy|$producer)\?ref=[0-9a-f]{40}|actions/runs/$id(?:/attempts/$id)?|actions/artifacts/$id)\z") 'Request escaped the fixed read-only API boundary.'
}

function New-QualificationCiHttpClient {
    Assert-QualificationCi (-not [string]::IsNullOrWhiteSpace($env:GH_TOKEN)) 'Read-only GitHub token is missing.'
    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $false
    $handler.UseCookies = $false
    $client = [Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromSeconds(30)
    $client.MaxResponseContentBufferSize = $script:QualificationCiResponseLimit
    $client.DefaultRequestHeaders.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $env:GH_TOKEN)
    $client.DefaultRequestHeaders.Add('Accept', 'application/vnd.github+json')
    $client.DefaultRequestHeaders.Add('X-GitHub-Api-Version', '2022-11-28')
    $client.DefaultRequestHeaders.Add('User-Agent', 'scribe-gpu-qualification-preflight')
    return $client
}

function Invoke-QualificationCiGitHubGet([string]$Path) {
    Assert-QualificationCiApiPath $Path
    $client = New-QualificationCiHttpClient
    try {
        $response = $client.GetAsync("https://api.github.com$Path").GetAwaiter().GetResult()
        try {
            Assert-QualificationCi ([int]$response.StatusCode -eq 200) 'GitHub provenance request failed.'
            return ConvertFrom-QualificationCiJson ($response.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult())
        }
        finally { $response.Dispose() }
    }
    catch {
        # Never echo a response body, supplied URL, token or HTTP exception.
        throw 'GPU qualification preflight rejected: GitHub provenance could not be authenticated.'
    }
    finally { $client.Dispose() }
}

function Get-QualificationCiNow { return [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() }

function Assert-QualificationCiController($Context) {
    Assert-QualificationCi ($Context.Repository -ceq $script:QualificationCiRepository -and $Context.RepositoryId -ceq [string]$script:QualificationCiRepositoryId) 'Controller repository does not match.'
    Assert-QualificationCi ($Context.Ref -ceq $script:QualificationCiRef -and $Context.Event -ceq 'workflow_dispatch') 'Controller requires a protected-main manual dispatch.'
    Assert-QualificationCi ($Context.WorkflowRef -ceq "$script:QualificationCiRepository/$script:QualificationCiWorkflow@$script:QualificationCiRef") 'Controller workflow does not match.'
    Assert-QualificationCiHash $Context.Revision 40
    Assert-QualificationCi ($Context.WorkflowRevision -ceq $Context.Revision) 'Controller workflow and checkout source differ.'
}

function Get-QualificationCiContentBytes($Content, [string]$Path, [int]$MaximumBytes) {
    Assert-QualificationCi ($Content.type -ceq 'file' -and $Content.path -ceq $Path -and $Content.encoding -ceq 'base64') 'Reviewed repository input is not the expected file.'
    Assert-QualificationCiInteger $Content.size 1 $MaximumBytes
    Assert-QualificationCi ($Content.content -is [string] -and $Content.content.Length -le (2 * $MaximumBytes)) 'Reviewed repository input exceeds its bound.'
    [byte[]]$bytes = [Convert]::FromBase64String($Content.content)
    Assert-QualificationCi ($bytes.Length -eq $Content.size) 'Reviewed repository input length differs.'
    return ,$bytes
}

function Get-QualificationCiPolicy([string]$ControllerRevision) {
    $branch = Invoke-QualificationCiGitHubGet "/repos/$script:QualificationCiRepository/git/ref/heads/main"
    Assert-QualificationCi ($branch.ref -ceq $script:QualificationCiRef -and $branch.object.type -ceq 'commit' -and $branch.object.sha -ceq $ControllerRevision) 'Protected main changed; start a fresh dispatch.'
    $content = Invoke-QualificationCiGitHubGet "/repos/$script:QualificationCiRepository/contents/$script:QualificationCiPolicyPath`?ref=$ControllerRevision"
    [byte[]]$bytes = Get-QualificationCiContentBytes $content $script:QualificationCiPolicyPath 65536
    $policy = ConvertFrom-QualificationCiJson $bytes
    Assert-QualificationCiKeys $policy @('approved_campaigns', 'kind', 'producer_workflow', 'schema_version', 'source_ref', 'source_repository')
    Assert-QualificationCiInteger $policy.schema_version 1 1
    Assert-QualificationCi ($policy.kind -ceq 'windows_gpu_qualification_ci_policy' -and $policy.source_repository -ceq $script:QualificationCiRepository -and $policy.source_ref -ceq $script:QualificationCiRef -and $policy.producer_workflow -ceq $script:QualificationCiProducer) 'Qualification policy namespace does not match.'
    Assert-QualificationCi ($script:QualificationCiUtf8.GetString($bytes) -ceq (Get-QualificationCiCanonicalJson $policy)) 'Qualification policy is not canonical JSON.'
    Assert-QualificationCi ($policy.approved_campaigns -is [Collections.IList] -and $policy.approved_campaigns.Count -le 64) 'Approved campaigns must be a bounded array.'
    $nonces = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $artifacts = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $attempts = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $previous = ''
    foreach ($entry in $policy.approved_campaigns) {
        Assert-QualificationCiKeys $entry @('artifact_id', 'artifact_sha256', 'authorization_sha256', 'campaign_nonce', 'expires_at_unix_seconds', 'issued_at_unix_seconds', 'performance_contract_sha256', 'producer_run_attempt', 'producer_run_id', 'producer_workflow_sha256', 'source_revision')
        foreach ($key in @('artifact_sha256', 'authorization_sha256', 'campaign_nonce', 'performance_contract_sha256', 'producer_workflow_sha256')) { Assert-QualificationCiHash $entry[$key] }
        Assert-QualificationCiHash $entry.source_revision 40
        foreach ($key in @('artifact_id', 'producer_run_attempt', 'producer_run_id')) { Assert-QualificationCiId $entry[$key] }
        Assert-QualificationCiInteger $entry.issued_at_unix_seconds 1 253402300799
        Assert-QualificationCiInteger $entry.expires_at_unix_seconds 1 253402300799
        Assert-QualificationCi ($entry.expires_at_unix_seconds -gt $entry.issued_at_unix_seconds -and ($entry.expires_at_unix_seconds - $entry.issued_at_unix_seconds) -le 604800) 'Campaign approval has an invalid validity interval.'
        Assert-QualificationCi ($nonces.Add($entry.campaign_nonce) -and $artifacts.Add($entry.artifact_id) -and $attempts.Add("$($entry.producer_run_id)/$($entry.producer_run_attempt)")) 'Campaign identities overlap.'
        Assert-QualificationCi ([StringComparer]::Ordinal.Compare($previous, $entry.campaign_nonce) -lt 0) 'Approved campaigns are not strictly nonce-sorted.'
        $previous = $entry.campaign_nonce
    }
    return @{ Document = $policy; Digest = Get-QualificationCiDigest $bytes }
}

function Assert-QualificationCiRun($Run, $Pins) {
    Assert-QualificationCiInteger $Run.id 1 ([int64]::MaxValue)
    Assert-QualificationCiInteger $Run.run_attempt 1 ([int64]::MaxValue)
    foreach ($repository in @($Run.repository, $Run.head_repository)) {
        Assert-QualificationCiInteger $repository.id $script:QualificationCiRepositoryId $script:QualificationCiRepositoryId
        Assert-QualificationCi ($repository.full_name -ceq $script:QualificationCiRepository) 'Producer repository does not match.'
    }
    Assert-QualificationCi ([string]$Run.id -ceq $Pins.producer_run_id -and [string]$Run.run_attempt -ceq $Pins.producer_run_attempt) 'Producer run or latest attempt differs.'
    Assert-QualificationCi ($Run.path -ceq $script:QualificationCiProducer -and $Run.event -ceq 'workflow_dispatch' -and $Run.head_branch -ceq 'main' -and $Run.head_sha -ceq $Pins.source_revision) 'Producer workflow or source is not approved.'
    Assert-QualificationCi ($Run.status -ceq 'completed' -and $Run.conclusion -ceq 'success') 'Producer has not completed successfully.'
}

function Get-QualificationCiTimestamp($Value) {
    Assert-QualificationCi ($Value -is [string] -and $Value -cmatch '\A[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,7})?Z\z') 'Producer timestamp is invalid.'
    return [DateTimeOffset]::Parse($Value, [Globalization.CultureInfo]::InvariantCulture).ToUnixTimeSeconds()
}

function Get-QualificationCiProducer($Pins) {
    $prefix = "/repos/$script:QualificationCiRepository"
    $run = Invoke-QualificationCiGitHubGet "$prefix/actions/runs/$($Pins.producer_run_id)/attempts/$($Pins.producer_run_attempt)"
    Assert-QualificationCiRun $run $Pins
    $latest = Invoke-QualificationCiGitHubGet "$prefix/actions/runs/$($Pins.producer_run_id)"
    Assert-QualificationCiRun $latest $Pins
    $artifact = Invoke-QualificationCiGitHubGet "$prefix/actions/artifacts/$($Pins.artifact_id)"
    Assert-QualificationCiInteger $artifact.id 1 ([int64]::MaxValue)
    Assert-QualificationCiInteger $artifact.workflow_run.id 1 ([int64]::MaxValue)
    foreach ($key in @('repository_id', 'head_repository_id')) { Assert-QualificationCiInteger $artifact.workflow_run[$key] $script:QualificationCiRepositoryId $script:QualificationCiRepositoryId }
    Assert-QualificationCi ([string]$artifact.id -ceq $Pins.artifact_id -and [string]$artifact.workflow_run.id -ceq $Pins.producer_run_id -and $artifact.workflow_run.head_sha -ceq $Pins.source_revision -and $artifact.workflow_run.head_branch -ceq 'main') 'Artifact belongs to another source or producer.'
    Assert-QualificationCi ($artifact.name -ceq "windows-gpu-performance-capture-$($Pins.producer_run_id)-$($Pins.producer_run_attempt)" -and $artifact.digest -ceq "sha256:$($Pins.artifact_sha256)" -and $artifact.expired -is [bool] -and -not $artifact.expired) 'Artifact name, digest or expiry differs.'
    Assert-QualificationCiInteger $artifact.size_in_bytes 1 536870912
    $start = Get-QualificationCiTimestamp $run.run_started_at
    $updated = Get-QualificationCiTimestamp $run.updated_at
    $created = Get-QualificationCiTimestamp $artifact.created_at
    $expires = Get-QualificationCiTimestamp $artifact.expires_at
    $now = Get-QualificationCiNow
    Assert-QualificationCi ($start -le $created -and $created -le $updated -and $updated -le $now -and $expires -gt $now) 'Artifact does not belong to the completed attempt window or has expired.'
    $workflowContent = Invoke-QualificationCiGitHubGet "$prefix/contents/$script:QualificationCiProducer`?ref=$($Pins.source_revision)"
    [byte[]]$workflow = Get-QualificationCiContentBytes $workflowContent $script:QualificationCiProducer 131072
    Assert-QualificationCi ((Get-QualificationCiDigest $workflow) -ceq $Pins.producer_workflow_sha256) 'Producer workflow bytes differ from reviewed policy.'
    return @{ artifact_size_bytes = $artifact.size_in_bytes; artifact_created_at = $artifact.created_at; artifact_expires_at = $artifact.expires_at; run_started_at = $run.run_started_at; run_updated_at = $run.updated_at }
}

function Invoke-QualificationCiPreflight($Context, $Pins) {
    Assert-QualificationCiController $Context
    Assert-QualificationCiKeys $Pins @('artifact_id', 'artifact_sha256', 'authorization_sha256', 'campaign_nonce', 'performance_contract_sha256', 'producer_run_attempt', 'producer_run_id', 'source_revision')
    foreach ($key in @('artifact_sha256', 'authorization_sha256', 'campaign_nonce', 'performance_contract_sha256')) { Assert-QualificationCiHash $Pins[$key] }
    Assert-QualificationCiHash $Pins.source_revision 40
    foreach ($key in @('artifact_id', 'producer_run_attempt', 'producer_run_id')) { Assert-QualificationCiId $Pins[$key] }
    $policy = Get-QualificationCiPolicy $Context.Revision
    $matches = @($policy.Document.approved_campaigns | Where-Object { $_.campaign_nonce -ceq $Pins.campaign_nonce })
    Assert-QualificationCi ($matches.Count -eq 1) 'Campaign has no independent reviewed approval.'
    $approval = $matches[0]
    foreach ($key in $Pins.Keys) { Assert-QualificationCi ($approval[$key] -ceq $Pins[$key]) 'Campaign pin differs from independent policy.' }
    $now = Get-QualificationCiNow
    Assert-QualificationCi ($approval.issued_at_unix_seconds -le $now -and $now -lt $approval.expires_at_unix_seconds) 'Campaign approval is not currently valid.'
    $comparison = Invoke-QualificationCiGitHubGet "/repos/$script:QualificationCiRepository/compare/$($Pins.source_revision)...$($Context.Revision)"
    Assert-QualificationCi ($comparison.merge_base_commit.sha -ceq $Pins.source_revision -and $comparison.status -cin @('ahead', 'identical')) 'Producer is not in protected-main history.'
    $producer = Get-QualificationCiProducer $approval
    # Reobserve the same exact identities, never replace pins with newer values.
    $currentPolicy = Get-QualificationCiPolicy $Context.Revision
    Assert-QualificationCi ($currentPolicy.Digest -ceq $policy.Digest) 'Policy changed during preflight.'
    $currentProducer = Get-QualificationCiProducer $approval
    Assert-QualificationCi ((Get-QualificationCiCanonicalJson $producer) -ceq (Get-QualificationCiCanonicalJson $currentProducer)) 'Producer provenance changed during preflight.'
    $finalMain = Invoke-QualificationCiGitHubGet "/repos/$script:QualificationCiRepository/git/ref/heads/main"
    Assert-QualificationCi ($finalMain.ref -ceq $script:QualificationCiRef -and $finalMain.object.type -ceq 'commit' -and $finalMain.object.sha -ceq $Context.Revision) 'Protected main changed before the receipt.'
    $now = Get-QualificationCiNow
    Assert-QualificationCi ($approval.issued_at_unix_seconds -le $now -and $now -lt $approval.expires_at_unix_seconds) 'Campaign approval expired during preflight.'
    return @{
        schema_version = 1; kind = 'windows_gpu_qualification_ci_preflight'
        source_repository = $script:QualificationCiRepository; source_ref = $script:QualificationCiRef
        controller_revision = $Context.Revision; producer_workflow = $script:QualificationCiProducer
        policy_sha256 = $policy.Digest; pins = $approval; provenance = $producer
        archive_contents_verified = $false; capture_authenticated = $false; nonce_consumed = $false
        signing_authorized = $false; auto_eligible = $false; release_approved = $false
    }
}

if (-not $FunctionsOnly) {
    $context = @{
        Repository = $env:GITHUB_REPOSITORY; RepositoryId = $env:GITHUB_REPOSITORY_ID
        Ref = $env:GITHUB_REF; Event = $env:GITHUB_EVENT_NAME; Revision = $env:GITHUB_SHA
        WorkflowRef = $env:GITHUB_WORKFLOW_REF; WorkflowRevision = $env:GITHUB_WORKFLOW_SHA
    }
    $pins = @{
        source_revision = $ProducerSourceRevision; producer_run_id = $ProducerRunId
        producer_run_attempt = $ProducerRunAttempt; artifact_id = $ArtifactId
        artifact_sha256 = $ExpectedArtifactSha256; performance_contract_sha256 = $ExpectedPerformanceContractSha256
        campaign_nonce = $ExpectedCampaignNonce; authorization_sha256 = $ExpectedAuthorizationSha256
    }
    $result = Invoke-QualificationCiPreflight $context $pins
    $json = Get-QualificationCiCanonicalJson $result
    Assert-QualificationCi ($script:QualificationCiUtf8.GetByteCount($json) -le 16384) 'Preflight output exceeds its bound.'
    # One complete data-only receipt, after all checks. No files or GitHub state
    # are written; stored receipts never become authorization for a later job.
    [Console]::Out.Write($json)
}
