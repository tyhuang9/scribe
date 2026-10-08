$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Exercise the real runner helpers in an isolated scope with a fake Cargo
# command. No compiler, process, provider or fixture file is needed here.
$runner = Join-Path $PSScriptRoot 'test-windows-gpu-capture-observation.ps1'
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($runner, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'Capture runner does not parse.' }
$captureTestLoops = @($ast.FindAll({
    param($node)
    $node -is [Management.Automation.Language.ForEachStatementAst] -and
        $node.Body.Extent.Text -cmatch 'Invoke-CaptureTests\s+\$feature\s+\$filter'
}, $true))
$providerTestLoops = @($ast.FindAll({
    param($node)
    $node -is [Management.Automation.Language.ForEachStatementAst] -and
        $node.Body.Extent.Text -cmatch 'Invoke-CaptureTests\s+"ui-harness,\$providerFeature"\s+\$filter'
}, $true))
if ($captureTestLoops.Count -ne 1 -or $providerTestLoops.Count -ne 1) {
    throw 'Capture runner must have one canonical and one provider-specific test group.'
}
function Get-LoopFilterLiterals([Management.Automation.Language.ForEachStatementAst]$Loop) {
    @($Loop.Condition.FindAll({
        param($node)
        $node -is [Management.Automation.Language.StringConstantExpressionAst]
    }, $true) | ForEach-Object Value)
}
$expectedCaptureFilters = @(
    'windows_gpu_capture::tests',
    'windows_gpu_capture::telemetry::tests',
    'windows_gpu_capture::campaign::tests',
    'windows_gpu_probe::tests',
    'onnx_worker::tests::gpu_pack_probe',
    'onnx_worker::tests::capture_observation',
    'onnx_worker::tests::vulkan_hello_memory',
    'embedded_runtime::tests::provider_memory_observation',
    'architecture_guard::windows_gpu_capture',
    'architecture_guard::windows_gpu_probe'
)
$expectedProviderFilters = @(
    'onnx_worker::tests::capture_observation',
    'onnx_worker::tests::vulkan_hello_memory',
    'embedded_runtime::tests::provider_memory_observation'
)
$actualCaptureFilters = @(Get-LoopFilterLiterals -Loop $captureTestLoops[0])
$actualProviderFilters = @(Get-LoopFilterLiterals -Loop $providerTestLoops[0])
if (($actualCaptureFilters -join "`0") -cne ($expectedCaptureFilters -join "`0") -or
    ($actualProviderFilters -join "`0") -cne ($expectedProviderFilters -join "`0")) {
    throw 'Capture runner changed its exact canonical Hello-memory test commands.'
}
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

# Exercise the full wrapper with a GUI executable, not a console mock. Named
# events control completion, so no sleep or GPU/app/profile access is needed.
$fixtureParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
$fixtureRoot = Join-Path $fixtureParent ('scribe-capture-gui-' + [guid]::NewGuid().ToString('N'))
$fixtureSource = Join-Path $fixtureRoot 'collector.cs'
$fixtureExe = Join-Path $fixtureRoot 'collector space Ω.exe'
$fixtureHost = Join-Path $fixtureRoot 'collector-host.ps1'
$savedFixtureExitCode = Get-Variable LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue
$hadFixtureExitCode = $null -ne $savedFixtureExitCode
$originalFixtureExitCode = if ($hadFixtureExitCode) { $savedFixtureExitCode.Value } else { $null }
$fixtureFailed = $false
$null = New-Item -ItemType Directory -Path $fixtureRoot
try {
    [IO.File]::WriteAllText($fixtureSource, @'
using System;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Threading;
class CaptureFixture {
  static int Main(string[] args) {
    int outputIndex = Array.IndexOf(args, "--output");
    if (outputIndex < 0 || outputIndex + 1 >= args.Length) return 90;
    string output = args[outputIndex + 1];
    string id = Path.GetFileNameWithoutExtension(output);
    using (var started = EventWaitHandle.OpenExisting(@"Local\ScribeCaptureStarted-" + id))
    using (var gate = EventWaitHandle.OpenExisting(@"Local\ScribeCaptureGate-" + id)) {
      File.WriteAllLines(output, args);
      File.WriteAllText(output + ".pid", Process.GetCurrentProcess().Id.ToString(CultureInfo.InvariantCulture));
      started.Set();
      if (!gate.WaitOne(10000)) return 91;
      return Int32.Parse(id.Split('_')[0], CultureInfo.InvariantCulture);
    }
  }
}
'@, [Text.UTF8Encoding]::new($false))
    # A disposable host also releases PowerShell's native GUI-command handles
    # when testing the old wrapper's early-return failure path.
    [IO.File]::WriteAllText($fixtureHost, @'
param([string]$Wrapper, [string]$OptionsPath, [int]$Previous)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$options = Get-Content -LiteralPath $OptionsPath -Raw | ConvertFrom-Json -AsHashtable
if ($Previous -lt 0) { Remove-Variable LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue }
else { $global:LASTEXITCODE = $Previous }
try { & $Wrapper @options; exit 0 }
catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }
'@, [Text.UTF8Encoding]::new($false))
    $csharpCompiler = 'C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe'
    if (-not (Test-Path -LiteralPath $csharpCompiler -PathType Leaf)) {
        throw 'The Windows .NET Framework C# compiler is required for the GUI capture fixture.'
    }
    & $csharpCompiler '/nologo' '/target:winexe' '/platform:x64' "/out:$fixtureExe" $fixtureSource
    if ($LASTEXITCODE -ne 0) { throw 'Could not compile the GUI capture fixture.' }
    $fixtureBytes = [IO.File]::ReadAllBytes($fixtureExe)
    $peOffset = [BitConverter]::ToInt32($fixtureBytes, 60)
    if ([BitConverter]::ToUInt16($fixtureBytes, $peOffset + 4) -ne 0x8664 -or
        [BitConverter]::ToUInt16($fixtureBytes, $peOffset + 92) -ne 2) {
        throw 'Capture regression fixture must be a Windows x64 GUI executable.'
    }
    $fixtureHash = (Get-FileHash -LiteralPath $fixtureExe -Algorithm SHA256).Hash.ToLowerInvariant()
    foreach ($case in @(
        @{ Exit = 0; Previous = -1 },
        @{ Exit = 0; Previous = 42 },
        @{ Exit = 23; Previous = 0 }
    )) {
        $id = "$($case.Exit)_$([guid]::NewGuid().ToString('N'))"
        $output = Join-Path $fixtureRoot "$id.txt"
        $started = [Threading.EventWaitHandle]::new($false, [Threading.EventResetMode]::ManualReset, "Local\ScribeCaptureStarted-$id")
        $gate = [Threading.EventWaitHandle]::new($false, [Threading.EventResetMode]::ManualReset, "Local\ScribeCaptureGate-$id")
        $hostProcess = [Diagnostics.Process]::new()
        $hostStarted = $false
        try {
            $options = @{
                CollectorPath = $fixtureExe; CollectorSha256 = $fixtureHash
                ModelPath = (Join-Path $fixtureRoot "model Ω & '(one).gguf"); ModelSha256 = 'a' * 64
                WavPath = (Join-Path $fixtureRoot 'audio;input.wav'); WavSha256 = 'b' * 64
                GpuPackId = 'fixture-pack'; GpuBackend = 'vulkan'
                GpuDevice = 'native:luid:0102030405060708'
                OutputPath = $output; CampaignPower = 'battery'
            }
            $optionsPath = Join-Path $fixtureRoot "$id-options.json"
            [IO.File]::WriteAllText($optionsPath, ($options | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
            $hostStartInfo = [Diagnostics.ProcessStartInfo]::new()
            $hostStartInfo.FileName = Join-Path $PSHOME 'pwsh.exe'
            $hostStartInfo.UseShellExecute = $false
            $hostStartInfo.CreateNoWindow = $true
            $hostStartInfo.RedirectStandardOutput = $true
            $hostStartInfo.RedirectStandardError = $true
            foreach ($argument in @('-NoLogo', '-NoProfile', '-NonInteractive', '-File', $fixtureHost,
                '-Wrapper', $wrapper, '-OptionsPath', $optionsPath, '-Previous', [string]$case.Previous)) {
                $hostStartInfo.ArgumentList.Add($argument)
            }
            $hostProcess.StartInfo = $hostStartInfo
            $hostStarted = $hostProcess.Start()
            if (-not $hostStarted) { throw 'Could not start the isolated capture fixture host.' }
            $hostStdout = $hostProcess.StandardOutput.ReadToEndAsync()
            $hostStderr = $hostProcess.StandardError.ReadToEndAsync()
            if (-not $started.WaitOne(10000)) { throw 'GUI fixture did not signal startup.' }
            if ($hostProcess.HasExited) { throw 'Capture wrapper returned while its GUI collector was still running.' }
            $expectedArguments = @(& {
                param($Definition, $Options)
                . ([scriptblock]::Create($Definition))
                Get-CaptureObservationArguments $Options
            } $argumentBuilder[0].Extent.Text $options)
            $actualArguments = @([IO.File]::ReadAllLines($output))
            if ($actualArguments.Count -ne $expectedArguments.Count) { throw 'GUI collector argument count differs.' }
            for ($index = 0; $index -lt $expectedArguments.Count; $index++) {
                if ($actualArguments[$index] -cne $expectedArguments[$index]) { throw 'GUI collector literal argument differs.' }
            }
            $null = $gate.Set()
            if (-not $hostProcess.WaitForExit(15000)) { throw 'Capture wrapper did not observe GUI termination.' }
            $expectedError = if ($case.Exit -eq 0) { '' } else {
                'Capture observation failed; no qualification or release approval was produced.'
            }
            $expectedExit = if ($case.Exit -eq 0) { 0 } else { 1 }
            if ($hostProcess.ExitCode -ne $expectedExit -or
                $hostStdout.GetAwaiter().GetResult().Trim() -cne '' -or
                $hostStderr.GetAwaiter().GetResult().Trim() -cne $expectedError) {
                throw 'Capture wrapper ignored the GUI process exit status or used stale LASTEXITCODE.'
            }
            if (-not (Test-Path -LiteralPath $output -PathType Leaf)) { throw 'Capture wrapper removed the fixture report.' }
            $exclusive = [IO.File]::Open($fixtureExe, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
            $exclusive.Dispose()
        }
        finally {
            $null = $gate.Set()
            if ($hostStarted -and -not $hostProcess.WaitForExit(15000)) {
                $hostProcess.Kill($true)
                if (-not $hostProcess.WaitForExit(10000)) { throw 'Owned capture fixture host did not terminate.' }
            }
            $hostProcess.Dispose()
            if (Test-Path -LiteralPath "$output.pid" -PathType Leaf) {
                $fixturePid = [int]([IO.File]::ReadAllText("$output.pid"))
                $ownedProcess = Get-Process -Id $fixturePid -ErrorAction SilentlyContinue
                if ($null -ne $ownedProcess) {
                    try {
                        if (-not $ownedProcess.WaitForExit(10000)) {
                            if (-not [string]::Equals($ownedProcess.MainModule.FileName, $fixtureExe, [StringComparison]::OrdinalIgnoreCase)) {
                                throw 'Refusing cleanup of a process outside the GUI fixture.'
                            }
                            $ownedProcess.Kill()
                            if (-not $ownedProcess.WaitForExit(10000)) { throw 'Owned GUI fixture did not terminate.' }
                        }
                    }
                    finally { $ownedProcess.Dispose() }
                }
            }
            $gate.Dispose()
            $started.Dispose()
        }
    }
    # A valid digest does not make a malformed executable launchable. Startup
    # failure must also release the read lock, without producing a report.
    $invalidExe = Join-Path $fixtureRoot 'invalid.exe'
    [IO.File]::WriteAllBytes($invalidExe, [byte[]]@(0, 1, 2, 3))
    $options.CollectorPath = $invalidExe
    $options.CollectorSha256 = (Get-FileHash -LiteralPath $invalidExe -Algorithm SHA256).Hash.ToLowerInvariant()
    $options.OutputPath = Join-Path $fixtureRoot 'invalid-output.txt'
    $failed = $false
    try { & $wrapper @options }
    catch { $failed = $true }
    if (-not $failed) { throw 'Capture wrapper accepted a malformed executable.' }
    if (Test-Path -LiteralPath $options.OutputPath) { throw 'Malformed executable produced a report.' }
    $exclusive = [IO.File]::Open($invalidExe, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    $exclusive.Dispose()
}
catch {
    $fixtureFailed = $true
    throw
}
finally {
    if (-not $hadFixtureExitCode) { Remove-Variable LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue }
    else { Set-Variable LASTEXITCODE -Scope Global -Value $originalFixtureExitCode }
    # This exact test-owned directory contains only generated regular files.
    if ([IO.Path]::GetFullPath($fixtureRoot) -cne $fixtureRoot -or
        -not [string]::Equals((Split-Path -Parent $fixtureRoot), $fixtureParent, [StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path -Leaf $fixtureRoot) -cnotmatch '\Ascribe-capture-gui-[0-9a-f]{32}\z' -or
        ((Get-Item -LiteralPath $fixtureRoot -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'Refusing GUI fixture cleanup outside the exact owned directory.'
    }
    foreach ($item in @(Get-ChildItem -LiteralPath $fixtureRoot -Force)) {
        if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw 'Refusing GUI fixture cleanup with unexpected directory/link content.'
        }
    }
    try { Remove-Item -LiteralPath $fixtureRoot -Recurse -Force }
    catch {
        if (-not $fixtureFailed) { throw }
        # Preserve the original assertion failure if Windows still holds a
        # generated image open. Report retention instead of masking that error.
        Write-Warning "Failed GUI fixture cleanup; retained generated files at $fixtureRoot"
    }
}
Write-Output 'Capture wrapper GUI process contracts passed (4 cases); local synthetic executable only, no GPU or Cargo process.'
