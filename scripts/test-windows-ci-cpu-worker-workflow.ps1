$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$workflow = Get-Content -LiteralPath (Join-Path (Split-Path -Parent $PSScriptRoot) '.github/workflows/release.yml') -Raw
$checks = 0
function Assert-CiCpuWorkflow([bool]$Condition, [string]$Message) {
    $script:checks++
    if (-not $Condition) { throw "CI CPU workflow: $Message" }
}
function Get-CiCpuStep([string]$Name) {
    $marker = "      - name: $Name"
    $start = $workflow.IndexOf($marker, [StringComparison]::Ordinal)
    if ($start -lt 0) { throw "Missing CI CPU workflow step: $Name" }
    $end = $workflow.IndexOf('      - name:', $start + $marker.Length, [StringComparison]::Ordinal)
    if ($end -lt 0) { $end = $workflow.Length }
    return $workflow.Substring($start, $end - $start)
}
function Get-CiCpuRun([string]$Name) {
    $step = Get-CiCpuStep $Name
    $run = [regex]::Match($step, '(?ms)^        run: \|\r?\n(?<body>(?:          [^\r\n]*(?:\r?\n|$)|\r?\n)+)')
    if (-not $run.Success) { throw "Missing CI CPU workflow run block: $Name" }
    $body = ($run.Groups['body'].Value -split '\r?\n' | ForEach-Object {
        if ($_.Length -ge 10) { $_.Substring(10) } else { $_ }
    }) -join [Environment]::NewLine
    $tokens = $null; $errors = $null
    $null = [Management.Automation.Language.Parser]::ParseInput($body, [ref]$tokens, [ref]$errors)
    Assert-CiCpuWorkflow (@($errors).Count -eq 0) "Actual run block does not parse: $Name"
    return $body
}
function Invoke-CiCpuWorkflowFailure([scriptblock]$Action, [string]$Expected) {
    $failed = $false
    try { & $Action } catch {
        if (-not $_.Exception.Message.Contains($Expected)) { throw }
        $failed = $true
    }
    Assert-CiCpuWorkflow $failed "Expected refusal: $Expected"
}

$policyName = 'Validate optional verified CPU worker request'
$preflightName = 'Authenticate exact verified CPU input'
$downloadName = 'Download authenticated raw CPU worker archive'
$buildName = 'Build validated portable payload'
$uploadName = 'Recheck verified CPU provenance before asset upload'
$publishName = 'Recheck verified CPU provenance before release publication'
$policy = Get-CiCpuRun $policyName
$preflight = Get-CiCpuRun $preflightName
$download = Get-CiCpuRun $downloadName
$build = Get-CiCpuRun $buildName
$upload = Get-CiCpuRun $uploadName
$publish = Get-CiCpuRun $publishName
$inputMap = [ordered]@{
    CPU_WORKER_SOURCE_REVISION = 'cpu_worker_source_revision'
    CPU_WORKER_PRODUCER_RUN_ID = 'cpu_worker_producer_run_id'
    CPU_WORKER_PRODUCER_RUN_ATTEMPT = 'cpu_worker_producer_run_attempt'
    CPU_WORKER_ARTIFACT_ID = 'cpu_worker_artifact_id'
}
foreach ($entry in $inputMap.GetEnumerator()) {
    Assert-CiCpuWorkflow ($workflow.Contains("  $($entry.Key): " + '${{ inputs.' + $entry.Value + ' }}')) 'Dispatch text must enter only through environment bindings.'
}
foreach ($name in @($preflightName, $downloadName, $uploadName)) {
    Assert-CiCpuWorkflow ((Get-CiCpuStep $name).Contains("if: steps.cpu-input-policy.outputs.requested == 'true'")) 'An optional acquisition step lost its request guard.'
}
Assert-CiCpuWorkflow ((Get-CiCpuStep $publishName).Contains("if: needs.build.outputs.cpu_inputs_requested == 'true'")) 'Publication provenance guard lost the build decision.'
Assert-CiCpuWorkflow ($workflow.IndexOf("      - name: $preflightName", [StringComparison]::Ordinal) -lt $workflow.IndexOf("      - name: $downloadName", [StringComparison]::Ordinal)) 'Authentication must precede download.'
Assert-CiCpuWorkflow ($workflow.IndexOf("      - name: $uploadName", [StringComparison]::Ordinal) -lt $workflow.IndexOf('      - name: Upload Windows release assets', [StringComparison]::Ordinal)) 'CPU provenance must be checked before asset upload.'
Assert-CiCpuWorkflow ($workflow.IndexOf("      - name: $publishName", [StringComparison]::Ordinal) -lt $workflow.IndexOf('      - name: Create release with generated notes', [StringComparison]::Ordinal)) 'CPU provenance must be checked before publication.'
$pinMap = [ordered]@{
    EXPECTED_CPU_WORKER_SOURCE_REVISION = 'worker_source_revision'
    EXPECTED_CPU_PRODUCER_RUN_ID = 'producer_run_id'
    EXPECTED_CPU_PRODUCER_RUN_ATTEMPT = 'producer_run_attempt'
    EXPECTED_CPU_ARTIFACT_ID = 'artifact_id'
    EXPECTED_CPU_ARTIFACT_SHA256 = 'artifact_sha256'
}
foreach ($name in @($downloadName, $buildName, $uploadName)) {
    $step = Get-CiCpuStep $name
    foreach ($entry in $pinMap.GetEnumerator()) {
        Assert-CiCpuWorkflow ($step.Contains("$($entry.Key): " + '${{ steps.cpu-input-preflight.outputs.' + $entry.Value + ' }}')) 'A later gate used mutable raw dispatch inputs rather than preflight outputs.'
    }
}
foreach ($entry in $pinMap.GetEnumerator()) {
    $jobOutput = 'cpu_worker_' + $entry.Value
    if ($entry.Value -ceq 'worker_source_revision') { $jobOutput = 'cpu_worker_source_revision' }
    Assert-CiCpuWorkflow ((Get-CiCpuStep $publishName).Contains("$($entry.Key): " + '${{ needs.build.outputs.' + $jobOutput + ' }}')) 'Publication lost the authenticated build outputs.'
    Assert-CiCpuWorkflow ($workflow.Contains("      ${jobOutput}: " + '${{ steps.cpu-input-preflight.outputs.' + $entry.Value + ' }}')) 'Build did not retain an immutable identity output.'
}
Assert-CiCpuWorkflow ($workflow.Contains('cpu_worker_artifact_size_bytes: ${{ steps.cpu-input-preflight.outputs.artifact_size_bytes }}')) 'Build did not retain the artifact size.'
Assert-CiCpuWorkflow ((Get-CiCpuStep $publishName).Contains('EXPECTED_CPU_ARTIFACT_SIZE_BYTES: ${{ needs.build.outputs.cpu_worker_artifact_size_bytes }}')) 'Publication did not bind artifact size.'
Assert-CiCpuWorkflow ((Get-CiCpuStep $uploadName).Contains('EXPECTED_CPU_ARTIFACT_SIZE_BYTES: ${{ steps.cpu-input-preflight.outputs.artifact_size_bytes }}')) 'Upload did not bind artifact size.'

$environmentNames = @($inputMap.Keys) + @($pinMap.Keys) + @(
    'EXPECTED_CPU_ARTIFACT_SIZE_BYTES', 'CPU_INPUTS_REQUESTED', 'GPU_INPUTS_REQUESTED',
    'GITHUB_REPOSITORY', 'GITHUB_EVENT_NAME', 'GITHUB_REF', 'GITHUB_SHA', 'GITHUB_WORKSPACE',
    'GITHUB_OUTPUT', 'GITHUB_ENV', 'RUNNER_TEMP', 'SCRIBE_BUILD_REVISION',
    'EXPECTED_GPU_WORKER_SOURCE_REVISION', 'GPU_SIGNING_RUN_ID', 'GPU_SIGNING_RUN_ATTEMPT',
    'GPU_SIGNED_ARTIFACT_ID', 'EXPECTED_GPU_ARTIFACT_SHA256', 'EXPECTED_GPU_POLICY_SHA256',
    'EXPECTED_GPU_SIGNER_PINS_SHA256'
)
$savedEnvironment = @{}
foreach ($name in $environmentNames) { $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name) }
$stateNames = @('CiCpuWorkflowCalls', 'CiCpuWorkflowReject', 'CiCpuWorkflowDigest', 'CiCpuWorkflowSize')
$savedState = @{}
foreach ($name in $stateNames) {
    $value = Get-Variable -Name $name -Scope Global -ErrorAction SilentlyContinue
    $savedState[$name] = @{ Exists = $null -ne $value; Value = $(if ($value) { $value.Value } else { $null }) }
}
$savedExit = Get-Variable LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue
$testRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-ci-cpu-workflow-$([guid]::NewGuid().ToString('N'))"
$pushed = $false
try {
    $null = New-Item -ItemType Directory -Path (Join-Path $testRoot 'scripts')
    [IO.File]::WriteAllText((Join-Path $testRoot 'Cargo.toml'), 'version = "0.1.0"')
    [IO.File]::WriteAllText((Join-Path $testRoot 'scripts/resolve-windows-cpu-worker-inputs.ps1'), @'
param($Mode, $SourceRevision, $WorkerSourceRevision, $ProducerRunId, $ProducerRunAttempt, $ArtifactId, $ExpectedArtifactSha256, $ArchivePath)
$global:CiCpuWorkflowCalls.Add(@{ Kind = $Mode; Parameters = @{} + $PSBoundParameters })
if ($global:CiCpuWorkflowReject -ceq $Mode) { throw 'Expected resolver refusal.' }
if ($SourceRevision -cne ('d' * 40) -or $WorkerSourceRevision -cne ('e' * 40) -or $ProducerRunId -cne '100' -or $ProducerRunAttempt -cne '2' -or $ArtifactId -cne '300') { throw 'Workflow lost pinned CPU identity.' }
if ($Mode -ceq 'Download') {
    if ($ExpectedArtifactSha256 -cne ('a' * 64) -or $ArchivePath -cne (Join-Path $env:RUNNER_TEMP 'scribe-ci-cpu-worker.zip')) { throw 'Download lost independently pinned digest or fixed raw ZIP destination.' }
} elseif ($PSBoundParameters.ContainsKey('ExpectedArtifactSha256') -or $PSBoundParameters.ContainsKey('ArchivePath')) { throw 'Preflight unexpectedly received resolution input.' }
[pscustomobject]@{ WorkerSourceRevision = 'e' * 40; ProducerRunId = '100'; ProducerRunAttempt = '2'; ArtifactId = '300'; ArtifactSha256 = $global:CiCpuWorkflowDigest; ArtifactSizeBytes = $global:CiCpuWorkflowSize }
'@)
    [IO.File]::WriteAllText((Join-Path $testRoot 'scripts/build-windows-release.ps1'), @'
param($ModelSource, $BundlePath, [string[]]$WorkerPackRoot, $CiCpuWorkerSourceRevision, $CiCpuWorkerProducerRunId, $CiCpuWorkerProducerRunAttempt, $CiCpuWorkerArtifactId, $CiCpuWorkerExpectedArtifactSha256, $CiCpuWorkerArchivePath, $CiCpuWorkerOutputDirectory)
$global:CiCpuWorkflowCalls.Add(@{ Kind = 'Build'; Parameters = @{} + $PSBoundParameters })
if ($global:CiCpuWorkflowReject -ceq 'Build') { throw 'Expected builder refusal.' }
'@)
    [IO.File]::WriteAllText((Join-Path $testRoot 'scripts/resolve-windows-signed-gpu-inputs.ps1'), @'
param($Mode, $SourceRevision, $WorkerSourceRevision, $SigningRunId, $SigningRunAttempt, $SignedArtifactId, $SignedRoot, $VerifierExecutable, $ExpectedArtifactSha256, $ExpectedPolicySha256, $ExpectedSignerPinsSha256)
$global:CiCpuWorkflowCalls.Add(@{ Kind = 'GPU'; Parameters = @{} + $PSBoundParameters })
[pscustomobject]@{ PackRoots = @('verified-cuda', 'verified-vulkan') }
'@)
    function cargo { $global:LASTEXITCODE = 0 }
    $global:CiCpuWorkflowCalls = [Collections.Generic.List[object]]::new()
    $global:CiCpuWorkflowReject = ''
    $global:CiCpuWorkflowDigest = 'a' * 64; $global:CiCpuWorkflowSize = [long]123
    $env:GITHUB_REPOSITORY = 'tyhuang9/scribe'; $env:GITHUB_EVENT_NAME = 'workflow_dispatch'
    $env:GITHUB_REF = 'refs/heads/main'; $env:GITHUB_SHA = 'd' * 40
    $env:GITHUB_WORKSPACE = $testRoot; $env:RUNNER_TEMP = $testRoot
    $env:GITHUB_OUTPUT = Join-Path $testRoot 'output.txt'; $env:GITHUB_ENV = Join-Path $testRoot 'environment.txt'
    $env:GPU_INPUTS_REQUESTED = 'false'
    Push-Location $testRoot; $pushed = $true

    $canonicalValues = @(('e' * 40), '100', '2', '300')
    $inputNames = @($inputMap.Keys)
    for ($mask = 0; $mask -lt 16; $mask++) {
        for ($i = 0; $i -lt 4; $i++) {
            [Environment]::SetEnvironmentVariable($inputNames[$i], $(if ($mask -band (1 -shl $i)) { $canonicalValues[$i] } else { $null }))
        }
        [IO.File]::WriteAllText($env:GITHUB_OUTPUT, '')
        if ($mask -eq 0 -or $mask -eq 15) {
            Invoke-Expression $policy
            $expected = if ($mask -eq 15) { 'true' } else { 'false' }
            Assert-CiCpuWorkflow ((Get-Content $env:GITHUB_OUTPUT -Raw).Trim() -ceq "requested=$expected") 'Actual request policy lost all-or-none behavior.'
        } else { Invoke-CiCpuWorkflowFailure { Invoke-Expression $policy } 'all four' }
    }
    foreach ($name in $inputNames) {
        $original = [Environment]::GetEnvironmentVariable($name)
        foreach ($invalid in @(' ', '0', "1`n2", "$original`n")) {
            [Environment]::SetEnvironmentVariable($name, $invalid)
            Invoke-CiCpuWorkflowFailure { Invoke-Expression $policy } 'canonical'
        }
        [Environment]::SetEnvironmentVariable($name, $original)
    }
    foreach ($entry in @(@('GITHUB_REPOSITORY', 'fork/scribe'), @('GITHUB_EVENT_NAME', 'push'), @('GITHUB_REF', 'refs/heads/feature'))) {
        $original = [Environment]::GetEnvironmentVariable($entry[0])
        [Environment]::SetEnvironmentVariable($entry[0], $entry[1])
        Invoke-CiCpuWorkflowFailure { Invoke-Expression $policy } 'restricted'
        [Environment]::SetEnvironmentVariable($entry[0], $original)
    }
    [IO.File]::WriteAllText($env:GITHUB_OUTPUT, '')
    Invoke-Expression $preflight
    $outputs = @{}
    foreach ($line in Get-Content $env:GITHUB_OUTPUT) { $parts = $line.Split('=', 2); $outputs[$parts[0]] = $parts[1] }
    Assert-CiCpuWorkflow ($outputs.Count -eq 6) 'Preflight failed to emit the complete authenticated tuple, digest and size.'
    foreach ($entry in $pinMap.GetEnumerator()) { [Environment]::SetEnvironmentVariable($entry.Key, $outputs[$entry.Value]) }
    $env:EXPECTED_CPU_ARTIFACT_SIZE_BYTES = $outputs.artifact_size_bytes
    foreach ($name in $inputNames) { [Environment]::SetEnvironmentVariable($name, 'drifted-raw-input') }
    $global:CiCpuWorkflowCalls.Clear()
    Invoke-Expression $download
    Assert-CiCpuWorkflow ($global:CiCpuWorkflowCalls.Count -eq 1 -and $global:CiCpuWorkflowCalls[0].Kind -ceq 'Download') 'Actual download block used an unexpected operation.'
    $global:CiCpuWorkflowReject = 'Download'
    Invoke-CiCpuWorkflowFailure { Invoke-Expression $download } 'Expected resolver refusal'
    $global:CiCpuWorkflowReject = ''
    $env:CPU_INPUTS_REQUESTED = 'true'
    $global:CiCpuWorkflowCalls.Clear()
    Invoke-Expression $build
    Assert-CiCpuWorkflow ($global:CiCpuWorkflowCalls.Count -eq 1 -and $global:CiCpuWorkflowCalls[0].Kind -ceq 'Build') 'CPU reuse workflow must leave resolution to the actual builder.'
    $call = $global:CiCpuWorkflowCalls[0].Parameters
    $expectedArguments = @{
        ModelSource = '.ci-release-inputs\model\whisper-base.en-Q8_0.gguf'; BundlePath = 'dist\portable'
        CiCpuWorkerSourceRevision = 'e' * 40; CiCpuWorkerProducerRunId = '100'; CiCpuWorkerProducerRunAttempt = '2'
        CiCpuWorkerArtifactId = '300'; CiCpuWorkerExpectedArtifactSha256 = 'a' * 64
        CiCpuWorkerArchivePath = Join-Path $testRoot 'scribe-ci-cpu-worker.zip'
        CiCpuWorkerOutputDirectory = Join-Path $testRoot 'scribe-ci-cpu-worker-inputs'
    }
    Assert-CiCpuWorkflow ($call.Count -eq $expectedArguments.Count) 'Builder received an unexpected authority parameter.'
    foreach ($name in $expectedArguments.Keys) { Assert-CiCpuWorkflow ($call[$name] -ceq $expectedArguments[$name]) "Builder lost expected scalar: $name" }
    $global:CiCpuWorkflowReject = 'Build'
    Invoke-CiCpuWorkflowFailure { Invoke-Expression $build } 'Expected builder refusal'
    $global:CiCpuWorkflowReject = ''
    $env:CPU_INPUTS_REQUESTED = 'false'
    $global:CiCpuWorkflowCalls.Clear(); Invoke-Expression $build
    Assert-CiCpuWorkflow ($global:CiCpuWorkflowCalls[0].Parameters.Count -eq 2) 'Default fresh build unexpectedly received CPU reuse input.'
    $env:CPU_INPUTS_REQUESTED = 'true'; $env:GPU_INPUTS_REQUESTED = 'true'
    $global:CiCpuWorkflowCalls.Clear(); Invoke-Expression $build
    Assert-CiCpuWorkflow (($global:CiCpuWorkflowCalls.Kind -join '|') -ceq 'GPU|Build') 'Combined CPU/GPU acquisition changed existing GPU resolution ordering.'
    Assert-CiCpuWorkflow (($global:CiCpuWorkflowCalls[1].Parameters.WorkerPackRoot -join '|') -ceq 'verified-cuda|verified-vulkan') 'CPU reuse dropped the independently verified GPU packs.'
    Assert-CiCpuWorkflow ($global:CiCpuWorkflowCalls[1].Parameters.CiCpuWorkerSourceRevision -ceq ('e' * 40)) 'GPU resolution replaced pinned CPU identity.'
    foreach ($body in @($upload, $publish)) {
        $global:CiCpuWorkflowCalls.Clear(); Invoke-Expression $body
        Assert-CiCpuWorkflow ($global:CiCpuWorkflowCalls.Count -eq 1 -and $global:CiCpuWorkflowCalls[0].Kind -ceq 'Preflight') 'A late gate failed to reauthenticate exact pinned provenance.'
        $global:CiCpuWorkflowDigest = 'b' * 64
        Invoke-CiCpuWorkflowFailure { Invoke-Expression $body } 'bytes changed'
        $global:CiCpuWorkflowDigest = 'a' * 64; $global:CiCpuWorkflowSize = [long]124
        Invoke-CiCpuWorkflowFailure { Invoke-Expression $body } 'bytes changed'
        $global:CiCpuWorkflowSize = [long]123; $global:CiCpuWorkflowReject = 'Preflight'
        Invoke-CiCpuWorkflowFailure { Invoke-Expression $body } 'Expected resolver refusal'
        $global:CiCpuWorkflowReject = ''
    }
    Assert-CiCpuWorkflow ($checks -ge 100) 'Expected executable workflow checks were not discovered.'
    Write-Output "Windows CI CPU workflow tests passed ($checks checks; actual run blocks, offline boundaries)."
}
finally {
    if ($pushed) { Pop-Location }
    foreach ($name in $environmentNames) { [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name]) }
    foreach ($name in $stateNames) {
        if ($savedState[$name].Exists) { Set-Variable -Name $name -Scope Global -Value $savedState[$name].Value }
        else { Remove-Variable -Name $name -Scope Global -ErrorAction SilentlyContinue }
    }
    if ($savedExit) { $global:LASTEXITCODE = $savedExit.Value }
    else { Remove-Variable LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue }
    if (Test-Path -LiteralPath $testRoot) {
        $resolved = [IO.Path]::GetFullPath($testRoot)
        $parent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
        if ((Split-Path -Parent $resolved) -cne $parent -or (Split-Path -Leaf $resolved) -notmatch '^scribe-ci-cpu-workflow-[0-9a-f]{32}$') { throw 'Refused unexpected CI CPU workflow cleanup path.' }
        if (@(Get-ChildItem -LiteralPath $resolved -Force -Recurse | Where-Object { ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 }).Count -ne 0) { throw 'Refused linked CI CPU workflow cleanup.' }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
