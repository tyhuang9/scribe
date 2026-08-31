[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ModelPath,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9a-f]{64}$')]
    [string]$ModelSha256,
    [Parameter(Mandatory = $true)]
    [string]$WavPath,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9a-f]{64}$')]
    [string]$WavSha256,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedPhrase,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^native:[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$')]
    [string]$ExpectedStableDevice,
    [Parameter(Mandatory = $true)]
    [string]$EvidenceDirectory,
    [Parameter(Mandatory = $true)]
    [string]$NativeArchiveDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-vulkan-evidence-preflight.ps1')

function Resolve-ScribeEvidencePath([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    if (-not [IO.Path]::IsPathFullyQualified($full)) {
        throw 'Evidence inputs must use absolute paths.'
    }
    return $full.TrimEnd([char[]]@('\', '/'))
}

function Resolve-ScribeEvidenceFreshDirectory([string]$Path) {
    $requested = Resolve-ScribeEvidencePath $Path
    if (Test-Path -LiteralPath $requested) { throw 'EvidenceDirectory must be a fresh explicit external directory.' }
    $parent = Split-Path -Parent $requested
    $leaf = Split-Path -Leaf $requested
    if (-not $parent -or $leaf -cnotmatch '^[a-zA-Z0-9][a-zA-Z0-9._-]{0,95}$') {
        throw 'EvidenceDirectory must be one bounded directory name below an existing parent.'
    }
    Assert-ScribeEvidenceNoReparse $parent
    $canonicalParent = (Get-Item -LiteralPath $parent -Force).FullName.TrimEnd([char[]]@('\', '/'))
    return Join-Path $canonicalParent $leaf
}

function Test-ScribeEvidenceActivationPath([string]$Path) {
    $segments = @(($Path -split '[\\/]') | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    for ($index = 0; $index -lt ($segments.Count - 1); $index++) {
        if ($segments[$index] -ieq 'workers' -and $segments[$index + 1] -ieq 'packs') { return $true }
    }
    return $false
}

function Test-ScribeEvidenceWithin([string]$Path, [string]$Root) {
    $pathValue = $Path.TrimEnd([char[]]@('\', '/'))
    $rootValue = $Root.TrimEnd([char[]]@('\', '/'))
    return $pathValue -ieq $rootValue -or $pathValue.StartsWith("$rootValue\", [StringComparison]::OrdinalIgnoreCase)
}

function New-ScribeEvidenceShortCargoTarget([string]$Label) {
    $localAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
    if ([string]::IsNullOrWhiteSpace($localAppData)) { throw 'LocalApplicationData is unavailable.' }
    $root = Join-Path $localAppData 'sgp'
    Assert-ScribeEvidenceNoReparse $root
    $leaf = "evidence-$Label-$([guid]::NewGuid().ToString('N').Substring(0, 12))"
    if ($leaf.Length -gt 60) { throw 'Evidence Cargo target leaf is not bounded.' }
    $target = Join-Path $root $leaf
    if (Test-Path -LiteralPath $target) { throw 'Evidence Cargo target must be fresh.' }
    return $target
}

function Invoke-ScribeEvidence([string]$Exe, [string[]]$Arguments, [string]$Failure) {
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw $Failure }
}

function Get-ScribeEvidencePinnedMsvcEnvironment([string]$Builder, [string]$NativeArchive, [string]$UnusedOutput) {
    try {
        $output = @(& $Builder -Backend Vulkan -PackVersion 'fixture-toolchain-check' -OutputDirectory $UnusedOutput -SigningMode Fixture -NativeArchiveDirectory $NativeArchive -ToolchainCheckOnly -ExportPinnedMsvcEnvironment)
    }
    catch {
        throw 'Could not obtain the validated pinned MSVC environment.'
    }
    if ($LASTEXITCODE -ne 0 -or $output.Count -ne 1) { throw 'Could not obtain the validated pinned MSVC environment.' }
    try {
        $export = [string]$output[0] | ConvertFrom-Json
        $expectedNames = @(
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
        $actualNames = @($export.environment.PSObject.Properties.Name | Sort-Object)
        $requiredNames = @($expectedNames | Sort-Object)
        if ($export.schema_version -ne 1 -or
            $actualNames.Count -ne $requiredNames.Count -or
            (Compare-Object -ReferenceObject $requiredNames -DifferenceObject $actualNames -CaseSensitive)) {
            throw 'invalid'
        }
        $environment = [ordered]@{}
        foreach ($name in $expectedNames) {
            $environment[$name] = [string]$export.environment.$name
        }
        return $environment
    }
    catch {
        throw 'Pinned MSVC environment export was malformed.'
    }
}

function Invoke-ScribeEvidenceWithPinnedMsvcEnvironment([System.Collections.IDictionary]$Environment, [scriptblock]$Operation) {
    $previous = $null
    $operationFailure = $null
    $restoreFailure = $null
    try {
        try {
            $previous = Set-ScribeEvidenceProcessEnvironment $Environment
            & $Operation
        }
        catch {
            $operationFailure = $_.Exception
        }
    }
    finally {
        if ($null -ne $previous) {
            try {
                Restore-ScribeEvidenceProcessEnvironment $previous
            }
            catch {
                $restoreFailure = $_.Exception
            }
        }
    }
    if ($null -ne $operationFailure) {
        if ($null -ne $restoreFailure) {
            Add-ScribeEvidenceSecondaryFailures $operationFailure @($restoreFailure)
        }
        throw $operationFailure
    }
    if ($null -ne $restoreFailure) { throw $restoreFailure }
}

function Enable-ScribeEvidenceCmakeBootstrap([string]$CargoTarget, [string]$BuildEnvironment) {
    $cargoTargetItem = Get-ScribeEvidencePhysicalDirectory $CargoTarget 'CMake bootstrap Cargo target'
    $buildEnvironmentItem = Get-ScribeEvidencePhysicalDirectory $BuildEnvironment 'CMake bootstrap build environment'
    $canonicalCargoTarget = $cargoTargetItem.FullName.TrimEnd([char[]]@('\', '/'))
    $canonicalBuildEnvironment = $buildEnvironmentItem.FullName.TrimEnd([char[]]@('\', '/'))
    $tcs = Join-Path $BuildEnvironment 'tcs'
    $tcsItem = Get-ScribeEvidencePhysicalDirectory $tcs 'CMake bootstrap tcs inventory'
    if ((Split-Path -Parent $tcsItem.FullName) -cne $canonicalBuildEnvironment) {
        throw 'CMake bootstrap tcs inventory escaped the exact build environment.'
    }
    $entries = @(Get-ChildItem -LiteralPath $tcs -Force)
    if ($entries.Count -ne 1) { throw 'CMake bootstrap tcs inventory is not exact.' }
    $outLink = $entries[0]
    if (-not $outLink.PSIsContainer -or ($outLink.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -or $outLink.LinkType -cne 'Junction' -or @($outLink.Target).Count -ne 1) {
        throw 'CMake bootstrap out directory is not one exact junction.'
    }
    $out = (Get-Item -LiteralPath @($outLink.Target)[0] -Force).FullName
    $relative = [IO.Path]::GetRelativePath($canonicalCargoTarget, $out).Replace('\', '/')
    if ($relative -cnotmatch '^(debug|release)/build/transcribe-cpp-sys-[0-9a-f]{16}/out$') {
        throw 'CMake bootstrap out junction escaped the exact Cargo target.'
    }
    $outItem = Get-ScribeEvidencePhysicalDirectory $out 'CMake bootstrap out target'
    if (-not (Test-ScribeEvidenceWithin $outItem.FullName $canonicalCargoTarget)) {
        throw 'CMake bootstrap out target is outside the exact Cargo target.'
    }
    $build = Join-Path $out 'build'
    if (Test-Path -LiteralPath $build) {
        $buildItem = Get-Item -LiteralPath $build -Force
        if (-not $buildItem.PSIsContainer -or ($buildItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or (Split-Path -Parent $buildItem.FullName) -cne $out) {
            throw 'Refusing to replace an unexpected CMake build directory.'
        }
        # Revalidate all mutable topology immediately before this only permitted deletion.
        $currentCargoTarget = Get-ScribeEvidencePhysicalDirectory $CargoTarget 'CMake bootstrap Cargo target'
        $currentBuildEnvironment = Get-ScribeEvidencePhysicalDirectory $BuildEnvironment 'CMake bootstrap build environment'
        $currentTcs = Get-ScribeEvidencePhysicalDirectory $tcs 'CMake bootstrap tcs inventory'
        $currentOut = Get-ScribeEvidencePhysicalDirectory $out 'CMake bootstrap out target'
        if ($currentCargoTarget.FullName -cne $canonicalCargoTarget -or
            $currentBuildEnvironment.FullName -cne $canonicalBuildEnvironment -or
            (Split-Path -Parent $currentTcs.FullName) -cne $canonicalBuildEnvironment -or
            $currentOut.FullName -cne $outItem.FullName -or
            -not (Test-ScribeEvidenceWithin $currentOut.FullName $canonicalCargoTarget)) {
            throw 'CMake bootstrap topology changed before mutation.'
        }
        $currentOutLink = Get-Item -LiteralPath $outLink.FullName -Force
        if (($currentOutLink.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -or @($currentOutLink.Target).Count -ne 1 -or (Get-Item -LiteralPath @($currentOutLink.Target)[0] -Force).FullName -cne $outItem.FullName) {
            throw 'CMake bootstrap out junction changed before mutation.'
        }
        $currentBuild = Get-Item -LiteralPath $build -Force
        if (($currentBuild.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or -not $currentBuild.PSIsContainer -or (Split-Path -Parent $currentBuild.FullName) -cne $outItem.FullName) {
            throw 'CMake bootstrap build topology changed before mutation.'
        }
        Assert-ScribeEvidenceNoReparseDescendants $currentBuild.FullName
        Remove-Item -LiteralPath $build -Recurse -Force
    }
    $native = Join-Path $BuildEnvironment 'native'
    if (Test-Path -LiteralPath $native) { throw 'CMake bootstrap native directory already exists.' }
    New-Item -ItemType Directory -Path $native | Out-Null
    New-Item -ItemType Junction -Path $build -Target $native | Out-Null
    $junction = Get-Item -LiteralPath $build -Force
    if (-not $junction.PSIsContainer -or ($junction.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -or $junction.LinkType -cne 'Junction' -or @($junction.Target).Count -ne 1 -or (Get-Item -LiteralPath @($junction.Target)[0] -Force).FullName -cne (Get-Item -LiteralPath $native -Force).FullName) {
        throw 'Could not validate the isolated CMake build junction.'
    }
}

function Invoke-ScribeEvidenceCargoWithCmakeRetry([string[]]$Arguments, [string]$Failure, [string]$CargoTarget, [string]$BuildEnvironment) {
    try {
        $null = Invoke-ScribeGpuWorkerBoundedNativeProcess $cargo $Arguments $Failure
        return
    }
    catch {
        $diagnostic = @(Get-ScribeGpuWorkerNativeProcessRetryDiagnostic $_.Exception)
        if (-not (Test-ScribeGpuWorkerKnownCmakeBootstrapFailure $diagnostic)) {
            throw $Failure
        }
    }
    Enable-ScribeEvidenceCmakeBootstrap $CargoTarget $BuildEnvironment
    try {
        $null = Invoke-ScribeGpuWorkerBoundedNativeProcess $cargo $Arguments "$Failure after validated CMake bootstrap retry."
    }
    catch {
        throw "$Failure after validated CMake bootstrap retry."
    }
}

if (-not $IsWindows) { throw 'Windows Vulkan evidence capture is Windows x64 only.' }
if ([Environment]::Is64BitOperatingSystem -ne $true) { throw 'Windows Vulkan evidence capture requires x64 Windows.' }
if ($ExpectedPhrase.Length -eq 0 -or $ExpectedPhrase.Length -gt 256) { throw 'ExpectedPhrase must be 1..=256 characters.' }
foreach ($name in (Get-ChildItem Env: | Select-Object -ExpandProperty Name)) {
    if ($name -match '(^|_)(PRODUCTION_PRIVATE_KEY|PRODUCTION_KEY_ID|GPU_PACK_RELEASE_POLICY|GPU_PACK_SIGNING_KEY)($|_)') {
        throw "Production signing/release input is forbidden for fixture-only evidence: $name"
    }
}

$repositoryRoot = (Get-Item -LiteralPath (Join-Path $PSScriptRoot '..') -Force).FullName.TrimEnd([char[]]@('\', '/'))
$actualSystem32 = Get-ScribeVulkanEvidenceActualSystem32
Assert-ScribeEvidenceNoReparse $actualSystem32
$trustedNvidiaSmi = Assert-ScribeVulkanEvidenceTrustedNvidiaSmi (Join-Path $actualSystem32 'nvidia-smi.exe')
$trustedFsutil = Assert-ScribeVulkanEvidenceTrustedNvidiaSmi (Join-Path $actualSystem32 'fsutil.exe')
$autoManifest = Join-Path $repositoryRoot 'runtime-manifests\gpu-auto-qualification-windows-x64.json'
$expectedAuto = [Text.Encoding]::UTF8.GetBytes("{`"schema_version`":2,`"mode`":`"default_deny`",`"target_os`":`"windows`",`"target_arch`":`"x86_64`",`"entries`":[]}`n")
$beforeAuto = [IO.File]::ReadAllBytes($autoManifest)
if (-not [Linq.Enumerable]::SequenceEqual[byte]($beforeAuto, $expectedAuto)) { throw 'Windows Auto manifest must be canonical empty default-deny before evidence.' }

$model = Assert-ScribeEvidenceFile $ModelPath 'Model' (8GB)
$wav = Assert-ScribeEvidenceFile $WavPath 'WAV' (256MB)
if ((Get-FileHash -LiteralPath $model -Algorithm SHA256).Hash.ToLowerInvariant() -cne $ModelSha256) { throw 'Model SHA-256 mismatch.' }
if ((Get-FileHash -LiteralPath $wav -Algorithm SHA256).Hash.ToLowerInvariant() -cne $WavSha256) { throw 'WAV SHA-256 mismatch.' }
$nativeArchive = Resolve-ScribeEvidencePath $NativeArchiveDirectory
Assert-ScribeEvidenceNoReparse $nativeArchive
$evidenceRoot = Resolve-ScribeEvidenceFreshDirectory $EvidenceDirectory
if (Test-ScribeEvidenceWithin $evidenceRoot $repositoryRoot) { throw 'EvidenceDirectory must be outside source-controlled paths.' }
if (Test-ScribeEvidenceActivationPath $evidenceRoot) { throw 'EvidenceDirectory may not be an activation/catalog workers\\packs root.' }
New-Item -ItemType Directory -Path $evidenceRoot | Out-Null
Assert-ScribeEvidenceNoReparse $evidenceRoot
$workRoot = Join-Path $evidenceRoot 'fixture-work'
New-Item -ItemType Directory -Path $workRoot | Out-Null
$finalEvidenceLeaf = 'windows-vulkan-fixture-evidence.json'
$pendingEvidenceLeaf = "windows-vulkan-fixture-evidence.pending-$([guid]::NewGuid().ToString('N')).json"
$evidenceOutput = Join-Path $evidenceRoot $finalEvidenceLeaf
$pendingEvidenceOutput = Join-Path $evidenceRoot $pendingEvidenceLeaf
$null = Assert-ScribeEvidenceDirectChildPath $evidenceOutput $evidenceRoot $finalEvidenceLeaf 'Final evidence report'
$null = Assert-ScribeEvidenceDirectChildPath $pendingEvidenceOutput $evidenceRoot $pendingEvidenceLeaf 'Pending evidence report'

$git = (Get-Command git.exe -ErrorAction Stop).Source
$cargo = (Get-Command cargo.exe -ErrorAction Stop).Source
$revision = (& $git -C $repositoryRoot rev-parse --verify HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $revision -cnotmatch '^[0-9a-f]{40}$') { throw 'Could not bind evidence to one canonical source revision.' }
& $git -C $repositoryRoot diff --quiet --exit-code
if ($LASTEXITCODE -ne 0) { throw 'Evidence capture requires a clean source worktree.' }
& $git -C $repositoryRoot diff --cached --quiet --exit-code
if ($LASTEXITCODE -ne 0) { throw 'Evidence capture requires a clean source index.' }

$previousRevision = $env:SCRIBE_BUILD_REVISION
$previousArchive = $env:SHERPA_ONNX_ARCHIVE_DIR
$previousTarget = $env:CARGO_TARGET_DIR
$previousLocalAppData = $env:LOCALAPPDATA
$previousWorkerDigest = $env:SCRIBE_BUNDLED_WORKER_SHA256
$previousBuildingWorker = $env:SCRIBE_BUILDING_WORKER
$packBuilder = Join-Path $PSScriptRoot 'build-windows-gpu-worker-pack.ps1'
$pinnedMsvcEnvironment = Get-ScribeEvidencePinnedMsvcEnvironment $packBuilder $nativeArchive (Join-Path $workRoot 'unused-pinned-toolchain-export')
$evidenceEnvironmentNames = @(
    'SCRIBE_VULKAN_EVIDENCE_PACK_ROOT',
    'SCRIBE_VULKAN_EVIDENCE_CPU_WORKER',
    'SCRIBE_VULKAN_EVIDENCE_MODEL',
    'SCRIBE_VULKAN_EVIDENCE_MODEL_SHA256',
    'SCRIBE_VULKAN_EVIDENCE_WAV',
    'SCRIBE_VULKAN_EVIDENCE_WAV_SHA256',
    'SCRIBE_VULKAN_EVIDENCE_EXPECTED_PHRASE',
    'SCRIBE_VULKAN_EVIDENCE_EXPECTED_STABLE_DEVICE',
    'SCRIBE_VULKAN_EVIDENCE_OUTPUT',
    'SCRIBE_VULKAN_EVIDENCE_NVIDIA_BASELINE_JSON'
)
$previousEvidenceEnvironment = @{}
foreach ($name in $evidenceEnvironmentNames) { $previousEvidenceEnvironment[$name] = [Environment]::GetEnvironmentVariable($name) }
$primaryFailure = $null
$secondaryFailures = [System.Collections.Generic.List[System.Exception]]::new()
try {
    $env:SCRIBE_BUILD_REVISION = $revision
    $env:SHERPA_ONNX_ARCHIVE_DIR = $nativeArchive
    $env:CARGO_TARGET_DIR = New-ScribeEvidenceShortCargoTarget 'cpu'
    $cpuBuildEnvironment = New-ScribeEvidenceShortCargoTarget 'cpu-env'
    New-Item -ItemType Directory -Path $cpuBuildEnvironment | Out-Null
    $env:LOCALAPPDATA = $cpuBuildEnvironment
    Invoke-ScribeEvidenceWithPinnedMsvcEnvironment $pinnedMsvcEnvironment {
        Set-ScribeEvidenceWorkerBuildMode $true
        try {
            Invoke-ScribeEvidenceCargoWithCmakeRetry @('build', '--locked', '--offline', '--release', '--bin', 'scribe-inference-worker', '--features', 'inference-worker', '--manifest-path', (Join-Path $repositoryRoot 'Cargo.toml')) 'Fresh isolated CPU worker build failed.' $env:CARGO_TARGET_DIR $cpuBuildEnvironment
        }
        finally {
            Set-ScribeEvidenceWorkerBuildMode $false
        }
    }
    $env:LOCALAPPDATA = $previousLocalAppData
    $cpuBundle = Join-Path $workRoot 'cpu-worker-bundle'
    New-Item -ItemType Directory -Path $cpuBundle | Out-Null
    Copy-Item -LiteralPath (Join-Path $env:CARGO_TARGET_DIR 'release\scribe-inference-worker.exe') -Destination (Join-Path $cpuBundle 'scribe-inference-worker.exe')
    $cpuWorker = Assert-ScribeEvidenceSingleLinkFile (Join-Path $cpuBundle 'scribe-inference-worker.exe') 'Materialized CPU worker' (512MB) $trustedFsutil
    $packRoot = Join-Path $workRoot 'fixture-vulkan-pack'
    $packVersion = New-ScribeEvidenceFixturePackVersion $revision ([guid]::NewGuid().ToString('N').Substring(0, 12))
    & $packBuilder -Backend Vulkan -PackVersion $packVersion -OutputDirectory $packRoot -SigningMode Fixture -NativeArchiveDirectory $nativeArchive
    if ($LASTEXITCODE -ne 0) { throw 'Fresh fixture-signed Vulkan pack build failed.' }
    $packManifest = Get-Content -LiteralPath (Join-Path $packRoot 'manifest.json') -Raw | ConvertFrom-Json
    if ([string]$packManifest.app_build -cnotmatch ("#" + [regex]::Escape($revision) + '$') -or
        [string]$packManifest.worker_build -cnotmatch ("#" + [regex]::Escape($revision) + '$')) {
        throw 'Fixture pack build identity is not bound to the runner-captured source revision.'
    }
    foreach ($forbiddenRoot in @($repositoryRoot, (Join-Path $repositoryRoot 'runtime-manifests'), $packRoot)) {
        if ((Test-ScribeEvidenceWithin $evidenceOutput $forbiddenRoot) -or
            (Test-ScribeEvidenceWithin $pendingEvidenceOutput $forbiddenRoot)) {
            throw 'Evidence output may not be under source, runtime-manifest, catalog, activation, or pack roots.'
        }
    }
    $env:CARGO_TARGET_DIR = New-ScribeEvidenceShortCargoTarget 'harness'
    $harnessBuildEnvironment = New-ScribeEvidenceShortCargoTarget 'harness-env'
    New-Item -ItemType Directory -Path $harnessBuildEnvironment | Out-Null
    $env:LOCALAPPDATA = $harnessBuildEnvironment
    Invoke-ScribeEvidenceWithPinnedMsvcEnvironment $pinnedMsvcEnvironment {
        Set-ScribeEvidenceWorkerBuildMode $false
        Invoke-ScribeEvidenceCargoWithCmakeRetry @('test', '--locked', '--offline', '--features', 'inference-worker', 'onnx_worker::tests::windows_vulkan_fixture_evidence_captures_five_cold_and_twenty_warm_runs', '--no-run') 'Vulkan evidence test precompilation failed.' $env:CARGO_TARGET_DIR $harnessBuildEnvironment
    }
    $baseline = Get-ScribeVulkanEvidenceNvidiaBaseline $ExpectedStableDevice $trustedNvidiaSmi
    $env:SCRIBE_VULKAN_EVIDENCE_PACK_ROOT = $packRoot
    $env:SCRIBE_VULKAN_EVIDENCE_CPU_WORKER = $cpuWorker
    $env:SCRIBE_VULKAN_EVIDENCE_MODEL = $model
    $env:SCRIBE_VULKAN_EVIDENCE_MODEL_SHA256 = $ModelSha256
    $env:SCRIBE_VULKAN_EVIDENCE_WAV = $wav
    $env:SCRIBE_VULKAN_EVIDENCE_WAV_SHA256 = $WavSha256
    $env:SCRIBE_VULKAN_EVIDENCE_EXPECTED_PHRASE = $ExpectedPhrase
    $env:SCRIBE_VULKAN_EVIDENCE_EXPECTED_STABLE_DEVICE = $ExpectedStableDevice
    $env:SCRIBE_VULKAN_EVIDENCE_OUTPUT = $pendingEvidenceOutput
    $env:SCRIBE_VULKAN_EVIDENCE_NVIDIA_BASELINE_JSON = $baseline | ConvertTo-Json -Compress
    Invoke-ScribeEvidenceWithPinnedMsvcEnvironment $pinnedMsvcEnvironment {
        Set-ScribeEvidenceWorkerBuildMode $false
        Invoke-ScribeEvidence $cargo @('test', '--locked', '--offline', '--features', 'inference-worker', 'onnx_worker::tests::windows_vulkan_fixture_evidence_captures_five_cold_and_twenty_warm_runs', '--', '--ignored', '--exact', '--test-threads=1') 'The exact Vulkan evidence test failed.'
    }
}
catch {
    $primaryFailure = $_.Exception
}
finally {
    $restoreEnvironment = [ordered]@{
        SCRIBE_BUILD_REVISION = $previousRevision
        SHERPA_ONNX_ARCHIVE_DIR = $previousArchive
        CARGO_TARGET_DIR = $previousTarget
        LOCALAPPDATA = $previousLocalAppData
        SCRIBE_BUNDLED_WORKER_SHA256 = $previousWorkerDigest
        SCRIBE_BUILDING_WORKER = $previousBuildingWorker
    }
    foreach ($entry in $restoreEnvironment.GetEnumerator()) {
        try {
            [Environment]::SetEnvironmentVariable([string]$entry.Key, $entry.Value, [EnvironmentVariableTarget]::Process)
        }
        catch {
            $secondaryFailures.Add([InvalidOperationException]::new("Could not restore the $($entry.Key) process environment variable.", $_.Exception))
        }
    }
    foreach ($name in $evidenceEnvironmentNames) {
        try {
            [Environment]::SetEnvironmentVariable($name, $previousEvidenceEnvironment[$name], [EnvironmentVariableTarget]::Process)
        }
        catch {
            $secondaryFailures.Add([InvalidOperationException]::new("Could not restore a fixture evidence process environment variable.", $_.Exception))
        }
    }
    try {
        $afterAuto = [IO.File]::ReadAllBytes($autoManifest)
        if (-not [Linq.Enumerable]::SequenceEqual[byte]($afterAuto, $beforeAuto) -or
            -not [Linq.Enumerable]::SequenceEqual[byte]($afterAuto, $expectedAuto)) {
            throw 'Windows Auto manifest changed during fixture-only evidence capture.'
        }
    }
    catch {
        $secondaryFailures.Add($_.Exception)
    }
}
$published = Complete-ScribeEvidencePendingReport `
    $pendingEvidenceOutput `
    $evidenceOutput `
    $evidenceRoot `
    $pendingEvidenceLeaf `
    $finalEvidenceLeaf `
    $primaryFailure `
    $secondaryFailures.ToArray()
Write-Output "Fixture-only untrusted Vulkan evidence: $($published.Path) ($($published.Digest))"
