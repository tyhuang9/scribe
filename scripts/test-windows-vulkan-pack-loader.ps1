#requires -Version 7.0
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-vulkan-policy-pack.ps1')
. (Join-Path $PSScriptRoot 'windows-pe-imports.ps1')

# Exercise the actual closure walker, with only PE parsing replaced by a
# deterministic dependency map. No fixture DLL is executable or loaded.
$tokens = $null
$parseErrors = $null
$builderAst = [Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'build-windows-gpu-worker-pack.ps1'), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'Pack builder did not parse.' }
$closure = @($builderAst.FindAll({ param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -ceq 'Copy-ReviewedGpuWorkerDependencyClosure'
}, $false))
if ($closure.Count -ne 1) { throw 'Expected exactly one production dependency-closure function.' }
. ([scriptblock]::Create($closure[0].Extent.Text))

$script:checks = 0
function Assert-PackLoader([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:checks++
}
function Assert-PackLoaderRejected([scriptblock]$Action, [string]$Message) {
    $rejected = $false
    try { & $Action | Out-Null } catch { $rejected = $true }
    Assert-PackLoader $rejected $Message
}

$repo = Split-Path -Parent $PSScriptRoot
$contract = Get-Content -LiteralPath (Join-Path $repo 'runtime-manifests/gpu-worker-toolchain-windows-x64.json') -Raw | ConvertFrom-Json
$manifest = Get-ScribeVulkanPolicyManifest $repo $contract.vulkan.policy_loader
Assert-PackLoader ($manifest.artifact.filename -ceq 'vulkan-1.dll') 'The policy artifact is not pinned.'
Assert-PackLoader (@($contract.vulkan.system_driver_imports).Count -eq 0) 'Vulkan has a system-loader exemption.'
Assert-PackLoader ((@($contract.vulkan.packaged_runtime_imports) -join '|') -ceq 'vulkan-1.dll') 'The Vulkan pack must contain its loader.'
foreach ($reference in @(
    [pscustomobject]@{manifest_path='../source-manifest.json'; manifest_sha256=$contract.vulkan.policy_loader.manifest_sha256},
    [pscustomobject]@{manifest_path=$contract.vulkan.policy_loader.manifest_path; manifest_sha256=('0' * 64)},
    [pscustomobject]@{manifest_path=$contract.vulkan.policy_loader.manifest_path},
    [pscustomobject]@{manifest_path=$contract.vulkan.policy_loader.manifest_path; manifest_sha256=$contract.vulkan.policy_loader.manifest_sha256; extra=1}
)) {
    Assert-PackLoaderRejected { Get-ScribeVulkanPolicyManifest $repo $reference } 'Invalid policy manifest reference was accepted.'
}

$tempParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\','/')
$scratch = Join-Path $tempParent ('scribe-vulkan-pack-tests-' + [Guid]::NewGuid().ToString('N'))
Assert-ScribeGpuWorkerNoReparse $scratch
if (Test-Path -LiteralPath $scratch) { throw 'Expected a fresh scratch directory.' }
[IO.Directory]::CreateDirectory($scratch) | Out-Null
try {
    $source = Join-Path $scratch 'source.dll'
    $destination = Join-Path $scratch 'copied.dll'
    [IO.File]::WriteAllText($source, 'deterministic authenticated runtime')
    $digest = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()
    Copy-ScribePackRuntimeFile $source $destination $digest
    Assert-PackLoader ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $digest) 'Pinned runtime copy changed bytes.'
    $stamp = (Get-Item -LiteralPath $destination).LastWriteTimeUtc
    Copy-ScribePackRuntimeFile $source $destination $digest
    Assert-PackLoader ((Get-Item -LiteralPath $destination).LastWriteTimeUtc -eq $stamp) 'Matching existing runtime was overwritten.'
    $absent = Join-Path $scratch 'must-remain-absent.dll'
    Assert-PackLoaderRejected { Copy-ScribePackRuntimeFile $source $absent ('0' * 64) } 'Wrong source digest was accepted.'
    Assert-PackLoader (-not (Test-Path -LiteralPath $absent)) 'Wrong source digest created output.'
    Assert-PackLoaderRejected { Copy-ScribePackRuntimeFile $source $absent 'invalid' } 'Malformed source digest was accepted.'
    [IO.File]::WriteAllText($destination, 'preexisting untrusted file')
    Assert-PackLoaderRejected { Copy-ScribePackRuntimeFile $source $destination $digest } 'Mismatched existing runtime was trusted.'
    Assert-PackLoader ((Get-Content -LiteralPath $destination -Raw) -ceq 'preexisting untrusted file') 'Mismatched existing runtime was overwritten.'
    Assert-PackLoaderRejected { Copy-ScribePackRuntimeFile $source $scratch $digest } 'Directory destination was accepted.'
    Assert-PackLoaderRejected { Copy-ScribePackRuntimeFile $scratch $absent } 'Directory source was accepted.'
    $cudaStyle = Join-Path $scratch 'sdk-runtime.dll'
    Copy-ScribePackRuntimeFile $source $cudaStyle
    Assert-PackLoader ((Get-FileHash -LiteralPath $cudaStyle -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $digest) 'SDK runtime copy changed bytes.'

    $sdk = Join-Path $scratch 'sdk'
    [IO.Directory]::CreateDirectory((Join-Path $sdk 'bin')) | Out-Null
    $vulkanBin = Join-Path $scratch 'vulkan-bin'
    $cudaBin = Join-Path $scratch 'cuda-bin'
    foreach ($bin in @($vulkanBin, $cudaBin)) {
        [IO.Directory]::CreateDirectory($bin) | Out-Null
        [IO.File]::WriteAllText((Join-Path $bin 'worker.exe'), 'non-executable worker fixture')
    }
    [IO.File]::WriteAllText((Join-Path $sdk 'bin/vulkan-1.dll'), 'must never select SDK loader')
    [IO.File]::WriteAllText((Join-Path $sdk 'bin/cublas64_12.dll'), 'CUDA BLAS fixture')
    [IO.File]::WriteAllText((Join-Path $sdk 'bin/cublaslt64_12.dll'), 'CUDA BLAS LT fixture')
    $originalPeParser = (Get-Item Function:Get-WindowsPeImportReport).ScriptBlock
    try {
        $script:closureReports = @{}
        $script:closureVisits = [Collections.Generic.List[string]]::new()
        function Get-WindowsPeImportReport([string]$Path) {
            $name = [IO.Path]::GetFileName($Path)
            $script:closureVisits.Add($name)
            if (-not $script:closureReports.ContainsKey($name)) { throw 'Unexpected PE dependency fixture.' }
            return [pscustomobject]@{ Machine=0x8664; Subsystem=3; NormalImports=$script:closureReports[$name]; DelayImports=@() }
        }
        $script:closureReports['worker.exe'] = @('vulkan-1.dll')
        $script:closureReports['vulkan-1.dll'] = @('advapi32.dll','cfgmgr32.dll','kernel32.dll')
        $pins = @{'vulkan-1.dll'=[pscustomobject]@{Path=$source;Sha256=$digest}}
        Copy-ReviewedGpuWorkerDependencyClosure (Join-Path $vulkanBin 'worker.exe') $sdk $vulkanBin @() @('vulkan-1.dll') $pins
        $vulkanDll = Join-Path $vulkanBin 'vulkan-1.dll'
        Assert-PackLoader ((Get-FileHash -LiteralPath $vulkanDll -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $digest) 'Vulkan closure ignored the pinned loader source.'
        Assert-PackLoader (($script:closureVisits -join '|') -ceq 'worker.exe|vulkan-1.dll') 'Vulkan closure did not traverse its pinned loader.'
        [IO.File]::WriteAllText($vulkanDll, 'preexisting wrong loader')
        Assert-PackLoaderRejected { Copy-ReviewedGpuWorkerDependencyClosure (Join-Path $vulkanBin 'worker.exe') $sdk $vulkanBin @() @('vulkan-1.dll') $pins } 'Closure accepted a mismatched existing loader.'
        Assert-PackLoader ((Get-Content -LiteralPath $vulkanDll -Raw) -ceq 'preexisting wrong loader') 'Closure overwrote a mismatched existing loader.'
        $pins['vulkan-1.dll'].Sha256 = '0' * 64
        Assert-PackLoaderRejected { Copy-ReviewedGpuWorkerDependencyClosure (Join-Path $vulkanBin 'worker.exe') $sdk $vulkanBin @() @('vulkan-1.dll') $pins } 'Closure accepted an unauthenticated pinned source.'

        $script:closureReports['worker.exe'] = @('cublas64_12.dll','nvcuda.dll')
        $script:closureReports['cublas64_12.dll'] = @('cublaslt64_12.dll')
        $script:closureReports['cublaslt64_12.dll'] = @('kernel32.dll')
        $script:closureVisits.Clear()
        Copy-ReviewedGpuWorkerDependencyClosure (Join-Path $cudaBin 'worker.exe') $sdk $cudaBin @('nvcuda.dll') @('cublas64_12.dll','cublaslt64_12.dll')
        Assert-PackLoader ((Get-Content -LiteralPath (Join-Path $cudaBin 'cublas64_12.dll') -Raw) -ceq 'CUDA BLAS fixture') 'CUDA closure stopped resolving its SDK runtime.'
        Assert-PackLoader ((Get-Content -LiteralPath (Join-Path $cudaBin 'cublaslt64_12.dll') -Raw) -ceq 'CUDA BLAS LT fixture') 'CUDA closure omitted a transitive runtime.'
        Assert-PackLoader (($script:closureVisits -join '|') -ceq 'worker.exe|cublas64_12.dll|cublaslt64_12.dll') 'CUDA dependency traversal changed.'
        $script:closureReports['worker.exe'] = @('unreviewed.dll')
        Assert-PackLoaderRejected { Copy-ReviewedGpuWorkerDependencyClosure (Join-Path $cudaBin 'worker.exe') $sdk $cudaBin @('nvcuda.dll') @('cublas64_12.dll') } 'Closure accepted an unknown DLL.'
        $script:closureReports['worker.exe'] = @('kernel32.dll')
        Assert-PackLoaderRejected { Copy-ReviewedGpuWorkerDependencyClosure (Join-Path $cudaBin 'worker.exe') $sdk $cudaBin @('nvcuda.dll') @('cublas64_12.dll') } 'Closure accepted a worker with no provider dependency.'
    } finally { Set-Item Function:Get-WindowsPeImportReport $originalPeParser }

    $loader = Join-Path $scratch 'loader'
    $headers = Join-Path $scratch 'headers'
    $built = [pscustomobject]@{
        LicensePath=(Join-Path $loader 'LICENSE.txt'); LicenseDirectory=(Join-Path $loader 'LICENSES')
        HeaderLicensePath=(Join-Path $headers 'LICENSE.md'); HeaderLicenseDirectory=(Join-Path $headers 'LICENSES')
    }
    foreach ($directory in @($built.LicenseDirectory, $built.HeaderLicenseDirectory)) {
        [IO.Directory]::CreateDirectory($directory) | Out-Null
    }
    [IO.File]::WriteAllText($built.LicensePath, 'loader license index')
    [IO.File]::WriteAllText($built.HeaderLicensePath, 'header license index')
    foreach ($name in @('Apache-2.0.txt','CC-BY-4.0.txt','HPND-Kevlin-Henney.txt','MIT-Khronos-old.txt','MIT.txt')) {
        [IO.File]::WriteAllText((Join-Path $built.LicenseDirectory $name), "loader $name")
    }
    foreach ($name in @('Apache-2.0.txt','MIT.txt')) {
        [IO.File]::WriteAllText((Join-Path $built.HeaderLicenseDirectory $name), "header $name")
    }
    $pack = Join-Path $scratch 'pack'
    [IO.Directory]::CreateDirectory($pack) | Out-Null
    Copy-ScribeVulkanPolicyLicenses $built $pack
    Assert-PackLoader (@(Get-ChildItem -LiteralPath $pack -File -Recurse).Count -eq 9) 'License payload inventory is incomplete.'
    Assert-PackLoader ((Get-Content -LiteralPath (Join-Path $pack 'licenses/vulkan-loader/LICENSES/MIT.txt') -Raw) -ceq 'loader MIT.txt') 'Loader license bytes changed.'
    Assert-PackLoader ((Get-Content -LiteralPath (Join-Path $pack 'licenses/vulkan-headers/LICENSE.md') -Raw) -ceq 'header license index') 'Header license index bytes changed.'
    Assert-PackLoaderRejected { Copy-ScribeVulkanPolicyLicenses $built $pack } 'Existing license output was reused.'
    [IO.File]::WriteAllText((Join-Path $built.LicenseDirectory 'unexpected.txt'), 'unexpected')
    $rejectedPack = Join-Path $scratch 'rejected-pack'
    [IO.Directory]::CreateDirectory($rejectedPack) | Out-Null
    Assert-PackLoaderRejected { Copy-ScribeVulkanPolicyLicenses $built $rejectedPack } 'Unexpected license inventory was accepted.'
    Assert-PackLoader (@(Get-ChildItem -LiteralPath $rejectedPack -Force).Count -eq 0) 'Unexpected license inventory produced partial output.'
    Assert-PackLoader ($script:checks -ge 33) 'Expected Vulkan pack tests were not executed.'
    "Windows Vulkan pack loader tests passed ($script:checks checks)."
} finally {
    $current = (Get-ScribeGpuWorkerPhysicalDirectory $scratch 'Owned Vulkan pack test scratch').FullName
    if ($current -cne $scratch -or (Split-Path -Parent $current) -cne $tempParent) { throw 'Cleanup target changed.' }
    Assert-ScribeGpuWorkerNoReparseDescendants $current
    Remove-Item -LiteralPath $current -Recurse
}
