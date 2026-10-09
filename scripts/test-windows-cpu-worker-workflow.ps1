$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$workflow = Get-Content -LiteralPath (Join-Path (Split-Path -Parent $PSScriptRoot) '.github/workflows/release.yml') -Raw
$checks = 0
function Assert-CpuWorkflow([bool]$Condition, [string]$Message) {
    $script:checks++
    if (-not $Condition) { throw "CPU artifact workflow: $Message" }
}
function Get-CpuWorkflowStep([string]$Name) {
    $marker = "      - name: $Name"
    $start = $workflow.IndexOf($marker, [StringComparison]::Ordinal)
    if ($start -lt 0) { throw "Missing CPU artifact workflow step: $Name" }
    $end = $workflow.IndexOf('      - name:', $start + $marker.Length, [StringComparison]::Ordinal)
    if ($end -lt 0) { $end = $workflow.Length }
    return $workflow.Substring($start, $end - $start)
}

$exportStep = Get-CpuWorkflowStep 'Export verified CPU worker for immutable reuse'
$uploadStep = Get-CpuWorkflowStep 'Upload immutable verified CPU worker'
$null = Get-CpuWorkflowStep 'Verify portable and installer payload parity'
$expectedGuard = "github.repository == 'tyhuang9/scribe' && github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main' && steps.cpu-input-policy.outputs.requested != 'true'"
foreach ($step in @($exportStep, $uploadStep)) {
    $guard = [regex]::Match($step, '(?m)^        if: ([^\r\n]+)').Groups[1].Value
    Assert-CpuWorkflow ($guard -ceq $expectedGuard) 'Artifact emission must remain fixed-repository/manual/main only.'
    # Exercise the actual restricted expression, not a separately maintained policy.
    $expression = $guard.Replace('github.repository', '$repository').Replace('github.event_name', '$eventName').Replace('github.ref', '$refName').Replace('steps.cpu-input-policy.outputs.requested', '$cpuRequested').Replace(' == ', ' -ceq ').Replace(' != ', ' -cne ').Replace(' && ', ' -and ')
    $matrixCases = 0
    foreach ($repository in @('tyhuang9/scribe', 'fork/scribe')) {
        foreach ($eventName in @('workflow_dispatch', 'pull_request', 'push')) {
            foreach ($refName in @('refs/heads/main', 'refs/heads/feature', 'refs/tags/v1.0.0')) {
                foreach ($cpuRequested in @('false', 'true')) {
                    $expected = $repository -ceq 'tyhuang9/scribe' -and $eventName -ceq 'workflow_dispatch' -and $refName -ceq 'refs/heads/main' -and $cpuRequested -cne 'true'
                    Assert-CpuWorkflow ((Invoke-Expression $expression) -eq $expected) 'Actual emission guard accepted an unexpected context.'
                    $matrixCases++
                }
            }
        }
    }
    Assert-CpuWorkflow ($matrixCases -eq 36) 'The complete emission-context matrix was not discovered.'
}
Assert-CpuWorkflow ($workflow.IndexOf('      - name: Verify portable and installer payload parity', [StringComparison]::Ordinal) -lt $workflow.IndexOf('      - name: Export verified CPU worker for immutable reuse', [StringComparison]::Ordinal)) 'CPU export must follow real installer parity verification.'
Assert-CpuWorkflow ($workflow.IndexOf('      - name: Export verified CPU worker for immutable reuse', [StringComparison]::Ordinal) -lt $workflow.IndexOf('      - name: Upload immutable verified CPU worker', [StringComparison]::Ordinal)) 'The artifact must not upload before export validation.'
Assert-CpuWorkflow ($uploadStep.Contains('uses: actions/upload-artifact@b7c566a772e6b6bfb58ed0dc250532a479d7789f')) 'CPU artifact upload must use the reviewed immutable action pin.'
Assert-CpuWorkflow ($uploadStep.Contains('name: windows-cpu-worker-${{ github.run_id }}-${{ github.run_attempt }}')) 'Artifact name must bind the exact run and attempt.'
Assert-CpuWorkflow ($uploadStep.Contains('if-no-files-found: error') -and -not $uploadStep.Contains('overwrite:')) 'Artifact upload must fail closed and never overwrite an old input.'
$paths = [regex]::Match($uploadStep, '(?ms)^          path: \|\r?\n(?<paths>(?:            [^\r\n]+\r?\n)+)').Groups['paths'].Value
$actualPaths = @($paths -split '\r?\n' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
Assert-CpuWorkflow (($actualPaths -join '|') -ceq '${{ runner.temp }}/scribe-verified-cpu-worker-input/scribe-inference-worker.exe|${{ runner.temp }}/scribe-verified-cpu-worker-input/windows-ci-cpu-worker.json') 'Only the fixed worker and distinct metadata may be uploaded outside the source checkout.'
Assert-CpuWorkflow (-not $exportStep.Contains('FrozenCpuWorkerRecordPath') -and -not $exportStep.Contains('cargo ') -and -not $exportStep.Contains('sign')) 'Export must not rebuild, sign, or promote a local freeze.'

$match = [regex]::Match($exportStep, '(?ms)^        run: \|\r?\n(?<body>(?:          [^\r\n]*(?:\r?\n|$)|\r?\n)+)')
Assert-CpuWorkflow $match.Success 'The CPU exporter has no executable run block.'
$body = ($match.Groups['body'].Value -split '\r?\n' | ForEach-Object { if ($_.Length -ge 10) { $_.Substring(10) } else { $_ } }) -join [Environment]::NewLine
$tokens = $null; $parseErrors = $null
$null = [Management.Automation.Language.Parser]::ParseInput($body, [ref]$tokens, [ref]$parseErrors)
Assert-CpuWorkflow (@($parseErrors).Count -eq 0) 'The actual export run block does not parse.'
$testRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-cpu-artifact-workflow-$([guid]::NewGuid().ToString('N'))"
$stateNames = @('CpuArtifactWorkflowCalls', 'CpuArtifactWorkflowReject')
$saved = @{}
foreach ($name in $stateNames) {
    $value = Get-Variable -Name $name -Scope Global -ErrorAction SilentlyContinue
    $saved[$name] = @{ Exists = $null -ne $value; Value = $(if ($value) { $value.Value } else { $null }) }
}
$pushed = $false
$savedRunnerTemp = [Environment]::GetEnvironmentVariable('RUNNER_TEMP')
try {
    $null = New-Item -ItemType Directory -Path (Join-Path $testRoot 'scripts')
    [IO.File]::WriteAllText((Join-Path $testRoot 'scripts/export-windows-cpu-worker-artifact.ps1'), @'
param([string]$BundlePath, [string]$OutputDirectory)
$global:CpuArtifactWorkflowCalls.Add(@{ BundlePath = $BundlePath; OutputDirectory = $OutputDirectory })
if ($global:CpuArtifactWorkflowReject) { throw 'Expected exporter refusal.' }
'@)
    $global:CpuArtifactWorkflowCalls = [Collections.Generic.List[object]]::new()
    $global:CpuArtifactWorkflowReject = $false
    $env:RUNNER_TEMP = $testRoot
    Push-Location $testRoot; $pushed = $true
    Invoke-Expression $body
    Assert-CpuWorkflow ($global:CpuArtifactWorkflowCalls.Count -eq 1) 'The actual workflow must invoke the exporter exactly once.'
    $call = $global:CpuArtifactWorkflowCalls[0]
    Assert-CpuWorkflow ($call.BundlePath -ceq 'dist\portable' -and $call.OutputDirectory -ceq (Join-Path $testRoot 'scribe-verified-cpu-worker-input')) 'Export lost the fixed verified-input/output locations.'
    $global:CpuArtifactWorkflowReject = $true
    $rejected = $false
    try { Invoke-Expression $body } catch { if ($_.Exception.Message -notlike '*Expected exporter refusal*') { throw }; $rejected = $true }
    Assert-CpuWorkflow $rejected 'Workflow swallowed an exporter failure.'
    Assert-CpuWorkflow ($checks -ge 88) 'Expected CPU workflow checks were not discovered.'
    Write-Output "Windows CPU artifact workflow tests passed ($checks checks; actual guarded run block, offline stubs)."
}
finally {
    if ($pushed) { Pop-Location }
    [Environment]::SetEnvironmentVariable('RUNNER_TEMP', $savedRunnerTemp)
    foreach ($name in $stateNames) {
        if ($saved[$name].Exists) { Set-Variable -Name $name -Scope Global -Value $saved[$name].Value }
        else { Remove-Variable -Name $name -Scope Global -ErrorAction SilentlyContinue }
    }
    if (Test-Path -LiteralPath $testRoot) {
        $resolved = [IO.Path]::GetFullPath($testRoot)
        $parent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
        if ((Split-Path -Parent $resolved) -cne $parent -or (Split-Path -Leaf $resolved) -notmatch '^scribe-cpu-artifact-workflow-[0-9a-f]{32}$') { throw 'Refused unexpected CPU workflow fixture cleanup path.' }
        if (@(Get-ChildItem -LiteralPath $resolved -Force -Recurse | Where-Object { ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 }).Count -ne 0) { throw 'Refused linked CPU workflow fixture cleanup.' }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
