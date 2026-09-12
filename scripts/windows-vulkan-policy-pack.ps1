Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-gpu-worker-cmake-bootstrap.ps1')

function Get-ScribeVulkanPolicyManifest([string]$RepositoryRoot, $Reference) {
    if ((@($Reference.PSObject.Properties.Name | Sort-Object) -join '|') -cne 'manifest_path|manifest_sha256' -or
        $Reference.manifest_path -cne 'native/vulkan-policy-loader/source-manifest.json' -or
        $Reference.manifest_sha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Invalid pinned Vulkan policy manifest reference.'
    }
    $path = Join-Path $RepositoryRoot $Reference.manifest_path
    Assert-ScribeGpuWorkerNoReparse $path
    $item = Get-Item -LiteralPath $path -Force
    if ($item.PSIsContainer -or $item.Length -gt 65536 -or
        (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -cne $Reference.manifest_sha256) {
        throw 'Pinned Vulkan policy manifest identity mismatch.'
    }
    $manifest = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    if ($manifest.policy_id -cne 'scribe-windows-vulkan-no-layers-v1' -or
        $manifest.target_triple -cne 'x86_64-pc-windows-msvc' -or
        $manifest.artifact.filename -cne 'vulkan-1.dll' -or
        $manifest.artifact.sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        $manifest.artifact.size_bytes -le 0 -or $manifest.artifact.size_bytes -gt 33554432) {
        throw 'Pinned Vulkan policy artifact contract mismatch.'
    }
    return $manifest
}

function Copy-ScribePackRuntimeFile([string]$Source, [string]$Destination, [string]$ExpectedSha256 = '') {
    Assert-ScribeGpuWorkerNoReparse $Source
    Assert-ScribeGpuWorkerNoReparse $Destination
    if ($ExpectedSha256 -and $ExpectedSha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Invalid pinned runtime digest.'
    }
    # This trusted-build copy never overwrites a preexisting destination. Hold
    # the source against replacement while authenticating and materializing it.
    $input = [IO.File]::Open($Source, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $sha = [Security.Cryptography.SHA256]::Create()
        try { $digest = [Convert]::ToHexString($sha.ComputeHash($input)).ToLowerInvariant() }
        finally { $sha.Dispose() }
        if ($ExpectedSha256 -and $digest -cne $ExpectedSha256) { throw 'Pinned runtime source digest mismatch.' }
        $input.Position = 0
        if (-not (Test-Path -LiteralPath $Destination)) {
            $output = [IO.File]::Open($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try { $input.CopyTo($output) } finally { $output.Dispose() }
        }
        Assert-ScribeGpuWorkerNoReparse $Destination
        $existing = Get-Item -LiteralPath $Destination -Force
        if ($existing.PSIsContainer -or $existing.Length -ne $input.Length -or
            (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant() -cne $digest) {
            throw 'Materialized runtime digest mismatch; existing output was not overwritten.'
        }
    } finally { $input.Dispose() }
}

function Copy-ScribeVulkanPolicyLicenses($BuiltLoader, [string]$PackRoot) {
    $groups = @(
        [pscustomobject]@{ Name='vulkan-loader'; Index=$BuiltLoader.LicensePath; IndexName='LICENSE.txt'; Directory=$BuiltLoader.LicenseDirectory; Files=@('Apache-2.0.txt','CC-BY-4.0.txt','HPND-Kevlin-Henney.txt','MIT-Khronos-old.txt','MIT.txt') },
        [pscustomobject]@{ Name='vulkan-headers'; Index=$BuiltLoader.HeaderLicensePath; IndexName='LICENSE.md'; Directory=$BuiltLoader.HeaderLicenseDirectory; Files=@('Apache-2.0.txt','MIT.txt') }
    )
    foreach ($group in $groups) {
        $source = (Get-ScribeGpuWorkerPhysicalDirectory $group.Directory 'Authenticated Vulkan license directory').FullName
        Assert-ScribeGpuWorkerNoReparseDescendants $source
        $entries = @(Get-ChildItem -LiteralPath $source -Force)
        if (@($entries | Where-Object PSIsContainer).Count -ne 0 -or
            (@($entries.Name | Sort-Object) -join '|') -cne (@($group.Files | Sort-Object) -join '|')) {
            throw 'Vulkan license inventory differs from the reviewed source distribution.'
        }
    }
    foreach ($group in $groups) {
        $destination = Join-Path $PackRoot "licenses/$($group.Name)"
        Assert-ScribeGpuWorkerNoReparse $destination
        if (Test-Path -LiteralPath $destination) { throw 'Vulkan license destination must be fresh.' }
        [IO.Directory]::CreateDirectory((Join-Path $destination 'LICENSES')) | Out-Null
        Copy-ScribePackRuntimeFile $group.Index (Join-Path $destination $group.IndexName)
        foreach ($name in $group.Files) {
            Copy-ScribePackRuntimeFile (Join-Path $group.Directory $name) (Join-Path $destination "LICENSES/$name")
        }
    }
}
