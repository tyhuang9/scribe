$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Exercise the real runner helpers in an isolated scope with a fake Cargo
# command. No compiler, process, provider or fixture file is needed here.
$runner = Join-Path $PSScriptRoot 'test-windows-gpu-capture-observation.ps1'
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($runner, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'Capture runner does not parse.' }
# Keep every collector group connected to the real discovery-before-run helper.
# Provider-only groups have their independent invocation contract below.
function Assert-CaptureCollectorGroups([Management.Automation.Language.Ast]$RunnerAst) {
    $loops = @($RunnerAst.FindAll({
        param($node)
        $node -is [Management.Automation.Language.ForEachStatementAst] -and
            $node.Variable.VariablePath.UserPath -ceq 'filter' -and
            @($node.Body.FindAll({
                param($call)
                $call -is [Management.Automation.Language.CommandAst] -and
                    $call.Extent.Text -ceq 'Invoke-CaptureTests $feature $filter'
            }, $true)).Count -eq 1
    }, $true))
    if ($loops.Count -ne 1) { throw 'Expected exactly one collector test-group loop.' }
    $groups = @($loops[0].Condition.FindAll({
        param($node)
        $node -is [Management.Automation.Language.StringConstantExpressionAst]
    }, $true) | ForEach-Object { $_.Value })
    $expected = @('windows_gpu_capture::tests', 'windows_gpu_capture::telemetry::tests',
        'windows_gpu_capture::power_scheme::tests', 'windows_gpu_capture::campaign::tests',
        'onnx_worker::tests::capture_observation', 'embedded_runtime::tests::provider_memory_observation',
        'architecture_guard::windows_gpu_capture')
    if (($groups -join "`n") -cne ($expected -join "`n")) {
        throw 'Collector test groups must include all seven required discovery/run filters.'
    }
}
Assert-CaptureCollectorGroups $ast
# Prove this wiring guard rejects a runner with the new group disconnected.
$disconnectedSource = $ast.Extent.Text.Replace("'windows_gpu_capture::power_scheme::tests',", '')
if ($disconnectedSource -ceq $ast.Extent.Text) { throw 'Collector wiring mutation did not change the source.' }
$mutationAst = [Management.Automation.Language.Parser]::ParseInput($disconnectedSource, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'Collector wiring mutation must remain valid PowerShell.' }
$mutationError = $null
try { Assert-CaptureCollectorGroups $mutationAst }
catch { $mutationError = $_.Exception.Message }
if ($mutationError -cne 'Collector test groups must include all seven required discovery/run filters.') {
    throw 'Collector wiring guard did not reject the missing power-scheme test group.'
}
Write-Output 'Collector test-group wiring contracts passed (2 cases); no Cargo process was launched.'
$definitions = foreach ($name in @('Invoke-CaptureCargo', 'Invoke-CaptureTests', 'Invoke-CaptureProviderCheck')) {
    $functions = @($ast.FindAll({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq $name
    }, $true))
    if ($functions.Count -ne 1) { throw "Expected exactly one capture runner helper: $name" }
    if ($name -ceq 'Invoke-CaptureTests') {
        # PowerShell consumes a literal -- when calling a function mock, so
        # verify the native discovery separator directly from the parsed call.
        $discovery = @($functions[0].FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -ceq 'cargo'
        }, $true))
        if ($discovery.Count -ne 1 -or $discovery[0].CommandElements.Count -lt 3 -or
            $discovery[0].CommandElements[-2].Extent.Text -cne '--' -or
            $discovery[0].CommandElements[-1].Extent.Text -cne '--list') {
            throw 'Capture runner discovery must pass --list to the native test executable.'
        }
    }
    $functions[0].Extent.Text
}
$savedExitCode = Get-Variable LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue
$hadExitCode = $null -ne $savedExitCode
$originalExitCode = if ($hadExitCode) { $savedExitCode.Value } else { $null }
try {
    & {
        param([string[]]$Definitions)
        . ([scriptblock]::Create($Definitions -join "`n"))
        $state = @{ Calls = 0; Features = ''; Filter = 'fixture::tests::'; Case = '' }
        function cargo {
            $actual = @($args)
            $state.Calls++
            $expected = @('test', '--locked', '--offline', '--bin', 'local-transcriber',
                '--features', $state.Features, $state.Filter)
            $expected += if ($state.Calls -eq 1) { '--list' } else { '--', '--test-threads=1' }
            if ($state.Calls -gt 2 -or $actual.Count -ne $expected.Count) {
                throw 'Capture runner called Cargo an unexpected number of times or with invalid arguments.'
            }
            for ($index = 0; $index -lt $expected.Count; $index++) {
                if ($actual[$index] -cne $expected[$index]) {
                    throw 'Capture runner changed locked/offline, feature, filter or serial execution arguments.'
                }
            }
            $global:LASTEXITCODE = 0
            if ($state.Calls -eq 1) {
                switch ($state.Case) {
                    'empty' { return }
                    'wrong_filter' { return 'other::tests::example: test' }
                    'discovery_failure' { $global:LASTEXITCODE = 1 }
                }
                return ($state.Filter + 'example: test')
            }
            if ($state.Case -eq 'execution_failure') { $global:LASTEXITCODE = 1 }
        }
        foreach ($features in @('windows-gpu-capture-observation', 'ui-harness,cuda-acceleration',
                'ui-harness,vulkan-acceleration')) {
            foreach ($case in @('success', 'empty', 'wrong_filter', 'discovery_failure', 'execution_failure')) {
                $state.Features = $features
                $state.Case = $case
                $state.Calls = 0
                $expectedError = switch ($case) {
                    'empty' { 'Expected capture observation tests were not discovered: ' + $state.Filter }
                    'wrong_filter' { 'Expected capture observation tests were not discovered: ' + $state.Filter }
                    'discovery_failure' { 'Capture observation test discovery failed.' }
                    'execution_failure' { 'Capture observation Cargo verification failed.' }
                    default { $null }
                }
                $actualError = $null
                try { Invoke-CaptureTests $features $state.Filter }
                catch { $actualError = $_.Exception.Message }
                if ($actualError -cne $expectedError) {
                    throw "Capture runner contract failed for $features/$case`: $actualError"
                }
                $expectedCalls = if ($case -in @('success', 'execution_failure')) { 2 } else { 1 }
                if ($state.Calls -ne $expectedCalls) {
                    throw "Capture runner executed tests after unsuccessful discovery: $features/$case"
                }
            }
        }
    } -Definitions $definitions
}
finally {
    if (-not $hadExitCode) { Remove-Variable LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue }
    else { Set-Variable LASTEXITCODE -Scope Global -Value $originalExitCode }
}
Write-Output 'Capture runner contracts passed (15 mocked cases); no Cargo process was launched.'

# Provider checks are worker-only builds. Exercise the real helper through the
# parsed AST while a fake Cargo command verifies every native invocation. This
# must cover both provider feature spellings, each discovery boundary and the
# exact worker-link context without starting a compiler or a GPU workload.
$providerEnvironmentNames = @('SCRIBE_BUILDING_WORKER', 'SCRIBE_BUNDLED_WORKER_SHA256')
$savedProviderEnvironment = @{}
foreach ($name in $providerEnvironmentNames) {
    $savedProviderEnvironment[$name] = Get-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
}
$savedProviderExitCode = Get-Variable LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue
$hadProviderExitCode = $null -ne $savedProviderExitCode
$originalProviderExitCode = if ($hadProviderExitCode) { $savedProviderExitCode.Value } else { $null }
try {
    & {
        param([string[]]$Definitions)
        . ([scriptblock]::Create($Definitions -join "`n"))

        $testGroups = @(
            'onnx_worker::tests::capture_observation',
            'onnx_worker::tests::vulkan_identity_catalog_',
            'embedded_runtime::tests::provider_memory_observation'
        )
        $state = @{ Calls = 0; Expected = @(); Failure = 'success'; FailureCall = -1 }

        function New-CaptureProviderExpectedCalls([string]$ProviderFeature) {
            $testFeatures = "ui-harness,$ProviderFeature"
            $providerTestFeatures = if ($ProviderFeature -ceq 'cuda-acceleration') {
                "$testFeatures,cuda-test-harness"
            }
            else {
                $testFeatures
            }
            $calls = @()
            $calls += [pscustomobject]@{
                Kind = 'check'
                Filter = $null
                Arguments = @('check', '--locked', '--offline', '--bin', 'scribe-inference-worker', '--features', $ProviderFeature)
            }
            $calls += [pscustomobject]@{
                Kind = 'lint'
                Filter = $null
                Arguments = @('clippy', '--locked', '--offline', '--all-targets', '--features', $testFeatures, '--', '-D', 'warnings')
            }
            foreach ($filter in $testGroups) {
                $calls += [pscustomobject]@{
                    Kind = 'list'
                    Filter = $filter
                    Arguments = @('test', '--locked', '--offline', '--bin', 'local-transcriber', '--features', $providerTestFeatures, $filter, '--list')
                }
                $calls += [pscustomobject]@{
                    Kind = 'test'
                    Filter = $filter
                    Arguments = @('test', '--locked', '--offline', '--bin', 'local-transcriber', '--features', $providerTestFeatures, $filter, '--', '--test-threads=1')
                }
            }
            return $calls
        }

        function Get-CaptureProviderEnvironmentState([string]$Name) {
            $item = Get-Item -LiteralPath "Env:$Name" -ErrorAction SilentlyContinue
            if ($null -eq $item) {
                return [pscustomobject]@{ Exists = $false; Value = $null }
            }
            return [pscustomobject]@{ Exists = $true; Value = [string]$item.Value }
        }

        function Set-CaptureProviderEnvironment([string]$Name, [psobject]$State) {
            if (-not $State.Exists) {
                Remove-Item -LiteralPath "Env:$Name" -ErrorAction SilentlyContinue
                return
            }
            [Environment]::SetEnvironmentVariable($Name, [string]$State.Value, 'Process')
        }

        function Assert-CaptureProviderEnvironment([string]$Name, [psobject]$Expected) {
            $actual = Get-CaptureProviderEnvironmentState $Name
            if ($actual.Exists -ne $Expected.Exists -or
                ($actual.Exists -and $actual.Value -cne [string]$Expected.Value)) {
                throw "Capture provider helper did not restore $Name exactly."
            }
        }

        function cargo {
            $actual = @($args)
            $callIndex = $state.Calls
            if ($callIndex -ge $state.Expected.Count) {
                throw 'Capture provider helper called Cargo more than expected.'
            }
            $expected = $state.Expected[$callIndex]
            $state.Calls++
            if ($actual.Count -ne $expected.Arguments.Count) {
                throw 'Capture provider helper changed a Cargo argument count.'
            }
            for ($index = 0; $index -lt $expected.Arguments.Count; $index++) {
                if ($actual[$index] -cne $expected.Arguments[$index]) {
                    throw 'Capture provider helper changed locked/offline, worker, feature, discovery or serial execution arguments.'
                }
            }
            if ($actual -ccontains 'windows-gpu-capture-observation') {
                throw 'Capture provider helper combined a GPU provider with the desktop collector feature.'
            }
            if ([Environment]::GetEnvironmentVariable('SCRIBE_BUILDING_WORKER', 'Process') -cne '1') {
                throw 'Capture provider helper did not set worker context for every Cargo invocation.'
            }
            if ($null -ne [Environment]::GetEnvironmentVariable('SCRIBE_BUNDLED_WORKER_SHA256', 'Process')) {
                throw 'Capture provider helper did not clear the desktop worker digest for every Cargo invocation.'
            }
            $featureIndex = [Array]::IndexOf($actual, '--features')
            if ($featureIndex -lt 0 -or $featureIndex + 1 -ge $actual.Count) {
                throw 'Capture provider helper omitted its exact feature set.'
            }
            $features = @($actual[$featureIndex + 1] -csplit ',')
            $hasCudaTestHarness = $features -ccontains 'cuda-test-harness'
            if ($expected.Kind -in @('list', 'test')) {
                $hasCudaProvider = $features -ccontains 'cuda-acceleration'
                if ($hasCudaTestHarness -ne $hasCudaProvider) {
                    throw 'Only CUDA provider unit-test invocations may enable cuda-test-harness.'
                }
            }
            elseif ($hasCudaTestHarness) {
                throw 'Worker checks and all-target lint must omit cuda-test-harness.'
            }
            $global:LASTEXITCODE = 0
            if ($callIndex -eq $state.FailureCall) {
                switch ($state.Failure) {
                    'empty' { return }
                    'wrong' { return 'other::tests::example: test' }
                    default {
                        $global:LASTEXITCODE = 1
                        return
                    }
                }
            }
            if ($expected.Kind -ceq 'list') { return ($expected.Filter + 'fixture: test') }
        }

        function Invoke-CaptureProviderFixtureCase(
            [ValidateSet('Cuda', 'Vulkan')]
            [string]$Provider,
            [ValidateSet('success', 'check_failure', 'lint_failure', 'empty', 'wrong', 'list_failure', 'test_failure')]
            [string]$Failure,
            [int]$GroupIndex,
            [psobject]$InitialBuildingWorker,
            [psobject]$InitialWorkerDigest
        ) {
            $providerFeature = $Provider.ToLowerInvariant() + '-acceleration'
            $state.Calls = 0
            $state.Expected = @(New-CaptureProviderExpectedCalls $providerFeature)
            $state.Failure = $Failure
            $state.FailureCall = switch ($Failure) {
                'check_failure' { 0 }
                'lint_failure' { 1 }
                'empty' { 2 + (2 * $GroupIndex) }
                'wrong' { 2 + (2 * $GroupIndex) }
                'list_failure' { 2 + (2 * $GroupIndex) }
                'test_failure' { 3 + (2 * $GroupIndex) }
                default { -1 }
            }
            if ($Failure -notin @('success', 'check_failure', 'lint_failure') -and ($GroupIndex -lt 0 -or $GroupIndex -ge $testGroups.Count)) {
                throw 'Capture provider fixture configured an invalid test group.'
            }
            Set-CaptureProviderEnvironment 'SCRIBE_BUILDING_WORKER' $InitialBuildingWorker
            Set-CaptureProviderEnvironment 'SCRIBE_BUNDLED_WORKER_SHA256' $InitialWorkerDigest
            $actualError = $null
            try { $null = Invoke-CaptureProviderCheck $Provider }
            catch { $actualError = $_.Exception.Message }

            $expectedError = switch ($Failure) {
                'empty' { 'Expected capture observation tests were not discovered: ' + $testGroups[$GroupIndex] }
                'wrong' { 'Expected capture observation tests were not discovered: ' + $testGroups[$GroupIndex] }
                'list_failure' { 'Capture observation test discovery failed.' }
                'success' { $null }
                default { 'Capture observation Cargo verification failed.' }
            }
            if ($actualError -cne $expectedError) {
                throw "Capture provider runner contract failed for $Provider/$Failure/$GroupIndex`: $actualError"
            }
            $expectedCalls = if ($state.FailureCall -ge 0) { $state.FailureCall + 1 } else { $state.Expected.Count }
            if ($state.Calls -ne $expectedCalls) {
                throw "Capture provider helper continued after an unsuccessful $Failure boundary."
            }
            Assert-CaptureProviderEnvironment 'SCRIBE_BUILDING_WORKER' $InitialBuildingWorker
            Assert-CaptureProviderEnvironment 'SCRIBE_BUNDLED_WORKER_SHA256' $InitialWorkerDigest
        }

        $environmentStates = @(
            [pscustomobject]@{
                BuildingWorker = [pscustomobject]@{ Exists = $false; Value = $null }
                WorkerDigest = [pscustomobject]@{ Exists = $false; Value = $null }
            },
            [pscustomobject]@{
                BuildingWorker = [pscustomobject]@{ Exists = $true; Value = '' }
                WorkerDigest = [pscustomobject]@{ Exists = $true; Value = '' }
            },
            [pscustomobject]@{
                BuildingWorker = [pscustomobject]@{ Exists = $true; Value = 'ambient-worker' }
                WorkerDigest = [pscustomobject]@{ Exists = $false; Value = $null }
            },
            [pscustomobject]@{
                BuildingWorker = [pscustomobject]@{ Exists = $false; Value = $null }
                WorkerDigest = [pscustomobject]@{ Exists = $true; Value = ('a' * 64) }
            },
            [pscustomobject]@{
                BuildingWorker = [pscustomobject]@{ Exists = $true; Value = 'ambient-worker' }
                WorkerDigest = [pscustomobject]@{ Exists = $true; Value = ('b' * 64) }
            }
        )
        $caseCount = 0
        foreach ($provider in @('Cuda', 'cuda', 'CUDA', 'Vulkan', 'vulkan', 'VULKAN')) {
            foreach ($environmentState in $environmentStates) {
                Invoke-CaptureProviderFixtureCase $provider 'success' -1 $environmentState.BuildingWorker $environmentState.WorkerDigest
                $caseCount++
            }
            $failureIndex = 0
            foreach ($failure in @('check_failure', 'lint_failure')) {
                $environmentState = $environmentStates[$failureIndex % $environmentStates.Count]
                Invoke-CaptureProviderFixtureCase $provider $failure -1 $environmentState.BuildingWorker $environmentState.WorkerDigest
                $caseCount++
                $failureIndex++
            }
            foreach ($groupIndex in 0..($testGroups.Count - 1)) {
                foreach ($failure in @('empty', 'wrong', 'list_failure', 'test_failure')) {
                    $environmentState = $environmentStates[$failureIndex % $environmentStates.Count]
                    Invoke-CaptureProviderFixtureCase $provider $failure $groupIndex $environmentState.BuildingWorker $environmentState.WorkerDigest
                    $caseCount++
                    $failureIndex++
                }
            }
        }
        if ($caseCount -ne 114) { throw 'Capture provider runner contract case count changed unexpectedly.' }
    } -Definitions $definitions
}
finally {
    foreach ($name in $providerEnvironmentNames) {
        $saved = $savedProviderEnvironment[$name]
        if ($null -eq $saved) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        }
        else {
            [Environment]::SetEnvironmentVariable($name, [string]$saved.Value, 'Process')
        }
    }
    if (-not $hadProviderExitCode) { Remove-Variable LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue }
    else { Set-Variable LASTEXITCODE -Scope Global -Value $originalProviderExitCode }
}
Write-Output 'Capture provider runner contracts passed (114 mocked cases); no Cargo process was launched.'

# Test the actual argument builder without running a collector, accessing
# inputs, or composing a shell command. Keep spaces/metacharacters as values.
$wrapper = Join-Path $PSScriptRoot 'run-windows-gpu-capture-observation.ps1'
$wrapperAst = [Management.Automation.Language.Parser]::ParseFile($wrapper, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'Capture wrapper does not parse.' }
$argumentBuilder = @($wrapperAst.FindAll({
    param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -ceq 'Get-CaptureObservationArguments'
}, $true))
if ($argumentBuilder.Count -ne 1) { throw 'Expected exactly one capture wrapper argument builder.' }
& {
    param([string]$Definition)
    . ([scriptblock]::Create($Definition))
    function Invoke-CaptureBoundArgumentFixture {
        param(
            [string]$ModelPath,
            [string]$ModelSha256,
            [string]$WavPath,
            [string]$WavSha256,
            [string]$GpuPackId,
            [string]$GpuBackend,
            [string]$GpuDevice,
            [string]$OutputPath,
            [string]$CampaignPower
        )
        # Production passes this dictionary, not a Hashtable. In particular,
        # Hashtable.Contains must not appear to work only in these fixtures.
        Get-CaptureObservationArguments $PSBoundParameters
    }
    foreach ($backend in @('cuda', 'vulkan')) {
        foreach ($power in @($null, 'ac', 'battery')) {
            $options = @{
                ModelPath = 'C:\fixture dir\model&(one).gguf'
                ModelSha256 = 'a' * 64
                WavPath = 'C:\fixture dir\input;audio.wav'
                WavSha256 = 'b' * 64
                GpuPackId = 'fixture-pack'
                GpuBackend = $backend
                GpuDevice = 'native:luid:0102030405060708'
                OutputPath = 'C:\fixture dir\new report.json'
            }
            if ($null -ne $power) { $options.CampaignPower = $power }
            $expected = @('--scribe-windows-gpu-capture-observation',
                '--model', $options.ModelPath, '--model-sha256', $options.ModelSha256,
                '--wav', $options.WavPath, '--wav-sha256', $options.WavSha256,
                '--gpu-pack-id', 'fixture-pack', '--gpu-backend', $backend,
                '--gpu-device', 'native:luid:0102030405060708', '--output', $options.OutputPath)
            if ($null -ne $power) { $expected += @('--campaign-power', $power) }
            $actual = @(Invoke-CaptureBoundArgumentFixture @options)
            if ($actual.Count -ne $expected.Count) { throw 'Capture wrapper changed its argument count.' }
            for ($index = 0; $index -lt $expected.Count; $index++) {
                if ($actual[$index] -cne $expected[$index]) {
                    throw 'Capture wrapper changed an input or the explicit campaign power.'
                }
            }
        }
    }
} -Definition $argumentBuilder[0].Extent.Text
Write-Output 'Capture wrapper forwarding contracts passed (6 cases); no collector was launched.'
