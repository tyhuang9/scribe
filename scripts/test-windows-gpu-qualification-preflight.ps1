[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'invoke-windows-gpu-qualification-preflight.ps1') -FunctionsOnly
$script:PreflightTestCount = 0
$transport = (Get-Command Invoke-QualificationCiGitHubGet).ScriptBlock
$script:FixtureNow = [int64]2000000000

function Assert-PreflightTest([bool]$Condition, [string]$Label) {
    if (-not $Condition) { throw "Qualification preflight test failed: $Label" }
    $script:PreflightTestCount++
}
function Assert-PreflightRejected([scriptblock]$Action, [string]$Label, [string]$Expected = '') {
    $failure = $null
    try { & $Action | Out-Null } catch { $failure = $_.Exception.Message }
    Assert-PreflightTest ($null -ne $failure -and (-not $Expected -or $failure.Contains($Expected))) $Label
}
function Copy-PreflightFixture($Value) { return ConvertFrom-QualificationCiJson ([Text.Encoding]::UTF8.GetBytes(($Value | ConvertTo-Json -Compress -Depth 32))) }
function New-PreflightContent([string]$Path, [byte[]]$Bytes) {
    return @{ type = 'file'; path = $Path; encoding = 'base64'; content = [Convert]::ToBase64String($Bytes); size = $Bytes.Length }
}
function Get-PreflightFixtureTime([int64]$Offset) { return [DateTimeOffset]::FromUnixTimeSeconds($script:FixtureNow + $Offset).ToString('yyyy-MM-ddTHH:mm:ssZ') }

function Reset-PreflightFixture {
    $script:ClockCalls = 0
    $script:ExpireOnFinalClock = $false
    $script:Requests = [Collections.Generic.List[string]]::new()
    $script:RequestCounts = @{}
    $script:ChangedResponses = @{}
    $script:Context = @{
        Repository = 'tyhuang9/scribe'; RepositoryId = '1273587431'; Ref = 'refs/heads/main'
        Event = 'workflow_dispatch'; Revision = 'b' * 40; WorkflowRevision = 'b' * 40
        WorkflowRef = 'tyhuang9/scribe/.github/workflows/windows-gpu-qualification-preflight.yml@refs/heads/main'
    }
    $script:Pins = @{
        artifact_id = '202'; artifact_sha256 = 'c' * 64; authorization_sha256 = 'd' * 64
        campaign_nonce = '1' * 64; performance_contract_sha256 = 'e' * 64
        producer_run_attempt = '2'; producer_run_id = '101'; source_revision = 'a' * 40
    }
    $script:ProducerBytes = [Text.Encoding]::UTF8.GetBytes("name: fixture-only acquisition`n")
    $script:Approval = Copy-PreflightFixture $script:Pins
    $script:Approval.producer_workflow_sha256 = Get-QualificationCiDigest $script:ProducerBytes
    $script:Approval.issued_at_unix_seconds = $script:FixtureNow - 60
    $script:Approval.expires_at_unix_seconds = $script:FixtureNow + 3600
    $script:Policy = @{
        approved_campaigns = @($script:Approval); kind = 'windows_gpu_qualification_ci_policy'
        producer_workflow = $script:QualificationCiProducer; schema_version = 1
        source_ref = 'refs/heads/main'; source_repository = 'tyhuang9/scribe'
    }
    $script:Run = @{
        id = [int64]101; run_attempt = 2; repository = @{ id = [int64]1273587431; full_name = 'tyhuang9/scribe' }
        head_repository = @{ id = [int64]1273587431; full_name = 'tyhuang9/scribe' }
        path = $script:QualificationCiProducer; event = 'workflow_dispatch'; head_branch = 'main'
        head_sha = 'a' * 40; status = 'completed'; conclusion = 'success'
        run_started_at = Get-PreflightFixtureTime -300; updated_at = Get-PreflightFixtureTime -60
    }
    $script:Artifact = @{
        id = [int64]202; workflow_run = @{ id = [int64]101; repository_id = [int64]1273587431; head_repository_id = [int64]1273587431; head_branch = 'main'; head_sha = 'a' * 40 }
        name = 'windows-gpu-performance-capture-101-2'; digest = 'sha256:' + ('c' * 64)
        size_in_bytes = [int64]1024; expired = $false
        created_at = Get-PreflightFixtureTime -120; expires_at = Get-PreflightFixtureTime 7200
    }
    $script:Prefix = '/repos/tyhuang9/scribe'
    $script:MainPath = "$script:Prefix/git/ref/heads/main"
    $script:PolicyPath = "$script:Prefix/contents/$script:QualificationCiPolicyPath`?ref=$($script:Context.Revision)"
    $script:AttemptPath = "$script:Prefix/actions/runs/101/attempts/2"
    $script:LatestPath = "$script:Prefix/actions/runs/101"
    $script:ArtifactPath = "$script:Prefix/actions/artifacts/202"
    $script:WorkflowPath = "$script:Prefix/contents/$script:QualificationCiProducer`?ref=$($script:Pins.source_revision)"
    $script:ComparePath = "$script:Prefix/compare/$($script:Pins.source_revision)...$($script:Context.Revision)"
    $script:Responses = @{}
    $script:Responses[$script:MainPath] = @{ ref = 'refs/heads/main'; object = @{ type = 'commit'; sha = $script:Context.Revision } }
    $script:Responses[$script:AttemptPath] = $script:Run
    $script:Responses[$script:LatestPath] = $script:Run
    $script:Responses[$script:ArtifactPath] = $script:Artifact
    $script:Responses[$script:WorkflowPath] = New-PreflightContent $script:QualificationCiProducer $script:ProducerBytes
    $script:Responses[$script:ComparePath] = @{ merge_base_commit = @{ sha = $script:Pins.source_revision }; status = 'ahead' }
    Sync-PreflightPolicy
}
function Sync-PreflightPolicy {
    $bytes = $script:QualificationCiUtf8.GetBytes((Get-QualificationCiCanonicalJson $script:Policy))
    $script:Responses[$script:PolicyPath] = New-PreflightContent $script:QualificationCiPolicyPath $bytes
}
function Get-QualificationCiNow {
    $script:ClockCalls++
    if ($script:ExpireOnFinalClock -and $script:ClockCalls -ge 4) { return $script:FixtureNow + 3600 }
    return $script:FixtureNow
}
# Every production HTTP call is replaced BEFORE running the core. Unexpected
# paths fail the test. No caller credentials, network or filesystem output.
function Invoke-QualificationCiGitHubGet([string]$Path) {
    Assert-QualificationCiApiPath $Path
    $script:Requests.Add($Path)
    if (-not $script:RequestCounts.ContainsKey($Path)) { $script:RequestCounts[$Path] = 0 }
    $script:RequestCounts[$Path]++
    if ($script:ChangedResponses.ContainsKey($Path) -and $script:RequestCounts[$Path] -eq $script:ChangedResponses[$Path].At) {
        return Copy-PreflightFixture $script:ChangedResponses[$Path].Value
    }
    if (-not $script:Responses.ContainsKey($Path)) { throw 'Unexpected mocked GitHub request.' }
    return Copy-PreflightFixture $script:Responses[$Path]
}

Reset-PreflightFixture
$decision = Invoke-QualificationCiPreflight $script:Context $script:Pins
Assert-PreflightTest ($decision.kind -ceq 'windows_gpu_qualification_ci_preflight' -and $decision.pins.artifact_id -ceq '202') 'valid provenance handoff'
Assert-PreflightTest ($script:Requests.Count -eq 14) 'exact initial and final metadata observations'
foreach ($flag in @('archive_contents_verified', 'capture_authenticated', 'nonce_consumed', 'signing_authorized', 'auto_eligible', 'release_approved')) {
    Assert-PreflightTest ($decision[$flag] -is [bool] -and -not $decision[$flag]) "receipt cannot grant $flag"
}
Assert-PreflightTest ((Get-QualificationCiCanonicalJson $decision).Length -lt 16384) 'bounded canonical output'
Assert-PreflightTest (@($script:Requests | Where-Object { $_ -match '/zip|download|jobs|secrets' }).Count -eq 0) 'metadata-only requests'

foreach ($key in @($script:Context.Keys)) {
    Reset-PreflightFixture
    $script:Context[$key] = 'wrong-context'
    Assert-PreflightRejected { Invoke-QualificationCiPreflight $script:Context $script:Pins } "wrong controller $key"
    Assert-PreflightTest ($script:Requests.Count -eq 0) 'controller denied before network'
}
foreach ($key in @($script:Pins.Keys)) {
    foreach ($bad in @('', "1`n", ('A' * 64), ('0' * 64))) {
        Reset-PreflightFixture
        $script:Pins[$key] = $bad
        Assert-PreflightRejected { Invoke-QualificationCiPreflight $script:Context $script:Pins } "noncanonical pin $key"
        Assert-PreflightTest ($script:Requests.Count -eq 0) 'bad pin denied before network'
    }
}
foreach ($key in @('artifact_sha256', 'authorization_sha256', 'performance_contract_sha256', 'source_revision', 'artifact_id', 'producer_run_id', 'producer_run_attempt')) {
    Reset-PreflightFixture
    $script:Approval[$key] = if ($key -ceq 'source_revision') { 'f' * 40 } elseif ($key.EndsWith('_sha256')) { 'f' * 64 } else { '909' }
    Sync-PreflightPolicy
    Assert-PreflightRejected { Invoke-QualificationCiPreflight $script:Context $script:Pins } "unapproved pin $key" 'independent policy'
}
foreach ($case in @('empty', 'unknown_field', 'bad_namespace', 'float_schema', 'duplicate_nonce', 'duplicate_artifact', 'unordered', 'unknown_entry', 'invalid_expiry', 'future', 'expired')) {
    Reset-PreflightFixture
    switch ($case) {
        empty { $script:Policy.approved_campaigns = @() }
        unknown_field { $script:Policy.fixture = $true }
        bad_namespace { $script:Policy.kind = 'windows_gpu_performance_campaign_authority' }
        float_schema { $script:Policy.schema_version = [double]1.5 }
        duplicate_nonce { $script:Policy.approved_campaigns = @($script:Approval, (Copy-PreflightFixture $script:Approval)) }
        duplicate_artifact { $second = Copy-PreflightFixture $script:Approval; $second.campaign_nonce = '2' * 64; $second.producer_run_id = '102'; $script:Policy.approved_campaigns += $second }
        unordered { $second = Copy-PreflightFixture $script:Approval; $second.campaign_nonce = '2' * 64; $second.artifact_id = '203'; $second.producer_run_id = '102'; $script:Policy.approved_campaigns = @($second, $script:Approval) }
        unknown_entry { $script:Approval.fixture = $true }
        invalid_expiry { $script:Approval.expires_at_unix_seconds = $script:Approval.issued_at_unix_seconds + 604801 }
        future { $script:Approval.issued_at_unix_seconds = $script:FixtureNow + 1 }
        expired { $script:Approval.expires_at_unix_seconds = $script:FixtureNow }
    }
    Sync-PreflightPolicy
    Assert-PreflightRejected { Invoke-QualificationCiPreflight $script:Context $script:Pins } "policy $case"
}
foreach ($case in @(
    @{ Key = 'id'; Value = [int64]102 }, @{ Key = 'run_attempt'; Value = 3 },
    @{ Key = 'run_attempt'; Value = '2' }, @{ Key = 'head_sha'; Value = ('c' * 40) },
    @{ Key = 'path'; Value = '.github/workflows/windows-gpu-qualification.yml' },
    @{ Key = 'event'; Value = 'pull_request_target' }, @{ Key = 'head_branch'; Value = 'feature' },
    @{ Key = 'status'; Value = 'in_progress' }, @{ Key = 'conclusion'; Value = 'failure' },
    @{ Key = 'head_repository'; Value = @{ id = [int64]1273587431; full_name = 'attacker/scribe' } },
    @{ Key = 'repository'; Value = @{ id = [int64]123; full_name = 'tyhuang9/scribe' } }
)) {
    Reset-PreflightFixture
    $script:Run[$case.Key] = $case.Value
    Assert-PreflightRejected { Invoke-QualificationCiPreflight $script:Context $script:Pins } "run $($case.Key)"
}
foreach ($case in @(
    @{ Key = 'id'; Value = [int64]203 }, @{ Key = 'name'; Value = 'windows-gpu-performance-capture-101-1' },
    @{ Key = 'expired'; Value = $true }, @{ Key = 'expired'; Value = 'false' },
    @{ Key = 'digest'; Value = ('sha256:' + ('f' * 64)) }, @{ Key = 'digest'; Value = ('SHA256:' + ('c' * 64)) },
    @{ Key = 'size_in_bytes'; Value = 0 }, @{ Key = 'size_in_bytes'; Value = [int64]536870913 },
    @{ Key = 'size_in_bytes'; Value = '1024' },
    @{ Key = 'created_at'; Value = (Get-PreflightFixtureTime -301) },
    @{ Key = 'created_at'; Value = (Get-PreflightFixtureTime 1) },
    @{ Key = 'expires_at'; Value = (Get-PreflightFixtureTime 0) },
    @{ Key = 'workflow_run'; Value = @{ id = [int64]101; repository_id = [int64]1273587431; head_repository_id = [int64]9; head_sha = ('a' * 40); head_branch = 'main' } }
)) {
    Reset-PreflightFixture
    $script:Artifact[$case.Key] = $case.Value
    Assert-PreflightRejected { Invoke-QualificationCiPreflight $script:Context $script:Pins } "artifact $($case.Key)"
}
foreach ($case in @('wrong_main', 'nonancestor', 'wrong_workflow_bytes', 'wrong_content_path', 'noncanonical_policy', 'missing_field')) {
    Reset-PreflightFixture
    switch ($case) {
        wrong_main { $script:Responses[$script:MainPath].object.sha = 'c' * 40 }
        nonancestor { $script:Responses[$script:ComparePath].merge_base_commit.sha = 'c' * 40 }
        wrong_workflow_bytes { $script:Responses[$script:WorkflowPath] = New-PreflightContent $script:QualificationCiProducer ([Text.Encoding]::UTF8.GetBytes('different bytes')) }
        wrong_content_path { $script:Responses[$script:WorkflowPath].path = 'scripts/untrusted.ps1' }
        noncanonical_policy { $bytes = [Text.Encoding]::UTF8.GetBytes(($script:Policy | ConvertTo-Json -Depth 32)); $script:Responses[$script:PolicyPath] = New-PreflightContent $script:QualificationCiPolicyPath $bytes }
        missing_field { $script:Run.Remove('status') }
    }
    Assert-PreflightRejected { Invoke-QualificationCiPreflight $script:Context $script:Pins } $case
}
foreach ($case in @('rerun', 'artifact_changed', 'policy_changed', 'main_changed')) {
    Reset-PreflightFixture
    switch ($case) {
        rerun { $value = Copy-PreflightFixture $script:Run; $value.run_attempt = 3; $path = $script:LatestPath; $at = 2 }
        artifact_changed { $value = Copy-PreflightFixture $script:Artifact; $value.size_in_bytes = 1025; $path = $script:ArtifactPath; $at = 2 }
        policy_changed { $script:Approval.producer_workflow_sha256 = 'f' * 64; $bytes = $script:QualificationCiUtf8.GetBytes((Get-QualificationCiCanonicalJson $script:Policy)); $value = New-PreflightContent $script:QualificationCiPolicyPath $bytes; $path = $script:PolicyPath; $at = 2 }
        main_changed { $value = Copy-PreflightFixture $script:Responses[$script:MainPath]; $value.object.sha = 'c' * 40; $path = $script:MainPath; $at = 3 }
    }
    $script:ChangedResponses[$path] = @{ At = $at; Value = $value }
    Assert-PreflightRejected { Invoke-QualificationCiPreflight $script:Context $script:Pins } "final recheck $case"
}
Reset-PreflightFixture
$script:ExpireOnFinalClock = $true
Assert-PreflightRejected { Invoke-QualificationCiPreflight $script:Context $script:Pins } 'expiry at final observation' 'expired during preflight'

foreach ($json in @('{"x":1,"x":2}', '{"x":1,"X":2}', '{"nested":[{"x":1,"x":2}]}', '{"x":1,}', '{/*comment*/"x":1}', '[]')) {
    Assert-PreflightRejected { ConvertFrom-QualificationCiJson ([Text.Encoding]::UTF8.GetBytes($json)) } 'strict JSON'
}
Assert-PreflightRejected { ConvertFrom-QualificationCiJson ([byte[]]@(123, 34, 120, 34, 58, 34, 255, 34, 125)) } 'invalid UTF-8'
Assert-PreflightRejected { ConvertFrom-QualificationCiJson ([byte[]]::new(4194305)) } 'oversized JSON'
foreach ($path in @('/repos/attacker/scribe/actions/runs/101', '/repos/tyhuang9/scribe/actions/artifacts/202/zip', '/repos/tyhuang9/scribe/../secrets', '/repos/tyhuang9/scribe/actions/runs/01', '/repos/tyhuang9/scribe/git/ref/heads/main?x=1', 'https://attacker.invalid/', "/repos/tyhuang9/scribe/actions/runs/101`n")) {
    Assert-PreflightRejected { Assert-QualificationCiApiPath $path } 'unsafe API path'
}

# Execute the checked-in entrypoint body and workflow block, not copies of their
# forwarding logic. Only process-local, non-secret environment fields change.
$root = Split-Path -Parent $PSScriptRoot
$implementationPath = Join-Path $PSScriptRoot 'invoke-windows-gpu-qualification-preflight.ps1'
$workflowText = [IO.File]::ReadAllText((Join-Path $root '.github/workflows/windows-gpu-qualification-preflight.yml'))
$runBlocks = [regex]::Matches($workflowText, '(?m)^        run: \|\r?\n(?<body>(?:          [^\r\n]*(?:\r?\n|$))+)')
Assert-PreflightTest ($runBlocks.Count -eq 1) 'one actual protected preflight run block'
$workflowBody = [scriptblock]::Create(($runBlocks[0].Groups['body'].Value -replace '(?m)^          ', ''))
$tokens = $null; $parseErrors = $null
$implementationAst = [Management.Automation.Language.Parser]::ParseFile($implementationPath, [ref]$tokens, [ref]$parseErrors)
$entrypoints = @($implementationAst.FindAll({ param($node)
    $node -is [Management.Automation.Language.IfStatementAst] -and $node.Clauses[0].Item1.Extent.Text -ceq '-not $FunctionsOnly'
}, $true))
Assert-PreflightTest ($parseErrors.Count -eq 0 -and $entrypoints.Count -eq 1) 'one actual guarded production entrypoint'
$entryText = $entrypoints[0].Clauses[0].Item2.Extent.Text
$entryBody = [scriptblock]::Create($entryText.Substring(1, $entryText.Length - 2))
$testEnv = @{
    GITHUB_REPOSITORY = 'tyhuang9/scribe'; GITHUB_REPOSITORY_ID = '1273587431'
    GITHUB_REF = 'refs/heads/main'; GITHUB_EVENT_NAME = 'workflow_dispatch'
    GITHUB_SHA = 'b' * 40; GITHUB_WORKFLOW_SHA = 'b' * 40
    GITHUB_WORKFLOW_REF = 'tyhuang9/scribe/.github/workflows/windows-gpu-qualification-preflight.yml@refs/heads/main'
    PRODUCER_RUN_ID = '101'; PRODUCER_RUN_ATTEMPT = '2'; ARTIFACT_ID = '202'
    REVIEWED_SOURCE_REVISION = 'a' * 40; REVIEWED_ARTIFACT_SHA256 = 'c' * 64
    REVIEWED_CONTRACT_SHA256 = 'e' * 64; REVIEWED_CAMPAIGN_NONCE = '1' * 64
    REVIEWED_AUTHORIZATION_SHA256 = 'd' * 64
}
$savedEnv = @{}
foreach ($name in $testEnv.Keys) {
    $savedEnv[$name] = @{ Present = Test-Path -LiteralPath "Env:$name"; Value = [Environment]::GetEnvironmentVariable($name) }
}
try {
    foreach ($name in $testEnv.Keys) { [Environment]::SetEnvironmentVariable($name, $testEnv[$name]) }
    Reset-PreflightFixture
    $entryReceipt = & {
        $ProducerSourceRevision = $script:Pins.source_revision
        $ProducerRunId = $script:Pins.producer_run_id; $ProducerRunAttempt = $script:Pins.producer_run_attempt
        $ArtifactId = $script:Pins.artifact_id; $ExpectedArtifactSha256 = $script:Pins.artifact_sha256
        $ExpectedPerformanceContractSha256 = $script:Pins.performance_contract_sha256
        $ExpectedCampaignNonce = $script:Pins.campaign_nonce; $ExpectedAuthorizationSha256 = $script:Pins.authorization_sha256
        $writer = [IO.StringWriter]::new([Globalization.CultureInfo]::InvariantCulture)
        $savedConsole = [Console]::Out
        try { [Console]::SetOut($writer); . $entryBody; return $writer.ToString() }
        finally { [Console]::SetOut($savedConsole); $writer.Dispose() }
    }
    Assert-PreflightTest ($entryReceipt -ceq (Get-QualificationCiCanonicalJson $decision)) 'actual entrypoint emits exact one-JSON receipt'
    Assert-PreflightTest ($script:Requests.Count -eq 14) 'entrypoint executes real provenance, not an uncalled validator'
    $expectedArguments = @('-NoProfile', '-File', '.\scripts\invoke-windows-gpu-qualification-preflight.ps1',
        '-ProducerSourceRevision', ('a' * 40), '-ProducerRunId', '101', '-ProducerRunAttempt', '2',
        '-ArtifactId', '202', '-ExpectedArtifactSha256', ('c' * 64),
        '-ExpectedPerformanceContractSha256', ('e' * 64), '-ExpectedCampaignNonce', ('1' * 64),
        '-ExpectedAuthorizationSha256', ('d' * 64))
    foreach ($case in @('success', 'wrong_checkout', 'git_failure', 'child_failure')) {
        $observed = & {
            $state = @{ Calls = 0; GitCalls = 0; Failure = $null; Case = $case }
            function git {
                $state.GitCalls++
                Assert-PreflightTest ((@($args) -join '|') -ceq 'rev-parse|--verify|HEAD') 'workflow verifies exact checkout'
                $global:LASTEXITCODE = if ($state.Case -ceq 'git_failure') { 128 } else { 0 }
                if ($state.Case -ceq 'wrong_checkout') { return 'c' * 40 }
                return 'b' * 40
            }
            function pwsh {
                $state.Calls++
                Assert-PreflightTest ((@($args) -join "`0") -ceq ($expectedArguments -join "`0")) 'actual workflow forwards all eight exact pins and script path'
                $global:LASTEXITCODE = if ($state.Case -ceq 'child_failure') { 1 } else { 0 }
                return $entryReceipt
            }
            $previousExit = Get-Variable LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue
            $hadExit = $null -ne $previousExit
            $savedExit = if ($hadExit) { $previousExit.Value } else { $null }
            $output = [Collections.Generic.List[object]]::new()
            try {
                try { & $workflowBody | ForEach-Object { $output.Add($_) } } catch { $state.Failure = $_.Exception.Message }
                return @{ State = $state; Output = @($output.ToArray()) }
            }
            finally {
                if ($hadExit) { $global:LASTEXITCODE = $savedExit }
                else { Remove-Variable LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue }
            }
        }
        Assert-PreflightTest ($observed.State.GitCalls -eq 1) 'one checkout verification per workflow'
        if ($case -ceq 'success') {
            Assert-PreflightTest ($null -eq $observed.State.Failure -and $observed.State.Calls -eq 1 -and $observed.Output.Count -eq 1 -and $observed.Output[0] -ceq $entryReceipt) 'successful actual workflow forwards exact receipt'
        }
        else {
            $expectedCalls = if ($case -ceq 'child_failure') { 1 } else { 0 }
            Assert-PreflightTest ($null -ne $observed.State.Failure -and $observed.State.Calls -eq $expectedCalls -and $observed.Output.Count -eq 0) 'failed workflow starts no unauthorized child and emits no receipt'
        }
    }
}
finally {
    foreach ($name in $savedEnv.Keys) {
        if ($savedEnv[$name].Present) { [Environment]::SetEnvironmentVariable($name, $savedEnv[$name].Value) }
        elseif (Test-Path -LiteralPath "Env:$name") { Remove-Item -LiteralPath "Env:$name" }
        Assert-PreflightTest ((Test-Path -LiteralPath "Env:$name") -eq $savedEnv[$name].Present -and [Environment]::GetEnvironmentVariable($name) -ceq $savedEnv[$name].Value) 'fixture restores exact environment presence and value'
    }
}

# Exercise the real HTTP/parser boundary using HttpClient's in-memory handler.
# Never instantiate the production client or inspect GH_TOKEN.
if ($null -eq ('ScribeQualificationCiTests.Handler' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Net;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;
namespace ScribeQualificationCiTests {
    public sealed class Handler : HttpMessageHandler {
        public static byte[] Bytes;
        public static int Status;
        public static bool Fail;
        public static string Uri;
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token) {
            Uri = request.RequestUri.AbsoluteUri;
            if (request.Method != HttpMethod.Get || Fail) throw new HttpRequestException("fixture-sensitive-message");
            return Task.FromResult(new HttpResponseMessage((HttpStatusCode)Status) { Content = new ByteArrayContent(Bytes) });
        }
    }
}
'@
}
function New-QualificationCiHttpClient {
    $client = [Net.Http.HttpClient]::new([ScribeQualificationCiTests.Handler]::new())
    $client.MaxResponseContentBufferSize = 4194304
    return $client
}
[ScribeQualificationCiTests.Handler]::Bytes = [Text.Encoding]::UTF8.GetBytes('{"ok":true}')
[ScribeQualificationCiTests.Handler]::Status = 200
[ScribeQualificationCiTests.Handler]::Fail = $false
$result = & $transport '/repos/tyhuang9/scribe/actions/runs/101'
Assert-PreflightTest ($result.ok -eq $true -and [ScribeQualificationCiTests.Handler]::Uri -ceq 'https://api.github.com/repos/tyhuang9/scribe/actions/runs/101') 'real GET/parser with fixed HTTPS host'
foreach ($status in @(301, 302, 403, 404, 429, 500)) {
    [ScribeQualificationCiTests.Handler]::Status = $status
    Assert-PreflightRejected { & $transport '/repos/tyhuang9/scribe/actions/runs/101' } 'HTTP failure/redirect' 'could not be authenticated'
}
[ScribeQualificationCiTests.Handler]::Status = 200
[ScribeQualificationCiTests.Handler]::Fail = $true
$privateFailure = $null
$privateOutput = [Collections.Generic.List[object]]::new()
try { & $transport '/repos/tyhuang9/scribe/actions/runs/101' | ForEach-Object { $privateOutput.Add($_) } }
catch { $privateFailure = $_.Exception.Message }
Assert-PreflightTest ($privateFailure -ceq 'GPU qualification preflight rejected: GitHub provenance could not be authenticated.' -and $privateFailure -notmatch 'fixture-sensitive-message' -and $privateOutput.Count -eq 0) 'transport error is exactly sanitized and emits no success output'
[ScribeQualificationCiTests.Handler]::Fail = $false
[ScribeQualificationCiTests.Handler]::Bytes = [Text.Encoding]::UTF8.GetBytes('{"private-diagnostic":"fixture-sensitive-message",}')
$privateFailure = $null; $privateOutput.Clear()
try { & $transport '/repos/tyhuang9/scribe/actions/runs/101' | ForEach-Object { $privateOutput.Add($_) } }
catch { $privateFailure = $_.Exception.Message }
Assert-PreflightTest ($privateFailure -ceq 'GPU qualification preflight rejected: GitHub provenance could not be authenticated.' -and $privateOutput.Count -eq 0) 'malformed HTTP-200 JSON stays private and emits no receipt'
[ScribeQualificationCiTests.Handler]::Bytes = [byte[]]::new(4194305)
Assert-PreflightRejected { & $transport '/repos/tyhuang9/scribe/actions/runs/101' } 'HTTP buffer bound' 'could not be authenticated'
[ScribeQualificationCiTests.Handler]::Bytes = $null

$root = Split-Path -Parent $PSScriptRoot
$checkedPolicy = ConvertFrom-QualificationCiJson ([IO.File]::ReadAllBytes((Join-Path $root 'runtime-manifests/windows-gpu-qualification-ci-policy.json')))
Assert-PreflightTest ($checkedPolicy.approved_campaigns.Count -eq 0) 'checked-in policy remains empty'
Reset-PreflightFixture
$script:Policy = $checkedPolicy
Sync-PreflightPolicy
Assert-PreflightRejected { Invoke-QualificationCiPreflight $script:Context $script:Pins } 'real empty policy rejects production' 'no independent reviewed approval'
Assert-PreflightTest ($script:Requests.Count -eq 2) 'empty policy stops before producer/artifact access'
foreach ($file in @('gpu-auto-qualification-windows-x64.json', 'windows-gpu-performance-authority.json', 'windows-gpu-qualification-production-authority.json')) {
    $document = ConvertFrom-QualificationCiJson ([IO.File]::ReadAllBytes((Join-Path $root "runtime-manifests/$file")))
    $count = if ($file.StartsWith('gpu-auto')) { $document.entries.Count } elseif ($file.Contains('performance')) { $document.keys.Count } else { $document.approved_plans.Count }
    Assert-PreflightTest ($count -eq 0) "existing authority stays empty: $file"
}
$workflow = [IO.File]::ReadAllText((Join-Path $root '.github/workflows/windows-gpu-qualification-preflight.yml'))
Assert-PreflightTest ($workflow.Contains("if: github.event_name == 'workflow_dispatch' && github.repository == 'tyhuang9/scribe' && github.ref == 'refs/heads/main'")) 'manual fixed-main guard'
Assert-PreflightTest ($workflow.Contains('environment: windows-gpu-qualification-preflight') -and $workflow.Contains('needs: contract')) 'separate reviewed environment and contracts'
Assert-PreflightTest ($workflow -notmatch 'secrets\.|id-token:|: write|self-hosted|workflow_run:|pull_request_target:|upload-artifact|download-artifact') 'no keys, GPU runners, privileged events, writes or artifact downloads'
Assert-PreflightTest ($workflow.Contains('run: pwsh -NoProfile -File .\scripts\test-windows-gpu-qualification-preflight.ps1')) 'CI calls the canonical local test command'
Assert-PreflightTest ($workflow.Contains('REVIEWED_CAMPAIGN_NONCE: ${{ vars.SCRIBE_GPU_CAPTURE_CAMPAIGN_NONCE }}') -and $workflow.Contains('REVIEWED_AUTHORIZATION_SHA256: ${{ vars.SCRIBE_GPU_CAPTURE_AUTHORIZATION_SHA256 }}')) 'independent reviewed pins are not dispatch inputs'
$implementation = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'invoke-windows-gpu-qualification-preflight.ps1'))
Assert-PreflightTest ($implementation.Contains('$handler.AllowAutoRedirect = $false') -and $implementation.Contains('$handler.UseCookies = $false') -and $implementation.Contains('$client.Timeout = [TimeSpan]::FromSeconds(30)')) 'HTTP factory restricts redirects, cookies and time'
Assert-PreflightTest ($implementation -notmatch 'Start-Process|ProcessStartInfo|Invoke-Expression|Set-Content|WriteAll|CreateNew|SigningGitHub|FixtureNow|AllowFixture') 'no candidate execution, filesystem output, pack-signing coupling or fixture clock argument'
$tokens = $null; $parseErrors = $null
$null = [Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot 'invoke-windows-gpu-qualification-preflight.ps1'), [ref]$tokens, [ref]$parseErrors)
Assert-PreflightTest ($parseErrors.Count -eq 0) 'production script parses'
Write-Output "Windows GPU qualification preflight tests passed ($script:PreflightTestCount assertions); mocked HTTP/clock only, no GPU, keys, network or persistent state."
