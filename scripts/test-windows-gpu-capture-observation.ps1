[CmdletBinding()]
param(
    [string]$CargoTargetDirectory,
    [string]$NativeArchiveDirectory,
    [switch]$ScriptOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or -not [Environment]::Is64BitProcess) {
    throw 'Capture observation verification requires 64-bit PowerShell on Windows.'
}

$repositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$previousTarget = $env:CARGO_TARGET_DIR
$previousArchive = $env:SHERPA_ONNX_ARCHIVE_DIR
$feature = 'windows-gpu-capture-observation'

function Invoke-CaptureCargo([string[]]$Arguments) {
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) { throw 'Capture observation Cargo verification failed.' }
}

Push-Location $repositoryRoot
try {
    if ($CargoTargetDirectory) { $env:CARGO_TARGET_DIR = [IO.Path]::GetFullPath($CargoTargetDirectory) }
    if ($NativeArchiveDirectory) { $env:SHERPA_ONNX_ARCHIVE_DIR = [IO.Path]::GetFullPath($NativeArchiveDirectory) }

    foreach ($script in @($PSCommandPath, (Join-Path $PSScriptRoot 'run-windows-gpu-capture-observation.ps1'))) {
        $tokens = $null
        $parseErrors = $null
        $null = [Management.Automation.Language.Parser]::ParseFile($script, [ref]$tokens, [ref]$parseErrors)
        if ($parseErrors.Count -ne 0) { throw 'Capture observation script does not parse.' }
    }

    $wrapper = Join-Path $PSScriptRoot 'run-windows-gpu-capture-observation.ps1'
    $wrapperArguments = @{
        CollectorPath = (Join-Path $repositoryRoot 'not-a-real-collector.exe')
        CollectorSha256 = ('a' * 64)
        ModelPath = (Join-Path $repositoryRoot 'not-a-real-model.gguf')
        ModelSha256 = ('b' * 64)
        WavPath = (Join-Path $repositoryRoot 'not-a-real-input.wav')
        WavSha256 = ('c' * 64)
        GpuPackId = 'test-pack'
        GpuBackend = 'cuda'
        GpuDevice = 'native:0000:01:00.0'
        OutputPath = (Join-Path $repositoryRoot 'not-a-real-report.json')
    }
    # Each mutation must fail before file access or native executable launch.
    # These argument-only cases create no executables, models, audio or reports.
    foreach ($field in @('CollectorPath', 'ModelPath', 'WavPath', 'OutputPath',
            'CollectorSha256', 'ModelSha256', 'WavSha256', 'GpuBackend')) {
        $arguments = $wrapperArguments.Clone()
        $expected = if ($field.EndsWith('Path')) {
            $arguments[$field] = 'relative-input'
            'Capture observation paths must be absolute.'
        }
        elseif ($field.EndsWith('Sha256')) {
            $arguments[$field] = 'A' * 64
            'Capture observation digests must be lowercase SHA-256.'
        }
        else {
            $arguments[$field] = 'CUDA'
            'Capture observation backend must be lowercase cuda or vulkan.'
        }
        $rejected = $false
        try { & $wrapper @arguments }
        catch {
            if ($_.Exception.Message -cne $expected) { throw 'Capture wrapper rejected an input at the wrong boundary.' }
            $rejected = $true
        }
        if (-not $rejected) { throw 'Capture wrapper accepted a malformed input.' }
    }

    $fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('scribe-capture-wrapper-' + [Guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Path $fixtureRoot -ErrorAction Stop
    $emptyExe = Join-Path $fixtureRoot 'empty.exe'
    $notExe = Join-Path $fixtureRoot 'not-executable.bin'
    $wrongDigestExe = Join-Path $fixtureRoot 'wrong-digest.exe'
    $ownedFiles = @($emptyExe, $notExe, $wrongDigestExe)
    try {
        foreach ($path in $ownedFiles) {
            $fixture = [IO.File]::Open($path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try { if ($path -cne $emptyExe) { $fixture.Write([byte[]]@(1, 2, 3)) } }
            finally { $fixture.Dispose() }
        }
        foreach ($path in @($fixtureRoot) + $ownedFiles) {
            $arguments = $wrapperArguments.Clone()
            $arguments.CollectorPath = $path
            $expected = if ($path -ceq $wrongDigestExe) {
                'Collector digest does not match the expected build.'
            }
            else { 'Collector must be a bounded regular executable in a trusted directory.' }
            $rejected = $false
            try { & $wrapper @arguments }
            catch {
                if ($_.Exception.Message -cne $expected) { throw 'Capture wrapper file validation failed at the wrong boundary.' }
                $rejected = $true
            }
            if (-not $rejected) { throw 'Capture wrapper accepted an invalid collector file.' }
        }
        # A rejected digest must release the read lock; none of these fixtures
        # is runnable, and no test is allowed to reach executable launch.
        $exclusive = [IO.File]::Open($wrongDigestExe, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        $exclusive.Dispose()
    }
    finally {
        # Delete only these three explicitly owned files, then the empty owned
        # directory. Never recursively delete a shared or computed temp tree.
        foreach ($path in $ownedFiles) {
            if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -ErrorAction Stop }
        }
        Remove-Item -LiteralPath $fixtureRoot -ErrorAction Stop
    }
    if ($ScriptOnly) {
        Write-Output 'Windows GPU capture observation script contracts passed (12 prelaunch cases); native checks not run.'
        return
    }

    Invoke-CaptureCargo @('fmt', '--all', '--', '--check')
    Invoke-CaptureCargo @('check', '--locked', '--offline', '--bin', 'local-transcriber')
    Invoke-CaptureCargo @('check', '--locked', '--offline', '--bin', 'local-transcriber', '--features', $feature)
    # The authenticated responder is in ordinary worker builds, while only the
    # opt-in collector initiates observation. Check that independent boundary.
    Invoke-CaptureCargo @('check', '--locked', '--offline', '--bin', 'scribe-inference-worker', '--features', 'inference-worker')
    # Match the existing release lint's shared UI-route coverage while keeping
    # both production checks above free of test-only UI features. Neither lint
    # configuration enables an inference provider in the desktop process.
    Invoke-CaptureCargo @('clippy', '--locked', '--offline', '--bin', 'local-transcriber', '--features', 'ui-harness', '--', '-D', 'warnings')
    Invoke-CaptureCargo @('clippy', '--locked', '--offline', '--bin', 'local-transcriber', '--features', "ui-harness,$feature", '--', '-D', 'warnings')

    # Cargo normally succeeds for an empty filter. Require discovery before
    # executing each group so a disabled module cannot produce a false pass.
    foreach ($filter in @('windows_gpu_capture::tests', 'windows_gpu_capture::telemetry::tests',
            'onnx_worker::tests::capture_observation', 'embedded_runtime::tests::provider_memory_observation',
            'architecture_guard::windows_gpu_capture')) {
        $listing = @(& cargo test --locked --offline --bin local-transcriber --features $feature $filter -- --list)
        if ($LASTEXITCODE -ne 0) { throw 'Capture observation test discovery failed.' }
        $pattern = '^' + [Regex]::Escape($filter) + '[^\r\n]*: test$'
        $tests = @($listing | Where-Object { $_ -cmatch $pattern })
        if ($tests.Count -eq 0) { throw "Expected capture observation tests were not discovered: $filter" }
        Invoke-CaptureCargo @('test', '--locked', '--offline', '--bin', 'local-transcriber', '--features', $feature, $filter, '--', '--test-threads=1')
    }
    Write-Output 'Windows GPU capture observation verification passed.'
}
finally {
    $env:CARGO_TARGET_DIR = $previousTarget
    $env:SHERPA_ONNX_ARCHIVE_DIR = $previousArchive
    Pop-Location
}
