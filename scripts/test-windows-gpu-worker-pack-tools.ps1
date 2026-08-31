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
$cmakeBootstrapScript = Join-Path $PSScriptRoot 'windows-gpu-worker-cmake-bootstrap.ps1'
$prepareScript = Join-Path $PSScriptRoot 'prepare-windows-gpu-worker-packs.ps1'
$cudaInventoryScript = Join-Path $PSScriptRoot 'windows-cuda-sdk-inventory.ps1'
$autoQualificationReportScript = Join-Path $PSScriptRoot 'report-windows-gpu-auto-qualification.ps1'
foreach ($script in @(
    $buildScript,
    $cmakeBootstrapScript,
    $prepareScript,
    $cudaInventoryScript,
    (Join-Path $PSScriptRoot 'report-windows-worker-pack-sizes.ps1'),
    $autoQualificationReportScript
)) {
    $parseErrors = $null
    [System.Management.Automation.Language.Parser]::ParseFile(
        $script,
        [ref]$null,
        [ref]$parseErrors
    ) | Out-Null
    Assert-True ($parseErrors.Count -eq 0) "GPU worker-pack script has PowerShell parse errors: $script"
}
. $cmakeBootstrapScript
$legacyCmakeBootstrapFailure = @('transcribe-cpp-sys v0.1.3', 'The directory name is invalid. (os error 267)')
if (-not (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure $legacyCmakeBootstrapFailure)) { throw 'Existing os-error-267 CMake bootstrap signature regressed.' }
if (-not (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure @('transcribe-cpp-sys v0.1.3', 'Could not open file for write in copy operation'))) { throw 'Existing copy-operation CMake bootstrap signature regressed.' }
$vulkanShortJunctionFailure = @(
    'transcribe-cpp-sys: could not create short build junction C:\safe\tcs; building in OUT_DIR (may exceed Windows MAX_PATH in deep checkouts)',
    'error: failed to run custom build command for `transcribe-cpp-sys v0.1.3`',
    'vulkan-shaders-gen: warning: object directory is near the configured limit',
    'CMAKE_OBJECT_PATH_MAX is in effect for this nested target',
    "LINK : fatal error LNK1104: cannot open file 'CMakeFiles\cmTC_1a2B3c.dir\intermediate.manifest'"
)
if (-not (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure $vulkanShortJunctionFailure)) { throw 'Sanitized observed Vulkan short-junction CMake excerpt was not classified.' }
$capturedVulkanShortJunctionFailure = [System.Collections.Generic.List[object]]::new()
foreach ($index in 1..157) {
    $line = switch ($index) {
        4 { $vulkanShortJunctionFailure[0] }
        5 { $vulkanShortJunctionFailure[1] }
        62 { $vulkanShortJunctionFailure[2] }
        81 { $vulkanShortJunctionFailure[3] }
        129 { $vulkanShortJunctionFailure[4] }
        default { 'sanitized Cargo/CMake diagnostic output' }
    }
    $capturedVulkanShortJunctionFailure.Add($line)
}
if (-not (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure $capturedVulkanShortJunctionFailure.ToArray())) { throw 'Sanitized 157-line observed Vulkan CMake ordering was not classified.' }
$wrappedProcessStartInfoVulkanFailure = [System.Collections.Generic.List[object]]::new()
foreach ($index in 1..153) {
    $line = switch ($index) {
        1 { $vulkanShortJunctionFailure[0] }
        2 { $vulkanShortJunctionFailure[1] }
        58 { $vulkanShortJunctionFailure[2] }
        77 { $vulkanShortJunctionFailure[3] }
        125 { $vulkanShortJunctionFailure[4] }
        default { 'sanitized ProcessStartInfo Cargo/CMake diagnostic output' }
    }
    $wrappedProcessStartInfoVulkanFailure.Add($line)
}
$wrappedProcessStartInfoVulkanFailure = $wrappedProcessStartInfoVulkanFailure -join "`r`n"
if (-not (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure $wrappedProcessStartInfoVulkanFailure)) { throw 'Single-string 153-line ProcessStartInfo Vulkan CMake diagnostic was not classified.' }
$capturedVulkanShortJunctionCrlf = $capturedVulkanShortJunctionFailure -join "`r`n"
$capturedVulkanShortJunctionLf = $capturedVulkanShortJunctionFailure -join "`n"
if (-not (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure $capturedVulkanShortJunctionCrlf) -or
    -not (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure $capturedVulkanShortJunctionLf)) {
    throw 'Single-string CRLF/LF Vulkan CMake diagnostics were not classified.'
}
$reverseCapturedVulkanShortJunctionFailure = [System.Collections.Generic.List[object]]::new($capturedVulkanShortJunctionFailure)
$reverseCapturedVulkanShortJunctionFailure[3] = $vulkanShortJunctionFailure[1]
$reverseCapturedVulkanShortJunctionFailure[4] = $vulkanShortJunctionFailure[0]
if (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure ($reverseCapturedVulkanShortJunctionFailure -join "`r`n")) { throw 'Reverse observed Vulkan CMake ordering was classified.' }
$missingCapturedVulkanShortJunctionFailure = [System.Collections.Generic.List[object]]::new($capturedVulkanShortJunctionFailure)
$missingCapturedVulkanShortJunctionFailure[80] = 'sanitized Cargo/CMake diagnostic output'
if (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure ($missingCapturedVulkanShortJunctionFailure -join "`n")) { throw 'Missing observed Vulkan CMake marker was classified.' }
$interveningVulkanShortJunctionFailure = [System.Collections.Generic.List[object]]::new()
$interveningVulkanShortJunctionFailure.Add($vulkanShortJunctionFailure[0])
$interveningVulkanShortJunctionFailure.Add($vulkanShortJunctionFailure[1])
foreach ($unused in 1..600) { $interveningVulkanShortJunctionFailure.Add('realistic CMake compile output') }
$interveningVulkanShortJunctionFailure.Add($vulkanShortJunctionFailure[2])
foreach ($unused in 1..600) { $interveningVulkanShortJunctionFailure.Add('realistic nested CMake output') }
$interveningVulkanShortJunctionFailure.Add($vulkanShortJunctionFailure[3])
foreach ($unused in 1..600) { $interveningVulkanShortJunctionFailure.Add('realistic CMake try-compile output') }
$interveningVulkanShortJunctionFailure.Add($vulkanShortJunctionFailure[4])
if (-not (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure $interveningVulkanShortJunctionFailure.ToArray())) { throw 'Bounded Vulkan CMake classifier lost ordered markers amid realistic output.' }
foreach ($malformedVulkanShortJunctionFailure in @(
    @($vulkanShortJunctionFailure | Select-Object -Skip 1),
    @($vulkanShortJunctionFailure[1], $vulkanShortJunctionFailure[0], $vulkanShortJunctionFailure[4], $vulkanShortJunctionFailure[2], $vulkanShortJunctionFailure[3]),
    @($vulkanShortJunctionFailure[0], 'error: failed to run custom build command for `transcribe-cpp-sys v0.1.3', $vulkanShortJunctionFailure[2], $vulkanShortJunctionFailure[3], $vulkanShortJunctionFailure[4]),
    @('transcribe-cpp-sys: could not create short build junction C:\safe\tcs; building in OUT_DIR (unexpected suffix)', $vulkanShortJunctionFailure[1], $vulkanShortJunctionFailure[2], $vulkanShortJunctionFailure[3], $vulkanShortJunctionFailure[4]),
    @($vulkanShortJunctionFailure[0], $vulkanShortJunctionFailure[1], $vulkanShortJunctionFailure[2], $vulkanShortJunctionFailure[3], 'LINK : fatal error LNK1104: cannot open file ''CMakeFiles\cmTC_xyz.dir\intermediate.manifest'''),
    @('unrelated LNK1104')
)) {
    if (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure $malformedVulkanShortJunctionFailure) { throw 'Malformed Vulkan CMake signature was classified.' }
}
$overlongVulkanShortJunctionFailure = [System.Collections.Generic.List[object]]::new()
foreach ($unused in 1..2048) { $overlongVulkanShortJunctionFailure.Add('noise') }
foreach ($line in $vulkanShortJunctionFailure) { $overlongVulkanShortJunctionFailure.Add($line) }
if (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure $overlongVulkanShortJunctionFailure.ToArray()) { throw 'Overlong Vulkan CMake output was classified outside the bounded window.' }
. $cudaInventoryScript
$autoQualificationReport = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-gpu-auto-qualification-$([guid]::NewGuid().ToString('N')).txt"
try {
    & $autoQualificationReportScript -OutputPath $autoQualificationReport | Out-Null
    $report = [System.IO.File]::ReadAllText($autoQualificationReport)
    Assert-True ($report.Contains('mode: default_deny')) 'GPU Auto qualification report lost default-deny mode.'
    Assert-True ($report.Contains('qualified_entries: 0')) 'Checked-in GPU Auto qualification manifest must contain zero production entries.'
    Assert-True ($report.Contains('no GPU backend is eligible for Auto')) 'GPU Auto qualification report lost CPU-safe default-deny evidence.'
}
finally {
    Remove-Item -LiteralPath $autoQualificationReport -Force -ErrorAction SilentlyContinue
}
$autoQualificationFixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-gpu-auto-qualification-fixtures-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $autoQualificationFixtureRoot | Out-Null
$validAutoQualificationManifest = '{"schema_version":2,"mode":"default_deny","target_os":"windows","target_arch":"x86_64","entries":[{"pack":{"pack_id":"scribe-cuda-windows-x64","pack_version":"1.0.0","pack_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","security_epoch":7,"runtime_abi":3},"model_digest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","backend":"cuda","provider_id":"transcribe-cpp-ggml-cuda","vendor":"nvidia","device_class":"discrete_gpu","minimum_total_memory_bytes":8589934592,"minimum_available_memory_bytes":4294967296,"driver":{"kind":"exact","value":"windows-display:32.0.16.1088"},"evidence":{"id":"windows-nvidia-cuda-fixture-v1","cold_runs":5,"warm_runs":20,"gpu_p95_ms":110,"cpu_p95_ms":100,"correctness_verified":true,"reliability_verified":true,"cold_evidence_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","warm_evidence_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","transcript_parity_evidence_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}}]}'
try {
    $fixtureManifest = Join-Path $autoQualificationFixtureRoot 'valid.json'
    [System.IO.File]::WriteAllText($fixtureManifest, $validAutoQualificationManifest, [System.Text.UTF8Encoding]::new($false))
    $validReport = Invoke-NativeProcess `
        (Join-Path $PSHOME 'pwsh.exe') `
        @('-NoProfile', '-File', $autoQualificationReportScript, '-ManifestPath', $fixtureManifest)
    Assert-True ($validReport.Stdout.Contains('qualified_entries: 1')) 'Valid GPU Auto qualification evidence was rejected by the CI reporter.'
    $repeatReport = Invoke-NativeProcess `
        (Join-Path $PSHOME 'pwsh.exe') `
        @('-NoProfile', '-File', $autoQualificationReportScript, '-ManifestPath', $fixtureManifest)
    Assert-True ($validReport.Stdout -ceq $repeatReport.Stdout) 'GPU Auto qualification report output is not deterministic.'

    $invalidAutoQualificationFixtures = [ordered]@{
        'unknown root field' = $validAutoQualificationManifest.Replace(',"entries":', ',"unexpected":true,"entries":')
        'string correctness boolean' = $validAutoQualificationManifest.Replace('"correctness_verified":true', '"correctness_verified":"false"')
        'string cold-run count' = $validAutoQualificationManifest.Replace('"cold_runs":5', '"cold_runs":"5"')
        'insufficient cold runs' = $validAutoQualificationManifest.Replace('"cold_runs":5', '"cold_runs":4')
        'insufficient warm runs' = $validAutoQualificationManifest.Replace('"warm_runs":20', '"warm_runs":19')
        'p95 slower than threshold' = $validAutoQualificationManifest.Replace('"gpu_p95_ms":110', '"gpu_p95_ms":111')
        'false correctness evidence' = $validAutoQualificationManifest.Replace('"correctness_verified":true', '"correctness_verified":false')
        'false reliability evidence' = $validAutoQualificationManifest.Replace('"reliability_verified":true', '"reliability_verified":false')
        'bad evidence digest' = $validAutoQualificationManifest.Replace(('"cold_evidence_sha256":"' + ('c' * 64) + '"'), ('"cold_evidence_sha256":"' + ('C' * 64) + '"'))
        'backend-provider mismatch' = $validAutoQualificationManifest.Replace('"provider_id":"transcribe-cpp-ggml-cuda"', '"provider_id":"transcribe-cpp-ggml-vulkan"')
        'CUDA-vendor mismatch' = $validAutoQualificationManifest.Replace('"vendor":"nvidia"', '"vendor":"amd"')
        'zero available-memory minimum' = $validAutoQualificationManifest.Replace('"minimum_available_memory_bytes":4294967296', '"minimum_available_memory_bytes":0')
        'available-memory minimum above total' = $validAutoQualificationManifest.Replace('"minimum_available_memory_bytes":4294967296', '"minimum_available_memory_bytes":8589934593')
        'noncanonical pack component' = $validAutoQualificationManifest.Replace('"pack_id":"scribe-cuda-windows-x64"', '"pack_id":"bad:pack"')
        'unsafe driver identity' = $validAutoQualificationManifest.Replace('windows-display:32.0.16.1088', 'windows-display:\override')
        'noncanonical document formatting' = (($validAutoQualificationManifest | ConvertFrom-Json -Depth 16) | ConvertTo-Json -Depth 16)
    }
    $acceptedInvalidFixtures = [System.Collections.Generic.List[string]]::new()
    foreach ($fixture in $invalidAutoQualificationFixtures.GetEnumerator()) {
        $invalidManifest = Join-Path $autoQualificationFixtureRoot (($fixture.Key -replace '[^A-Za-z0-9]', '-') + '.json')
        [System.IO.File]::WriteAllText($invalidManifest, [string]$fixture.Value, [System.Text.UTF8Encoding]::new($false))
        $invalidResult = Invoke-NativeProcess `
            (Join-Path $PSHOME 'pwsh.exe') `
            @('-NoProfile', '-File', $autoQualificationReportScript, '-ManifestPath', $invalidManifest) `
            -AllowFailure
        if ($invalidResult.ExitCode -eq 0) {
            $acceptedInvalidFixtures.Add([string]$fixture.Key)
        }
    }
    Assert-True `
        ($acceptedInvalidFixtures.Count -eq 0) `
        "GPU Auto qualification reporter accepted invalid fixtures: $($acceptedInvalidFixtures -join ', ')"
}
finally {
    Remove-Item -LiteralPath $autoQualificationFixtureRoot -Recurse -Force -ErrorAction SilentlyContinue
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
    'Resolve-PinnedMsvcPayloadProfile',
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
$exportedPinnedEnvironment = @(& $buildScript `
    -Backend Vulkan `
    -PackVersion '0.1.0-fixture' `
    -OutputDirectory $toolchainOutput `
    -SigningMode Fixture `
    -ToolchainCheckOnly `
    -ExportPinnedMsvcEnvironment)
Assert-True ($LASTEXITCODE -eq 0 -and $exportedPinnedEnvironment.Count -eq 1) 'Pinned MSVC environment export did not return one validated document.'
try {
    $exportedPinnedEnvironment = [string]$exportedPinnedEnvironment[0] | ConvertFrom-Json
}
catch {
    throw 'Pinned MSVC environment export was not canonical JSON.'
}
Assert-True ($exportedPinnedEnvironment.schema_version -eq 1) 'Pinned MSVC environment export has an unexpected schema.'
$exportedNames = @($exportedPinnedEnvironment.environment.PSObject.Properties.Name | Sort-Object)
$expectedExportedNames = @(
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
) | Sort-Object
Assert-True `
    ($exportedNames.Count -eq $expectedExportedNames.Count -and
    -not (Compare-Object -ReferenceObject $expectedExportedNames -DifferenceObject $exportedNames -CaseSensitive)) `
    'Pinned MSVC environment export has an unexpected field set.'
foreach ($name in $toolchainEnvironmentNames) {
    $before = $toolchainEnvironmentBefore[$name]
    $after = Get-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
    Assert-True (($null -ne $after) -eq $before.Exists) "Pinned MSVC export changed whether process environment variable $name exists."
    if ($before.Exists) { Assert-True ([string]$after.Value -ceq [string]$before.Value) "Pinned MSVC export changed process environment variable $name." }
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
    foreach ($profile in @($wrongHashContract.msvc.payload_profiles)) {
        $profile.tools.cl.sha256 = '0' * 64
    }
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
            'MSVC tool payload does not match exactly one approved profile'
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
    $productionCudaFailedClosed = $false
    try {
        & $buildScript `
            -Backend Cuda `
            -PackVersion '0.1.0-production-contract' `
            -OutputDirectory $toolchainOutput `
            -SigningMode Production `
            -ToolchainCheckOnly | Out-Null
    }
    catch {
        $productionCudaFailedClosed = $_.Exception.Message.Contains(
            'Production CUDA inputs are unprovisioned'
        )
    }
    Assert-True $productionCudaFailedClosed 'Production CUDA accepted unauthenticated same-version toolkit inputs.'
    Assert-True (-not (Test-Path -LiteralPath $toolchainOutput)) 'Rejected unauthenticated CUDA contract created a pack output.'

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

$cudaInventoryTestRoot = Join-Path `
    ([System.IO.Path]::GetTempPath()) `
    "scribe-cuda-inventory-$([guid]::NewGuid().ToString('N'))"
$cudaSdkRoot = Join-Path $cudaInventoryTestRoot 'v12.8'
$cudaJunction = Join-Path $cudaSdkRoot 'junction'
New-Item -ItemType Directory -Path $cudaSdkRoot | Out-Null
try {
    $cudaInventoryPaths = @(
        'bin/nvcc.exe',
        'include/cuda.h',
        'lib/x64/cuda.lib',
        'bin/cublas64_12.dll',
        'bin/cublaslt64_12.dll',
        'bin/cudart64_12.dll',
        'docs/license.txt'
    )
    foreach ($relative in $cudaInventoryPaths) {
        $path = Join-Path $cudaSdkRoot $relative.Replace('/', '\')
        $null = New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force
        [System.IO.File]::WriteAllText(
            $path,
            "authenticated CUDA fixture: $relative",
            [System.Text.UTF8Encoding]::new($false)
        )
    }
    $cudaInventory = @($cudaInventoryPaths | ForEach-Object {
        $path = Join-Path $cudaSdkRoot $_.Replace('/', '\')
        [pscustomobject]@{
            path = $_
            sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    })
    $requiredCudaInventoryPaths = $cudaInventoryPaths[0..5]

    Assert-AuthenticatedCudaSdkInventory `
        $cudaSdkRoot `
        $cudaInventory `
        $requiredCudaInventoryPaths

    $wrongHashInventory = @($cudaInventory | ForEach-Object {
        [pscustomobject]@{
            path = $_.path
            sha256 = if ($_.path -ceq 'include/cuda.h') { '0' * 64 } else { $_.sha256 }
        }
    })
    $wrongCudaHashRejected = $false
    try {
        Assert-AuthenticatedCudaSdkInventory `
            $cudaSdkRoot `
            $wrongHashInventory `
            $requiredCudaInventoryPaths
    }
    catch {
        $wrongCudaHashRejected = $_.Exception.Message.Contains('SHA-256 mismatch')
    }
    Assert-True $wrongCudaHashRejected 'Production CUDA inventory accepted an altered file hash.'

    $missingCudaFile = Join-Path $cudaSdkRoot 'docs\license.txt'
    Remove-Item -LiteralPath $missingCudaFile -Force
    $missingCudaFileRejected = $false
    try {
        Assert-AuthenticatedCudaSdkInventory `
            $cudaSdkRoot `
            $cudaInventory `
            $requiredCudaInventoryPaths
    }
    catch {
        $missingCudaFileRejected = $_.Exception.Message.Contains(
            'omitted authenticated inventory entries'
        )
    }
    Assert-True $missingCudaFileRejected 'Production CUDA inventory accepted a missing file.'
    [System.IO.File]::WriteAllText(
        $missingCudaFile,
        'authenticated CUDA fixture: docs/license.txt',
        [System.Text.UTF8Encoding]::new($false)
    )

    $unexpectedCudaFile = Join-Path $cudaSdkRoot 'bin\unexpected.dll'
    [System.IO.File]::WriteAllBytes($unexpectedCudaFile, [byte[]](1, 2, 3))
    $unexpectedCudaFileRejected = $false
    try {
        Assert-AuthenticatedCudaSdkInventory `
            $cudaSdkRoot `
            $cudaInventory `
            $requiredCudaInventoryPaths
    }
    catch {
        $unexpectedCudaFileRejected = $_.Exception.Message.Contains(
            'unexpected, duplicate, or case-colliding file'
        )
    }
    Assert-True $unexpectedCudaFileRejected 'Production CUDA inventory accepted an unexpected file.'
    Remove-Item -LiteralPath $unexpectedCudaFile -Force

    $cudaJunctionTarget = Join-Path $cudaInventoryTestRoot 'junction-target'
    New-Item -ItemType Directory -Path $cudaJunctionTarget | Out-Null
    New-Item -ItemType Junction -Path $cudaJunction -Target $cudaJunctionTarget | Out-Null
    $cudaJunctionRejected = $false
    try {
        Assert-AuthenticatedCudaSdkInventory `
            $cudaSdkRoot `
            $cudaInventory `
            $requiredCudaInventoryPaths
    }
    catch {
        $cudaJunctionRejected = $_.Exception.Message.Contains('link or reparse point')
    }
    Assert-True $cudaJunctionRejected 'Production CUDA inventory accepted a reparse entry.'
    Remove-Item -LiteralPath $cudaJunction -Force

    $cudaAdsFile = Join-Path $cudaSdkRoot 'include\cuda.h'
    [System.IO.File]::WriteAllText(
        "${cudaAdsFile}:scribe-test",
        'untrusted alternate stream',
        [System.Text.UTF8Encoding]::new($false)
    )
    $cudaAdsRejected = $false
    try {
        Assert-AuthenticatedCudaSdkInventory `
            $cudaSdkRoot `
            $cudaInventory `
            $requiredCudaInventoryPaths
    }
    catch {
        $cudaAdsRejected = $_.Exception.Message.Contains('alternate data stream')
    }
    Assert-True $cudaAdsRejected 'Production CUDA inventory accepted an alternate data stream.'
}
finally {
    if (Test-Path -LiteralPath $cudaJunction) {
        $junctionItem = Get-Item -LiteralPath $cudaJunction -Force
        if (($junctionItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            Remove-Item -LiteralPath $cudaJunction -Force
        }
    }
    $canonicalCudaTestRoot = [System.IO.Path]::GetFullPath($cudaInventoryTestRoot)
    $expectedCudaTempPrefix = [System.IO.Path]::GetFullPath(
        (Join-Path ([System.IO.Path]::GetTempPath()) 'scribe-cuda-inventory-')
    )
    if (-not $canonicalCudaTestRoot.StartsWith(
        $expectedCudaTempPrefix,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw 'Refusing to clean a CUDA inventory test directory outside the dedicated temp prefix.'
    }
    Remove-Item -LiteralPath $canonicalCudaTestRoot -Recurse -Force -ErrorAction SilentlyContinue
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
