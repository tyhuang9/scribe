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

function Assert-ScribeEvidenceNoReparse([string]$Path) {
    $current = Resolve-ScribeEvidencePath $Path
    while (-not (Test-Path -LiteralPath $current)) {
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) { throw "Could not find an existing ancestor for $Path" }
        $current = $parent
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Evidence path crosses a reparse point: $current"
        }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) { break }
        $current = $parent
    }
}

function Assert-ScribeEvidenceFile([string]$Path, [string]$Label, [UInt64]$MaxBytes) {
    $full = Resolve-ScribeEvidencePath $Path
    Assert-ScribeEvidenceNoReparse $full
    if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { throw "$Label is missing." }
    $item = Get-Item -LiteralPath $full -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or $item.Length -eq 0 -or $item.Length -gt $MaxBytes) {
        throw "$Label is not a bounded regular non-reparse file."
    }
    return $full
}

function Assert-ScribeEvidenceSingleLinkFile([string]$Path, [string]$Label, [UInt64]$MaxBytes, [string]$FsutilPath) {
    $full = Assert-ScribeEvidenceFile $Path $Label $MaxBytes
    $links = @(& $FsutilPath hardlink list $full)
    if ($LASTEXITCODE -ne 0 -or $links.Count -ne 1) { throw "$Label must have exactly one hard link." }
    return $full
}

function Invoke-ScribeEvidence([string]$Exe, [string[]]$Arguments, [string]$Failure) {
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw $Failure }
}

function Enable-ScribeEvidenceCmakeBootstrap([string]$CargoTarget, [string]$BuildEnvironment) {
    $tcs = Join-Path $BuildEnvironment 'tcs'
    $entries = @(Get-ChildItem -LiteralPath $tcs -Force)
    if ($entries.Count -ne 1) { throw 'CMake bootstrap tcs inventory is not exact.' }
    $outLink = $entries[0]
    if (-not $outLink.PSIsContainer -or ($outLink.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -or $outLink.LinkType -cne 'Junction' -or @($outLink.Target).Count -ne 1) {
        throw 'CMake bootstrap out directory is not one exact junction.'
    }
    $out = (Get-Item -LiteralPath @($outLink.Target)[0] -Force).FullName
    $relative = [IO.Path]::GetRelativePath($CargoTarget, $out).Replace('\', '/')
    if ($relative -cnotmatch '^(debug|release)/build/transcribe-cpp-sys-[0-9a-f]{16}/out$') {
        throw 'CMake bootstrap out junction escaped the exact Cargo target.'
    }
    $outItem = Get-Item -LiteralPath $out -Force
    if (-not $outItem.PSIsContainer -or ($outItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'CMake bootstrap out target is not a physical directory.'
    }
    $build = Join-Path $out 'build'
    if (Test-Path -LiteralPath $build) {
        $buildItem = Get-Item -LiteralPath $build -Force
        if (-not $buildItem.PSIsContainer -or ($buildItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or (Split-Path -Parent $buildItem.FullName) -cne $out) {
            throw 'Refusing to replace an unexpected CMake build directory.'
        }
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
    $first = & $cargo @Arguments 2>&1
    if ($LASTEXITCODE -eq 0) { return }
    $detail = ($first | Out-String)
    if (-not ($detail.Contains('transcribe-cpp-sys') -and ($detail.Contains('The directory name is invalid. (os error 267)') -or $detail.Contains('Could not open file for write in copy operation')))) {
        throw "$Failure $detail"
    }
    Enable-ScribeEvidenceCmakeBootstrap $CargoTarget $BuildEnvironment
    & $cargo @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Failure after validated CMake bootstrap retry." }
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
try {
    $env:SCRIBE_BUILD_REVISION = $revision
    $env:SHERPA_ONNX_ARCHIVE_DIR = $nativeArchive
    $env:CARGO_TARGET_DIR = New-ScribeEvidenceShortCargoTarget 'cpu'
    $cpuBuildEnvironment = New-ScribeEvidenceShortCargoTarget 'cpu-env'
    New-Item -ItemType Directory -Path $cpuBuildEnvironment | Out-Null
    $env:LOCALAPPDATA = $cpuBuildEnvironment
    Invoke-ScribeEvidenceCargoWithCmakeRetry @('build', '--locked', '--offline', '--release', '--bin', 'scribe-inference-worker', '--features', 'inference-worker', '--manifest-path', (Join-Path $repositoryRoot 'Cargo.toml')) 'Fresh isolated CPU worker build failed.' $env:CARGO_TARGET_DIR $cpuBuildEnvironment
    $env:LOCALAPPDATA = $previousLocalAppData
    $cpuBundle = Join-Path $workRoot 'cpu-worker-bundle'
    New-Item -ItemType Directory -Path $cpuBundle | Out-Null
    Copy-Item -LiteralPath (Join-Path $env:CARGO_TARGET_DIR 'release\scribe-inference-worker.exe') -Destination (Join-Path $cpuBundle 'scribe-inference-worker.exe')
    $cpuWorker = Assert-ScribeEvidenceSingleLinkFile (Join-Path $cpuBundle 'scribe-inference-worker.exe') 'Materialized CPU worker' (512MB) $trustedFsutil
    $packRoot = Join-Path $workRoot 'fixture-vulkan-pack'
    $packVersion = "fixture-evidence-$($revision.Substring(0, 12))-$([guid]::NewGuid().ToString('N').Substring(0, 12))"
    & (Join-Path $PSScriptRoot 'build-windows-gpu-worker-pack.ps1') -Backend Vulkan -PackVersion $packVersion -OutputDirectory $packRoot -SigningMode Fixture -NativeArchiveDirectory $nativeArchive
    if ($LASTEXITCODE -ne 0) { throw 'Fresh fixture-signed Vulkan pack build failed.' }
    $packManifest = Get-Content -LiteralPath (Join-Path $packRoot 'manifest.json') -Raw | ConvertFrom-Json
    if ([string]$packManifest.app_build -cnotmatch ("#" + [regex]::Escape($revision) + '$') -or
        [string]$packManifest.worker_build -cnotmatch ("#" + [regex]::Escape($revision) + '$')) {
        throw 'Fixture pack build identity is not bound to the runner-captured source revision.'
    }
    $evidenceOutput = Join-Path $evidenceRoot 'windows-vulkan-fixture-evidence.json'
    foreach ($forbiddenRoot in @($repositoryRoot, (Join-Path $repositoryRoot 'runtime-manifests'), $packRoot)) {
        if (Test-ScribeEvidenceWithin $evidenceOutput $forbiddenRoot) {
            throw 'Evidence output may not be under source, runtime-manifest, catalog, activation, or pack roots.'
        }
    }
    $env:CARGO_TARGET_DIR = New-ScribeEvidenceShortCargoTarget 'harness'
    $harnessBuildEnvironment = New-ScribeEvidenceShortCargoTarget 'harness-env'
    New-Item -ItemType Directory -Path $harnessBuildEnvironment | Out-Null
    $env:LOCALAPPDATA = $harnessBuildEnvironment
    Invoke-ScribeEvidenceCargoWithCmakeRetry @('test', '--locked', '--offline', '--features', 'inference-worker', 'onnx_worker::tests::windows_vulkan_fixture_evidence_captures_five_cold_and_twenty_warm_runs', '--no-run') 'Vulkan evidence test precompilation failed.' $env:CARGO_TARGET_DIR $harnessBuildEnvironment
    $baseline = Get-ScribeVulkanEvidenceNvidiaBaseline $ExpectedStableDevice $trustedNvidiaSmi
    $env:SCRIBE_VULKAN_EVIDENCE_PACK_ROOT = $packRoot
    $env:SCRIBE_VULKAN_EVIDENCE_CPU_WORKER = $cpuWorker
    $env:SCRIBE_VULKAN_EVIDENCE_MODEL = $model
    $env:SCRIBE_VULKAN_EVIDENCE_MODEL_SHA256 = $ModelSha256
    $env:SCRIBE_VULKAN_EVIDENCE_WAV = $wav
    $env:SCRIBE_VULKAN_EVIDENCE_WAV_SHA256 = $WavSha256
    $env:SCRIBE_VULKAN_EVIDENCE_EXPECTED_PHRASE = $ExpectedPhrase
    $env:SCRIBE_VULKAN_EVIDENCE_EXPECTED_STABLE_DEVICE = $ExpectedStableDevice
    $env:SCRIBE_VULKAN_EVIDENCE_OUTPUT = $evidenceOutput
    $env:SCRIBE_VULKAN_EVIDENCE_NVIDIA_BASELINE_JSON = $baseline | ConvertTo-Json -Compress
    Invoke-ScribeEvidence $cargo @('test', '--locked', '--offline', '--features', 'inference-worker', 'onnx_worker::tests::windows_vulkan_fixture_evidence_captures_five_cold_and_twenty_warm_runs', '--', '--ignored', '--exact', '--test-threads=1') 'The exact Vulkan evidence test failed.'
}
finally {
    $env:SCRIBE_BUILD_REVISION = $previousRevision
    $env:SHERPA_ONNX_ARCHIVE_DIR = $previousArchive
    $env:CARGO_TARGET_DIR = $previousTarget
    $env:LOCALAPPDATA = $previousLocalAppData
    foreach ($name in $evidenceEnvironmentNames) {
        if ($null -eq $previousEvidenceEnvironment[$name]) { Remove-Item "Env:$name" -ErrorAction SilentlyContinue }
        else { Set-Item "Env:$name" $previousEvidenceEnvironment[$name] }
    }
    $afterAuto = [IO.File]::ReadAllBytes($autoManifest)
    if (-not [Linq.Enumerable]::SequenceEqual[byte]($afterAuto, $expectedAuto)) { throw 'Windows Auto manifest changed during fixture-only evidence capture.' }
}
$report = Assert-ScribeEvidenceFile (Join-Path $evidenceRoot 'windows-vulkan-fixture-evidence.json') 'Evidence report' (1MB)
$reportDigest = (Get-FileHash -LiteralPath $report -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Output "Fixture-only untrusted Vulkan evidence: $report ($reportDigest)"
