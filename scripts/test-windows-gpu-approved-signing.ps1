[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'invoke-windows-gpu-approved-signing.ps1') -FunctionsOnly
$script:SigningTestCount = 0

function Assert-Test([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:SigningTestCount++
}

function Assert-Rejected([string]$Label, [scriptblock]$Action) {
    $rejected = $false
    try { & $Action | Out-Null } catch { $rejected = $true }
    Assert-Test $rejected "Approved signing accepted invalid $Label."
}

function Copy-TestValue($Value) {
    return ($Value | ConvertTo-Json -Depth 16 -Compress | ConvertFrom-Json -AsHashtable)
}

$revision = 'a' * 40
$headRevision = 'b' * 40
$digest = 'c' * 64
$run = @{
    id = 101; run_attempt = 2
    repository = @{ full_name = 'tyhuang9/scribe' }
    head_repository = @{ full_name = 'tyhuang9/scribe' }
    path = '.github/workflows/windows-gpu-pack-promotion.yml'
    event = 'workflow_dispatch'; head_branch = 'main'
    status = 'completed'; conclusion = 'success'; head_sha = $revision
}
$artifact = @{
    id = 202; workflow_run = @{ id = 101; head_sha = $revision; head_branch = 'main' }
    name = 'windows-gpu-unsigned-101'; expired = $false
    digest = "sha256:$digest"; size_in_bytes = 1024
}
Assert-SigningRunMetadata $run '101' '2' $script:ProducerWorkflow $revision
Assert-SigningArtifactMetadata $artifact '202' '101' $revision 'windows-gpu-unsigned-101' $digest
$script:SigningTestCount += 2

foreach ($case in @(
    @{ Name = 'id'; Value = 102 }, @{ Name = 'run_attempt'; Value = 1 },
    @{ Name = 'path'; Value = '.github/workflows/other.yml' },
    @{ Name = 'event'; Value = 'pull_request' }, @{ Name = 'head_branch'; Value = 'feature' },
    @{ Name = 'status'; Value = 'in_progress' }, @{ Name = 'conclusion'; Value = 'failure' },
    @{ Name = 'head_sha'; Value = $headRevision },
    @{ Name = 'repository'; Value = @{ full_name = 'attacker/scribe' } },
    @{ Name = 'head_repository'; Value = @{ full_name = 'attacker/scribe' } }
)) {
    $bad = Copy-TestValue $run
    $bad[$case.Name] = $case.Value
    Assert-Rejected "producer $($case.Name)" { Assert-SigningRunMetadata $bad '101' '2' $script:ProducerWorkflow $revision }
}
foreach ($case in @(
    @{ Name = 'id'; Value = 203 }, @{ Name = 'name'; Value = 'other-artifact' },
    @{ Name = 'expired'; Value = $true }, @{ Name = 'expired'; Value = 'false' },
    @{ Name = 'digest'; Value = "sha256:$('d' * 64)" },
    @{ Name = 'digest'; Value = $digest },
    @{ Name = 'size_in_bytes'; Value = 0 }, @{ Name = 'size_in_bytes'; Value = 4294967297 },
    @{ Name = 'workflow_run'; Value = @{ id = 102; head_sha = $revision; head_branch = 'main' } },
    @{ Name = 'workflow_run'; Value = @{ id = 101; head_sha = $headRevision; head_branch = 'main' } },
    @{ Name = 'workflow_run'; Value = @{ id = 101; head_sha = $revision; head_branch = 'other' } }
)) {
    $bad = Copy-TestValue $artifact
    $bad[$case.Name] = $case.Value
    Assert-Rejected "artifact $($case.Name)" { Assert-SigningArtifactMetadata $bad '202' '101' $revision 'windows-gpu-unsigned-101' $digest }
}
foreach ($badId in @('', '0', '01', '-1', '1/attempts/2', "1`n", ('1' * 21))) {
    Assert-Rejected 'noncanonical ID' { Assert-SigningId $badId 'test' }
}
foreach ($badHash in @('', ('A' * 64), ('0' * 63), (('0' * 64) + "`n"))) {
    Assert-Rejected 'noncanonical digest' { Assert-SigningHash $badHash 'test' }
}
foreach ($json in @('{"x":1,"x":2}', '{"x":1,"X":2}', '{"nested":{"x":1,"x":2}}', '{"x":1,}', '{/*comment*/"x":1}')) {
    Assert-Rejected 'ambiguous JSON' { ConvertFrom-SigningJson ([Text.Encoding]::UTF8.GetBytes($json)) }
}
$decoded = ConvertFrom-SigningJson ([Text.Encoding]::UTF8.GetBytes('{"x":1,"nested":[{"y":true}]}'))
Assert-Test ($decoded.x -eq 1 -and $decoded.nested[0].y -eq $true) 'Valid JSON was not decoded.'

$pins = [ordered]@{
    SOURCE_SHA = $revision; RUN_ID = '301'; RUN_ATTEMPT = '1'; ARTIFACT_ID = '302'
    ARTIFACT_SHA256 = $digest; BINARY_SHA256 = ('d' * 64); WRAPPER_SHA256 = ('e' * 64)
}
$pinsDigest = Get-SigningPinsHash $pins
Assert-SigningPinsUnchanged $pins $pinsDigest
$script:SigningTestCount++
foreach ($name in @($pins.Keys)) {
    $changedPins = Copy-TestValue $pins
    $changedPins[$name] = 'changed-pin'
    Assert-Rejected "signer $name changed across approval" { Assert-SigningPinsUnchanged $changedPins $pinsDigest }
}

# Replace the sole HTTP seam. These tests cannot contact GitHub or use credentials.
$script:SigningResponses = @{}
function Invoke-SigningGitHubGet([string]$Path) {
    if (-not $script:SigningResponses.ContainsKey($Path)) { throw "Unexpected fixture request: $Path" }
    return Copy-TestValue $script:SigningResponses[$Path]
}
$prefix = '/repos/tyhuang9/scribe'
$script:SigningResponses["$prefix/actions/runs/101/attempts/2"] = $run
$script:SigningResponses["$prefix/actions/runs/101"] = $run
$script:SigningResponses["$prefix/actions/artifacts/202"] = $artifact
$producer = Get-SigningProducer '101' '2' '202' $digest
Assert-Test ($producer.Run.head_sha -ceq $revision) 'Producer round trip lost the exact revision.'
$rerun = Copy-TestValue $run
$rerun.run_attempt = 3
$script:SigningResponses["$prefix/actions/runs/101"] = $rerun
Assert-Rejected 'successful stale attempt after rerun' { Get-SigningProducer '101' '2' '202' $digest }
$script:SigningResponses["$prefix/actions/runs/101"] = $run

$policyBytes = [IO.File]::ReadAllBytes((Join-Path (Split-Path -Parent $PSScriptRoot) $script:PolicyPath))
$policyDigest = Get-SigningHash $policyBytes
$checkedPolicyBytes = Read-SigningFile (Join-Path (Split-Path -Parent $PSScriptRoot) $script:PolicyPath) 65536
Assert-Test ((Get-SigningHash $checkedPolicyBytes) -ceq $policyDigest) 'Physical policy read changed its bytes.'
Assert-Rejected 'oversized local policy' { Read-SigningFile (Join-Path (Split-Path -Parent $PSScriptRoot) $script:PolicyPath) 1 }
$policyContent = @{
    type = 'file'; path = $script:PolicyPath; encoding = 'base64'
    size = $policyBytes.Length; content = [Convert]::ToBase64String($policyBytes)
}
$script:SigningResponses["$prefix/git/ref/heads/main"] = @{ object = @{ sha = $headRevision } }
$comparePath = "$prefix/compare/$revision...$headRevision"
$script:SigningResponses[$comparePath] = @{ merge_base_commit = @{ sha = $revision }; status = 'ahead' }
$contentPath = "$prefix/contents/$script:PolicyPath`?ref=$headRevision"
$script:SigningResponses[$contentPath] = $policyContent
$current = Get-CurrentSigningPolicy $revision $policyDigest
Assert-Test ($current.Revision -ceq $headRevision -and $current.Digest -ceq $policyDigest) 'Policy did not bind its exact default-branch revision and bytes.'
Assert-Rejected 'changed policy after approval' { Get-CurrentSigningPolicy $revision ('e' * 64) }
$script:SigningResponses[$comparePath] = @{ merge_base_commit = @{ sha = $headRevision }; status = 'diverged' }
Assert-Rejected 'candidate outside reviewed history' { Get-CurrentSigningPolicy $revision $policyDigest }
$script:SigningResponses[$comparePath] = @{ merge_base_commit = @{ sha = $revision }; status = 'ahead' }
$badPolicy = Copy-TestValue $policyContent
$badPolicy.size = $policyBytes.Length + 1
$script:SigningResponses[$contentPath] = $badPolicy
Assert-Rejected 'policy length mismatch' { Get-CurrentSigningPolicy $revision $policyDigest }

# Exercise parameter binding and invariant formatting without running the builder.
$tokens = $null; $parseErrors = $null
$builder = [Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot 'build-windows-gpu-worker-pack.ps1'), [ref]$tokens, [ref]$parseErrors)
Assert-Test ($parseErrors.Count -eq 0) 'GPU builder does not parse.'
$parameter = @($builder.ParamBlock.Parameters | Where-Object { $_.Name.VariablePath.UserPath -ceq 'SecurityEpoch' })
Assert-Test ($parameter.Count -eq 1) 'Builder epoch parameter is missing or duplicated.'
$probe = [scriptblock]::Create('param(' + $parameter[0].Extent.Text + ') $SecurityEpoch.ToString([Globalization.CultureInfo]::InvariantCulture)')
Assert-Test ((& $probe) -ceq '1') 'Legacy builder default epoch changed.'
Assert-Test ((& $probe -SecurityEpoch 7) -ceq '7') 'Reviewed builder epoch is ignored.'
Assert-Test ((& $probe -SecurityEpoch ([uint64]::MaxValue)) -ceq '18446744073709551615') 'Builder epoch is not a canonical u64.'
Assert-Rejected 'zero builder epoch' { & $probe -SecurityEpoch 0 }
Assert-Rejected 'negative builder epoch' { & $probe -SecurityEpoch -1 }

# Parse every multiline PowerShell run block as code, not only source substrings.
# GitHub still provides the authoritative workflow YAML/schema validation in CI.
foreach ($workflowName in @('windows-gpu-pack-promotion.yml', 'windows-gpu-signer-tool.yml')) {
    $workflowPath = Join-Path (Split-Path -Parent $PSScriptRoot) ".github/workflows/$workflowName"
    $lines = [IO.File]::ReadAllLines($workflowPath)
    $blockCount = 0
    for ($index = 0; $index -lt $lines.Count; $index++) {
        if ($lines[$index] -cnotmatch '^        run: \|$') { continue }
        $block = [Collections.Generic.List[string]]::new()
        for ($next = $index + 1; $next -lt $lines.Count; $next++) {
            if ($lines[$next] -ceq '') { $block.Add(''); continue }
            if (-not $lines[$next].StartsWith('          ', [StringComparison]::Ordinal)) { break }
            $block.Add($lines[$next].Substring(10))
        }
        $workflowTokens = $null; $workflowErrors = $null
        $null = [Management.Automation.Language.Parser]::ParseInput(($block -join "`n"), [ref]$workflowTokens, [ref]$workflowErrors)
        Assert-Test ($workflowErrors.Count -eq 0) "$workflowName contains an invalid PowerShell run block near line $($index + 1)."
        $blockCount++
    }
    $minimumBlocks = if ($workflowName -ceq 'windows-gpu-pack-promotion.yml') { 8 } else { 2 }
    Assert-Test ($blockCount -ge $minimumBlocks) "Expected $workflowName run blocks were not discovered."
}

Assert-Test ($script:SigningTestCount -ge 50) 'Expected approved-signing cases were not discovered.'
Write-Output "Windows GPU approved signing orchestration tests passed ($script:SigningTestCount cases; no network, keys, workers, or persistent fixtures)."
