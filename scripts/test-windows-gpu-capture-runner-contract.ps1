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
