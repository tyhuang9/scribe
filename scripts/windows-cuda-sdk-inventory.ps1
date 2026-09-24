Set-StrictMode -Version Latest

function ConvertTo-AuthenticatedCudaInventory(
    [object[]]$Inventory,
    [string[]]$RequiredPaths
) {
    if (@($Inventory).Count -eq 0) {
        throw 'Production CUDA inputs are unprovisioned. Record the complete exact CUDA Toolkit file inventory and SHA-256 values before production signing.'
    }

    $caseInsensitivePaths = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $authenticatedByPath = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($authenticated in @($Inventory)) {
        if ($null -eq $authenticated) {
            throw 'Production CUDA file contract is missing.'
        }
        $actualProperties = @($authenticated.PSObject.Properties.Name | Sort-Object)
        $expectedProperties = @('path', 'sha256' | Sort-Object)
        if ($actualProperties.Count -ne $expectedProperties.Count -or
            (Compare-Object `
                -ReferenceObject $expectedProperties `
                -DifferenceObject $actualProperties `
                -CaseSensitive)) {
            throw 'Production CUDA file contract has unknown or missing fields.'
        }

        $relative = ([string]$authenticated.path).Replace('\', '/')
        $sha256 = [string]$authenticated.sha256
        $segments = @($relative -split '/')
        $hasDotSegment = $segments -contains '.' -or $segments -contains '..'
        if ($relative -cnotmatch '\A[A-Za-z0-9._+-]+(?:/[A-Za-z0-9._+-]+)*\z' -or
            $hasDotSegment -or
            -not $caseInsensitivePaths.Add($relative) -or
            $sha256 -cnotmatch '^[0-9a-f]{64}$') {
            throw "Production CUDA inventory contains a duplicate, unsafe, or noncanonical entry: $relative"
        }
        $authenticatedByPath.Add($relative, $sha256)
    }

    foreach ($required in @($RequiredPaths)) {
        if (-not $caseInsensitivePaths.Contains([string]$required)) {
            throw "Production CUDA inventory omitted required input: $required"
        }
    }
    return ,$authenticatedByPath
}

function Assert-CudaInventoryNoReparseAncestors([string]$Path) {
    $current = [System.IO.Path]::GetFullPath($Path)
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force -ErrorAction Stop
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Production CUDA Toolkit path cannot cross a link or reparse point: $current"
        }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            break
        }
        $current = $parent
    }
}

function Assert-AuthenticatedCudaSdkInventory(
    [string]$Root,
    [object[]]$Inventory,
    [string[]]$RequiredPaths
) {
    $authenticatedByPath = ConvertTo-AuthenticatedCudaInventory $Inventory $RequiredPaths
    $canonicalRoot = [System.IO.Path]::GetFullPath($Root).TrimEnd([char[]]@('\', '/'))
    if (-not (Test-Path -LiteralPath $canonicalRoot -PathType Container)) {
        throw "Production CUDA Toolkit root is missing: $canonicalRoot"
    }
    Assert-CudaInventoryNoReparseAncestors $canonicalRoot

    $observedPaths = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    $caseInsensitiveObservedPaths = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($entry in @(Get-ChildItem -LiteralPath $canonicalRoot -Recurse -Force)) {
        if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Production CUDA Toolkit inventory contains a link or reparse point: $($entry.FullName)"
        }
        if ($entry.PSIsContainer) {
            continue
        }

        $streams = @(Get-Item -LiteralPath $entry.FullName -Stream * -ErrorAction Stop)
        if ($streams.Count -ne 1 -or ([string]$streams[0].Stream) -cne ':$DATA') {
            throw "Production CUDA Toolkit file contains an alternate data stream: $($entry.FullName)"
        }
        $relative = [System.IO.Path]::GetRelativePath(
            $canonicalRoot,
            $entry.FullName
        ).Replace('\', '/')
        if (-not $observedPaths.Add($relative) -or
            -not $caseInsensitiveObservedPaths.Add($relative) -or
            -not $authenticatedByPath.ContainsKey($relative)) {
            throw "Production CUDA Toolkit contains an unexpected, duplicate, or case-colliding file: $relative"
        }

        $actualHash = (Get-FileHash -LiteralPath $entry.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        $expectedHash = $authenticatedByPath[$relative]
        if ($actualHash -cne $expectedHash) {
            throw "Authenticated production CUDA input $relative SHA-256 mismatch: expected $expectedHash, got $actualHash"
        }
    }

    if ($observedPaths.Count -ne $authenticatedByPath.Count) {
        $missing = @($authenticatedByPath.Keys | Where-Object { -not $observedPaths.Contains($_) })
        throw "Production CUDA Toolkit omitted authenticated inventory entries: $($missing -join ', ')"
    }
}

function Assert-CudaInventoryNoReparseAncestorsAllowMissing([string]$Path) {
    $current = [System.IO.Path]::GetFullPath($Path)
    while (-not (Test-Path -LiteralPath $current)) {
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            throw "Could not resolve an existing CUDA pack-input ancestor: $Path"
        }
        $current = $parent
    }
    Assert-CudaInventoryNoReparseAncestors $current
}

function Assert-CudaInventoryCanonicalPhysicalPath(
    [string]$Root,
    [string]$RelativePath
) {
    $current = [System.IO.Path]::GetFullPath($Root).TrimEnd([char[]]@('\', '/'))
    foreach ($segment in @($RelativePath -split '/')) {
        $matches = @(Get-ChildItem -LiteralPath $current -Force | Where-Object {
            [string]::Equals($_.Name, $segment, [System.StringComparison]::OrdinalIgnoreCase)
        })
        if ($matches.Count -ne 1 -or $matches[0].Name -cne $segment) {
            throw "Authenticated CUDA input has noncanonical physical case: $RelativePath"
        }
        $current = $matches[0].FullName
    }
}

function Get-ScribeCudaPackNoticeContract {
    $noticeDigest = 'e2c71babfd18a8e69542dd7e9ca018f9caa438094001a58e6bc4d8c999bf0d07'
    return @(
        [pscustomobject]@{
            SourceRelativePath = 'licenses/cuda_cudart/LICENSE'
            DestinationRelativePath = 'licenses/cuda-cudart/LICENSE'
            Sha256 = $noticeDigest
            SizeBytes = [int64]63021
        },
        [pscustomobject]@{
            SourceRelativePath = 'licenses/libcublas/LICENSE'
            DestinationRelativePath = 'licenses/cuda-cublas/LICENSE'
            Sha256 = $noticeDigest
            SizeBytes = [int64]63021
        }
    )
}

function Get-AuthenticatedCudaSdkFile(
    [string]$Root,
    [object[]]$Inventory,
    [string]$RequestedRelativePath,
    [string]$ExpectedSha256 = '',
    [int64]$ExpectedSizeBytes = -1
) {
    if ($RequestedRelativePath -cnotmatch '\A[A-Za-z0-9._+-]+(?:/[A-Za-z0-9._+-]+)*\z') {
        throw "Authenticated CUDA input path is noncanonical: $RequestedRelativePath"
    }
    if ($ExpectedSha256 -and $ExpectedSha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Authenticated CUDA input expected digest is noncanonical.'
    }
    if ($ExpectedSizeBytes -lt -1) {
        throw 'Authenticated CUDA input expected size is invalid.'
    }

    $null = ConvertTo-AuthenticatedCudaInventory $Inventory @($RequestedRelativePath)
    $matchingRecords = @($Inventory | Where-Object {
        [string]::Equals(
            ([string]$_.path).Replace('\', '/'),
            $RequestedRelativePath,
            [System.StringComparison]::OrdinalIgnoreCase
        )
    })
    if ($matchingRecords.Count -ne 1) {
        throw "Authenticated CUDA inventory did not resolve one exact input: $RequestedRelativePath"
    }
    $record = $matchingRecords[0]
    $canonicalRelativePath = ([string]$record.path).Replace('\', '/')
    $expectedDigest = [string]$record.sha256
    if ($ExpectedSha256 -and $expectedDigest -cne $ExpectedSha256) {
        throw "Authenticated CUDA input pin mismatch: $canonicalRelativePath"
    }

    $canonicalRoot = [System.IO.Path]::GetFullPath($Root).TrimEnd([char[]]@('\', '/'))
    if (-not (Test-Path -LiteralPath $canonicalRoot -PathType Container)) {
        throw "Production CUDA Toolkit root is missing: $canonicalRoot"
    }
    Assert-CudaInventoryNoReparseAncestors $canonicalRoot
    $sourcePath = Join-Path $canonicalRoot $canonicalRelativePath.Replace('/', '\')
    if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
        throw "Authenticated CUDA input is missing: $canonicalRelativePath"
    }
    Assert-CudaInventoryNoReparseAncestors $sourcePath
    Assert-CudaInventoryCanonicalPhysicalPath $canonicalRoot $canonicalRelativePath
    $sourceItem = Get-Item -LiteralPath $sourcePath -Force -ErrorAction Stop
    if ($sourceItem.PSIsContainer -or
        ($sourceItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Authenticated CUDA input must be a regular non-reparse file: $canonicalRelativePath"
    }
    $observedRelativePath = [System.IO.Path]::GetRelativePath(
        $canonicalRoot,
        $sourceItem.FullName
    ).Replace('\', '/')
    if ($observedRelativePath -cne $canonicalRelativePath) {
        throw "Authenticated CUDA input has noncanonical physical case: $canonicalRelativePath"
    }
    if ($ExpectedSizeBytes -ge 0 -and $sourceItem.Length -ne $ExpectedSizeBytes) {
        throw "Authenticated CUDA input $canonicalRelativePath size mismatch: expected $ExpectedSizeBytes, got $($sourceItem.Length)"
    }
    $streams = @(Get-Item -LiteralPath $sourceItem.FullName -Stream * -ErrorAction Stop)
    if ($streams.Count -ne 1 -or ([string]$streams[0].Stream) -cne ':$DATA') {
        throw "Authenticated CUDA input contains an alternate data stream: $canonicalRelativePath"
    }

    $input = [System.IO.File]::Open(
        $sourceItem.FullName,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
    try {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        try {
            $observedDigest = [System.Convert]::ToHexString(
                $sha.ComputeHash($input)
            ).ToLowerInvariant()
        }
        finally {
            $sha.Dispose()
        }
    }
    finally {
        $input.Dispose()
    }
    if ($observedDigest -cne $expectedDigest) {
        throw "Authenticated CUDA input $canonicalRelativePath SHA-256 mismatch: expected $expectedDigest, got $observedDigest"
    }

    return [pscustomobject]@{
        SourcePath = $sourceItem.FullName
        SourceRelativePath = $canonicalRelativePath
        Sha256 = $expectedDigest
    }
}

function Get-AuthenticatedCudaRuntimeSources(
    [string]$Root,
    [object[]]$Inventory,
    [string[]]$RuntimeImports
) {
    $sources = @{}
    foreach ($runtimeImport in @($RuntimeImports)) {
        if ($runtimeImport -cnotmatch '^[a-z0-9._-]+\.dll$' -or
            $sources.ContainsKey($runtimeImport)) {
            throw "CUDA packaged-runtime import is unsafe or duplicated: $runtimeImport"
        }
        $source = Get-AuthenticatedCudaSdkFile `
            $Root `
            $Inventory `
            "bin/$runtimeImport"
        $sources[$runtimeImport] = [pscustomobject]@{
            Path = $source.SourcePath
            Sha256 = $source.Sha256
        }
    }
    return $sources
}

function Get-AuthenticatedCudaPackNoticeSources(
    [string]$Root,
    [object[]]$Inventory
) {
    $contracts = @(Get-ScribeCudaPackNoticeContract)
    $requiredPaths = @($contracts | ForEach-Object { $_.SourceRelativePath })
    $null = ConvertTo-AuthenticatedCudaInventory $Inventory $requiredPaths
    $sources = [System.Collections.Generic.List[object]]::new()
    foreach ($contract in $contracts) {
        $sourceDirectoryRelative = Split-Path -Parent $contract.SourceRelativePath
        $sourceDirectoryPrefix = "$($sourceDirectoryRelative.Replace('\', '/'))/"
        $groupInventory = @($Inventory | Where-Object {
            ([string]$_.path).Replace('\', '/').StartsWith(
                $sourceDirectoryPrefix,
                [System.StringComparison]::OrdinalIgnoreCase
            )
        })
        if ($groupInventory.Count -ne 1 -or
            ([string]$groupInventory[0].path).Replace('\', '/') -cne $contract.SourceRelativePath) {
            throw "Authenticated CUDA notice inventory has unexpected inputs beneath $sourceDirectoryRelative."
        }

        $source = Get-AuthenticatedCudaSdkFile `
            $Root `
            $Inventory `
            $contract.SourceRelativePath `
            $contract.Sha256 `
            $contract.SizeBytes
        $sourceDirectory = Split-Path -Parent $source.SourcePath
        Assert-CudaInventoryNoReparseAncestors $sourceDirectory
        $sourceDirectoryEntries = @(Get-ChildItem -LiteralPath $sourceDirectory -Force)
        if ($sourceDirectoryEntries.Count -ne 1 -or
            $sourceDirectoryEntries[0].PSIsContainer -or
            ($sourceDirectoryEntries[0].Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
            $sourceDirectoryEntries[0].Name -cne 'LICENSE') {
            throw "Authenticated CUDA notice directory has unexpected inputs: $sourceDirectoryRelative"
        }
        $sources.Add([pscustomobject]@{
            SourcePath = $source.SourcePath
            SourceRelativePath = $source.SourceRelativePath
            DestinationRelativePath = $contract.DestinationRelativePath
            Sha256 = $source.Sha256
            SizeBytes = [int64]$contract.SizeBytes
        })
    }
    return $sources.ToArray()
}

function Copy-AuthenticatedCudaPackFile(
    [string]$Source,
    [string]$Destination,
    [string]$ExpectedSha256,
    [int64]$ExpectedSizeBytes = -1
) {
    if ($ExpectedSha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Authenticated CUDA pack-file digest is noncanonical.'
    }
    if ($ExpectedSizeBytes -lt -1) {
        throw 'Authenticated CUDA pack-file size is invalid.'
    }
    Assert-CudaInventoryNoReparseAncestors $Source
    $sourceItem = Get-Item -LiteralPath $Source -Force -ErrorAction Stop
    if ($sourceItem.PSIsContainer -or
        ($sourceItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'Authenticated CUDA pack-file source must be a regular non-reparse file.'
    }
    if ($ExpectedSizeBytes -ge 0 -and $sourceItem.Length -ne $ExpectedSizeBytes) {
        throw 'Authenticated CUDA pack-file source size mismatch.'
    }
    $sourceStreams = @(Get-Item -LiteralPath $sourceItem.FullName -Stream * -ErrorAction Stop)
    if ($sourceStreams.Count -ne 1 -or ([string]$sourceStreams[0].Stream) -cne ':$DATA') {
        throw 'Authenticated CUDA pack-file source contains an alternate data stream.'
    }
    if (Test-Path -LiteralPath $Destination) {
        throw 'Authenticated CUDA pack-file destination must be fresh.'
    }
    Assert-CudaInventoryNoReparseAncestorsAllowMissing $Destination

    # Copy-ScribePackRuntimeFile holds the source handle while hashing and
    # copying, uses create-new output semantics, and verifies the materialized
    # bytes. The CUDA wrapper additionally rejects ADS and all preexisting
    # destinations instead of accepting an identical retained destination.
    Copy-ScribePackRuntimeFile $sourceItem.FullName $Destination $ExpectedSha256
    Assert-CudaInventoryNoReparseAncestors $Destination
    $destinationItem = Get-Item -LiteralPath $Destination -Force -ErrorAction Stop
    $destinationStreams = @(Get-Item -LiteralPath $destinationItem.FullName -Stream * -ErrorAction Stop)
    if ($destinationItem.PSIsContainer -or
        ($destinationItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $destinationStreams.Count -ne 1 -or
        ([string]$destinationStreams[0].Stream) -cne ':$DATA' -or
        ($ExpectedSizeBytes -ge 0 -and $destinationItem.Length -ne $ExpectedSizeBytes) -or
        (Get-FileHash -LiteralPath $destinationItem.FullName -Algorithm SHA256).Hash.ToLowerInvariant() -cne $ExpectedSha256) {
        throw 'Materialized authenticated CUDA pack file failed validation.'
    }
}

function Copy-AuthenticatedCudaPackNotices(
    [object[]]$Sources,
    [string]$PackRoot
) {
    $contracts = @(Get-ScribeCudaPackNoticeContract)
    if (@($Sources).Count -ne $contracts.Count) {
        throw 'Authenticated CUDA notice source set is incomplete.'
    }
    $canonicalPackRoot = [System.IO.Path]::GetFullPath($PackRoot).TrimEnd([char[]]@('\', '/'))
    Assert-CudaInventoryNoReparseAncestors $canonicalPackRoot
    $packRootItem = Get-Item -LiteralPath $canonicalPackRoot -Force -ErrorAction Stop
    if (-not $packRootItem.PSIsContainer -or
        ($packRootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'CUDA pack root must be a physical directory.'
    }

    $copyPlan = [System.Collections.Generic.List[object]]::new()
    for ($index = 0; $index -lt $contracts.Count; $index++) {
        $contract = $contracts[$index]
        $source = @($Sources)[$index]
        $actualProperties = @($source.PSObject.Properties.Name | Sort-Object)
        $expectedProperties = @('SourcePath', 'SourceRelativePath', 'DestinationRelativePath', 'Sha256', 'SizeBytes' | Sort-Object)
        if ($actualProperties.Count -ne $expectedProperties.Count -or
            (Compare-Object -ReferenceObject $expectedProperties -DifferenceObject $actualProperties -CaseSensitive) -or
            $source.SourceRelativePath -cne $contract.SourceRelativePath -or
            $source.DestinationRelativePath -cne $contract.DestinationRelativePath -or
            $source.Sha256 -cne $contract.Sha256 -or
            [int64]$source.SizeBytes -ne [int64]$contract.SizeBytes) {
            throw 'Authenticated CUDA notice source set does not match the fixed pack policy.'
        }
        $destination = Join-Path $canonicalPackRoot $contract.DestinationRelativePath.Replace('/', '\')
        Assert-CudaInventoryNoReparseAncestorsAllowMissing $destination
        if (Test-Path -LiteralPath $destination) {
            throw 'Authenticated CUDA notice destination must be fresh.'
        }
        $destinationDirectory = Split-Path -Parent $destination
        Assert-CudaInventoryNoReparseAncestorsAllowMissing $destinationDirectory
        if (Test-Path -LiteralPath $destinationDirectory) {
            throw 'Authenticated CUDA notice destination directory must be fresh.'
        }
        $copyPlan.Add([pscustomobject]@{
            Source = [string]$source.SourcePath
            Destination = $destination
            DestinationDirectory = $destinationDirectory
            Sha256 = [string]$source.Sha256
            SizeBytes = [int64]$source.SizeBytes
        })
    }

    foreach ($copy in $copyPlan) {
        if (Test-Path -LiteralPath $copy.DestinationDirectory) {
            throw 'Authenticated CUDA notice destination directory must be fresh.'
        }
        [System.IO.Directory]::CreateDirectory($copy.DestinationDirectory) | Out-Null
        Copy-AuthenticatedCudaPackFile `
            $copy.Source `
            $copy.Destination `
            $copy.Sha256 `
            $copy.SizeBytes
    }
}
