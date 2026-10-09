[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$workflowPath = Join-Path (Split-Path -Parent $PSScriptRoot) '.github/workflows/release.yml'
$workflow = Get-Content -LiteralPath $workflowPath -Raw
$checks = 0

function Assert-WorkflowTest([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:checks++
}

function Get-ActualRunBlock([string]$Name) {
    $marker = "      - name: $Name"
    $start = $workflow.IndexOf($marker, [StringComparison]::Ordinal)
    if ($start -lt 0) { throw "Missing workflow step: $Name" }
    $end = $workflow.IndexOf('      - name:', $start + $marker.Length, [StringComparison]::Ordinal)
    if ($end -lt 0) { $end = $workflow.Length }
    $step = $workflow.Substring($start, $end - $start)
    $run = [regex]::Match($step, '(?ms)^        run: \|\r?\n(?<body>.*)$')
    if (-not $run.Success) { throw "Missing run block: $Name" }
    return (($run.Groups['body'].Value -split '\r?\n' | ForEach-Object {
        if ($_.StartsWith('          ', [StringComparison]::Ordinal)) { $_.Substring(10) } else { $_ }
    }) -join [Environment]::NewLine).Trim()
}

foreach ($block in [regex]::Matches($workflow, '(?m)^        run: \|\r?\n(?<body>(?:          [^\r\n]*(?:\r?\n|$)|\r?\n)+)')) {
    $body = ($block.Groups['body'].Value -split '\r?\n' | ForEach-Object {
        if ($_.Length -ge 10) { $_.Substring(10) } else { $_ }
    }) -join [Environment]::NewLine
    $tokens = $null; $parseErrors = $null
    $null = [Management.Automation.Language.Parser]::ParseInput($body, [ref]$tokens, [ref]$parseErrors)
    Assert-WorkflowTest (@($parseErrors).Count -eq 0) 'A release workflow PowerShell block does not parse.'
}
$buildBlock = Get-ActualRunBlock 'Build validated portable payload'
$catalogBlock = Get-ActualRunBlock 'Verify actual staged GPU catalog before claiming inclusion'
$preflightBlock = Get-ActualRunBlock 'Authenticate exact signed GPU inputs without signing authority'
$uploadBlock = Get-ActualRunBlock 'Recheck signed GPU provenance before asset upload'
$publishBlock = Get-ActualRunBlock 'Recheck signed GPU provenance before release publication'
Assert-WorkflowTest ($workflow.Contains('GPU_WORKER_SOURCE_REVISION: ${{ inputs.gpu_worker_source_revision }}')) 'Workflow did not bind the optional worker-source dispatch input.'
Assert-WorkflowTest ($workflow.Contains('gpu_worker_source_revision: ${{ steps.gpu-input-preflight.outputs.worker_source_revision }}')) 'Build job did not expose the resolved worker source for publication.'
Assert-WorkflowTest ([regex]::Matches($workflow, [regex]::Escape('EXPECTED_GPU_WORKER_SOURCE_REVISION: ${{ steps.gpu-input-preflight.outputs.worker_source_revision }}')).Count -eq 3 -and $workflow.Contains('EXPECTED_GPU_WORKER_SOURCE_REVISION: ${{ needs.build.outputs.gpu_worker_source_revision }}')) 'Later gate environments did not bind worker source to preflight/build outputs.'

$environmentNames = @(
    'GPU_INPUTS_REQUESTED', 'GITHUB_SHA', 'GITHUB_WORKSPACE', 'RUNNER_TEMP', 'GITHUB_ENV', 'GITHUB_OUTPUT',
    'GPU_SIGNING_RUN_ID', 'GPU_SIGNING_RUN_ATTEMPT', 'GPU_SIGNED_ARTIFACT_ID',
    'EXPECTED_GPU_ARTIFACT_SHA256', 'EXPECTED_GPU_POLICY_SHA256', 'EXPECTED_GPU_SIGNER_PINS_SHA256',
    'GPU_WORKER_SOURCE_REVISION', 'EXPECTED_GPU_WORKER_SOURCE_REVISION', 'SCRIBE_BUILD_REVISION', 'CPU_INPUTS_REQUESTED'
)
$savedEnvironment = @{}
foreach ($name in $environmentNames) { $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name) }
$stateNames = @('SignedGpuWorkflowEvents', 'SignedGpuWorkflowRejectMode', 'SignedGpuWorkflowCargoExit', 'SignedGpuWorkflowRoots', 'SignedGpuWorkflowCalls', 'SignedGpuWorkflowExpectedWorkerSource')
$savedState = @{}
foreach ($name in $stateNames) {
    $existingState = Get-Variable -Name $name -Scope Global -ErrorAction SilentlyContinue
    $savedState[$name] = [pscustomobject]@{ Exists = $null -ne $existingState; Value = $(if ($null -ne $existingState) { $existingState.Value } else { $null }) }
}
$existingExitCode = Get-Variable -Name LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue
$hadExitCode = $null -ne $existingExitCode
$savedExitCode = if ($hadExitCode) { $existingExitCode.Value } else { $null }
$testRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-signed-gpu-workflow-$([guid]::NewGuid().ToString('N'))"
$testRoot = [IO.Path]::GetFullPath($testRoot)
$pushed = $false
try {
    New-Item -ItemType Directory -Path $testRoot | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $testRoot 'scripts'), (Join-Path $testRoot 'dist/portable') | Out-Null
    [IO.File]::WriteAllText((Join-Path $testRoot 'Cargo.toml'), 'version = "0.1.0"')
    [IO.File]::WriteAllText((Join-Path $testRoot 'environment.txt'), '')
    [IO.File]::WriteAllText((Join-Path $testRoot 'output.txt'), '')
    [IO.File]::WriteAllText((Join-Path $testRoot 'dist/portable/worker-pack-catalog.json'), '{"schema_version":1,"packs":[]}')
    [IO.File]::WriteAllText((Join-Path $testRoot 'scripts/resolve-windows-signed-gpu-inputs.ps1'), @'
param($Mode, $SourceRevision, $WorkerSourceRevision, $SigningRunId, $SigningRunAttempt, $SignedArtifactId, $SignedRoot, $VerifierExecutable, $CatalogPath, $ExpectedArtifactSha256, $ExpectedPolicySha256, $ExpectedSignerPinsSha256)
$global:SignedGpuWorkflowEvents.Add($Mode)
$hasWorkerPin = $PSBoundParameters.ContainsKey('WorkerSourceRevision')
$global:SignedGpuWorkflowCalls.Add([pscustomobject]@{ Mode = $Mode; SourceRevision = $SourceRevision; WorkerSourceRevision = $WorkerSourceRevision; HasWorkerPin = $hasWorkerPin })
if ($global:SignedGpuWorkflowRejectMode -ceq $Mode) { throw 'Fixture rejection.' }
if ($SourceRevision -cne $env:GITHUB_SHA -or $SigningRunId -cne '100' -or $SigningRunAttempt -cne '2' -or $SignedArtifactId -cne '300') { throw 'Workflow lost immutable source/signing identity.' }
if ($Mode -ceq 'Preflight' -and -not $ExpectedArtifactSha256) {
    if ([string]::IsNullOrEmpty($env:GPU_WORKER_SOURCE_REVISION)) {
        if ($hasWorkerPin) { throw 'Initial omitted worker source was passed as an explicit blank pin.' }
    } elseif (-not $hasWorkerPin -or $WorkerSourceRevision -cne $env:GPU_WORKER_SOURCE_REVISION) { throw 'Initial preflight lost the explicit worker source.' }
} elseif (-not $hasWorkerPin -or $WorkerSourceRevision -cne $global:SignedGpuWorkflowExpectedWorkerSource) { throw 'A later gate lost the preflight-pinned worker source.' }
if ($Mode -cne 'Preflight' -or $ExpectedArtifactSha256) {
    if ($ExpectedArtifactSha256 -cne ('a' * 64) -or $ExpectedPolicySha256 -cne ('b' * 64) -or $ExpectedSignerPinsSha256 -cne ('c' * 64)) { throw 'Workflow lost preflight bindings.' }
}
if ($Mode -cne 'Preflight') {
    if ($SignedRoot -cne (Join-Path $env:RUNNER_TEMP 'scribe-signed-gpu-installer-inputs') -or $VerifierExecutable -cne (Join-Path $env:GITHUB_WORKSPACE 'target/gpu-installer-author/release/scribe-worker-pack-tool.exe')) { throw 'Workflow lost fixed verifier/input paths.' }
}
if ($Mode -ceq 'VerifyCatalog' -and $CatalogPath -cne (Join-Path $env:GITHUB_WORKSPACE 'dist/portable/worker-pack-catalog.json')) { throw 'Workflow lost fixed catalog path.' }
[pscustomobject]@{ PackRoots = $global:SignedGpuWorkflowRoots; Included = $true; ArtifactSha256 = 'a' * 64; PolicySha256 = 'b' * 64; SignerPinsSha256 = 'c' * 64; WorkerSourceRevision = $global:SignedGpuWorkflowExpectedWorkerSource }
'@)
    [IO.File]::WriteAllText((Join-Path $testRoot 'scripts/build-windows-release.ps1'), @'
param($ModelSource, $BundlePath, [string[]]$WorkerPackRoot = @())
$global:SignedGpuWorkflowEvents.Add('Build')
if ($ModelSource -cne '.ci-release-inputs\model\whisper-base.en-Q8_0.gguf' -or $BundlePath -cne 'dist\portable') { throw 'Workflow changed release paths.' }
if ($env:GPU_INPUTS_REQUESTED -ceq 'true') {
    if ($WorkerPackRoot.Count -ne 2 -or $WorkerPackRoot[0] -cne $global:SignedGpuWorkflowRoots[0] -or $WorkerPackRoot[1] -cne $global:SignedGpuWorkflowRoots[1]) { throw 'Workflow did not forward exactly the verified ordered pair.' }
} elseif ($WorkerPackRoot.Count -ne 0) { throw 'CPU workflow received GPU inputs.' }
'@)
    function cargo {
        $global:SignedGpuWorkflowEvents.Add('Cargo')
        if (($args -join ' ') -cne 'build --locked --offline --release --manifest-path tools/worker-pack-author/Cargo.toml --target-dir target/gpu-installer-author') { throw 'Unexpected verifier build command.' }
        if ($env:SCRIBE_BUILD_REVISION -cne $env:GITHUB_SHA) { throw 'Verifier build identity was not bound.' }
        $global:LASTEXITCODE = $global:SignedGpuWorkflowCargoExit
    }
    $env:GITHUB_SHA = 'd' * 40
    $env:CPU_INPUTS_REQUESTED = 'false'
    $env:GITHUB_WORKSPACE = $testRoot; $env:RUNNER_TEMP = $testRoot
    $env:GITHUB_ENV = Join-Path $testRoot 'environment.txt'; $env:GITHUB_OUTPUT = Join-Path $testRoot 'output.txt'
    $env:GPU_SIGNING_RUN_ID = '100'; $env:GPU_SIGNING_RUN_ATTEMPT = '2'; $env:GPU_SIGNED_ARTIFACT_ID = '300'
    $env:EXPECTED_GPU_ARTIFACT_SHA256 = 'a' * 64
    $env:EXPECTED_GPU_POLICY_SHA256 = 'b' * 64
    $env:EXPECTED_GPU_SIGNER_PINS_SHA256 = 'c' * 64
    $global:SignedGpuWorkflowEvents = [Collections.Generic.List[string]]::new()
    $global:SignedGpuWorkflowCalls = [Collections.Generic.List[object]]::new()
    $global:SignedGpuWorkflowRejectMode = ''; $global:SignedGpuWorkflowCargoExit = 0
    $global:SignedGpuWorkflowRoots = @((Join-Path $testRoot 'cuda'), (Join-Path $testRoot 'vulkan'))
    Push-Location $testRoot; $pushed = $true

    $originScenarios = 0
    foreach ($workerPin in @('', ('e' * 40))) {
        $env:GPU_WORKER_SOURCE_REVISION = $workerPin
        $global:SignedGpuWorkflowExpectedWorkerSource = if ($workerPin) { $workerPin } else { $env:GITHUB_SHA }
        $global:SignedGpuWorkflowEvents.Clear(); $global:SignedGpuWorkflowCalls.Clear()
        [IO.File]::WriteAllText($env:GITHUB_OUTPUT, '')
        Invoke-Expression $preflightBlock
        $preflightOutput = Get-Content -LiteralPath $env:GITHUB_OUTPUT -Raw
        Assert-WorkflowTest (($global:SignedGpuWorkflowEvents -join '|') -ceq 'Preflight') 'Initial preflight used an unexpected operation.'
        Assert-WorkflowTest ($preflightOutput.Contains("artifact_sha256=$('a' * 64)")) 'Preflight did not export its immutable digest.'
        $resolvedWorker = [regex]::Match($preflightOutput, '(?m)^worker_source_revision=([0-9a-f]{40})\r?$').Groups[1].Value
        Assert-WorkflowTest ($resolvedWorker -ceq $global:SignedGpuWorkflowExpectedWorkerSource) 'Initial preflight did not export the resolved R/default-M source.'
        Assert-WorkflowTest ($global:SignedGpuWorkflowCalls[0].HasWorkerPin -eq (-not [string]::IsNullOrEmpty($workerPin))) 'Initial preflight did not distinguish omitted from explicit worker source.'
        $env:EXPECTED_GPU_WORKER_SOURCE_REVISION = $resolvedWorker
        # Subsequent gates must use the actual step output, not a changed raw
        # dispatch value. The trusted verifier build remains bound to M.
        $env:GPU_WORKER_SOURCE_REVISION = 'f' * 40
        $env:GPU_INPUTS_REQUESTED = 'true'
        $global:SignedGpuWorkflowEvents.Clear(); $global:SignedGpuWorkflowCalls.Clear()
        Invoke-Expression $buildBlock
        [IO.File]::WriteAllText($env:GITHUB_OUTPUT, '')
        Invoke-Expression $catalogBlock
        Assert-WorkflowTest ((Get-Content -LiteralPath $env:GITHUB_OUTPUT -Raw).Trim() -ceq 'included=true') 'Verified complete GPU catalog was not reported.'
        Invoke-Expression $uploadBlock
        Invoke-Expression $publishBlock
        Assert-WorkflowTest (($global:SignedGpuWorkflowEvents -join '|') -ceq 'Cargo|Resolve|Build|VerifyCatalog|Preflight|Preflight') 'Actual GPU gates did not verify before packaging/upload/publication.'
        Assert-WorkflowTest ($global:SignedGpuWorkflowCalls.Count -eq 4 -and @($global:SignedGpuWorkflowCalls | Where-Object { -not $_.HasWorkerPin -or $_.WorkerSourceRevision -cne $resolvedWorker -or $_.SourceRevision -cne $env:GITHUB_SHA }).Count -eq 0) 'The four later gates did not retain one preflight-pinned R and independent desktop M.'
        $originScenarios++
    }
    Assert-WorkflowTest ($originScenarios -eq 2) 'Omitted and distinct-origin workflow scenarios were not both discovered.'
    $global:SignedGpuWorkflowEvents.Clear()
    $env:GPU_WORKER_SOURCE_REVISION = ''
    $env:GPU_INPUTS_REQUESTED = 'false'
    Invoke-Expression $buildBlock
    Assert-WorkflowTest (($global:SignedGpuWorkflowEvents -join '|') -ceq 'Build') 'CPU build invoked the signed-input verifier.'
    [IO.File]::WriteAllText($env:GITHUB_OUTPUT, '')
    Invoke-Expression $catalogBlock
    Assert-WorkflowTest ((Get-Content -LiteralPath $env:GITHUB_OUTPUT -Raw).Trim() -ceq 'included=false') 'CPU catalog incorrectly claimed GPU inclusion.'

    $global:SignedGpuWorkflowEvents.Clear()
    $env:GPU_INPUTS_REQUESTED = 'true'
    foreach ($rejectedMode in @('Resolve', 'VerifyCatalog')) {
        $global:SignedGpuWorkflowEvents.Clear(); $global:SignedGpuWorkflowRejectMode = $rejectedMode
        [IO.File]::WriteAllText($env:GITHUB_OUTPUT, '')
        $rejected = $false
        try { if ($rejectedMode -ceq 'Resolve') { Invoke-Expression $buildBlock } else { Invoke-Expression $catalogBlock } } catch { $rejected = $true }
        Assert-WorkflowTest $rejected "Workflow ignored $rejectedMode rejection."
        Assert-WorkflowTest (-not $global:SignedGpuWorkflowEvents.Contains('Build')) 'Packaging ran after rejected signed inputs.'
        Assert-WorkflowTest ((Get-Item -LiteralPath $env:GITHUB_OUTPUT).Length -eq 0) 'Failed verification reported inclusion.'
    }
    $global:SignedGpuWorkflowRejectMode = ''
    $global:SignedGpuWorkflowCargoExit = 1
    $global:SignedGpuWorkflowEvents.Clear()
    $rejected = $false
    try { Invoke-Expression $buildBlock } catch { $rejected = $true }
    Assert-WorkflowTest ($rejected -and ($global:SignedGpuWorkflowEvents -join '|') -ceq 'Cargo') 'Verifier build failure did not stop GPU packaging.'
    $global:SignedGpuWorkflowCargoExit = 0

    foreach ($block in @($uploadBlock, $publishBlock)) {
        $global:SignedGpuWorkflowEvents.Clear()
        Invoke-Expression $block
        Assert-WorkflowTest (($global:SignedGpuWorkflowEvents -join '|') -ceq 'Preflight') 'Publication did not recheck fixed provenance.'
        $global:SignedGpuWorkflowRejectMode = 'Preflight'
        $global:SignedGpuWorkflowEvents.Clear()
        $rejected = $false
        try { Invoke-Expression $block; $global:SignedGpuWorkflowEvents.Add('Publication') } catch { $rejected = $true }
        Assert-WorkflowTest $rejected 'Publication ignored changed provenance.'
        Assert-WorkflowTest (-not $global:SignedGpuWorkflowEvents.Contains('Publication')) 'Publication continued after the rejected provenance gate.'
        $global:SignedGpuWorkflowRejectMode = ''
    }
    $global:SignedGpuWorkflowEvents.Clear(); $global:SignedGpuWorkflowRejectMode = 'Preflight'
    $env:GPU_WORKER_SOURCE_REVISION = 'e' * 40
    $rejected = $false
    try { Invoke-Expression $preflightBlock; Invoke-Expression $buildBlock } catch { $rejected = $true }
    Assert-WorkflowTest ($rejected -and ($global:SignedGpuWorkflowEvents -join '|') -ceq 'Preflight') 'Initial preflight rejection did not stop before verifier build/packaging.'
    $global:SignedGpuWorkflowRejectMode = ''
    $env:GPU_INPUTS_REQUESTED = 'false'
    [IO.File]::WriteAllText((Join-Path $testRoot 'dist/portable/worker-pack-catalog.json'), '{"schema_version":1,"packs":[{}]}')
    $rejected = $false
    try { Invoke-Expression $catalogBlock } catch { $rejected = $true }
    Assert-WorkflowTest $rejected 'CPU build accepted an unexpected GPU pack.'
}
finally {
    if ($pushed) { Pop-Location }
    foreach ($name in $environmentNames) { [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name]) }
    foreach ($name in $stateNames) {
        if (-not $savedState[$name].Exists) { Remove-Variable -Name $name -Scope Global -ErrorAction SilentlyContinue }
        else { Set-Variable -Name $name -Scope Global -Value $savedState[$name].Value }
    }
    if (-not $hadExitCode) { Remove-Variable -Name LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue }
    else { $global:LASTEXITCODE = $savedExitCode }
    if (Test-Path -LiteralPath $testRoot) {
        $tempParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\', '/')
        if ((Split-Path -Parent $testRoot) -cne $tempParent -or (Split-Path -Leaf $testRoot) -cnotmatch '\Ascribe-signed-gpu-workflow-[0-9a-f]{32}\z') { throw 'Refusing cleanup outside the exact owned workflow fixture.' }
        foreach ($entry in @((Get-Item -LiteralPath $testRoot)) + @(Get-ChildItem -LiteralPath $testRoot -Recurse -Force)) {
            if (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Refusing workflow fixture cleanup across a reparse point.' }
        }
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}
Write-Host "Signed GPU installer workflow tests passed ($checks checks; actual run blocks, offline process stubs)."
