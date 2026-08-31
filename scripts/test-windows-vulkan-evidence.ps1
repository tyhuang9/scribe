$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-vulkan-evidence-preflight.ps1')

if ((ConvertTo-ScribeVulkanEvidencePci 'native:0000:01:00.0') -cne '0000:01:00.0') { throw 'Native PCI parsing regressed.' }
if ((ConvertTo-ScribeVulkanEvidencePci '00000000:01:00.0') -cne '0000:01:00.0') { throw 'nvidia-smi PCI parsing regressed.' }
foreach ($value in @('native:0000:01:00.8', 'native:0000:01:00.0 ', 'uuid:secret')) {
    $accepted = $false
    try { $null = ConvertTo-ScribeVulkanEvidencePci $value; $accepted = $true } catch {}
    if ($accepted) { throw "Malformed PCI identity was accepted: $value" }
}
$fixturePackVersion = New-ScribeEvidenceFixturePackVersion ('a' * 40 -join '') ('b' * 12 -join '')
$fixtureCargoLeaf = "vulkan-$fixturePackVersion-cargo"
if ($fixturePackVersion -cne 'fixture-aaaaaaaaaaaa-bbbbbbbbbbbb' -or
    $fixtureCargoLeaf.Length -ne 46 -or
    $fixtureCargoLeaf -cnotmatch '^[a-z0-9](?:[a-z0-9._-]{0,46}[a-z0-9])?$') {
    throw 'Fixture pack version does not compose into the builder Cargo target bound.'
}
foreach ($invalidPackVersionInput in @(
    @('A' * 40 -join '', 'b' * 12 -join ''),
    @('a' * 40 -join '', 'b' * 11 -join '')
)) {
    $accepted = $false
    try { $null = New-ScribeEvidenceFixturePackVersion $invalidPackVersionInput[0] $invalidPackVersionInput[1]; $accepted = $true } catch {}
    if ($accepted) { throw 'Noncanonical fixture pack version input was accepted.' }
}
$knownCmakeFailure = @(
    'error: failed to run custom build command for `transcribe-cpp-sys v0.1.3 (C:\\safe\\crate)`',
    '  Error: failed to execute command: cmake -S C:\\safe\\source',
    '  The directory name is invalid. (os error 267)'
)
if (-not (Test-ScribeEvidenceKnownCmakeBootstrapFailure $knownCmakeFailure)) { throw 'Known bounded CMake failure was not classified.' }
foreach ($malformedCmakeFailure in @(
    @('transcribe-cpp-sys v0.1.3', 'failed to execute command:', 'The directory name is invalid. (os error 267)'),
    @('error: failed to run custom build command for `transcribe-cpp-sys v0.1.4 (C:\\safe\\crate)`', '  Error: failed to execute command: cmake', '  The directory name is invalid. (os error 267)'),
    @('error: failed to run custom build command for `transcribe-cpp-sys v0.1.3 (C:\\safe\\crate)`', '  Error: failed to execute command: cmake', '  access denied'),
    @('  The directory name is invalid. (os error 267)', '  Error: failed to execute command: cmake', 'error: failed to run custom build command for `transcribe-cpp-sys v0.1.3 (C:\\safe\\crate)`')
)) {
    if (Test-ScribeEvidenceKnownCmakeBootstrapFailure $malformedCmakeFailure) { throw 'Malformed CMake failure was classified.' }
}
$sanitizedClassifierResult = Test-ScribeEvidenceKnownCmakeBootstrapFailure @('secret-token', 'unrelated failure')
if ($sanitizedClassifierResult -isnot [bool] -or $sanitizedClassifierResult) { throw 'CMake classifier exposed or accepted unrelated output.' }
$overlongCmakeFailure = [System.Collections.Generic.List[object]]::new()
foreach ($unused in 1..64) { $overlongCmakeFailure.Add('noise') }
foreach ($line in $knownCmakeFailure) { $overlongCmakeFailure.Add($line) }
if (Test-ScribeEvidenceKnownCmakeBootstrapFailure -Output $overlongCmakeFailure.ToArray()) { throw 'Unbounded CMake output was classified outside the bounded window.' }
$previousTestWorkerDigest = $env:SCRIBE_BUNDLED_WORKER_SHA256
$previousTestBuildingWorker = $env:SCRIBE_BUILDING_WORKER
try {
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = 'a' * 64 -join ''
    $env:SCRIBE_BUILDING_WORKER = 'ambient'
    Set-ScribeEvidenceWorkerBuildMode $true
    if ($env:SCRIBE_BUILDING_WORKER -cne '1' -or $null -ne $env:SCRIBE_BUNDLED_WORKER_SHA256) { throw 'Worker build mode did not clear the desktop digest.' }
    Set-ScribeEvidenceWorkerBuildMode $false
    if ($null -ne $env:SCRIBE_BUILDING_WORKER -or $null -ne $env:SCRIBE_BUNDLED_WORKER_SHA256) { throw 'Harness mode retained worker-build state.' }
}
finally {
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $previousTestWorkerDigest
    $env:SCRIBE_BUILDING_WORKER = $previousTestBuildingWorker
}
$previousToolchainTestValue = $env:SCRIBE_EVIDENCE_TOOLCHAIN_TEST
try {
    $env:SCRIBE_EVIDENCE_TOOLCHAIN_TEST = 'ambient'
    $toolchainRestoreState = Set-ScribeEvidenceProcessEnvironment ([ordered]@{ SCRIBE_EVIDENCE_TOOLCHAIN_TEST = 'pinned' })
    if ($env:SCRIBE_EVIDENCE_TOOLCHAIN_TEST -cne 'pinned') { throw 'Pinned toolchain environment was not applied.' }
    Restore-ScribeEvidenceProcessEnvironment $toolchainRestoreState
    if ($env:SCRIBE_EVIDENCE_TOOLCHAIN_TEST -cne 'ambient') { throw 'Pinned toolchain environment was not restored.' }
}
finally {
    $env:SCRIBE_EVIDENCE_TOOLCHAIN_TEST = $previousToolchainTestValue
}
$topologyRoot = Join-Path ([IO.Path]::GetTempPath()) ("scribe-evidence-topology-$([guid]::NewGuid().ToString('N'))")
try {
    $buildDirectory = Join-Path $topologyRoot 'cargo\\debug\\build\\transcribe-cpp-sys-0123456789abcdef\\out\\build'
    $outsideDirectory = Join-Path $topologyRoot 'outside'
    New-Item -ItemType Directory -Path $buildDirectory -Force | Out-Null
    New-Item -ItemType Directory -Path $outsideDirectory -Force | Out-Null
    $sentinel = Join-Path $outsideDirectory 'must-not-delete.txt'
    [IO.File]::WriteAllText($sentinel, 'sentinel')
    New-Item -ItemType Junction -Path (Join-Path $buildDirectory 'escaped') -Target $outsideDirectory | Out-Null
    $rejected = $false
    try { Assert-ScribeEvidenceNoReparseDescendants $buildDirectory } catch { $rejected = $true }
    if (-not $rejected) { throw 'CMake deletion topology accepted a descendant junction.' }
    if (-not (Test-Path -LiteralPath $sentinel -PathType Leaf)) { throw 'CMake topology validation deleted outside-scope data.' }
    $regularFileRejected = $false
    try { $null = Get-ScribeEvidencePhysicalDirectory $sentinel 'test file' } catch { $regularFileRejected = $true }
    if (-not $regularFileRejected) { throw 'CMake topology validation accepted a regular file as a directory.' }
}
finally {
    if (Test-Path -LiteralPath $topologyRoot) { Remove-Item -LiteralPath $topologyRoot -Recurse -Force }
}
$runner = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'run-windows-vulkan-evidence.ps1') -Raw
foreach ($required in @('--locked', '--offline', '-SigningMode Fixture', '--ignored', '--exact', '--test-threads=1', '--no-run', 'Invoke-ScribeEvidenceCargoWithCmakeRetry', 'Enable-ScribeEvidenceCmakeBootstrap', 'Assert-ScribeEvidenceNoReparseDescendants', 'Get-ScribeEvidencePinnedMsvcEnvironment', 'Invoke-ScribeEvidenceWithPinnedMsvcEnvironment', '-ToolchainCheckOnly -ExportPinnedMsvcEnvironment', 'Set-ScribeEvidenceWorkerBuildMode $true', 'Set-ScribeEvidenceWorkerBuildMode $false', 'previousWorkerDigest', 'previousBuildingWorker', 'transcribe-cpp-sys-[0-9a-f]{16}', 'onnx_worker::tests::windows_vulkan_fixture_evidence_captures_five_cold_and_twenty_warm_runs', 'gpu-auto-qualification-windows-x64.json', 'Production signing/release input is forbidden', 'Resolve-ScribeEvidenceFreshDirectory', 'Evidence output may not be under source')) {
    if ($runner -notmatch [regex]::Escape($required)) { throw "Runner is missing required source contract: $required" }
}
if ($runner -notmatch 'finally\s*\{\s*if \(\$null -ne \$previous\) \{ Restore-ScribeEvidenceProcessEnvironment \$previous \}') {
    throw 'Pinned toolchain scope does not restore the ambient environment.'
}
foreach ($required in @('Get-FileHash -LiteralPath $model', 'Get-FileHash -LiteralPath $wav', 'Test-ScribeEvidenceActivationPath', 'Test-ScribeEvidenceWithin', 'New-ScribeEvidenceShortCargoTarget', 'Assert-ScribeEvidenceSingleLinkFile', 'Get-ScribeVulkanEvidenceActualSystem32', 'fsutil.exe', 'manifest.json', 'Fixture pack build identity is not bound', 'New-ScribeEvidenceFixturePackVersion $revision', 'Fixture-only untrusted Vulkan evidence', 'previousEvidenceEnvironment')) {
    if ($runner -notmatch [regex]::Escape($required)) { throw "Runner is missing required safety contract: $required" }
}
$compileAt = $runner.IndexOf("'--no-run'")
$baselineAt = $runner.IndexOf('Get-ScribeVulkanEvidenceNvidiaBaseline')
if ($compileAt -lt 0 -or $baselineAt -lt 0 -or $compileAt -ge $baselineAt) { throw 'Runner must precompile before NVIDIA baseline capture.' }
$cpuBuildAt = $runner.IndexOf("Invoke-ScribeEvidenceCargoWithCmakeRetry @('build'")
$workerModeAt = $runner.IndexOf('Set-ScribeEvidenceWorkerBuildMode $true')
$harnessModeAt = $runner.IndexOf('Set-ScribeEvidenceWorkerBuildMode $false', $cpuBuildAt)
$ignoredTestAt = $runner.IndexOf("Invoke-ScribeEvidence `$cargo @('test'")
$executionModeAt = $runner.LastIndexOf('Set-ScribeEvidenceWorkerBuildMode $false', $ignoredTestAt)
$pinnedCpuAt = $runner.LastIndexOf('Invoke-ScribeEvidenceWithPinnedMsvcEnvironment $pinnedMsvcEnvironment', $cpuBuildAt)
$packBuildAt = $runner.IndexOf('& $packBuilder -Backend Vulkan -PackVersion $packVersion')
$pinnedHarnessAt = $runner.LastIndexOf('Invoke-ScribeEvidenceWithPinnedMsvcEnvironment $pinnedMsvcEnvironment', $compileAt)
$pinnedExecutionAt = $runner.LastIndexOf('Invoke-ScribeEvidenceWithPinnedMsvcEnvironment $pinnedMsvcEnvironment', $ignoredTestAt)
if ($workerModeAt -lt 0 -or $cpuBuildAt -lt 0 -or $workerModeAt -ge $cpuBuildAt -or
    $harnessModeAt -lt $cpuBuildAt -or $harnessModeAt -ge $compileAt -or
    $executionModeAt -lt $compileAt -or $executionModeAt -ge $ignoredTestAt -or
    $pinnedCpuAt -lt 0 -or $pinnedCpuAt -ge $packBuildAt -or
    $pinnedHarnessAt -lt 0 -or $pinnedHarnessAt -gt $compileAt -or
    $pinnedExecutionAt -lt $compileAt -or $pinnedExecutionAt -ge $ignoredTestAt) {
    throw 'Runner worker-build mode is not isolated to the CPU worker build.'
}
if ($runner -match 'SigningMode Production|ProductionPrivateKeyPath|ProductionKeyId') { throw 'Fixture runner references a production signing input.' }
if ($runner -match '\$detail\s*=\s*\(\$first\s*\|\s*Out-String\)|throw\s+"\$Failure\s+\$detail"') { throw 'Runner can throw raw Cargo child output.' }
$preflight = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'windows-vulkan-evidence-preflight.ps1') -Raw
foreach ($required in @('GetSystemDirectory', 'nvidia-smi.exe', 'matching.Count -ne 1', '$utilization -gt 10', '$usedMib -gt ($totalMib / 4)', 'pci.bus_id')) {
    if ($preflight -notmatch [regex]::Escape($required)) { throw "Preflight is missing required source contract: $required" }
}
Write-Output 'Windows Vulkan fixture-evidence script contracts passed.'
