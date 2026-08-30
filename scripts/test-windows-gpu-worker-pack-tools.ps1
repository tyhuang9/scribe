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
$prepareScript = Join-Path $PSScriptRoot 'prepare-windows-gpu-worker-packs.ps1'
foreach ($script in @(
    $buildScript,
    $prepareScript,
    (Join-Path $PSScriptRoot 'report-windows-worker-pack-sizes.ps1')
)) {
    $parseErrors = $null
    [System.Management.Automation.Language.Parser]::ParseFile(
        $script,
        [ref]$null,
        [ref]$parseErrors
    ) | Out-Null
    Assert-True ($parseErrors.Count -eq 0) "GPU worker-pack script has PowerShell parse errors: $script"
}
$prepareSource = Get-Content -LiteralPath $prepareScript -Raw
Assert-True `
    (-not $prepareSource.Contains('-CargoTargetDirectory')) `
    'Production pack preparation must let each builder allocate its fresh isolated LocalApplicationData target.'
$buildSource = Get-Content -LiteralPath $buildScript -Raw
Assert-True `
    (-not $buildSource.Contains('visual_studio_installation_version')) `
    'GPU pack builds must pin compiler payloads instead of the mutable Visual Studio shell version.'
foreach ($requiredToolchainBinding in @(
    'preferred_component_id',
    'toolset_version',
    'windows_sdk_version',
    'Invoke-PinnedVcVarsEnvironment',
    'Assert-PinnedMsvcTool',
    'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER',
    "CMAKE_GENERATOR'] = 'NMake Makefiles'"
)) {
    Assert-True `
        ($buildSource.Contains($requiredToolchainBinding)) `
        "GPU pack build lost exact toolchain binding: $requiredToolchainBinding"
}

$toolchainOutput = Join-Path ([System.IO.Path]::GetTempPath()) 'scribe-gpu-toolchain-check-unused'
$toolchainEnvironmentNames = @(
    'Path', 'INCLUDE', 'LIB', 'LIBPATH', 'VCINSTALLDIR',
    'VCToolsInstallDir', 'VCToolsVersion', 'VSINSTALLDIR',
    'WindowsSdkDir', 'WindowsSDKVersion', 'WindowsSdkBinPath',
    'WindowsSdkVerBinPath', 'UniversalCRTSdkDir', 'UCRTVersion',
    'Platform', 'VSCMD_ARG_HOST_ARCH', 'VSCMD_ARG_TGT_ARCH',
    'VSCMD_ARG_VCVARS_VER', 'VSCMD_ARG_winsdk', 'CC', 'CXX', 'AR',
    'CC_x86_64_pc_windows_msvc', 'CXX_x86_64_pc_windows_msvc',
    'AR_x86_64_pc_windows_msvc', 'CMAKE_C_COMPILER',
    'CMAKE_CXX_COMPILER', 'CMAKE_LINKER', 'CMAKE_AR',
    'CMAKE_MAKE_PROGRAM', 'CMAKE_GENERATOR',
    'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER'
)
$toolchainEnvironmentBefore = @{}
foreach ($name in $toolchainEnvironmentNames) {
    $value = Get-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
    $toolchainEnvironmentBefore[$name] = [pscustomobject]@{
        Exists = $null -ne $value
        Value = if ($null -eq $value) { $null } else { [string]$value.Value }
    }
}
& $buildScript `
    -Backend Vulkan `
    -PackVersion '0.1.0-fixture' `
    -OutputDirectory $toolchainOutput `
    -SigningMode Fixture `
    -ToolchainCheckOnly | Out-Null
foreach ($name in $toolchainEnvironmentNames) {
    $before = $toolchainEnvironmentBefore[$name]
    $after = Get-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
    Assert-True `
        (($null -ne $after) -eq $before.Exists) `
        "Toolchain-only validation changed whether process environment variable $name exists."
    if ($before.Exists) {
        Assert-True `
            ([string]$after.Value -ceq [string]$before.Value) `
            "Toolchain-only validation changed process environment variable $name."
    }
}

$toolchainManifest = Join-Path $repositoryRoot 'runtime-manifests\gpu-worker-toolchain-windows-x64.json'
$toolchainFixtureRoot = Join-Path `
    ([System.IO.Path]::GetTempPath()) `
    "scribe-toolchain-contract-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $toolchainFixtureRoot | Out-Null
try {
    $wrongHashContract = Get-Content -LiteralPath $toolchainManifest -Raw | ConvertFrom-Json
    $wrongHashContract.msvc.tools.cl.sha256 = '0' * 64
    $wrongHashPath = Join-Path $toolchainFixtureRoot 'wrong-cl-hash.json'
    [System.IO.File]::WriteAllText(
        $wrongHashPath,
        ($wrongHashContract | ConvertTo-Json -Depth 16),
        [System.Text.UTF8Encoding]::new($false)
    )
    $wrongHashRejected = $false
    try {
        & $buildScript `
            -Backend Vulkan `
            -PackVersion '0.1.0-fixture' `
            -OutputDirectory $toolchainOutput `
            -SigningMode Fixture `
            -ToolchainManifestPath $wrongHashPath `
            -ToolchainCheckOnly | Out-Null
    }
    catch {
        $wrongHashRejected = $_.Exception.Message.Contains(
            'Pinned MSVC compiler SHA-256 mismatch'
        )
    }
    Assert-True $wrongHashRejected 'Wrong pinned compiler identity was not rejected.'

    $wrongComponentContract = Get-Content -LiteralPath $toolchainManifest -Raw | ConvertFrom-Json
    $wrongComponentContract.msvc.preferred_component_id = `
        'Microsoft.VisualStudio.Component.VC.14.43.17.13.x86.x64'
    $wrongComponentPath = Join-Path $toolchainFixtureRoot 'wrong-component.json'
    [System.IO.File]::WriteAllText(
        $wrongComponentPath,
        ($wrongComponentContract | ConvertTo-Json -Depth 16),
        [System.Text.UTF8Encoding]::new($false)
    )
    $wrongComponentRejected = $false
    try {
        & $buildScript `
            -Backend Vulkan `
            -PackVersion '0.1.0-fixture' `
            -OutputDirectory $toolchainOutput `
            -SigningMode Fixture `
            -ToolchainManifestPath $wrongComponentPath `
            -ToolchainCheckOnly | Out-Null
    }
    catch {
        $wrongComponentRejected = $_.Exception.Message.Contains(
            'violates the reviewed Windows x64 static-runtime contract'
        )
    }
    Assert-True $wrongComponentRejected 'Wrong MSVC compatibility component was not rejected.'
}
finally {
    Remove-Item -LiteralPath $toolchainFixtureRoot -Recurse -Force -ErrorAction SilentlyContinue
}

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

$longTargetRejected = $false
try {
    & $buildScript `
        -Backend Vulkan `
        -PackVersion '0.1.0-fixture' `
        -OutputDirectory $toolchainOutput `
        -SigningMode Fixture `
        -CargoTargetDirectory (Join-Path $repositoryRoot 'target-native-build-not-short') | Out-Null
}
catch {
    $longTargetRejected = $_.Exception.Message.Contains(
        'one direct child of the short LocalApplicationData build root'
    )
}
Assert-True $longTargetRejected 'Native GPU pack build accepted a repository-local Cargo target.'
Assert-True (-not (Test-Path -LiteralPath $toolchainOutput)) 'Rejected native build target created a pack output.'

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

    $unexpectedRoot = Join-Path $testRoot 'unexpected'
    New-FixturePayload $unexpectedRoot
    $null = Invoke-NativeProcess $tool ($common[0..6] + @('--pack-root', $unexpectedRoot) + $common[7..($common.Count - 1)])
    [System.IO.File]::WriteAllBytes(
        (Join-Path $unexpectedRoot 'bin\unexpected-provider.dll'),
        [byte[]](1, 2, 3)
    )
    $unexpected = Invoke-NativeProcess $tool @('verify-fixture', '--pack-root', $unexpectedRoot) -AllowFailure
    Assert-True ($unexpected.ExitCode -ne 0) 'Unexpected DLL outside the signed inventory was accepted.'

    $signatureRoot = Join-Path $testRoot 'signature'
    New-FixturePayload $signatureRoot
    $null = Invoke-NativeProcess $tool ($common[0..6] + @('--pack-root', $signatureRoot) + $common[7..($common.Count - 1)])
    $signaturePath = Join-Path $signatureRoot 'pack-manifest.sig'
    $signature = Get-Content -LiteralPath $signaturePath -Raw | ConvertFrom-Json
    $replacementNibble = if ($signature.signature_hex.StartsWith('0')) { '1' } else { '0' }
    $signature.signature_hex = ($replacementNibble + $signature.signature_hex.Substring(1))
    [System.IO.File]::WriteAllText(
        $signaturePath,
        ($signature | ConvertTo-Json -Compress),
        [System.Text.UTF8Encoding]::new($false)
    )
    $badSignature = Invoke-NativeProcess $tool @('verify-fixture', '--pack-root', $signatureRoot) -AllowFailure
    Assert-True ($badSignature.ExitCode -ne 0) 'Mismatched manifest signature was accepted.'

    $wrongKeyRoot = Join-Path $testRoot 'wrong-key'
    New-FixturePayload $wrongKeyRoot
    $null = Invoke-NativeProcess $tool ($common[0..6] + @('--pack-root', $wrongKeyRoot) + $common[7..($common.Count - 1)])
    $wrongKeyPath = Join-Path $wrongKeyRoot 'pack-manifest.sig'
    $wrongKey = Get-Content -LiteralPath $wrongKeyPath -Raw | ConvertFrom-Json
    $wrongKey.key_id = 'fixture-ed25519-v2'
    [System.IO.File]::WriteAllText(
        $wrongKeyPath,
        ($wrongKey | ConvertTo-Json -Compress),
        [System.Text.UTF8Encoding]::new($false)
    )
    $wrongKeyResult = Invoke-NativeProcess $tool @('verify-fixture', '--pack-root', $wrongKeyRoot) -AllowFailure
    Assert-True ($wrongKeyResult.ExitCode -ne 0) 'Unknown fixture signing key ID was accepted.'

    $adsRoot = Join-Path $testRoot 'ads'
    New-FixturePayload $adsRoot
    [System.IO.File]::WriteAllText(
        "$(Join-Path $adsRoot 'bin\scribe-inference-worker.exe'):hidden",
        'hidden stream',
        [System.Text.UTF8Encoding]::new($false)
    )
    $ads = Invoke-NativeProcess $tool ($common[0..6] + @('--pack-root', $adsRoot) + $common[7..($common.Count - 1)]) -AllowFailure
    Assert-True ($ads.ExitCode -ne 0) 'Alternate data stream payload unexpectedly authored.'

    $junctionRoot = Join-Path $testRoot 'junction'
    $junctionTarget = Join-Path $testRoot 'junction-target'
    New-FixturePayload $junctionRoot
    New-Item -ItemType Directory -Path $junctionTarget | Out-Null
    New-Item -ItemType Junction -Path (Join-Path $junctionRoot 'bin\linked') -Target $junctionTarget | Out-Null
    $junction = Invoke-NativeProcess $tool ($common[0..6] + @('--pack-root', $junctionRoot) + $common[7..($common.Count - 1)]) -AllowFailure
    Assert-True ($junction.ExitCode -ne 0) 'Junction payload unexpectedly authored.'

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
