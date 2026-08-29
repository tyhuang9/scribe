[CmdletBinding()]
param(
    [string]$CargoTargetDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Invoke-NativeProcess(
    [string]$Executable,
    [string[]]$Arguments,
    [switch]$AllowFailure
) {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) {
        $startInfo.ArgumentList.Add($argument)
    }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            throw "Could not start $Executable."
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $result = [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout.GetAwaiter().GetResult()
            Stderr = $stderr.GetAwaiter().GetResult()
        }
        if (-not $AllowFailure -and $result.ExitCode -ne 0) {
            throw "$Executable failed with exit code $($result.ExitCode): $($result.Stderr.Trim())"
        }
        return $result
    }
    finally {
        $process.Dispose()
    }
}

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw $Message
    }
}

function New-FixturePayload([string]$Root) {
    $bin = Join-Path $Root 'bin'
    New-Item -ItemType Directory -Path $bin | Out-Null
    [System.IO.File]::WriteAllBytes(
        (Join-Path $bin 'scribe-inference-worker.exe'),
        [System.Text.Encoding]::UTF8.GetBytes('deterministic fixture worker')
    )
}

if ($env:OS -ne 'Windows_NT') {
    throw 'Windows worker-pack tooling tests require Windows.'
}

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$buildScript = Join-Path $PSScriptRoot 'build-windows-gpu-worker-pack.ps1'
$parseErrors = $null
[System.Management.Automation.Language.Parser]::ParseFile(
    $buildScript,
    [ref]$null,
    [ref]$parseErrors
) | Out-Null
Assert-True ($parseErrors.Count -eq 0) 'GPU worker-pack build script has PowerShell parse errors.'

$toolchainOutput = Join-Path ([System.IO.Path]::GetTempPath()) 'scribe-gpu-toolchain-check-unused'
& $buildScript `
    -Backend Vulkan `
    -PackVersion '0.1.0-fixture' `
    -OutputDirectory $toolchainOutput `
    -SigningMode Fixture `
    -ToolchainCheckOnly | Out-Null

$productionArgumentsFailedClosed = $false
try {
    & $buildScript `
        -Backend Vulkan `
        -PackVersion '0.1.0-production' `
        -OutputDirectory $toolchainOutput | Out-Null
}
catch {
    $productionArgumentsFailedClosed = $_.Exception.Message.Contains(
        'externally supplied PKCS#8 private key path and reviewed key ID'
    )
}
Assert-True $productionArgumentsFailedClosed 'Production pack build did not fail before compilation when signing authority was absent.'
Assert-True (-not (Test-Path -LiteralPath $toolchainOutput)) 'Failed production signing gate created a pack output.'

$previousCudaPath = $env:CUDA_PATH
try {
    $env:CUDA_PATH = Join-Path ([System.IO.Path]::GetTempPath()) 'scribe-absent-cuda-v12.8'
    $cudaFailedClosed = $false
    try {
        & $buildScript `
            -Backend Cuda `
            -PackVersion '0.1.0-fixture' `
            -OutputDirectory $toolchainOutput `
            -SigningMode Fixture `
            -ToolchainCheckOnly | Out-Null
    }
    catch {
        $cudaFailedClosed = $_.Exception.Message.Contains('Pinned CUDA Toolkit 12.8 is missing')
    }
    Assert-True $cudaFailedClosed 'Missing pinned CUDA Toolkit did not fail with the expected clear gate.'
}
finally {
    $env:CUDA_PATH = $previousCudaPath
}

if (-not $CargoTargetDirectory) {
    $CargoTargetDirectory = Join-Path $repositoryRoot 'target-gpu-pack-tool-tests'
}
$cargoTarget = [System.IO.Path]::GetFullPath($CargoTargetDirectory)
$cargo = (Get-Command cargo.exe -CommandType Application | Select-Object -First 1).Source
$git = (Get-Command git.exe -CommandType Application | Select-Object -First 1).Source
$revision = (Invoke-NativeProcess $git @('-C', $repositoryRoot, 'rev-parse', '--verify', 'HEAD')).Stdout.Trim()
$previousTarget = $env:CARGO_TARGET_DIR
$previousRevision = $env:SCRIBE_BUILD_REVISION
$previousWorkerDigest = $env:SCRIBE_BUNDLED_WORKER_SHA256
$previousBuildingWorker = $env:SCRIBE_BUILDING_WORKER
try {
    $env:CARGO_TARGET_DIR = $cargoTarget
    $env:SCRIBE_BUILD_REVISION = $revision
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $null
    $env:SCRIBE_BUILDING_WORKER = '1'
    $null = Invoke-NativeProcess $cargo @(
        'build', '--locked', '--offline',
        '--bin', 'scribe-worker-pack-tool',
        '--manifest-path', (Join-Path $repositoryRoot 'tools\worker-pack-author\Cargo.toml')
    )
}
finally {
    $env:CARGO_TARGET_DIR = $previousTarget
    $env:SCRIBE_BUILD_REVISION = $previousRevision
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $previousWorkerDigest
    $env:SCRIBE_BUILDING_WORKER = $previousBuildingWorker
}

$tool = Join-Path $cargoTarget 'debug\scribe-worker-pack-tool.exe'
Assert-True (Test-Path -LiteralPath $tool -PathType Leaf) 'Pack authoring tool was not built.'
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-pack-tools-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $testRoot | Out-Null
try {
    $first = Join-Path $testRoot 'first'
    $second = Join-Path $testRoot 'second'
    New-FixturePayload $first
    New-FixturePayload $second
    $common = @(
        'author',
        '--backend', 'vulkan',
        '--pack-id', 'scribe-vulkan-windows-x64',
        '--pack-version', '0.1.0-fixture',
        '--provider', 'transcribe-cpp-ggml-vulkan',
        '--security-epoch', '1',
        '--worker-path', 'bin/scribe-inference-worker.exe',
        '--fixture-signing'
    )
    $firstResult = Invoke-NativeProcess $tool ($common[0..6] + @('--pack-root', $first) + $common[7..($common.Count - 1)])
    $secondResult = Invoke-NativeProcess $tool ($common[0..6] + @('--pack-root', $second) + $common[7..($common.Count - 1)])
    $firstDescriptor = $firstResult.Stdout | ConvertFrom-Json
    $secondDescriptor = $secondResult.Stdout | ConvertFrom-Json
    Assert-True ($firstDescriptor.pack_digest -ceq $secondDescriptor.pack_digest) 'Deterministic fixture pack digests differ.'
    Assert-True (
        [System.Linq.Enumerable]::SequenceEqual(
            [System.IO.File]::ReadAllBytes((Join-Path $first 'pack-manifest.json')),
            [System.IO.File]::ReadAllBytes((Join-Path $second 'pack-manifest.json'))
        )
    ) 'Deterministic fixture manifests differ.'
    Assert-True (
        [System.Linq.Enumerable]::SequenceEqual(
            [System.IO.File]::ReadAllBytes((Join-Path $first 'pack-manifest.sig')),
            [System.IO.File]::ReadAllBytes((Join-Path $second 'pack-manifest.sig'))
        )
    ) 'Deterministic fixture signatures differ.'
    $verified = Invoke-NativeProcess $tool @('verify-fixture', '--pack-root', $first)
    Assert-True (($verified.Stdout | ConvertFrom-Json).pack_digest -ceq $firstDescriptor.pack_digest) 'Fixture verifier returned a mismatched digest.'

    [System.IO.File]::AppendAllText(
        (Join-Path $second 'bin\scribe-inference-worker.exe'),
        'tamper',
        [System.Text.UTF8Encoding]::new($false)
    )
    $tampered = Invoke-NativeProcess $tool @('verify-fixture', '--pack-root', $second) -AllowFailure
    Assert-True ($tampered.ExitCode -ne 0) 'Tampered fixture pack unexpectedly verified.'

    $production = Invoke-NativeProcess $tool @(
        'check-production-key',
        '--key-id', 'scribe-production-ed25519-v1',
        '--private-key', (Join-Path $testRoot 'missing-production-key.pk8')
    ) -AllowFailure
    Assert-True ($production.ExitCode -ne 0) 'Production signing unexpectedly accepted an unprovisioned key.'
    Assert-True ($production.Stderr.Contains('no separately reviewed public key embedded')) 'Production signing failure did not identify the missing embedded public key.'

    $productionRoot = Join-Path $testRoot 'production'
    New-FixturePayload $productionRoot
    $productionAuthor = Invoke-NativeProcess $tool @(
        'author',
        '--backend', 'cuda',
        '--key-id', 'scribe-production-ed25519-v1',
        '--pack-id', 'scribe-cuda-windows-x64',
        '--pack-root', $productionRoot,
        '--pack-version', '0.1.0-production',
        '--private-key', (Join-Path $testRoot 'missing-production-key.pk8'),
        '--provider', 'transcribe-cpp-ggml-cuda',
        '--security-epoch', '1',
        '--worker-path', 'bin/scribe-inference-worker.exe'
    ) -AllowFailure
    Assert-True ($productionAuthor.ExitCode -ne 0) 'Production pack authoring unexpectedly succeeded without provisioned trust.'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $productionRoot 'pack-manifest.json'))) 'Failed production authoring wrote a manifest.'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $productionRoot 'pack-manifest.sig'))) 'Failed production authoring wrote a signature.'

    $hardlinkRoot = Join-Path $testRoot 'hardlink'
    New-FixturePayload $hardlinkRoot
    New-Item -ItemType HardLink `
        -Path (Join-Path $hardlinkRoot 'bin\worker-alias.exe') `
        -Target (Join-Path $hardlinkRoot 'bin\scribe-inference-worker.exe') | Out-Null
    $hardlink = Invoke-NativeProcess $tool ($common[0..6] + @('--pack-root', $hardlinkRoot) + $common[7..($common.Count - 1)]) -AllowFailure
    Assert-True ($hardlink.ExitCode -ne 0) 'Hardlinked fixture payload unexpectedly authored.'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $hardlinkRoot 'pack-manifest.json'))) 'Hardlink rejection wrote a manifest.'
}
finally {
    $canonicalTemp = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\')
    $canonicalTestRoot = [System.IO.Path]::GetFullPath($testRoot).TrimEnd('\')
    if (-not $canonicalTestRoot.StartsWith("$canonicalTemp\scribe-pack-tools-", [System.StringComparison]::OrdinalIgnoreCase)) {
        throw 'Refusing to clean a worker-pack test directory outside the dedicated temp prefix.'
    }
    if (Test-Path -LiteralPath $canonicalTestRoot) {
        Remove-Item -LiteralPath $canonicalTestRoot -Recurse -Force
    }
}

Write-Output 'Windows GPU worker-pack build/signing contract tests passed.'
