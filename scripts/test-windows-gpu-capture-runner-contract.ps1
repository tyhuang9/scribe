$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Exercise the real runner helpers in an isolated scope with a fake Cargo
# command. No compiler, process, provider or fixture file is needed here.
$runner = Join-Path $PSScriptRoot 'test-windows-gpu-capture-observation.ps1'
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($runner, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'Capture runner does not parse.' }
$definitions = foreach ($name in @('Invoke-CaptureCargo', 'Invoke-CaptureTests')) {
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
