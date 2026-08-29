[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Cuda', 'Vulkan')]
    [string]$Backend,
    [Parameter(Mandatory = $true)]
    [string]$PackVersion,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [ValidateSet('Production', 'Fixture')]
    [string]$SigningMode = 'Production',
    [string]$ProductionPrivateKeyPath,
    [string]$ProductionKeyId,
    [string]$ToolchainManifestPath,
    [string]$NativeArchiveDirectory,
    [string]$CargoTargetDirectory,
    [switch]$ToolchainCheckOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $ToolchainCheckOnly -and
    $SigningMode -eq 'Production' -and
    ([string]::IsNullOrWhiteSpace($ProductionPrivateKeyPath) -or
    [string]::IsNullOrWhiteSpace($ProductionKeyId))) {
    throw 'Production pack builds require an externally supplied PKCS#8 private key path and reviewed key ID.'
}

function Get-NormalizedFullPath([string]$Path) {
    $full = [System.IO.Path]::GetFullPath($Path)
    $root = [System.IO.Path]::GetPathRoot($full)
    if ([string]::Equals($full, $root, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $root
    }
    return $full.TrimEnd([char[]]@('\', '/'))
}

function Assert-ExactProperties([psobject]$Value, [string[]]$Names, [string]$Label) {
    if ($null -eq $Value) {
        throw "$Label is missing."
    }
    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $expected = @($Names | Sort-Object)
    if ($actual.Count -ne $expected.Count -or
        (Compare-Object -ReferenceObject $expected -DifferenceObject $actual -CaseSensitive)) {
        throw "$Label has unknown or missing fields."
    }
}

function Assert-RegularNonReparseFile([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label must be a regular non-reparse file: $Path"
    }
    return $item
}

function Assert-NoReparseAncestors([string]$Path) {
    $current = Get-NormalizedFullPath $Path
    while (-not (Test-Path -LiteralPath $current)) {
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            throw "Could not resolve an existing ancestor for path: $Path"
        }
        $current = $parent
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "GPU worker-pack build path cannot cross a link or reparse point: $current"
        }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            break
        }
        $current = $parent
    }
}

function Invoke-NativeProcess(
    [string]$Executable,
    [string[]]$Arguments,
    [string]$FailureMessage
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
            throw $FailureMessage
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $output = $stdout.GetAwaiter().GetResult()
        $errorOutput = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) {
            $detail = $errorOutput.Trim()
            if (-not $detail) {
                $detail = $output.Trim()
            }
            throw "$FailureMessage Exit code $($process.ExitCode). $detail"
        }
        return [pscustomobject]@{
            Stdout = $output
            Stderr = $errorOutput
        }
    }
    finally {
        $process.Dispose()
    }
}

function Assert-ExactHash([string]$Path, [string]$Expected, [string]$Label) {
    $null = Assert-RegularNonReparseFile $Path $Label
    $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -cne $Expected) {
        throw "$Label SHA-256 mismatch: expected $Expected, got $actual"
    }
}

function Assert-LockedPackage(
    [string]$CargoLock,
    [string]$Name,
    [string]$Version,
    [string]$Checksum
) {
    $escapedName = [regex]::Escape($Name)
    $escapedVersion = [regex]::Escape($Version)
    $escapedChecksum = [regex]::Escape($Checksum)
    $blockPattern = '(?ms)^\[\[package\]\]\r?\n(?<body>.*?)(?=^\[\[package\]\]|\z)'
    $matches = @([regex]::Matches($CargoLock, $blockPattern) | Where-Object {
        $_.Groups['body'].Value -match "(?m)^name = `"$escapedName`"$" -and
        $_.Groups['body'].Value -match "(?m)^version = `"$escapedVersion`"$" -and
        $_.Groups['body'].Value -match "(?m)^checksum = `"$escapedChecksum`"$"
    })
    if ($matches.Count -ne 1) {
        throw "Cargo.lock does not contain the pinned $Name $Version checksum."
    }
}

function Get-CommandPath([string]$Name, [string]$FailureMessage) {
    $command = Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -eq $command) {
        throw $FailureMessage
    }
    return $command.Source
}

function Assert-BaseToolchain($Contract, [string]$RepositoryRoot) {
    if ($env:OS -ne 'Windows_NT' -or
        -not [System.Environment]::Is64BitOperatingSystem -or
        -not [System.Environment]::Is64BitProcess) {
        throw 'Windows GPU worker packs require a native Windows x64 build process.'
    }
    $rustc = Get-CommandPath 'rustc.exe' 'Pinned Rust 1.96.0 is not installed or not on PATH.'
    $rustVersion = (Invoke-NativeProcess $rustc @('-Vv') 'Could not inspect rustc.').Stdout
    foreach ($required in @(
        "release: $($Contract.rust.release)",
        "commit-hash: $($Contract.rust.commit)",
        "host: $($Contract.rust.host)"
    )) {
        if (-not $rustVersion.Contains($required)) {
            throw "rustc does not match the pinned worker-pack toolchain ($required)."
        }
    }
    $rustup = Get-CommandPath 'rustup.exe' 'rustup is required to verify the pinned Windows target.'
    $installedTargets = (Invoke-NativeProcess $rustup @('target', 'list', '--installed') 'Could not inspect installed Rust targets.').Stdout
    if (-not @($installedTargets -split "`r?`n").Contains([string]$Contract.target_triple)) {
        throw "Pinned Rust target $($Contract.target_triple) is not installed."
    }

    $cmake = Get-CommandPath 'cmake.exe' "Pinned CMake $($Contract.msvc.cmake_version) is not installed or not on PATH."
    $cmakeVersion = (Invoke-NativeProcess $cmake @('--version') 'Could not inspect CMake.').Stdout
    if (-not $cmakeVersion.StartsWith("cmake version $($Contract.msvc.cmake_version)`n") -and
        -not $cmakeVersion.StartsWith("cmake version $($Contract.msvc.cmake_version)`r`n")) {
        throw "CMake does not match pinned version $($Contract.msvc.cmake_version)."
    }

    $vswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
    $null = Assert-RegularNonReparseFile $vswhere 'Visual Studio locator'
    $vsArguments = @(
        '-latest', '-products', '*',
        '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64'
    )
    $installationVersion = (Invoke-NativeProcess $vswhere ($vsArguments + @('-property', 'installationVersion')) 'Could not locate MSVC.').Stdout.Trim()
    if ($installationVersion -cne [string]$Contract.msvc.visual_studio_installation_version) {
        throw "Visual Studio build tools must be exactly $($Contract.msvc.visual_studio_installation_version); found $installationVersion."
    }
    $installationPath = (Invoke-NativeProcess $vswhere ($vsArguments + @('-property', 'installationPath')) 'Could not locate MSVC.').Stdout.Trim()
    $toolset = Join-Path $installationPath "VC\Tools\MSVC\$($Contract.msvc.toolset_version)\bin\Hostx64\x64\cl.exe"
    $null = Assert-RegularNonReparseFile $toolset 'Pinned MSVC compiler'

    $cargoLockPath = Join-Path $RepositoryRoot 'Cargo.lock'
    $cargoLock = Get-Content -LiteralPath $cargoLockPath -Raw
    Assert-LockedPackage $cargoLock 'transcribe-cpp' `
        ([string]$Contract.native_source.transcribe_cpp_version) `
        ([string]$Contract.native_source.transcribe_cpp_checksum)
    Assert-LockedPackage $cargoLock 'transcribe-cpp-sys' `
        ([string]$Contract.native_source.transcribe_cpp_sys_version) `
        ([string]$Contract.native_source.transcribe_cpp_sys_checksum)
    $toolchain = Get-Content -LiteralPath (Join-Path $RepositoryRoot 'rust-toolchain.toml') -Raw
    if ($toolchain -notmatch "channel\s*=\s*`"$([regex]::Escape([string]$Contract.rust.release))`"") {
        throw 'rust-toolchain.toml does not match the worker-pack toolchain contract.'
    }
    $cargoConfig = Get-Content -LiteralPath (Join-Path $RepositoryRoot '.cargo\config.toml') -Raw
    foreach ($required in @('+crt-static', 'CMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded')) {
        if (-not $cargoConfig.Contains($required)) {
            throw "Windows worker build lost required static-runtime setting $required."
        }
    }
    foreach ($manifest in @(
        (Join-Path $RepositoryRoot 'Cargo.toml'),
        (Join-Path $RepositoryRoot 'tools\worker-pack-author\Cargo.toml')
    )) {
        $manifestText = Get-Content -LiteralPath $manifest -Raw
        if ($manifestText -notmatch "(?m)^version\s*=\s*`"$([regex]::Escape([string]$Contract.app_version))`"\s*$") {
            throw "Package version in $manifest does not match the worker-pack app version contract."
        }
    }
}

function Resolve-VulkanSdk($Contract) {
    $expectedVersion = [string]$Contract.vulkan.sdk_version
    $candidate = if (-not [string]::IsNullOrWhiteSpace($env:VULKAN_SDK)) {
        $env:VULKAN_SDK
    } else {
        "C:\VulkanSDK\$expectedVersion"
    }
    $root = Get-NormalizedFullPath $candidate
    if ((Split-Path -Leaf $root) -cne $expectedVersion -or
        -not (Test-Path -LiteralPath $root -PathType Container)) {
        throw "Pinned Vulkan SDK $expectedVersion is missing. Set VULKAN_SDK to its exact installation root."
    }
    foreach ($required in @($Contract.vulkan.required_files)) {
        Assert-ExactProperties $required @('path', 'sha256') 'Vulkan SDK file contract'
        $relative = ([string]$required.path).Replace('/', '\')
        Assert-ExactHash (Join-Path $root $relative) ([string]$required.sha256) "Pinned Vulkan SDK file $($required.path)"
    }
    return $root
}

function Resolve-CudaSdk($Contract) {
    $expectedDirectoryVersion = [string]$Contract.cuda.sdk_directory_version
    $candidate = if (-not [string]::IsNullOrWhiteSpace($env:CUDA_PATH)) {
        $env:CUDA_PATH
    } else {
        "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v$expectedDirectoryVersion"
    }
    $root = Get-NormalizedFullPath $candidate
    if ((Split-Path -Leaf $root) -cne "v$expectedDirectoryVersion" -or
        -not (Test-Path -LiteralPath $root -PathType Container)) {
        throw "Pinned CUDA Toolkit $expectedDirectoryVersion is missing. Install that exact toolkit or set CUDA_PATH to its exact installation root."
    }
    foreach ($required in @($Contract.cuda.required_files)) {
        $path = Join-Path $root (([string]$required).Replace('/', '\'))
        $null = Assert-RegularNonReparseFile $path "Pinned CUDA Toolkit file $required"
    }
    $nvcc = Join-Path $root 'bin\nvcc.exe'
    $nvccVersion = (Invoke-NativeProcess $nvcc @('--version') 'Could not inspect the pinned CUDA compiler.').Stdout
    if (-not $nvccVersion.Contains("V$($Contract.cuda.nvcc_version)")) {
        throw "CUDA nvcc must be exactly V$($Contract.cuda.nvcc_version)."
    }
    return $root
}

function Copy-ReviewedGpuWorkerDependencyClosure(
    [string]$WorkerPath,
    [string]$SdkRoot,
    [string]$PackBin,
    [string[]]$SystemDriverImports,
    [string[]]$PackagedRuntimeImports
) {
    . (Join-Path $PSScriptRoot 'windows-pe-imports.ps1')
    $system = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($name in @($ReviewedWindowsSystemDlls) + @($SystemDriverImports)) {
        if ([string]$name -cnotmatch '^[a-z0-9._-]+\.dll$') {
            throw "System dependency allowlist contains an unsafe DLL name: $name"
        }
        $null = $system.Add([string]$name)
    }
    $packaged = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($name in $PackagedRuntimeImports) {
        if ([string]$name -cnotmatch '^[a-z0-9._-]+\.dll$' -or
            $system.Contains([string]$name) -or
            -not $packaged.Add([string]$name)) {
            throw "Packaged dependency allowlist contains an unsafe or duplicate DLL name: $name"
        }
    }

    $pending = [System.Collections.Generic.Queue[string]]::new()
    $pending.Enqueue($WorkerPath)
    $processed = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    $observedProviderDependency = $false
    while ($pending.Count -gt 0) {
        $binary = $pending.Dequeue()
        $binaryName = [System.IO.Path]::GetFileName($binary)
        if (-not $processed.Add($binaryName)) {
            continue
        }
        $report = Get-WindowsPeImportReport $binary
        if ($report.Machine -ne 0x8664) {
            throw "GPU pack dependency is not an AMD64 PE: $binaryName"
        }
        if ([string]::Equals($binary, $WorkerPath, [System.StringComparison]::OrdinalIgnoreCase) -and
            $report.Subsystem -ne 3) {
            throw 'GPU inference worker must use the Windows console subsystem.'
        }
        foreach ($import in @($report.NormalImports) + @($report.DelayImports)) {
            if ($system.Contains([string]$import)) {
                if (@($SystemDriverImports).Contains([string]$import)) {
                    $observedProviderDependency = $true
                }
                continue
            }
            if (-not $packaged.Contains([string]$import)) {
                throw "GPU pack contains an undeclared native dependency: $binaryName imports $import"
            }
            $observedProviderDependency = $true
            $destination = Join-Path $PackBin ([string]$import)
            if (-not (Test-Path -LiteralPath $destination -PathType Leaf)) {
                $source = Join-Path $SdkRoot "bin\$import"
                $null = Assert-RegularNonReparseFile $source "Pinned provider runtime $import"
                Copy-Item -LiteralPath $source -Destination $destination
                $null = Assert-RegularNonReparseFile $destination "Materialized provider runtime $import"
            }
            $pending.Enqueue($destination)
        }
    }
    if (-not $observedProviderDependency) {
        throw 'GPU worker dependency closure does not contain the selected provider runtime or driver interface.'
    }
}

$repositoryRoot = Get-NormalizedFullPath (Split-Path -Parent $PSScriptRoot)
if (-not $ToolchainManifestPath) {
    $ToolchainManifestPath = Join-Path $repositoryRoot 'runtime-manifests\gpu-worker-toolchain-windows-x64.json'
}
$contractPath = Get-NormalizedFullPath $ToolchainManifestPath
$null = Assert-RegularNonReparseFile $contractPath 'GPU worker-pack toolchain manifest'
$contract = Get-Content -LiteralPath $contractPath -Raw | ConvertFrom-Json
Assert-ExactProperties $contract @('schema_version', 'app_version', 'target_triple', 'rust', 'native_source', 'msvc', 'build', 'vulkan', 'cuda') 'GPU worker-pack toolchain manifest'
Assert-ExactProperties $contract.rust @('release', 'commit', 'host') 'Rust toolchain contract'
Assert-ExactProperties $contract.native_source @('transcribe_cpp_version', 'transcribe_cpp_checksum', 'transcribe_cpp_sys_version', 'transcribe_cpp_sys_checksum', 'source_revision', 'sherpa_onnx_archive') 'Native source contract'
Assert-ExactProperties $contract.native_source.sherpa_onnx_archive @('filename', 'size_bytes', 'sha256') 'Sherpa ONNX archive contract'
Assert-ExactProperties $contract.msvc @('visual_studio_installation_version', 'toolset_version', 'platform_toolset', 'cmake_version', 'runtime', 'reproducible_flag') 'MSVC toolchain contract'
Assert-ExactProperties $contract.build @('profile', 'static_cpu_scheduling', 'dynamic_backends', 'openmp') 'Worker build contract'
Assert-ExactProperties $contract.vulkan @('sdk_version', 'provider', 'required_files', 'system_driver_imports', 'packaged_runtime_imports') 'Vulkan provider contract'
Assert-ExactProperties $contract.cuda @('sdk_directory_version', 'nvcc_version', 'provider', 'cmake_architectures', 'required_files', 'system_driver_imports', 'packaged_runtime_imports') 'CUDA provider contract'
if ($contract.schema_version -ne 1 -or
    $contract.app_version -cnotmatch '^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$' -or
    $contract.target_triple -cne 'x86_64-pc-windows-msvc' -or
    $contract.msvc.platform_toolset -cne 'v143' -or
    $contract.msvc.runtime -cne 'MultiThreaded' -or
    $contract.msvc.reproducible_flag -cne '/Brepro' -or
    $contract.build.profile -cne 'release' -or
    -not $contract.build.static_cpu_scheduling -or
    $contract.build.dynamic_backends -or
    $contract.build.openmp) {
    throw 'GPU worker-pack toolchain manifest violates the reviewed Windows x64 static-runtime contract.'
}

Assert-BaseToolchain $contract $repositoryRoot
$archiveContract = $contract.native_source.sherpa_onnx_archive
if (-not $NativeArchiveDirectory) {
    $NativeArchiveDirectory = Join-Path $repositoryRoot '.ci-native'
}
$nativeArchiveRoot = Get-NormalizedFullPath $NativeArchiveDirectory
Assert-NoReparseAncestors $nativeArchiveRoot
$nativeArchive = Join-Path $nativeArchiveRoot ([string]$archiveContract.filename)
$archiveItem = Assert-RegularNonReparseFile $nativeArchive 'Pinned Sherpa ONNX archive'
if ($archiveItem.Length -ne [int64]$archiveContract.size_bytes) {
    throw 'Pinned Sherpa ONNX archive size mismatch.'
}
Assert-ExactHash $nativeArchive ([string]$archiveContract.sha256) 'Pinned Sherpa ONNX archive'
$backendName = $Backend.ToLowerInvariant()
$providerContract = if ($Backend -eq 'Vulkan') { $contract.vulkan } else { $contract.cuda }
$sdkRoot = if ($Backend -eq 'Vulkan') {
    Resolve-VulkanSdk $contract
} else {
    Resolve-CudaSdk $contract
}
if ($ToolchainCheckOnly) {
    Write-Output "$Backend worker-pack toolchain matches the pinned contract."
    return
}
if ($PackVersion -cnotmatch '^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$') {
    throw 'PackVersion must be a canonical lowercase immutable store component.'
}
$outputRoot = Get-NormalizedFullPath $OutputDirectory
Assert-NoReparseAncestors $outputRoot
if (Test-Path -LiteralPath $outputRoot) {
    throw "GPU worker-pack output already exists: $outputRoot"
}
if (-not $CargoTargetDirectory) {
    $CargoTargetDirectory = Join-Path $repositoryRoot "target-gpu-pack-build-$backendName"
}
$cargoTarget = Get-NormalizedFullPath $CargoTargetDirectory
Assert-NoReparseAncestors $cargoTarget
if (Test-Path -LiteralPath $cargoTarget) {
    throw "GPU worker Cargo target must be fresh to prevent feature/output reuse: $cargoTarget"
}

$git = Get-CommandPath 'git.exe' 'Git is required to bind worker packs to an exact source revision.'
$revision = (Invoke-NativeProcess $git @('-C', $repositoryRoot, 'rev-parse', '--verify', 'HEAD') 'Could not resolve source revision.').Stdout.Trim()
if ($revision -cnotmatch '^[0-9a-f]{40}$') {
    throw 'Git returned a noncanonical source revision.'
}
$sourceDateEpoch = (Invoke-NativeProcess $git @('-C', $repositoryRoot, 'show', '-s', '--format=%ct', $revision) 'Could not resolve source revision timestamp.').Stdout.Trim()
if ($sourceDateEpoch -cnotmatch '^[1-9][0-9]{8,11}$') {
    throw 'Git returned a noncanonical source revision timestamp.'
}
$null = Invoke-NativeProcess $git @('-C', $repositoryRoot, 'diff', '--quiet', '--exit-code') 'GPU worker release builds require a clean worktree.'
$null = Invoke-NativeProcess $git @('-C', $repositoryRoot, 'diff', '--cached', '--quiet', '--exit-code') 'GPU worker release builds require a clean index.'
foreach ($ambientName in @(
    'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CMAKE_ARGS', 'CFLAGS', 'CXXFLAGS',
    'CC', 'CXX', 'CL', '_CL_', 'LINK', 'CMAKE_GENERATOR', 'CMAKE_TOOLCHAIN_FILE',
    'NVCC_PREPEND_FLAGS', 'NVCC_APPEND_FLAGS'
)) {
    $ambient = Get-Item -LiteralPath "Env:$ambientName" -ErrorAction SilentlyContinue
    if ($null -ne $ambient -and -not [string]::IsNullOrWhiteSpace([string]$ambient.Value)) {
        throw "GPU worker release builds reject ambient toolchain override $ambientName."
    }
}

$manifestPath = Join-Path $repositoryRoot 'Cargo.toml'
$authoringManifestPath = Join-Path $repositoryRoot 'tools\worker-pack-author\Cargo.toml'
$cargo = Get-CommandPath 'cargo.exe' 'Cargo is required to build GPU worker packs.'
$previousCargoTarget = $env:CARGO_TARGET_DIR
$previousRevision = $env:SCRIBE_BUILD_REVISION
$previousWorkerDigest = $env:SCRIBE_BUNDLED_WORKER_SHA256
$previousBuildingWorker = $env:SCRIBE_BUILDING_WORKER
$previousVulkanSdk = $env:VULKAN_SDK
$previousCudaPath = $env:CUDA_PATH
$previousCmakeArguments = $env:TRANSCRIBE_CMAKE_ARGS
$previousSherpaArchiveRoot = $env:SHERPA_ONNX_ARCHIVE_DIR
$previousSourceDateEpoch = $env:SOURCE_DATE_EPOCH
$previousMsvcFlags = $env:_CL_
$stagingRoot = "$outputRoot.staging-$([guid]::NewGuid().ToString('N'))"
$stagingCreated = $false

try {
    $env:CARGO_TARGET_DIR = $cargoTarget
    $env:SCRIBE_BUILD_REVISION = $revision
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $null
    $env:SCRIBE_BUILDING_WORKER = '1'
    $env:SHERPA_ONNX_ARCHIVE_DIR = $nativeArchiveRoot
    $env:SOURCE_DATE_EPOCH = $sourceDateEpoch
    $env:_CL_ = [string]$contract.msvc.reproducible_flag
    $env:TRANSCRIBE_CMAKE_ARGS = '-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded -DTRANSCRIBE_GGML_BACKEND_DL=OFF -DGGML_OPENMP=OFF'
    if ($Backend -eq 'Vulkan') {
        $env:VULKAN_SDK = $sdkRoot
        $env:CUDA_PATH = $null
    } else {
        $env:CUDA_PATH = $sdkRoot
        $env:VULKAN_SDK = $null
        $env:TRANSCRIBE_CMAKE_ARGS += " -DCMAKE_CUDA_ARCHITECTURES=$($contract.cuda.cmake_architectures)"
    }

    $null = Invoke-NativeProcess $cargo @(
        'build', '--locked', '--offline', '--release',
        '--bin', 'scribe-worker-pack-tool',
        '--manifest-path', $authoringManifestPath
    ) 'Worker-pack authoring tool build failed.'
    $authoringTool = Join-Path $cargoTarget 'release\scribe-worker-pack-tool.exe'
    $null = Assert-RegularNonReparseFile $authoringTool 'Worker-pack authoring tool'

    if ($SigningMode -eq 'Production') {
        $privateKey = Get-NormalizedFullPath $ProductionPrivateKeyPath
        $null = Invoke-NativeProcess $authoringTool @(
            'check-production-key', '--key-id', $ProductionKeyId,
            '--private-key', $privateKey
        ) 'Production GPU pack signing is not provisioned.'
    }

    $feature = "$backendName-acceleration"
    $null = Invoke-NativeProcess $cargo @(
        'build', '--locked', '--offline', '--release',
        '--bin', 'scribe-inference-worker', '--features', $feature,
        '--manifest-path', $manifestPath
    ) "$Backend inference worker build failed."
    $worker = Join-Path $cargoTarget 'release\scribe-inference-worker.exe'
    $null = Assert-RegularNonReparseFile $worker "$Backend inference worker"

    New-Item -ItemType Directory -Path (Join-Path $stagingRoot 'bin') | Out-Null
    $stagingCreated = $true
    $stagedWorker = Join-Path $stagingRoot 'bin\scribe-inference-worker.exe'
    Copy-Item -LiteralPath $worker -Destination $stagedWorker
    $null = Assert-RegularNonReparseFile $stagedWorker 'Materialized GPU inference worker'
    Copy-ReviewedGpuWorkerDependencyClosure `
        $stagedWorker `
        $sdkRoot `
        (Join-Path $stagingRoot 'bin') `
        @($providerContract.system_driver_imports) `
        @($providerContract.packaged_runtime_imports)

    $packId = "scribe-$backendName-windows-x64"
    $authorArguments = @(
        'author',
        '--backend', $backendName,
        '--pack-id', $packId,
        '--pack-root', $stagingRoot,
        '--pack-version', $PackVersion,
        '--provider', ([string]$providerContract.provider),
        '--security-epoch', '1',
        '--worker-path', 'bin/scribe-inference-worker.exe'
    )
    if ($SigningMode -eq 'Fixture') {
        $authorArguments += '--fixture-signing'
    } else {
        $authorArguments += @(
            '--key-id', $ProductionKeyId,
            '--private-key', (Get-NormalizedFullPath $ProductionPrivateKeyPath)
        )
    }
    $authored = Invoke-NativeProcess $authoringTool $authorArguments 'GPU worker-pack authoring failed.'
    try {
        $descriptor = $authored.Stdout | ConvertFrom-Json
    }
    catch {
        throw "Worker-pack authoring returned invalid JSON: $($_.Exception.Message)"
    }
    Assert-ExactProperties $descriptor @('pack_id', 'pack_version', 'pack_digest', 'key_id', 'payload_files', 'installed_payload_bytes') 'Authored worker-pack descriptor'
    if ($descriptor.pack_id -cne $packId -or
        $descriptor.pack_version -cne $PackVersion -or
        [string]$descriptor.pack_digest -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Worker-pack authoring returned a mismatched identity.'
    }
    Move-Item -LiteralPath $stagingRoot -Destination $outputRoot
    $stagingCreated = $false
    [pscustomobject]@{
        Backend = $Backend
        PackRoot = $outputRoot
        PackId = [string]$descriptor.pack_id
        PackVersion = [string]$descriptor.pack_version
        PackDigest = [string]$descriptor.pack_digest
        SigningKeyId = [string]$descriptor.key_id
        PayloadFiles = [int]$descriptor.payload_files
        InstalledPayloadBytes = [int64]$descriptor.installed_payload_bytes
        SourceRevision = $revision
    }
}
finally {
    $env:CARGO_TARGET_DIR = $previousCargoTarget
    $env:SCRIBE_BUILD_REVISION = $previousRevision
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $previousWorkerDigest
    $env:SCRIBE_BUILDING_WORKER = $previousBuildingWorker
    $env:VULKAN_SDK = $previousVulkanSdk
    $env:CUDA_PATH = $previousCudaPath
    $env:TRANSCRIBE_CMAKE_ARGS = $previousCmakeArguments
    $env:SHERPA_ONNX_ARCHIVE_DIR = $previousSherpaArchiveRoot
    $env:SOURCE_DATE_EPOCH = $previousSourceDateEpoch
    $env:_CL_ = $previousMsvcFlags
    if ($stagingCreated -and (Test-Path -LiteralPath $stagingRoot)) {
        $expectedParent = Get-NormalizedFullPath (Split-Path -Parent $outputRoot)
        $observedParent = Get-NormalizedFullPath (Split-Path -Parent $stagingRoot)
        if ($observedParent -cne $expectedParent -or
            -not (Split-Path -Leaf $stagingRoot).StartsWith("$(Split-Path -Leaf $outputRoot).staging-", [System.StringComparison]::Ordinal)) {
            throw 'Refusing to clean a staging path outside the exact GPU worker-pack output parent.'
        }
        Remove-Item -LiteralPath $stagingRoot -Recurse -Force
    }
}
