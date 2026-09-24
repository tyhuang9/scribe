[CmdletBinding()]
param(
    [ValidateSet('Preflight', 'Sign', 'PublicationCheck')][string]$Mode,
    [string]$HandoffRoot,
    [string]$ApprovalRoot,
    [string]$OutputRoot,
    [string]$ProducerRunId,
    [string]$ProducerRunAttempt,
    [string]$ArtifactId,
    [string]$ExpectedApprovalSha256,
    [string]$ExpectedSignerPinsSha256,
    [switch]$FunctionsOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:SigningRepository = 'tyhuang9/scribe'
$script:SigningRef = 'refs/heads/main'
$script:ProducerWorkflow = '.github/workflows/windows-gpu-pack-promotion.yml'
$script:SignerWorkflow = '.github/workflows/windows-gpu-signer-tool.yml'
$script:PolicyPath = 'runtime-manifests/windows-gpu-signing-policy.json'
$script:Utf8 = [Text.UTF8Encoding]::new($false, $true)

function Assert-SigningCondition([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-SigningHash([string]$Value, [string]$Label) {
    Assert-SigningCondition ($Value -cmatch '\A[0-9a-f]{64}\z') "$Label is not a canonical SHA-256."
}

function Assert-SigningId([string]$Value, [string]$Label) {
    Assert-SigningCondition ($Value -cmatch '\A[1-9][0-9]{0,19}\z') "$Label is not a canonical GitHub ID."
}

function Get-SigningHash([byte[]]$Bytes) {
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Bytes)).ToLowerInvariant()
}

function Read-SigningFile([string]$Path, [int64]$MaximumBytes = 262144) {
    $full = [IO.Path]::GetFullPath($Path)
    $cursor = $full
    while ($cursor) {
        $item = Get-Item -LiteralPath $cursor -Force
        Assert-SigningCondition (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Signing inputs cannot traverse reparse points.'
        $parent = Split-Path -Parent $cursor
        if ($parent -eq $cursor) { break }
        $cursor = $parent
    }
    $item = Get-Item -LiteralPath $full -Force
    Assert-SigningCondition (-not $item.PSIsContainer -and -not $item.LinkType -and $item.Length -gt 0 -and $item.Length -le $MaximumBytes) 'Signing input is not a bounded unlinked regular file.'
    $streams = @(Get-Item -LiteralPath $full -Stream * -ErrorAction Stop)
    Assert-SigningCondition ($streams.Count -eq 1 -and $streams[0].Stream -ceq ':$DATA') 'Signing input contains alternate streams.'
    $handle = [IO.FileStream]::new($full, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $bytes = [byte[]]::new([int]$handle.Length)
        $handle.ReadExactly($bytes)
        Assert-SigningCondition ($handle.ReadByte() -eq -1) 'Signing input changed length.'
        return ,$bytes
    }
    finally { $handle.Dispose() }
}

function ConvertFrom-SigningJson([byte[]]$Bytes) {
    # JsonDocument rejects comments/trailing commas; enumerate properties before
    # PowerShell conversion so duplicate fields cannot acquire last-value wins.
    $document = [Text.Json.JsonDocument]::Parse([ReadOnlyMemory[byte]]::new($Bytes))
    function Assert-UniqueJsonProperties([Text.Json.JsonElement]$Element, [int]$Depth) {
        Assert-SigningCondition ($Depth -le 32) 'Signing JSON exceeds the nesting bound.'
        if ($Element.ValueKind -eq [Text.Json.JsonValueKind]::Object) {
            $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
            foreach ($property in $Element.EnumerateObject()) {
                Assert-SigningCondition ($names.Add($property.Name)) 'Signing JSON contains duplicate or case-colliding fields.'
                Assert-UniqueJsonProperties $property.Value ($Depth + 1)
            }
        }
        elseif ($Element.ValueKind -eq [Text.Json.JsonValueKind]::Array) {
            foreach ($entry in $Element.EnumerateArray()) { Assert-UniqueJsonProperties $entry ($Depth + 1) }
        }
    }
    try { Assert-UniqueJsonProperties $document.RootElement 0 }
    finally { $document.Dispose() }
    return ($script:Utf8.GetString($Bytes) | ConvertFrom-Json -AsHashtable -Depth 32)
}

function Invoke-SigningGitHubGet([string]$Path) {
    Assert-SigningCondition ($Path.StartsWith("/repos/$script:SigningRepository/", [StringComparison]::Ordinal)) 'GitHub request escaped the fixed repository.'
    Assert-SigningCondition (-not [string]::IsNullOrWhiteSpace($env:GH_TOKEN)) 'Read-only GitHub token is missing.'
    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $false
    $client = [Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromSeconds(30)
    $client.MaxResponseContentBufferSize = 4194304
    $client.DefaultRequestHeaders.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $env:GH_TOKEN)
    $client.DefaultRequestHeaders.Add('Accept', 'application/vnd.github+json')
    $client.DefaultRequestHeaders.Add('X-GitHub-Api-Version', '2022-11-28')
    $client.DefaultRequestHeaders.Add('User-Agent', 'scribe-approved-gpu-signing')
    try {
        $response = $client.GetAsync("https://api.github.com$Path").GetAwaiter().GetResult()
        try {
            Assert-SigningCondition ([int]$response.StatusCode -eq 200) 'GitHub provenance request failed; no signing authority was used.'
            $bytes = $response.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
            Assert-SigningCondition ($bytes.Length -gt 0 -and $bytes.Length -le 4194304) 'GitHub response exceeds the bound.'
            return ConvertFrom-SigningJson $bytes
        }
        finally { $response.Dispose() }
    }
    finally { $client.Dispose() }
}

function Assert-SigningRunMetadata($Run, [string]$RunId, [string]$Attempt, [string]$Workflow, [string]$ExpectedRevision = '') {
    Assert-SigningId $RunId 'Run ID'
    Assert-SigningId $Attempt 'Run attempt'
    Assert-SigningCondition ([string]$Run.id -ceq $RunId -and [string]$Run.run_attempt -ceq $Attempt) 'GitHub producer run/attempt does not match.'
    Assert-SigningCondition ($Run.repository.full_name -ceq $script:SigningRepository -and $Run.head_repository.full_name -ceq $script:SigningRepository) 'GitHub producer repository does not match.'
    Assert-SigningCondition ($Run.path -ceq $Workflow -and $Run.event -ceq 'workflow_dispatch' -and $Run.head_branch -ceq 'main') 'GitHub producer is not the fixed default-branch workflow.'
    Assert-SigningCondition ($Run.status -ceq 'completed' -and $Run.conclusion -ceq 'success') 'GitHub producer run must have completed successfully.'
    Assert-SigningCondition ($Run.head_sha -cmatch '\A[0-9a-f]{40}\z') 'GitHub source revision is not canonical.'
    if ($ExpectedRevision) { Assert-SigningCondition ($Run.head_sha -ceq $ExpectedRevision) 'GitHub source revision does not match its independent pin.' }
}

function Assert-SigningArtifactMetadata($Artifact, [string]$Id, [string]$RunId, [string]$Revision, [string]$Name, [string]$Digest = '') {
    Assert-SigningId $Id 'Artifact ID'
    Assert-SigningCondition ([string]$Artifact.id -ceq $Id -and [string]$Artifact.workflow_run.id -ceq $RunId) 'GitHub artifact belongs to a different producer.'
    Assert-SigningCondition ($Artifact.workflow_run.head_sha -ceq $Revision -and $Artifact.workflow_run.head_branch -ceq 'main') 'GitHub artifact source identity does not match.'
    Assert-SigningCondition ($Artifact.name -ceq $Name -and $Artifact.expired -is [bool] -and -not $Artifact.expired) 'GitHub artifact is expired or has an unexpected name.'
    Assert-SigningCondition ($Artifact.digest -cmatch '\Asha256:[0-9a-f]{64}\z' -and $Artifact.size_in_bytes -gt 0 -and $Artifact.size_in_bytes -le 4294967296) 'GitHub artifact digest or size is invalid.'
    if ($Digest) { Assert-SigningCondition ($Artifact.digest -ceq "sha256:$Digest") 'GitHub artifact digest does not match the approved digest.' }
}

function Get-SigningProducer([string]$RunId, [string]$Attempt, [string]$Id, [string]$ExpectedDigest = '') {
    Assert-SigningId $RunId 'Producer run ID'; Assert-SigningId $Attempt 'Producer attempt'; Assert-SigningId $Id 'Producer artifact ID'
    $run = Invoke-SigningGitHubGet "/repos/$script:SigningRepository/actions/runs/$RunId/attempts/$Attempt"
    Assert-SigningRunMetadata $run $RunId $Attempt $script:ProducerWorkflow
    # Re-running an unsigned producer requires a new preflight/approval; an
    # older successful attempt must not silently stand in for the current run.
    $latest = Invoke-SigningGitHubGet "/repos/$script:SigningRepository/actions/runs/$RunId"
    Assert-SigningRunMetadata $latest $RunId $Attempt $script:ProducerWorkflow $run.head_sha
    $artifact = Invoke-SigningGitHubGet "/repos/$script:SigningRepository/actions/artifacts/$Id"
    Assert-SigningArtifactMetadata $artifact $Id $RunId $run.head_sha "windows-gpu-unsigned-$RunId" $ExpectedDigest
    return [pscustomobject]@{ Run = $run; Artifact = $artifact }
}

function Get-CurrentSigningPolicy([string]$CandidateRevision, [string]$ExpectedSha256 = '') {
    Assert-SigningCondition ($CandidateRevision -cmatch '\A[0-9a-f]{40}\z') 'Candidate revision is not canonical.'
    $branch = Invoke-SigningGitHubGet "/repos/$script:SigningRepository/git/ref/heads/main"
    $head = [string]$branch.object.sha
    Assert-SigningCondition ($head -cmatch '\A[0-9a-f]{40}\z') 'Current default-branch revision is invalid.'
    $comparison = Invoke-SigningGitHubGet "/repos/$script:SigningRepository/compare/$CandidateRevision...$head"
    Assert-SigningCondition ($comparison.merge_base_commit.sha -ceq $CandidateRevision -and $comparison.status -cin @('ahead', 'identical')) 'Candidate is not part of the current protected default-branch history.'
    $content = Invoke-SigningGitHubGet "/repos/$script:SigningRepository/contents/$script:PolicyPath`?ref=$head"
    Assert-SigningCondition ($content.type -ceq 'file' -and $content.path -ceq $script:PolicyPath -and $content.encoding -ceq 'base64' -and $content.size -gt 0 -and $content.size -le 65536) 'Current signing policy is not a bounded repository file.'
    $bytes = [Convert]::FromBase64String([string]$content.content)
    Assert-SigningCondition ($bytes.Length -eq $content.size) 'Current signing policy length does not match.'
    $digest = Get-SigningHash $bytes
    if ($ExpectedSha256) { Assert-SigningCondition ($digest -ceq $ExpectedSha256) 'Signing policy changed after approval; start a fresh preflight.' }
    $policy = ConvertFrom-SigningJson $bytes
    Assert-SigningCondition ($policy.source_repository -ceq $script:SigningRepository -and $policy.source_ref -ceq $script:SigningRef) 'Signing policy repository binding does not match.'
    return [pscustomobject]@{ Bytes = $bytes; Digest = $digest; Document = $policy; Revision = $head }
}

function Get-ApprovedSignerPins {
    $pins = [ordered]@{}
    foreach ($name in @('SOURCE_SHA', 'RUN_ID', 'RUN_ATTEMPT', 'ARTIFACT_ID', 'ARTIFACT_SHA256', 'BINARY_SHA256', 'WRAPPER_SHA256')) {
        $value = [Environment]::GetEnvironmentVariable("SCRIBE_GPU_SIGNER_$name")
        Assert-SigningCondition (-not [string]::IsNullOrWhiteSpace($value)) "Reviewed signer pin $name is missing."
        $pins[$name] = $value
    }
    Assert-SigningCondition ($pins.SOURCE_SHA -cmatch '\A[0-9a-f]{40}\z') 'Signer source revision is invalid.'
    foreach ($name in @('RUN_ID', 'RUN_ATTEMPT', 'ARTIFACT_ID')) { Assert-SigningId $pins[$name] "Signer $name" }
    foreach ($name in @('ARTIFACT_SHA256', 'BINARY_SHA256', 'WRAPPER_SHA256')) { Assert-SigningHash $pins[$name] "Signer $name" }
    return $pins
}

function Get-SigningPinsHash($Pins) {
    return Get-SigningHash ($script:Utf8.GetBytes(($Pins | ConvertTo-Json -Compress)))
}

function Assert-SigningPinsUnchanged($Pins, [string]$ExpectedHash) {
    Assert-SigningHash $ExpectedHash 'Expected signer pins digest'
    Assert-SigningCondition ((Get-SigningPinsHash $Pins) -ceq $ExpectedHash) 'Signer bundle pins changed after preflight; request a fresh approval.'
}

function Assert-TrustedSignerBundle($Pins) {
    $entries = @(Get-ChildItem -LiteralPath $PSScriptRoot -Force)
    $names = @($entries.Name | Sort-Object -CaseSensitive)
    Assert-SigningCondition ($entries.Count -eq 2 -and ($names -join '|') -ceq 'invoke-windows-gpu-approved-signing.ps1|scribe-worker-pack-tool.exe') 'Trusted signer bundle has an unexpected inventory.'
    $wrapperBytes = Read-SigningFile (Join-Path $PSScriptRoot 'invoke-windows-gpu-approved-signing.ps1') 262144
    Assert-SigningCondition ((Get-SigningHash $wrapperBytes) -ceq $Pins.WRAPPER_SHA256) 'Trusted wrapper digest does not match.'
    $binaryBytes = Read-SigningFile (Join-Path $PSScriptRoot 'scribe-worker-pack-tool.exe') 67108864
    Assert-SigningCondition ((Get-SigningHash $binaryBytes) -ceq $Pins.BINARY_SHA256) 'Trusted signer executable digest does not match.'
    $run = Invoke-SigningGitHubGet "/repos/$script:SigningRepository/actions/runs/$($Pins.RUN_ID)/attempts/$($Pins.RUN_ATTEMPT)"
    Assert-SigningRunMetadata $run $Pins.RUN_ID $Pins.RUN_ATTEMPT $script:SignerWorkflow $Pins.SOURCE_SHA
    $artifact = Invoke-SigningGitHubGet "/repos/$script:SigningRepository/actions/artifacts/$($Pins.ARTIFACT_ID)"
    Assert-SigningArtifactMetadata $artifact $Pins.ARTIFACT_ID $Pins.RUN_ID $Pins.SOURCE_SHA "windows-gpu-signer-$($Pins.SOURCE_SHA)" $Pins.ARTIFACT_SHA256
}

function Invoke-ApprovedSigner([string[]]$Arguments, [byte[]]$PrivateKey = @()) {
    $tool = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot 'scribe-worker-pack-tool.exe'))
    $lock = [IO.FileStream]::new($tool, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $process = $null
    try {
        $digest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($lock)).ToLowerInvariant()
        Assert-SigningCondition ($digest -ceq $env:SCRIBE_GPU_SIGNER_BINARY_SHA256) 'Signer image changed before launch.'
        $info = [Diagnostics.ProcessStartInfo]::new()
        $info.FileName = $tool; $info.WorkingDirectory = $PSScriptRoot; $info.UseShellExecute = $false
        $info.RedirectStandardInput = $true; $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
        $info.Environment.Remove('SCRIBE_GPU_PACK_PRIVATE_KEY_BASE64') | Out-Null
        $info.Environment.Remove('GH_TOKEN') | Out-Null
        foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($info)
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if ($PrivateKey.Length -gt 0) { $process.StandardInput.BaseStream.Write($PrivateKey, 0, $PrivateKey.Length) }
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(120000)) { $process.Kill($true); $process.WaitForExit(); throw 'Approved signer exceeded its two-minute execution bound.' }
        $output = $stdout.GetAwaiter().GetResult(); $errorText = $stderr.GetAwaiter().GetResult()
        # Never relay child diagnostics from a secret-bearing invocation. The
        # trusted tool returns categorical failures; no key bytes reach logs.
        Assert-SigningCondition ($process.ExitCode -eq 0 -and $output.Length -le 262144 -and $errorText.Length -le 262144) 'Approved signer rejected the inputs or exceeded the output bound.'
        $null = ConvertFrom-SigningJson ($script:Utf8.GetBytes($output))
        return $output
    }
    finally { if ($null -ne $process) { $process.Dispose() }; $lock.Dispose() }
}

function Write-NewSigningFile([string]$Path, [byte[]]$Bytes) {
    $stream = [IO.FileStream]::new($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $stream.Write($Bytes); $stream.Flush($true) }
    finally { $stream.Dispose() }
}

function Invoke-WindowsApprovedSigning {
    Assert-SigningCondition ($env:OS -ceq 'Windows_NT' -and [Environment]::Is64BitProcess) 'Approved signing requires Windows x64.'
    Assert-SigningCondition ($env:GITHUB_REPOSITORY -ceq $script:SigningRepository -and $env:GITHUB_REF -ceq $script:SigningRef -and $env:GITHUB_EVENT_NAME -ceq 'workflow_dispatch') 'Approved signing is restricted to the default-branch dispatch.'
    Assert-SigningCondition ($Mode -cin @('Preflight', 'Sign', 'PublicationCheck')) 'Approved signing mode is missing.'
    $pins = Get-ApprovedSignerPins
    if ($Mode -cne 'Preflight') { Assert-SigningPinsUnchanged $pins $ExpectedSignerPinsSha256 }
    Assert-TrustedSignerBundle $pins
    if ($Mode -ceq 'Preflight') {
        Assert-SigningCondition (-not (Test-Path -LiteralPath $ApprovalRoot)) 'Preflight output must be fresh.'
        $producer = Get-SigningProducer $ProducerRunId $ProducerRunAttempt $ArtifactId
        $policy = Get-CurrentSigningPolicy $producer.Run.head_sha
        $handoffBytes = Read-SigningFile (Join-Path $HandoffRoot 'windows-gpu-pack-handoff.json')
        $handoff = ConvertFrom-SigningJson $handoffBytes
        $approval = [ordered]@{
            schema_version = 1; policy_sha256 = $policy.Digest
            signer_source_revision = $pins.SOURCE_SHA; signer_sha256 = $pins.BINARY_SHA256
            source_repository = $script:SigningRepository; source_ref = $script:SigningRef
            source_revision = [string]$producer.Run.head_sha
            workflow_ref = "$script:SigningRepository/$script:ProducerWorkflow@$script:SigningRef"
            run_id = $ProducerRunId; run_attempt = $ProducerRunAttempt; artifact_id = $ArtifactId
            artifact_digest = $producer.Artifact.digest.Substring(7)
            handoff_sha256 = Get-SigningHash $handoffBytes
            release_set_digest = $handoff.release_set_digest; toolchain_manifest_sha256 = $handoff.toolchain_manifest_sha256
            pack_version = $handoff.pack_version; packs = $handoff.packs
        }
        [IO.Directory]::CreateDirectory([IO.Path]::GetFullPath($ApprovalRoot)) | Out-Null
        $approvalPath = Join-Path $ApprovalRoot 'approval.json'; $policyPath = Join-Path $ApprovalRoot 'policy.json'
        $approvalBytes = $script:Utf8.GetBytes(($approval | ConvertTo-Json -Depth 16 -Compress))
        Write-NewSigningFile $approvalPath $approvalBytes
        Write-NewSigningFile $policyPath $policy.Bytes
        $descriptor = Invoke-ApprovedSigner @('inspect-approved-windows-set', '--handoff-root', $HandoffRoot, '--approval', $approvalPath, '--policy', $policyPath)
        if ($env:GITHUB_OUTPUT) {
            Add-Content -LiteralPath $env:GITHUB_OUTPUT -Encoding utf8NoBOM -Value "approval_sha256=$(Get-SigningHash $approvalBytes)"
            Add-Content -LiteralPath $env:GITHUB_OUTPUT -Encoding utf8NoBOM -Value "artifact_digest=$($approval.artifact_digest)"
            Add-Content -LiteralPath $env:GITHUB_OUTPUT -Encoding utf8NoBOM -Value "signer_pins_sha256=$(Get-SigningPinsHash $pins)"
        }
        if ($env:GITHUB_STEP_SUMMARY) {
            Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Encoding utf8NoBOM -Value @(
                '## Windows GPU pair awaiting maintainer approval', '',
                "Source: $($approval.source_revision)", "Signer source: $($pins.SOURCE_SHA)",
                "Signer SHA-256: $($pins.BINARY_SHA256)", "Policy SHA-256: $($policy.Digest)",
                "Complete signer pins SHA-256: $(Get-SigningPinsHash $pins)",
                "Unsigned artifact: $ArtifactId / $($approval.artifact_digest)",
                "Approval SHA-256: $(Get-SigningHash $approvalBytes)", '',
                'Exact manifest and pack digests:', '```json', $descriptor, '```', '',
                'Retries sign only these identical approved inputs. Signing does not enable Auto or publish an installer.'
            )
        }
        return
    }
    Assert-SigningHash $ExpectedApprovalSha256 'Expected approval digest'
    $approvalPath = Join-Path $ApprovalRoot 'approval.json'; $policyPath = Join-Path $ApprovalRoot 'policy.json'
    $approvalBytes = Read-SigningFile $approvalPath
    Assert-SigningCondition ((Get-SigningHash $approvalBytes) -ceq $ExpectedApprovalSha256) 'Approval artifact changed after preflight.'
    $approval = ConvertFrom-SigningJson $approvalBytes
    Assert-SigningCondition ($approval.signer_source_revision -ceq $pins.SOURCE_SHA -and $approval.signer_sha256 -ceq $pins.BINARY_SHA256) 'Protected signer pins differ from the approved tool.'
    $producer = Get-SigningProducer $approval.run_id $approval.run_attempt $approval.artifact_id $approval.artifact_digest
    Assert-SigningCondition ($producer.Run.head_sha -ceq $approval.source_revision) 'Approved source differs from the authenticated producer source.'
    $policy = Get-CurrentSigningPolicy $approval.source_revision $approval.policy_sha256
    Assert-SigningCondition ((Get-SigningHash (Read-SigningFile $policyPath 65536)) -ceq $policy.Digest) 'Approved policy artifact does not match the current policy.'
    if ($Mode -ceq 'PublicationCheck') {
        $receipt = Read-SigningFile (Join-Path $OutputRoot 'windows-gpu-pack-signing-receipt.json')
        $null = ConvertFrom-SigningJson $receipt
        Write-Output "Signed pair receipt SHA-256: $(Get-SigningHash $receipt)"
        return
    }
    $null = Invoke-ApprovedSigner @('inspect-approved-windows-set', '--handoff-root', $HandoffRoot, '--approval', $approvalPath, '--policy', $policyPath)
    $keyBytes = [byte[]]@()
    try {
        $encodedKey = $env:SCRIBE_GPU_PACK_PRIVATE_KEY_BASE64
        $env:SCRIBE_GPU_PACK_PRIVATE_KEY_BASE64 = $null
        Assert-SigningCondition (-not [string]::IsNullOrWhiteSpace($encodedKey) -and $encodedKey.Length -le 8192) 'Protected PKCS8 signing secret is missing or exceeds its bound.'
        try { $keyBytes = [Convert]::FromBase64String($encodedKey) }
        catch { throw 'Protected PKCS8 signing secret is not valid base64.' }
        $encodedKey = $null
        Assert-SigningCondition ($keyBytes.Length -gt 0 -and $keyBytes.Length -le 4096) 'Protected PKCS8 signing secret exceeds its binary bound.'
        $null = Invoke-ApprovedSigner @('sign-approved-windows-set', '--handoff-root', $HandoffRoot, '--approval', $approvalPath, '--policy', $policyPath, '--output-root', $OutputRoot) $keyBytes
    }
    finally { if ($keyBytes.Length -gt 0) { [Security.Cryptography.CryptographicOperations]::ZeroMemory($keyBytes) }; $env:SCRIBE_GPU_PACK_PRIVATE_KEY_BASE64 = $null }
    $null = Get-CurrentSigningPolicy $approval.source_revision $approval.policy_sha256
}

if (-not $FunctionsOnly) { Invoke-WindowsApprovedSigning }
