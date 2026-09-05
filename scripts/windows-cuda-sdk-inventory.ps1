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
        if ($relative -cnotmatch '^[A-Za-z0-9._-]+(?:/[A-Za-z0-9._-]+)*$' -or
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
