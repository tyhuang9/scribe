[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($env:OS -ne 'Windows_NT') {
    throw 'Windows CUDA pack-input tests require Windows.'
}

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-ThrowsContaining(
    [scriptblock]$Action,
    [string]$ExpectedMessage,
    [string]$FailureMessage
) {
    $rejected = $false
    try { & $Action }
    catch { $rejected = $_.Exception.Message.Contains($ExpectedMessage) }
    Assert-True $rejected $FailureMessage
}

. (Join-Path $PSScriptRoot 'windows-gpu-worker-cmake-bootstrap.ps1')
. (Join-Path $PSScriptRoot 'windows-vulkan-policy-pack.ps1')
. (Join-Path $PSScriptRoot 'windows-cuda-sdk-inventory.ps1')

$builderPath = Join-Path $PSScriptRoot 'build-windows-gpu-worker-pack.ps1'
$builderTokens = $null
$builderParseErrors = $null
$builderAst = [System.Management.Automation.Language.Parser]::ParseFile(
    $builderPath,
    [ref]$builderTokens,
    [ref]$builderParseErrors
)
Assert-True (@($builderParseErrors).Count -eq 0) 'CUDA pack-input tests could not parse the worker-pack builder.'
$dependencyClosureDefinitions = @($builderAst.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $node.Name -ceq 'Copy-ReviewedGpuWorkerDependencyClosure'
}, $true))
Assert-True ($dependencyClosureDefinitions.Count -eq 1) 'CUDA pack-input tests did not find one dependency-closure implementation.'
Invoke-Expression $dependencyClosureDefinitions[0].Extent.Text
$ReviewedWindowsSystemDlls = @()
$fixtureRuntimePin = [pscustomobject]@{ Path = 'unused'; Sha256 = '0' * 64 }
Assert-ThrowsContaining `
    {
        Copy-ReviewedGpuWorkerDependencyClosure `
            'unused-worker.exe' `
            'unused-sdk' `
            'unused-pack-bin' `
            @('nvcuda.dll') `
            @('cudart64_12.dll') `
            @{ 'wrong-runtime.dll' = $fixtureRuntimePin } `
            $true
    } `
    'Authenticated provider runtime source is missing: cudart64_12.dll' `
    'Required pinned-runtime closure accepted a same-size wrong-key source set.'
Assert-ThrowsContaining `
    {
        Copy-ReviewedGpuWorkerDependencyClosure `
            'unused-worker.exe' `
            'unused-sdk' `
            'unused-pack-bin' `
            @('nvcuda.dll') `
            @('cudart64_12.dll') `
            @{
                'cudart64_12.dll' = $fixtureRuntimePin
                'unexpected.dll' = $fixtureRuntimePin
            } `
            $true
    } `
    'source set does not match' `
    'Required pinned-runtime closure accepted an extra source.'

$noticeContract = @(Get-ScribeCudaPackNoticeContract)
Assert-True ($noticeContract.Count -eq 2) 'CUDA notice policy must contain exactly two runtime-component notices.'
Assert-True (
    (($noticeContract | ForEach-Object { $_.SourceRelativePath }) -join '|') -ceq
    'licenses/cuda_cudart/LICENSE|licenses/libcublas/LICENSE'
) 'CUDA notice source paths changed.'
Assert-True (
    (($noticeContract | ForEach-Object { $_.DestinationRelativePath }) -join '|') -ceq
    'licenses/cuda-cudart/LICENSE|licenses/cuda-cublas/LICENSE'
) 'CUDA notice destinations changed.'
Assert-True (
    @($noticeContract | Where-Object {
        $_.Sha256 -cne 'e2c71babfd18a8e69542dd7e9ca018f9caa438094001a58e6bc4d8c999bf0d07'
    }).Count -eq 0
) 'CUDA notice digest pins changed.'
Assert-True (
    @($noticeContract | Where-Object { [int64]$_.SizeBytes -ne 63021 }).Count -eq 0
) 'CUDA notice size pins changed.'

$fixtureRoot = Join-Path `
    ([System.IO.Path]::GetTempPath()) `
    "scribe-cuda-pack-inputs-$([guid]::NewGuid().ToString('N'))"
$sdkRoot = Join-Path $fixtureRoot 'v12.8'
$copyRoot = Join-Path $fixtureRoot 'copy-output'
$junctionTarget = Join-Path $fixtureRoot 'junction-target'
$junctionPath = Join-Path $sdkRoot 'linked'
$noticeJunctionPath = Join-Path $fixtureRoot 'notice-link-root\licenses\cuda_cudart'
New-Item -ItemType Directory -Path $sdkRoot | Out-Null
New-Item -ItemType Directory -Path $copyRoot | Out-Null
try {
    $fixtureFiles = [ordered]@{
        'bin/cublasLt64_12.dll' = 'fixture cublasLt runtime bytes'
        'licenses/cuda_cudart/LICENSE' = 'fixture cudart notice bytes'
        'licenses/libcublas/LICENSE' = 'fixture cublas notice bytes'
    }
    foreach ($entry in $fixtureFiles.GetEnumerator()) {
        $path = Join-Path $sdkRoot $entry.Key.Replace('/', '\')
        $null = New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force
        [System.IO.File]::WriteAllText(
            $path,
            $entry.Value,
            [System.Text.UTF8Encoding]::new($false)
        )
    }
    $inventory = @($fixtureFiles.Keys | ForEach-Object {
        $path = Join-Path $sdkRoot $_.Replace('/', '\')
        [pscustomobject]@{
            path = $_
            sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    })

    $runtimeSources = Get-AuthenticatedCudaRuntimeSources `
        $sdkRoot `
        $inventory `
        @('cublaslt64_12.dll')
    Assert-True ($runtimeSources.Count -eq 1) 'Authenticated CUDA runtime source count changed.'
    Assert-True (
        [System.IO.Path]::GetFileName([string]$runtimeSources['cublaslt64_12.dll'].Path) -ceq
        'cublasLt64_12.dll'
    ) 'CUDA runtime source did not preserve authenticated canonical filename case.'

    $wrongCaseInventory = @($inventory | ForEach-Object {
        [pscustomobject]@{
            path = if ($_.path -ceq 'bin/cublasLt64_12.dll') {
                'bin/cublaslt64_12.dll'
            } else { $_.path }
            sha256 = $_.sha256
        }
    })
    Assert-ThrowsContaining `
        { $null = Get-AuthenticatedCudaSdkFile $sdkRoot $wrongCaseInventory 'bin/cublaslt64_12.dll' } `
        'noncanonical physical case' `
        'CUDA pack input accepted noncanonical physical filename case.'

    $missingInventory = @($inventory) + @([pscustomobject]@{
        path = 'bin/missing.dll'
        sha256 = '0' * 64
    })
    Assert-ThrowsContaining `
        { $null = Get-AuthenticatedCudaSdkFile $sdkRoot $missingInventory 'bin/missing.dll' } `
        'is missing' `
        'CUDA pack input accepted a missing authenticated source.'

    $wrongHashInventory = @($inventory | ForEach-Object {
        [pscustomobject]@{
            path = $_.path
            sha256 = if ($_.path -ceq 'bin/cublasLt64_12.dll') { '0' * 64 } else { $_.sha256 }
        }
    })
    Assert-ThrowsContaining `
        { $null = Get-AuthenticatedCudaSdkFile $sdkRoot $wrongHashInventory 'bin/cublasLt64_12.dll' } `
        'SHA-256 mismatch' `
        'CUDA pack input accepted a wrong authenticated source digest.'

    $runtimeSource = Join-Path $sdkRoot 'bin\cublasLt64_12.dll'
    [System.IO.File]::WriteAllText(
        "${runtimeSource}:scribe-test",
        'untrusted stream',
        [System.Text.UTF8Encoding]::new($false)
    )
    Assert-ThrowsContaining `
        { $null = Get-AuthenticatedCudaSdkFile $sdkRoot $inventory 'bin/cublasLt64_12.dll' } `
        'alternate data stream' `
        'CUDA pack input accepted an alternate data stream.'

    New-Item -ItemType Directory -Path $junctionTarget | Out-Null
    [System.IO.File]::WriteAllText(
        (Join-Path $junctionTarget 'runtime.dll'),
        'linked runtime bytes',
        [System.Text.UTF8Encoding]::new($false)
    )
    New-Item -ItemType Junction -Path $junctionPath -Target $junctionTarget | Out-Null
    $linkedDigest = (Get-FileHash -LiteralPath (Join-Path $junctionPath 'runtime.dll') -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-ThrowsContaining `
        {
            $null = Get-AuthenticatedCudaSdkFile `
                $sdkRoot `
                @([pscustomobject]@{ path = 'linked/runtime.dll'; sha256 = $linkedDigest }) `
                'linked/runtime.dll'
        } `
        'link or reparse point' `
        'CUDA pack input accepted a reparse-point ancestor.'
    Remove-Item -LiteralPath $junctionPath -Force

    $unexpectedNoticeInventory = @(
        [pscustomobject]@{
            path = 'licenses/cuda_cudart/LICENSE'
            sha256 = $noticeContract[0].Sha256
        },
        [pscustomobject]@{
            path = 'licenses/cuda_cudart/README'
            sha256 = '0' * 64
        },
        [pscustomobject]@{
            path = 'licenses/libcublas/LICENSE'
            sha256 = $noticeContract[1].Sha256
        }
    )
    Assert-ThrowsContaining `
        { $null = @(Get-AuthenticatedCudaPackNoticeSources $sdkRoot $unexpectedNoticeInventory) } `
        'unexpected inputs' `
        'CUDA notice policy accepted an unexpected source-group input.'

    $fixedNoticeInventory = @($noticeContract | ForEach-Object {
        [pscustomobject]@{
            path = $_.SourceRelativePath
            sha256 = $_.Sha256
        }
    })
    $missingNoticeRoot = Join-Path $fixtureRoot 'notice-missing-root'
    $null = New-Item -ItemType Directory -Path (Join-Path $missingNoticeRoot 'licenses\cuda_cudart') -Force
    Assert-ThrowsContaining `
        { $null = @(Get-AuthenticatedCudaPackNoticeSources $missingNoticeRoot $fixedNoticeInventory) } `
        'is missing' `
        'CUDA notice policy accepted a missing fixed notice source.'

    Assert-ThrowsContaining `
        { $null = @(Get-AuthenticatedCudaPackNoticeSources $sdkRoot $inventory) } `
        'pin mismatch' `
        'CUDA notice policy accepted a wrong notice digest pin.'
    Assert-ThrowsContaining `
        { $null = @(Get-AuthenticatedCudaPackNoticeSources $sdkRoot $fixedNoticeInventory) } `
        'size mismatch' `
        'CUDA notice policy accepted a wrong notice byte size.'

    $adsNoticeRoot = Join-Path $fixtureRoot 'notice-ads-root'
    $adsNoticePath = Join-Path $adsNoticeRoot 'licenses\cuda_cudart\LICENSE'
    $null = New-Item -ItemType Directory -Path (Split-Path -Parent $adsNoticePath) -Force
    [System.IO.File]::WriteAllBytes($adsNoticePath, [byte[]]::new(63021))
    [System.IO.File]::WriteAllText(
        "${adsNoticePath}:scribe-test",
        'untrusted notice stream',
        [System.Text.UTF8Encoding]::new($false)
    )
    Assert-ThrowsContaining `
        { $null = @(Get-AuthenticatedCudaPackNoticeSources $adsNoticeRoot $fixedNoticeInventory) } `
        'alternate data stream' `
        'CUDA notice policy accepted an alternate data stream.'

    $noticeLinkRoot = Join-Path $fixtureRoot 'notice-link-root'
    $noticeLinkTarget = Join-Path $fixtureRoot 'notice-link-target'
    $noticeLinkTargetLicense = Join-Path $noticeLinkTarget 'LICENSE'
    $null = New-Item -ItemType Directory -Path (Join-Path $noticeLinkRoot 'licenses') -Force
    $null = New-Item -ItemType Directory -Path $noticeLinkTarget -Force
    [System.IO.File]::WriteAllBytes($noticeLinkTargetLicense, [byte[]]::new(63021))
    New-Item -ItemType Junction -Path $noticeJunctionPath -Target $noticeLinkTarget | Out-Null
    Assert-ThrowsContaining `
        { $null = @(Get-AuthenticatedCudaPackNoticeSources $noticeLinkRoot $fixedNoticeInventory) } `
        'link or reparse point' `
        'CUDA notice policy accepted a reparse-point source group.'
    Remove-Item -LiteralPath $noticeJunctionPath -Force

    $copySource = Join-Path $fixtureRoot 'copy-source.txt'
    [System.IO.File]::WriteAllText(
        $copySource,
        'authenticated fixture notice bytes',
        [System.Text.UTF8Encoding]::new($false)
    )
    $copyDigest = (Get-FileHash -LiteralPath $copySource -Algorithm SHA256).Hash.ToLowerInvariant()
    $copyDestination = Join-Path $copyRoot 'LICENSE'
    Copy-AuthenticatedCudaPackFile $copySource $copyDestination $copyDigest
    Assert-True (
        [System.Linq.Enumerable]::SequenceEqual(
            [System.IO.File]::ReadAllBytes($copySource),
            [System.IO.File]::ReadAllBytes($copyDestination)
        )
    ) 'Authenticated CUDA pack copy changed source bytes.'

    $preexistingDestination = Join-Path $copyRoot 'preexisting-LICENSE'
    [System.IO.File]::WriteAllText(
        $preexistingDestination,
        'must remain unchanged',
        [System.Text.UTF8Encoding]::new($false)
    )
    Assert-ThrowsContaining `
        { Copy-AuthenticatedCudaPackFile $copySource $preexistingDestination $copyDigest } `
        'destination must be fresh' `
        'Authenticated CUDA pack copy accepted a preexisting destination.'
    Assert-True (
        [System.IO.File]::ReadAllText($preexistingDestination) -ceq 'must remain unchanged'
    ) 'Rejected CUDA pack copy overwrote its preexisting destination.'

    $syntheticNoticeSourceRoot = Join-Path $fixtureRoot 'synthetic-notice-sources'
    $null = New-Item -ItemType Directory -Path $syntheticNoticeSourceRoot
    $syntheticNoticeContracts = @(
        [pscustomobject]@{
            SourceRelativePath = 'licenses/cuda_cudart/LICENSE'
            DestinationRelativePath = 'licenses/cuda-cudart/LICENSE'
            SourcePath = Join-Path $syntheticNoticeSourceRoot 'cudart-LICENSE'
            Bytes = [System.Text.Encoding]::UTF8.GetBytes('synthetic authenticated cudart notice')
        },
        [pscustomobject]@{
            SourceRelativePath = 'licenses/libcublas/LICENSE'
            DestinationRelativePath = 'licenses/cuda-cublas/LICENSE'
            SourcePath = Join-Path $syntheticNoticeSourceRoot 'cublas-LICENSE'
            Bytes = [System.Text.Encoding]::UTF8.GetBytes('synthetic authenticated cublas notice')
        }
    )
    foreach ($syntheticNotice in $syntheticNoticeContracts) {
        [System.IO.File]::WriteAllBytes($syntheticNotice.SourcePath, $syntheticNotice.Bytes)
        $syntheticNotice | Add-Member `
            -NotePropertyName Sha256 `
            -NotePropertyValue ((Get-FileHash -LiteralPath $syntheticNotice.SourcePath -Algorithm SHA256).Hash.ToLowerInvariant())
        $syntheticNotice | Add-Member `
            -NotePropertyName SizeBytes `
            -NotePropertyValue ([int64]$syntheticNotice.Bytes.Length)
    }
    $script:syntheticCudaNoticeContract = @($syntheticNoticeContracts | ForEach-Object {
        [pscustomobject]@{
            SourceRelativePath = $_.SourceRelativePath
            DestinationRelativePath = $_.DestinationRelativePath
            Sha256 = $_.Sha256
            SizeBytes = $_.SizeBytes
        }
    })
    $syntheticNoticeSources = @($syntheticNoticeContracts | ForEach-Object {
        [pscustomobject]@{
            SourcePath = $_.SourcePath
            SourceRelativePath = $_.SourceRelativePath
            DestinationRelativePath = $_.DestinationRelativePath
            Sha256 = $_.Sha256
            SizeBytes = $_.SizeBytes
        }
    })
    $originalNoticeContractFunction = ${function:Get-ScribeCudaPackNoticeContract}
    try {
        Set-Item -Path Function:Get-ScribeCudaPackNoticeContract -Value {
            return @($script:syntheticCudaNoticeContract)
        }
        $syntheticNoticePackRoot = Join-Path $fixtureRoot 'synthetic-notice-pack'
        New-Item -ItemType Directory -Path $syntheticNoticePackRoot | Out-Null
        Copy-AuthenticatedCudaPackNotices $syntheticNoticeSources $syntheticNoticePackRoot
        foreach ($syntheticNotice in $syntheticNoticeContracts) {
            $syntheticNoticeDestination = Join-Path `
                $syntheticNoticePackRoot `
                $syntheticNotice.DestinationRelativePath.Replace('/', '\')
            Assert-True (
                [System.Linq.Enumerable]::SequenceEqual(
                    $syntheticNotice.Bytes,
                    [System.IO.File]::ReadAllBytes($syntheticNoticeDestination)
                )
            ) "CUDA notice-set copy changed bytes for $($syntheticNotice.DestinationRelativePath)."
        }

        $lateConflictPackRoot = Join-Path $fixtureRoot 'synthetic-notice-late-conflict-pack'
        $lateConflictDirectory = Join-Path $lateConflictPackRoot 'licenses\cuda-cublas'
        New-Item -ItemType Directory -Path $lateConflictDirectory -Force | Out-Null
        $lateConflictSentinel = Join-Path $lateConflictDirectory 'sentinel.txt'
        [System.IO.File]::WriteAllText(
            $lateConflictSentinel,
            'must remain unchanged',
            [System.Text.UTF8Encoding]::new($false)
        )
        Assert-ThrowsContaining `
            { Copy-AuthenticatedCudaPackNotices $syntheticNoticeSources $lateConflictPackRoot } `
            'destination directory must be fresh' `
            'CUDA notice-set copy accepted a preexisting late-group destination directory.'
        Assert-True (
            -not (Test-Path -LiteralPath (Join-Path $lateConflictPackRoot 'licenses\cuda-cudart\LICENSE'))
        ) 'Late-group CUDA notice conflict left a partially copied first notice.'
        Assert-True (
            [System.IO.File]::ReadAllText($lateConflictSentinel) -ceq 'must remain unchanged'
        ) 'Late-group CUDA notice conflict changed preexisting output.'
    }
    finally {
        Set-Item -Path Function:Get-ScribeCudaPackNoticeContract -Value $originalNoticeContractFunction
        Remove-Variable -Name syntheticCudaNoticeContract -Scope Script -ErrorAction SilentlyContinue
    }
    $restoredNoticeContract = @(Get-ScribeCudaPackNoticeContract)
    Assert-True (
        $restoredNoticeContract.Count -eq 2 -and
        $restoredNoticeContract[0].Sha256 -ceq 'e2c71babfd18a8e69542dd7e9ca018f9caa438094001a58e6bc4d8c999bf0d07'
    ) 'Synthetic CUDA notice policy test did not restore the fixed real policy.'
}
finally {
    if (Test-Path -LiteralPath $noticeJunctionPath) {
        $noticeJunctionItem = Get-Item -LiteralPath $noticeJunctionPath -Force
        if (($noticeJunctionItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            Remove-Item -LiteralPath $noticeJunctionPath -Force
        }
    }
    if (Test-Path -LiteralPath $junctionPath) {
        $junctionItem = Get-Item -LiteralPath $junctionPath -Force
        if (($junctionItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            Remove-Item -LiteralPath $junctionPath -Force
        }
    }
    $canonicalFixtureRoot = [System.IO.Path]::GetFullPath($fixtureRoot)
    $expectedPrefix = [System.IO.Path]::GetFullPath(
        (Join-Path ([System.IO.Path]::GetTempPath()) 'scribe-cuda-pack-inputs-')
    )
    if (-not $canonicalFixtureRoot.StartsWith(
        $expectedPrefix,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw 'Refusing to clean a CUDA pack-input test directory outside the dedicated temp prefix.'
    }
    Remove-Item -LiteralPath $canonicalFixtureRoot -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output 'Windows authenticated CUDA pack-input tests passed.'
