$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$script:WindowsGpuPackHistorySchemaVersion = [uint64]1
$script:WindowsGpuPackHistoryEpoch = [uint64]1
$script:WindowsGpuPackHistoryMaximumJsonBytes = 4MB
$script:WindowsGpuPackHistoryMaximumReleases = 128
$script:WindowsGpuPackHistoryMaximumPacksPerRelease = 8
$script:WindowsGpuPackHistoryMinimumCompleteFilesPerPack = 3
$script:WindowsGpuPackHistoryMaximumCompleteFilesPerPack = 258
$script:WindowsGpuPackHistoryMaximumUniqueFiles = 1024
$script:WindowsGpuPackHistoryMaximumAncestorDirectories = 900
$script:WindowsGpuPackHistoryMaximumPathLength = 512
$script:WindowsGpuPackHistoryMaximumPathDepth = 16
$script:WindowsGpuPackHistoryMaximumSegmentLength = 128
$script:WindowsGpuPackHistoryMaximumPayloadFileSize = [uint64](2GB)
$script:WindowsGpuPackHistoryMaximumPayloadAggregateSize = [uint64](4GB)
$script:WindowsGpuPackHistoryMaximumManifestSize = [uint64](256KB)
$script:WindowsGpuPackHistoryMaximumSignatureSize = [uint64](4KB)
$script:WindowsGpuPackHistoryMaximumCatalogSize = [uint64](512KB)
$script:WindowsGpuPackHistoryPackIds = @(
    'scribe-cuda-windows-x64',
    'scribe-vulkan-windows-x64'
)

function Assert-WindowsGpuPackHistoryExactProperties(
    [object]$Value,
    [string[]]$Names,
    [string]$Label
) {
    if ($null -eq $Value -or $Value -isnot [psobject]) {
        throw "$Label must be an object."
    }
    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $expected = @($Names | Sort-Object)
    if ($actual.Count -ne $expected.Count -or
        (Compare-Object -ReferenceObject $expected -DifferenceObject $actual -CaseSensitive)) {
        throw "$Label has unknown or missing fields."
    }
}

function Assert-WindowsGpuPackHistoryJsonObjectKeys(
    [System.Text.Json.JsonElement]$Element,
    [string]$Label
) {
    switch ($Element.ValueKind) {
        ([System.Text.Json.JsonValueKind]::Object) {
            $keys = [System.Collections.Generic.HashSet[string]]::new(
                [System.StringComparer]::OrdinalIgnoreCase
            )
            foreach ($property in $Element.EnumerateObject()) {
                if (-not $keys.Add($property.Name)) {
                    throw "$Label contains a duplicate or case-colliding JSON key: $($property.Name)"
                }
                Assert-WindowsGpuPackHistoryJsonObjectKeys `
                    -Element $property.Value `
                    -Label "$Label.$($property.Name)"
            }
        }
        ([System.Text.Json.JsonValueKind]::Array) {
            $index = 0
            foreach ($item in $Element.EnumerateArray()) {
                Assert-WindowsGpuPackHistoryJsonObjectKeys `
                    -Element $item `
                    -Label "$Label[$index]"
                $index++
            }
        }
    }
}

function Assert-WindowsGpuPackHistoryJsonKeys([string]$Json, [string]$Label) {
    if ([string]::IsNullOrWhiteSpace($Json)) {
        throw "$Label is empty."
    }
    $jsonByteCount = [System.Text.Encoding]::UTF8.GetByteCount($Json)
    if ($jsonByteCount -gt $script:WindowsGpuPackHistoryMaximumJsonBytes) {
        throw "$Label exceeds the bounded JSON size."
    }
    $options = [System.Text.Json.JsonDocumentOptions]::new()
    $options.AllowTrailingCommas = $false
    $options.CommentHandling = [System.Text.Json.JsonCommentHandling]::Disallow
    $options.MaxDepth = 20
    try {
        $document = [System.Text.Json.JsonDocument]::Parse($Json, $options)
    }
    catch {
        throw "$Label is malformed JSON: $($_.Exception.Message)"
    }
    try {
        Assert-WindowsGpuPackHistoryJsonObjectKeys -Element $document.RootElement -Label $Label
    }
    finally {
        $document.Dispose()
    }
}

function Assert-WindowsGpuPackHistoryArray([object]$Value, [string]$Label) {
    if ($Value -isnot [System.Array]) {
        throw "$Label must be an array."
    }
}

function ConvertTo-WindowsGpuPackHistoryUInt64(
    [object]$Value,
    [uint64]$Minimum,
    [uint64]$Maximum,
    [string]$Label
) {
    if ($null -eq $Value) {
        throw "$Label must be an unsigned integer."
    }
    $integerTypes = @(
        [byte], [sbyte], [int16], [uint16], [int32], [uint32],
        [int64], [uint64], [System.Numerics.BigInteger]
    )
    $isInteger = $false
    foreach ($integerType in $integerTypes) {
        if ($Value -is $integerType) {
            $isInteger = $true
            break
        }
    }
    if (-not $isInteger) {
        throw "$Label must be an unsigned integer."
    }
    $number = [System.Numerics.BigInteger]$Value
    if ($number -lt [System.Numerics.BigInteger]$Minimum -or
        $number -gt [System.Numerics.BigInteger]$Maximum) {
        throw "$Label is outside its bounded range."
    }
    return [uint64]$number
}

function Assert-WindowsGpuPackHistoryAuditToken([object]$Value, [string]$Label) {
    if ($Value -isnot [string] -or
        [string]$Value -cnotmatch '^[A-Za-z0-9][A-Za-z0-9._+-]{0,95}$') {
        throw "$Label is not a canonical audit token."
    }
    return [string]$Value
}

function Assert-WindowsGpuPackHistoryStoreComponent([object]$Value, [string]$Label) {
    if ($Value -isnot [string] -or
        [string]$Value -cnotmatch '^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$') {
        throw "$Label is not a canonical signed-pack store component."
    }
    return [string]$Value
}

function Assert-WindowsGpuPackHistoryDigest([object]$Value, [string]$Label) {
    if ($Value -isnot [string] -or [string]$Value -cnotmatch '^[0-9a-f]{64}$') {
        throw "$Label is not a canonical SHA-256 digest."
    }
    return [string]$Value
}

function Assert-WindowsGpuPackHistorySafePath([object]$Value, [string]$Label) {
    if ($Value -isnot [string]) {
        throw "$Label must be a string path."
    }
    $path = [string]$Value
    if ([string]::IsNullOrWhiteSpace($path) -or
        $path.Length -gt $script:WindowsGpuPackHistoryMaximumPathLength -or
        [System.IO.Path]::IsPathRooted($path) -or
        $path.Contains('\') -or
        $path.Contains(':') -or
        $path -cnotmatch '^[A-Za-z0-9._+/-]+$') {
        throw "$Label is not a canonical safe relative path."
    }
    $segments = @($path.Split('/'))
    if ($segments.Count -eq 0 -or
        $segments.Count -gt $script:WindowsGpuPackHistoryMaximumPathDepth) {
        throw "$Label has an invalid path depth."
    }
    foreach ($segment in $segments) {
        if ([string]::IsNullOrWhiteSpace($segment) -or
            $segment -in @('.', '..') -or
            $segment.Length -gt $script:WindowsGpuPackHistoryMaximumSegmentLength -or
            $segment.EndsWith('.') -or
            $segment.EndsWith(' ')) {
            throw "$Label contains an unsafe path segment."
        }
        $stem = $segment.Split('.')[0].ToUpperInvariant()
        if ($stem -in @('CON', 'PRN', 'AUX', 'NUL', 'CLOCK$') -or
            $stem -match '^(COM|LPT)[1-9]$') {
            throw "$Label uses a reserved Windows device name."
        }
    }
    return $path
}

function Test-WindowsGpuPackHistoryPathAncestor([string]$Ancestor, [string]$Path) {
    return $Path.StartsWith(
        $Ancestor + '/',
        [System.StringComparison]::OrdinalIgnoreCase
    )
}

function Assert-WindowsGpuPackHistoryStrictOrdinalOrder(
    [string[]]$Values,
    [string]$Label
) {
    for ($index = 1; $index -lt $Values.Count; $index++) {
        if ([System.StringComparer]::Ordinal.Compare($Values[$index - 1], $Values[$index]) -ge 0) {
            throw "$Label must be strictly ordinal-sorted."
        }
    }
}

function Get-WindowsGpuPackHistoryCanonicalValue([object]$Value) {
    return ($Value | ConvertTo-Json -Depth 16 -Compress)
}

function ConvertTo-WindowsGpuPackHistoryValidatedDocument(
    [object]$Value,
    [string]$Label
) {
    if ($null -eq $Value) {
        throw "$Label is missing."
    }
    try {
        $json = $Value | ConvertTo-Json -Depth 16 -Compress
    }
    catch {
        throw "$Label cannot be represented as a bounded history document: $($_.Exception.Message)"
    }
    return ConvertFrom-WindowsGpuPackHistoryJson -Json $json -SourceLabel $Label
}

function Update-WindowsGpuPackHistorySecurityEpochHighWater(
    [object[]]$Packs,
    [System.Collections.Generic.Dictionary[string, uint64]]$HighWater,
    [string]$Label
) {
    # Only normalized packs enter here. Every root in a release is a catalog
    # member, so a lower-epoch root cannot be treated as inactive rollback data.
    $releaseEpochs = [System.Collections.Generic.Dictionary[string, uint64]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($pack in $Packs) {
        $packId = [string]$pack.pack_id
        $epoch = [uint64]$pack.security_epoch
        if ($releaseEpochs.ContainsKey($packId) -and
            $releaseEpochs[$packId] -ne $epoch) {
            throw "$Label mixes security epochs for one pack ID: $packId"
        }
        $releaseEpochs[$packId] = $epoch
    }
    foreach ($packId in $releaseEpochs.Keys) {
        if ($HighWater.ContainsKey($packId) -and
            $releaseEpochs[$packId] -lt $HighWater[$packId]) {
            throw "$Label security epoch is below the historical high-water mark for pack ID: $packId"
        }
    }
    # Advance only after the entire row passes. Empty rows deliberately leave
    # all existing floors intact, and direct UInt64 comparisons avoid overflow.
    foreach ($packId in $releaseEpochs.Keys) {
        $HighWater[$packId] = $releaseEpochs[$packId]
    }
}

function ConvertFrom-WindowsGpuPackHistoryJson {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Json,
        [string]$SourceLabel = 'Windows GPU pack history'
    )

    Assert-WindowsGpuPackHistoryJsonKeys -Json $Json -Label $SourceLabel
    try {
        $source = ConvertFrom-Json -InputObject $Json -Depth 20
    }
    catch {
        throw "$SourceLabel could not be converted from JSON: $($_.Exception.Message)"
    }
    Assert-WindowsGpuPackHistoryExactProperties `
        -Value $source `
        -Names @('schema_version', 'history_epoch', 'releases') `
        -Label $SourceLabel
    $schemaVersion = ConvertTo-WindowsGpuPackHistoryUInt64 `
        -Value $source.schema_version -Minimum 1 -Maximum 1 `
        -Label "$SourceLabel.schema_version"
    $historyEpoch = ConvertTo-WindowsGpuPackHistoryUInt64 `
        -Value $source.history_epoch -Minimum 1 -Maximum 1 `
        -Label "$SourceLabel.history_epoch"
    Assert-WindowsGpuPackHistoryArray -Value $source.releases -Label "$SourceLabel.releases"
    $releaseValues = @($source.releases)
    if ($releaseValues.Count -gt $script:WindowsGpuPackHistoryMaximumReleases) {
        throw "$SourceLabel exhausted the finite release-history bound; a reviewed history-epoch and upgrade design is required."
    }

    $releaseIds = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $globalRoots = [System.Collections.Generic.Dictionary[string, object]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $globalFiles = [System.Collections.Generic.Dictionary[string, object]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $globalDirectories = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $catalogIdentities = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::Ordinal
    )
    $securityEpochHighWater = [System.Collections.Generic.Dictionary[string, uint64]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $releases = [System.Collections.Generic.List[object]]::new()

    for ($releaseIndex = 0; $releaseIndex -lt $releaseValues.Count; $releaseIndex++) {
        $releaseLabel = "$SourceLabel.releases[$releaseIndex]"
        $release = $releaseValues[$releaseIndex]
        Assert-WindowsGpuPackHistoryExactProperties `
            -Value $release `
            -Names @(
                'release_id', 'source_revision', 'catalog_size_bytes',
                'catalog_sha256', 'catalog_pack_roots', 'packs'
            ) `
            -Label $releaseLabel
        $releaseId = Assert-WindowsGpuPackHistoryAuditToken `
            -Value $release.release_id -Label "$releaseLabel.release_id"
        if (-not $releaseIds.Add($releaseId)) {
            throw "$SourceLabel contains duplicate or case-colliding release IDs."
        }
        if ($release.source_revision -isnot [string] -or
            [string]$release.source_revision -cnotmatch '^[0-9a-f]{40}$') {
            throw "$releaseLabel.source_revision must be a canonical 40-hex Git revision."
        }
        $sourceRevision = [string]$release.source_revision
        $catalogSize = ConvertTo-WindowsGpuPackHistoryUInt64 `
            -Value $release.catalog_size_bytes -Minimum 1 `
            -Maximum $script:WindowsGpuPackHistoryMaximumCatalogSize `
            -Label "$releaseLabel.catalog_size_bytes"
        $catalogSha256 = Assert-WindowsGpuPackHistoryDigest `
            -Value $release.catalog_sha256 -Label "$releaseLabel.catalog_sha256"
        Assert-WindowsGpuPackHistoryArray `
            -Value $release.catalog_pack_roots -Label "$releaseLabel.catalog_pack_roots"
        Assert-WindowsGpuPackHistoryArray -Value $release.packs -Label "$releaseLabel.packs"
        $catalogRootValues = @($release.catalog_pack_roots)
        $packValues = @($release.packs)
        if ($catalogRootValues.Count -gt $script:WindowsGpuPackHistoryMaximumPacksPerRelease -or
            $packValues.Count -gt $script:WindowsGpuPackHistoryMaximumPacksPerRelease) {
            throw "$releaseLabel exceeds the eight-pack release bound."
        }

        $catalogRootSet = [System.Collections.Generic.HashSet[string]]::new(
            [System.StringComparer]::OrdinalIgnoreCase
        )
        $catalogRoots = [System.Collections.Generic.List[string]]::new()
        foreach ($catalogRootValue in $catalogRootValues) {
            $catalogRoot = Assert-WindowsGpuPackHistorySafePath `
                -Value $catalogRootValue -Label "$releaseLabel.catalog_pack_roots"
            if (-not $catalogRootSet.Add($catalogRoot)) {
                throw "$releaseLabel contains duplicate or case-colliding catalog pack roots."
            }
            $catalogRoots.Add($catalogRoot)
        }
        Assert-WindowsGpuPackHistoryStrictOrdinalOrder `
            -Values $catalogRoots.ToArray() `
            -Label "$releaseLabel.catalog_pack_roots"

        $releaseRootSet = [System.Collections.Generic.HashSet[string]]::new(
            [System.StringComparer]::OrdinalIgnoreCase
        )
        $releaseFileSet = [System.Collections.Generic.HashSet[string]]::new(
            [System.StringComparer]::OrdinalIgnoreCase
        )
        $packs = [System.Collections.Generic.List[object]]::new()
        $previousPackRoot = $null
        for ($packIndex = 0; $packIndex -lt $packValues.Count; $packIndex++) {
            $packLabel = "$releaseLabel.packs[$packIndex]"
            $pack = $packValues[$packIndex]
            Assert-WindowsGpuPackHistoryExactProperties `
                -Value $pack `
                -Names @(
                    'pack_id', 'pack_version', 'pack_digest',
                    'security_epoch', 'root', 'files'
                ) `
                -Label $packLabel
            if ($pack.pack_id -isnot [string] -or
                [string]$pack.pack_id -cnotin $script:WindowsGpuPackHistoryPackIds) {
                throw "$packLabel.pack_id is not an approved Windows GPU pack identity."
            }
            $packId = [string]$pack.pack_id
            $packVersion = Assert-WindowsGpuPackHistoryStoreComponent `
                -Value $pack.pack_version -Label "$packLabel.pack_version"
            $packDigest = Assert-WindowsGpuPackHistoryDigest `
                -Value $pack.pack_digest -Label "$packLabel.pack_digest"
            $securityEpoch = ConvertTo-WindowsGpuPackHistoryUInt64 `
                -Value $pack.security_epoch -Minimum 1 -Maximum ([uint64]::MaxValue) `
                -Label "$packLabel.security_epoch"
            $packRoot = Assert-WindowsGpuPackHistorySafePath `
                -Value $pack.root -Label "$packLabel.root"
            $expectedRoot = "workers/packs/$packId/$packVersion/$packDigest"
            if ($packRoot -cne $expectedRoot) {
                throw "$packLabel.root is outside the canonical immutable Windows GPU pack layout."
            }
            if ($null -ne $previousPackRoot -and
                [System.StringComparer]::Ordinal.Compare($previousPackRoot, $packRoot) -ge 0) {
                throw "$releaseLabel.packs must be strictly root-sorted."
            }
            $previousPackRoot = $packRoot
            if (-not $releaseRootSet.Add($packRoot)) {
                throw "$releaseLabel contains duplicate or case-colliding pack roots."
            }
            foreach ($existingRoot in $releaseRootSet) {
                if ($existingRoot -cne $packRoot -and
                    ((Test-WindowsGpuPackHistoryPathAncestor $existingRoot $packRoot) -or
                     (Test-WindowsGpuPackHistoryPathAncestor $packRoot $existingRoot))) {
                    throw "$releaseLabel contains ambiguously overlapping pack roots."
                }
            }
            Assert-WindowsGpuPackHistoryArray -Value $pack.files -Label "$packLabel.files"
            $fileValues = @($pack.files)
            if ($fileValues.Count -lt $script:WindowsGpuPackHistoryMinimumCompleteFilesPerPack -or
                $fileValues.Count -gt $script:WindowsGpuPackHistoryMaximumCompleteFilesPerPack) {
                throw "$packLabel.files is outside its bounded inventory size."
            }
            $files = [System.Collections.Generic.List[object]]::new()
            $packFilePaths = [System.Collections.Generic.List[string]]::new()
            $packFileSet = [System.Collections.Generic.HashSet[string]]::new(
                [System.StringComparer]::OrdinalIgnoreCase
            )
            $packDirectories = [System.Collections.Generic.Dictionary[string, string]]::new(
                [System.StringComparer]::OrdinalIgnoreCase
            )
            $manifestCount = 0
            $signatureCount = 0
            $payloadAggregateSize = [uint64]0
            foreach ($fileValue in $fileValues) {
                $fileLabel = "$packLabel.files[$($files.Count)]"
                Assert-WindowsGpuPackHistoryExactProperties `
                    -Value $fileValue `
                    -Names @('path', 'size_bytes', 'sha256') `
                    -Label $fileLabel
                $filePath = Assert-WindowsGpuPackHistorySafePath `
                    -Value $fileValue.path -Label "$fileLabel.path"
                if (-not $filePath.StartsWith($packRoot + '/', [System.StringComparison]::Ordinal)) {
                    throw "$fileLabel.path escapes its canonical immutable pack root."
                }
                if (-not $packFileSet.Add($filePath) -or -not $releaseFileSet.Add($filePath)) {
                    throw "$releaseLabel contains duplicate or case-colliding file paths."
                }
                if ($packDirectories.ContainsKey($filePath)) {
                    throw "$packLabel.files contains an ambiguous file/descendant overlap."
                }
                $fileSegments = @($filePath.Split('/'))
                for ($directoryIndex = 1; $directoryIndex -lt $fileSegments.Count; $directoryIndex++) {
                    $directory = $fileSegments[0..($directoryIndex - 1)] -join '/'
                    if ($packFileSet.Contains($directory)) {
                        throw "$packLabel.files contains an ambiguous file/descendant overlap."
                    }
                    if ($packDirectories.ContainsKey($directory)) {
                        if ($packDirectories[$directory] -cne $directory) {
                            throw "$packLabel.files contains a case-colliding directory identity."
                        }
                    }
                    else {
                        $packDirectories.Add($directory, $directory)
                    }
                }
                $isManifest = $filePath -ceq "$packRoot/pack-manifest.json"
                $isSignature = $filePath -ceq "$packRoot/pack-manifest.sig"
                if (-not $isManifest -and -not $isSignature) {
                    $relativeFilePath = $filePath.Substring($packRoot.Length + 1)
                    foreach ($relativeSegment in $relativeFilePath.Split('/')) {
                        if ($relativeSegment -ieq 'pack-manifest.json' -or
                            $relativeSegment -ieq 'pack-manifest.sig') {
                            throw "$fileLabel.path ambiguously reuses a reserved pack control-envelope name."
                        }
                    }
                }
                $minimumFileSize = if ($isManifest -or $isSignature) { [uint64]1 } else { [uint64]0 }
                $maximumFileSize = if ($isManifest) {
                    $script:WindowsGpuPackHistoryMaximumManifestSize
                }
                elseif ($isSignature) {
                    $script:WindowsGpuPackHistoryMaximumSignatureSize
                }
                else {
                    $script:WindowsGpuPackHistoryMaximumPayloadFileSize
                }
                $fileSize = ConvertTo-WindowsGpuPackHistoryUInt64 `
                    -Value $fileValue.size_bytes -Minimum $minimumFileSize `
                    -Maximum $maximumFileSize -Label "$fileLabel.size_bytes"
                if (-not $isManifest -and -not $isSignature) {
                    if ($payloadAggregateSize -gt
                        ($script:WindowsGpuPackHistoryMaximumPayloadAggregateSize - $fileSize)) {
                        throw "$packLabel.files exceeds the signed-pack aggregate payload bound."
                    }
                    $payloadAggregateSize += $fileSize
                }
                $fileSha256 = Assert-WindowsGpuPackHistoryDigest `
                    -Value $fileValue.sha256 -Label "$fileLabel.sha256"
                if ($isManifest) {
                    $manifestCount++
                }
                if ($isSignature) {
                    $signatureCount++
                }
                $normalizedFile = [pscustomobject][ordered]@{
                    path = $filePath
                    size_bytes = $fileSize
                    sha256 = $fileSha256
                }
                $files.Add($normalizedFile)
                $packFilePaths.Add($filePath)
            }
            Assert-WindowsGpuPackHistoryStrictOrdinalOrder `
                -Values $packFilePaths.ToArray() -Label "$packLabel.files"
            if ($manifestCount -ne 1 -or $signatureCount -ne 1) {
                throw "$packLabel.files must contain exact pack-manifest.json and pack-manifest.sig control files."
            }
            $normalizedPack = [pscustomobject][ordered]@{
                pack_id = $packId
                pack_version = $packVersion
                pack_digest = $packDigest
                security_epoch = $securityEpoch
                root = $packRoot
                files = [object[]]$files.ToArray()
            }
            if ($globalRoots.ContainsKey($packRoot)) {
                $existingPack = $globalRoots[$packRoot]
                if ($existingPack.root -cne $packRoot -or
                    (Get-WindowsGpuPackHistoryCanonicalValue $existingPack) -cne
                    (Get-WindowsGpuPackHistoryCanonicalValue $normalizedPack)) {
                    throw "$SourceLabel changes or case-collides an immutable historical pack root."
                }
            }
            else {
                foreach ($existingRoot in $globalRoots.Keys) {
                    if ((Test-WindowsGpuPackHistoryPathAncestor $existingRoot $packRoot) -or
                        (Test-WindowsGpuPackHistoryPathAncestor $packRoot $existingRoot)) {
                        throw "$SourceLabel contains ambiguously overlapping pack roots."
                    }
                }
                $globalRoots.Add($packRoot, $normalizedPack)
            }
            foreach ($normalizedFile in $files) {
                if ($globalFiles.ContainsKey($normalizedFile.path)) {
                    $existingFile = $globalFiles[$normalizedFile.path]
                    if ($existingFile.path -cne $normalizedFile.path -or
                        $existingFile.size_bytes -ne $normalizedFile.size_bytes -or
                        $existingFile.sha256 -cne $normalizedFile.sha256) {
                        throw "$SourceLabel changes or case-collides an immutable historical file identity."
                    }
                }
                else {
                    if ($globalDirectories.ContainsKey($normalizedFile.path)) {
                        throw "$SourceLabel contains an ambiguous historical file/descendant overlap."
                    }
                    $fileSegments = @($normalizedFile.path.Split('/'))
                    for ($directoryIndex = 1; $directoryIndex -lt $fileSegments.Count; $directoryIndex++) {
                        $directory = $fileSegments[0..($directoryIndex - 1)] -join '/'
                        if ($globalFiles.ContainsKey($directory)) {
                            throw "$SourceLabel contains an ambiguous historical file/descendant overlap."
                        }
                        if ($globalDirectories.ContainsKey($directory)) {
                            if ($globalDirectories[$directory] -cne $directory) {
                                throw "$SourceLabel contains a case-colliding historical directory identity."
                            }
                        }
                        else {
                            $globalDirectories.Add($directory, $directory)
                        }
                    }
                    $globalFiles.Add($normalizedFile.path, $normalizedFile)
                }
            }
            $packs.Add($normalizedPack)
        }
        $packRoots = @($packs | ForEach-Object { $_.root })
        if ($catalogRoots.Count -ne $packRoots.Count) {
            throw "$releaseLabel catalog pack roots do not match its exact pack set."
        }
        for ($rootIndex = 0; $rootIndex -lt $packRoots.Count; $rootIndex++) {
            if ($catalogRoots[$rootIndex] -cne $packRoots[$rootIndex]) {
                throw "$releaseLabel catalog pack roots do not match its exact pack set."
            }
        }
        $catalogIdentity = "$catalogSize`:$catalogSha256"
        $catalogRootMaterial = $catalogRoots -join "`n"
        if ($catalogIdentities.ContainsKey($catalogIdentity) -and
            $catalogIdentities[$catalogIdentity] -cne $catalogRootMaterial) {
            throw "$SourceLabel assigns one catalog byte identity to different pack-root sets."
        }
        $catalogIdentities[$catalogIdentity] = $catalogRootMaterial
        Update-WindowsGpuPackHistorySecurityEpochHighWater `
            -Packs $packs.ToArray() -HighWater $securityEpochHighWater -Label $releaseLabel
        $releases.Add([pscustomobject][ordered]@{
            release_id = $releaseId
            source_revision = $sourceRevision
            catalog_size_bytes = $catalogSize
            catalog_sha256 = $catalogSha256
            catalog_pack_roots = [string[]]$catalogRoots.ToArray()
            packs = [object[]]$packs.ToArray()
        })
    }

    return [pscustomobject][ordered]@{
        schema_version = $schemaVersion
        history_epoch = $historyEpoch
        releases = [object[]]$releases.ToArray()
    }
}

function Assert-WindowsGpuPackHistoryInputItem([string]$Path, [bool]$RequireFile) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    $linkProperty = $item.PSObject.Properties['LinkType']
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        ($null -ne $linkProperty -and -not [string]::IsNullOrEmpty([string]$linkProperty.Value))) {
        throw "Windows GPU pack history input contains a link or reparse point: $Path"
    }
    if ($RequireFile -and $item.PSIsContainer) {
        throw "Windows GPU pack history input is not a regular file: $Path"
    }
    if (-not $RequireFile -and -not $item.PSIsContainer) {
        throw "Windows GPU pack history input ancestor is not a directory: $Path"
    }
    return $item
}

function Assert-WindowsGpuPackHistoryInputPath([string]$Path) {
    $root = [System.IO.Path]::GetPathRoot($Path)
    if ([string]::IsNullOrEmpty($root) -or
        $Path.StartsWith('\\', [System.StringComparison]::Ordinal) -or
        $Path.StartsWith('\\?\', [System.StringComparison]::Ordinal) -or
        $Path.StartsWith('\\.\', [System.StringComparison]::Ordinal)) {
        throw 'Windows GPU pack history input must be a local filesystem path.'
    }
    $relative = $Path.Substring($root.Length)
    $segments = @($relative.Split([char[]]@('\', '/'), [System.StringSplitOptions]::RemoveEmptyEntries))
    $current = $root
    for ($index = 0; $index -lt $segments.Count; $index++) {
        $current = Join-Path $current $segments[$index]
        $null = Assert-WindowsGpuPackHistoryInputItem `
            -Path $current `
            -RequireFile ($index -eq $segments.Count - 1)
    }
}

function Read-WindowsGpuPackHistory {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$LiteralPath
    )

    $fullPath = [System.IO.Path]::GetFullPath($LiteralPath)
    Assert-WindowsGpuPackHistoryInputPath -Path $fullPath
    $stream = [System.IO.FileStream]::new(
        $fullPath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
    try {
        if ($stream.Length -le 0 -or
            $stream.Length -gt $script:WindowsGpuPackHistoryMaximumJsonBytes) {
            throw 'Windows GPU pack history input is empty or exceeds the bounded JSON size.'
        }
        $bytes = [byte[]]::new([int]$stream.Length)
        $offset = 0
        while ($offset -lt $bytes.Length) {
            $read = $stream.Read($bytes, $offset, $bytes.Length - $offset)
            if ($read -le 0) {
                throw 'Windows GPU pack history input ended before its declared length.'
            }
            $offset += $read
        }
        if ($stream.ReadByte() -ne -1) {
            throw 'Windows GPU pack history input changed or exceeded its bounded size while reading.'
        }
    }
    finally {
        $stream.Dispose()
    }
    Assert-WindowsGpuPackHistoryInputPath -Path $fullPath
    if ($bytes.Length -ge 3 -and
        $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
        throw 'Windows GPU pack history input must be canonical UTF-8 without a byte-order mark.'
    }
    try {
        $utf8 = [System.Text.UTF8Encoding]::new($false, $true)
        $json = $utf8.GetString($bytes)
    }
    catch {
        throw "Windows GPU pack history input is not valid UTF-8: $($_.Exception.Message)"
    }
    return ConvertFrom-WindowsGpuPackHistoryJson `
        -Json $json -SourceLabel "Windows GPU pack history '$fullPath'"
}

function Assert-WindowsGpuPackHistoryAppendOnly {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [object]$PreviousDocument,
        [Parameter(Mandatory = $true)]
        [object]$NewDocument
    )

    $previous = ConvertTo-WindowsGpuPackHistoryValidatedDocument `
        -Value $PreviousDocument -Label 'Previous Windows GPU pack history'
    $new = ConvertTo-WindowsGpuPackHistoryValidatedDocument `
        -Value $NewDocument -Label 'New Windows GPU pack history'
    if ($previous.schema_version -ne $new.schema_version -or
        $previous.history_epoch -ne $new.history_epoch) {
        throw 'Windows GPU pack history schema or epoch changed; a reviewed history-epoch and upgrade design is required.'
    }
    if ($new.releases.Count -lt $previous.releases.Count) {
        throw 'Windows GPU pack history removed a previously published release row.'
    }
    for ($index = 0; $index -lt $previous.releases.Count; $index++) {
        if ((Get-WindowsGpuPackHistoryCanonicalValue $previous.releases[$index]) -cne
            (Get-WindowsGpuPackHistoryCanonicalValue $new.releases[$index])) {
            throw "Windows GPU pack history modified or reordered immutable release row $index."
        }
    }
    return $true
}

function Add-WindowsGpuPackHistoryAncestorDirectories(
    [string]$FilePath,
    [System.Collections.Generic.Dictionary[string, string]]$Directories
) {
    $segments = @($FilePath.Split('/'))
    for ($index = 1; $index -lt $segments.Count; $index++) {
        $directory = $segments[0..($index - 1)] -join '/'
        if (-not $Directories.ContainsKey($directory)) {
            $Directories.Add($directory, $directory)
        }
    }
}

function Add-WindowsGpuPackHistoryRetirementDirectories(
    [object]$Pack,
    [System.Collections.Generic.Dictionary[string, string]]$Directories
) {
    foreach ($file in $Pack.files) {
        Add-WindowsGpuPackHistoryAncestorDirectories `
            -FilePath $file.path -Directories $Directories
    }
}

function Get-WindowsGpuPackHistoryOrdinalStrings([System.Collections.IEnumerable]$Values) {
    $result = [System.Collections.Generic.List[string]]::new()
    foreach ($value in $Values) {
        $result.Add([string]$value)
    }
    $result.Sort([System.StringComparer]::Ordinal)
    return ,([string[]]$result.ToArray())
}

function Get-WindowsGpuPackHistoryRetirementDirectoryOrder(
    [System.Collections.IEnumerable]$Values
) {
    $result = [System.Collections.Generic.List[string]]::new()
    foreach ($value in $Values) {
        $result.Add([string]$value)
    }
    $comparison = [System.Comparison[string]]{
        param([string]$left, [string]$right)
        $leftDepth = @($left.Split('/')).Count
        $rightDepth = @($right.Split('/')).Count
        if ($leftDepth -ne $rightDepth) {
            return $rightDepth.CompareTo($leftDepth)
        }
        return [System.StringComparer]::Ordinal.Compare($left, $right)
    }
    $result.Sort($comparison)
    return ,([string[]]$result.ToArray())
}

function Get-WindowsGpuPackRetirementPlan {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [object]$HistoryDocument,
        [Parameter(Mandatory = $true)]
        [object]$CurrentDocument
    )

    $history = ConvertTo-WindowsGpuPackHistoryValidatedDocument `
        -Value $HistoryDocument -Label 'Historical Windows GPU pack document'
    $current = ConvertTo-WindowsGpuPackHistoryValidatedDocument `
        -Value $CurrentDocument -Label 'Current Windows GPU pack document'
    if ($current.releases.Count -ne 1) {
        throw 'Current Windows GPU pack document must contain exactly one release row.'
    }
    $currentRelease = $current.releases[0]

    foreach ($historicalRelease in $history.releases) {
        if ([string]::Equals(
            $historicalRelease.release_id,
            $currentRelease.release_id,
            [System.StringComparison]::OrdinalIgnoreCase
        ) -and
            (Get-WindowsGpuPackHistoryCanonicalValue $historicalRelease) -cne
            (Get-WindowsGpuPackHistoryCanonicalValue $currentRelease)) {
            throw 'Current release ID case-collides with or changes an immutable historical release row.'
        }
    }
    $historicalCatalogIdentities = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($historicalRelease in $history.releases) {
        $identity = "$($historicalRelease.catalog_size_bytes):$($historicalRelease.catalog_sha256)"
        $historicalCatalogIdentities[$identity] = $historicalRelease.catalog_pack_roots -join "`n"
    }
    $currentCatalogIdentity = "$($currentRelease.catalog_size_bytes):$($currentRelease.catalog_sha256)"
    if ($historicalCatalogIdentities.ContainsKey($currentCatalogIdentity) -and
        $historicalCatalogIdentities[$currentCatalogIdentity] -cne
        ($currentRelease.catalog_pack_roots -join "`n")) {
        throw 'Current document assigns a historical catalog byte identity to a different pack-root set.'
    }

    $historicalRoots = [System.Collections.Generic.Dictionary[string, object]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $currentRoots = [System.Collections.Generic.Dictionary[string, object]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $securityEpochHighWater = [System.Collections.Generic.Dictionary[string, uint64]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($release in $history.releases) {
        Update-WindowsGpuPackHistorySecurityEpochHighWater `
            -Packs $release.packs -HighWater $securityEpochHighWater `
            -Label 'Historical Windows GPU pack release'
        foreach ($pack in $release.packs) {
            if (-not $historicalRoots.ContainsKey($pack.root)) {
                $historicalRoots.Add($pack.root, $pack)
            }
        }
    }
    foreach ($pack in $currentRelease.packs) {
        if ($historicalRoots.ContainsKey($pack.root)) {
            $historicalPack = $historicalRoots[$pack.root]
            if ($historicalPack.root -cne $pack.root -or
                (Get-WindowsGpuPackHistoryCanonicalValue $historicalPack) -cne
                (Get-WindowsGpuPackHistoryCanonicalValue $pack)) {
                throw 'Current document reuses a historical pack root with a different exact inventory.'
            }
        }
        $currentRoots.Add($pack.root, $pack)
    }
    Update-WindowsGpuPackHistorySecurityEpochHighWater `
        -Packs $currentRelease.packs -HighWater $securityEpochHighWater `
        -Label 'Current Windows GPU pack release'

    $allFiles = [System.Collections.Generic.Dictionary[string, object]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $allDirectories = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($pack in @($historicalRoots.Values) + @($currentRoots.Values)) {
        foreach ($file in $pack.files) {
            if ($allFiles.ContainsKey($file.path)) {
                $existing = $allFiles[$file.path]
                if ($existing.path -cne $file.path -or
                    $existing.size_bytes -ne $file.size_bytes -or
                    $existing.sha256 -cne $file.sha256) {
                    throw 'Current and historical documents contain an ambiguous file identity.'
                }
            }
            else {
                if ($allDirectories.ContainsKey($file.path)) {
                    throw 'Current and historical documents contain an ambiguous file/descendant overlap.'
                }
                $fileSegments = @($file.path.Split('/'))
                for ($directoryIndex = 1; $directoryIndex -lt $fileSegments.Count; $directoryIndex++) {
                    $directory = $fileSegments[0..($directoryIndex - 1)] -join '/'
                    if ($allFiles.ContainsKey($directory)) {
                        throw 'Current and historical documents contain an ambiguous file/descendant overlap.'
                    }
                    if ($allDirectories.ContainsKey($directory)) {
                        if ($allDirectories[$directory] -cne $directory) {
                            throw 'Current and historical documents contain a case-colliding directory identity.'
                        }
                    }
                    else {
                        $allDirectories.Add($directory, $directory)
                    }
                }
                $allFiles.Add($file.path, $file)
            }
        }
    }
    if ($allFiles.Count -gt $script:WindowsGpuPackHistoryMaximumUniqueFiles -or
        $allDirectories.Count -gt $script:WindowsGpuPackHistoryMaximumAncestorDirectories) {
        throw 'Combined current and historical packs exhausted the installer handle bound; a reviewed history-epoch and upgrade design is required.'
    }

    $currentFiles = [System.Collections.Generic.Dictionary[string, object]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $currentDirectories = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($pack in $currentRoots.Values) {
        foreach ($file in $pack.files) {
            $currentFiles[$file.path] = [pscustomobject][ordered]@{
                Path = $file.path
                SizeBytes = $file.size_bytes
                Sha256 = $file.sha256
            }
            Add-WindowsGpuPackHistoryAncestorDirectories `
                -FilePath $file.path -Directories $currentDirectories
        }
    }

    $retiredFiles = [System.Collections.Generic.Dictionary[string, object]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $retiredRootSet = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $retirementDirectoryCandidates = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($historicalPack in $historicalRoots.Values) {
        if ($currentRoots.ContainsKey($historicalPack.root)) {
            continue
        }
        $retiredRootSet[$historicalPack.root] = $historicalPack.root
        foreach ($file in $historicalPack.files) {
            $retiredFiles[$file.path] = [pscustomobject][ordered]@{
                Path = $file.path
                SizeBytes = $file.size_bytes
                Sha256 = $file.sha256
                PackRoot = $historicalPack.root
            }
        }
        Add-WindowsGpuPackHistoryRetirementDirectories `
            -Pack $historicalPack -Directories $retirementDirectoryCandidates
    }
    $exclusiveRetirementDirectories = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($candidate in $retirementDirectoryCandidates.Values) {
        $sharedWithCurrent = $currentDirectories.ContainsKey($candidate)
        if (-not $sharedWithCurrent) {
            foreach ($currentRoot in $currentRoots.Keys) {
                if ((Test-WindowsGpuPackHistoryPathAncestor $candidate $currentRoot) -or
                    [string]::Equals($candidate, $currentRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
                    $sharedWithCurrent = $true
                    break
                }
            }
        }
        if (-not $sharedWithCurrent) {
            $insideRetiredRoot = $candidate -cin @('workers', 'workers/packs')
            foreach ($retiredRoot in $retiredRootSet.Keys) {
                if ([string]::Equals($candidate, $retiredRoot, [System.StringComparison]::Ordinal) -or
                    $retiredRoot.StartsWith($candidate + '/', [System.StringComparison]::Ordinal) -or
                    $candidate.StartsWith($retiredRoot + '/', [System.StringComparison]::Ordinal)) {
                    $insideRetiredRoot = $true
                    break
                }
            }
            if (-not $insideRetiredRoot) {
                throw 'Retirement planning produced a directory outside a canonical retired pack chain.'
            }
            $exclusiveRetirementDirectories[$candidate] = $candidate
        }
    }

    $orderedCurrentFilePaths = Get-WindowsGpuPackHistoryOrdinalStrings $currentFiles.Keys
    $orderedCurrentFiles = [System.Collections.Generic.List[object]]::new()
    foreach ($path in $orderedCurrentFilePaths) {
        $orderedCurrentFiles.Add($currentFiles[$path])
    }
    $orderedRetiredFilePaths = Get-WindowsGpuPackHistoryOrdinalStrings $retiredFiles.Keys
    $orderedRetiredFiles = [System.Collections.Generic.List[object]]::new()
    foreach ($path in $orderedRetiredFilePaths) {
        $orderedRetiredFiles.Add($retiredFiles[$path])
    }
    $historicalCatalogs = [System.Collections.Generic.List[object]]::new()
    foreach ($release in $history.releases) {
        $historicalCatalogs.Add([pscustomobject][ordered]@{
            ReleaseId = $release.release_id
            SourceRevision = $release.source_revision
            SizeBytes = $release.catalog_size_bytes
            Sha256 = $release.catalog_sha256
            PackRoots = [string[]]$release.catalog_pack_roots.Clone()
        })
    }
    return [pscustomobject][ordered]@{
        HistoryEpoch = $history.history_epoch
        CurrentReleaseId = $currentRelease.release_id
        CurrentCatalog = [pscustomobject][ordered]@{
            ReleaseId = $currentRelease.release_id
            SourceRevision = $currentRelease.source_revision
            SizeBytes = $currentRelease.catalog_size_bytes
            Sha256 = $currentRelease.catalog_sha256
            PackRoots = [string[]]$currentRelease.catalog_pack_roots.Clone()
        }
        HistoricalCatalogs = [object[]]$historicalCatalogs.ToArray()
        CurrentPackRoots = [string[]](Get-WindowsGpuPackHistoryOrdinalStrings $currentRoots.Keys)
        CurrentFiles = [object[]]$orderedCurrentFiles.ToArray()
        CurrentDirectories = [string[]](Get-WindowsGpuPackHistoryOrdinalStrings $currentDirectories.Keys)
        RetiredPackRoots = [string[]](Get-WindowsGpuPackHistoryOrdinalStrings $retiredRootSet.Keys)
        RetiredFiles = [object[]]$orderedRetiredFiles.ToArray()
        RetiredDirectories = [string[]](
            Get-WindowsGpuPackHistoryRetirementDirectoryOrder $exclusiveRetirementDirectories.Values
        )
        UniquePackFileCount = $allFiles.Count
        UniqueAncestorDirectoryCount = $allDirectories.Count
    }
}
