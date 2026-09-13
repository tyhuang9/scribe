$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'windows-gpu-pack-history.ps1')

$script:WindowsGpuPackHistoryTestCount = 0

function Assert-TestTrue([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw $Message
    }
}

function Assert-TestFailure([scriptblock]$Action, [string]$ExpectedText) {
    try {
        & $Action
    }
    catch {
        if (-not $_.Exception.Message.Contains($ExpectedText)) {
            throw "Expected failure containing '$ExpectedText', got: $($_.Exception.Message)"
        }
        return
    }
    throw "Expected failure containing '$ExpectedText', but the action succeeded."
}

function Invoke-TestCase([string]$Name, [scriptblock]$Action) {
    & $Action
    $script:WindowsGpuPackHistoryTestCount++
    Write-Output "PASS: $Name"
}

function Get-TestDigest([char]$Character) {
    return ([string]$Character) * 64
}

function New-TestPack {
    param(
        [ValidateSet('cuda', 'vulkan')]
        [string]$Backend,
        [string]$Version,
        [char]$DigestCharacter,
        [char]$FileHashCharacter = $DigestCharacter,
        [string[]]$AdditionalRelativeFiles = @(),
        [uint64]$SecurityEpoch = 1
    )
    $packId = "scribe-$Backend-windows-x64"
    $digest = Get-TestDigest $DigestCharacter
    $root = "workers/packs/$packId/$Version/$digest"
    $relativeFiles = [System.Collections.Generic.List[string]]::new()
    $relativeFiles.Add('pack-manifest.json')
    $relativeFiles.Add('pack-manifest.sig')
    $payloadRelativeFiles = if ($AdditionalRelativeFiles.Count -eq 0) {
        [string[]]@('bin/worker.exe')
    }
    else {
        $AdditionalRelativeFiles
    }
    foreach ($relativeFile in $payloadRelativeFiles) {
        $relativeFiles.Add($relativeFile)
    }
    $relativeFiles.Sort([System.StringComparer]::Ordinal)
    $fileIndex = 0
    $files = @($relativeFiles | ForEach-Object {
        $fileIndex++
        [ordered]@{
            path = "$root/$_"
            size_bytes = $fileIndex
            sha256 = Get-TestDigest $FileHashCharacter
        }
    })
    return [ordered]@{
        pack_id = $packId
        pack_version = $Version
        pack_digest = $digest
        security_epoch = $SecurityEpoch
        root = $root
        files = [object[]]$files
    }
}

function New-TestRelease {
    param(
        [string]$ReleaseId,
        [char]$RevisionCharacter,
        [char]$CatalogCharacter,
        [object[]]$Packs = @()
    )
    $orderedPacks = [System.Collections.Generic.List[object]]::new()
    foreach ($pack in $Packs) {
        $orderedPacks.Add($pack)
    }
    $orderedPacks.Sort([System.Comparison[object]]{
        param($left, $right)
        [System.StringComparer]::Ordinal.Compare([string]$left.root, [string]$right.root)
    })
    return [ordered]@{
        release_id = $ReleaseId
        source_revision = ([string]$RevisionCharacter) * 40
        catalog_size_bytes = 100 + $orderedPacks.Count
        catalog_sha256 = Get-TestDigest $CatalogCharacter
        catalog_pack_roots = [string[]]@($orderedPacks | ForEach-Object { $_.root })
        packs = [object[]]$orderedPacks.ToArray()
    }
}

function New-TestDocument([object[]]$Releases = @()) {
    return [ordered]@{
        schema_version = 1
        history_epoch = 1
        releases = [object[]]$Releases
    }
}

function Convert-TestDocument([object]$Document, [string]$Label = 'Test document') {
    $json = $Document | ConvertTo-Json -Depth 16 -Compress
    return ConvertFrom-WindowsGpuPackHistoryJson -Json $json -SourceLabel $Label
}

function Copy-TestValue([object]$Value) {
    return (($Value | ConvertTo-Json -Depth 16 -Compress) | ConvertFrom-Json -Depth 16 -AsHashtable)
}

$packA = New-TestPack `
    -Backend cuda -Version '1.0.0' -DigestCharacter 'a' `
    -AdditionalRelativeFiles @('bin/worker.exe')
$packB = New-TestPack `
    -Backend cuda -Version '2.0.0' -DigestCharacter 'b' `
    -AdditionalRelativeFiles @('bin/worker.exe')
$packC = New-TestPack `
    -Backend vulkan -Version '3.0.0' -DigestCharacter 'c' `
    -AdditionalRelativeFiles @('bin/worker.exe')
$releaseA = New-TestRelease `
    -ReleaseId '1.0.0' -RevisionCharacter '1' -CatalogCharacter 'a' -Packs @($packA)
$releaseB = New-TestRelease `
    -ReleaseId '2.0.0' -RevisionCharacter '2' -CatalogCharacter 'b' -Packs @($packB)
$releaseC = New-TestRelease `
    -ReleaseId '3.0.0' -RevisionCharacter '3' -CatalogCharacter 'c' -Packs @($packC)
$emptyRelease = New-TestRelease `
    -ReleaseId 'cpu-only' -RevisionCharacter '4' -CatalogCharacter 'd'

Invoke-TestCase 'empty history and empty current release' {
    $history = Convert-TestDocument (New-TestDocument)
    $current = Convert-TestDocument (New-TestDocument @($emptyRelease))
    $plan = Get-WindowsGpuPackRetirementPlan `
        -HistoryDocument $history -CurrentDocument $current
    Assert-TestTrue ($plan.CurrentPackRoots.Count -eq 0) 'Empty current release returned pack roots.'
    Assert-TestTrue ($plan.CurrentFiles.Count -eq 0) 'Empty current release returned files.'
    Assert-TestTrue ($plan.RetiredFiles.Count -eq 0) 'Empty history returned retired files.'
    Assert-TestTrue ($plan.HistoricalCatalogs.Count -eq 0) 'Empty history returned catalog identities.'
    Assert-TestTrue ($plan.CurrentCatalog.PackRoots.Count -eq 0) 'Empty current catalog returned pack roots.'
}

Invoke-TestCase 'A to B retirement' {
    $history = Convert-TestDocument (New-TestDocument @($releaseA))
    $current = Convert-TestDocument (New-TestDocument @($releaseB))
    $plan = Get-WindowsGpuPackRetirementPlan `
        -HistoryDocument $history -CurrentDocument $current
    Assert-TestTrue ($plan.RetiredPackRoots.Count -eq 1 -and
        $plan.RetiredPackRoots[0] -ceq $packA.root) 'A to B did not retire exactly A.'
    Assert-TestTrue ($plan.RetiredFiles.Count -eq 3) 'A to B did not retain A byte identities.'
    Assert-TestTrue ($plan.CurrentDirectories -ccontains 'workers') 'Current directories omitted workers.'
    Assert-TestTrue ($plan.CurrentDirectories -ccontains 'workers/packs') 'Current directories omitted workers/packs.'
    Assert-TestTrue ($plan.RetiredDirectories[0] -ceq "$($packA.root)/bin") 'Retired directories are not deepest-first.'
    Assert-TestTrue ($plan.RetiredDirectories -ccontains $packA.root) 'Retired directories omitted the exact pack root.'
    foreach ($retirementPath in @($plan.RetiredFiles.Path)) {
        Assert-TestTrue (
            $retirementPath -ceq $packA.root -or
            $retirementPath.StartsWith($packA.root + '/', [StringComparison]::Ordinal)
        ) "Retirement plan escaped A's canonical root: $retirementPath"
    }
    foreach ($retirementDirectory in $plan.RetiredDirectories) {
        Assert-TestTrue (
            $retirementDirectory -ceq 'workers' -or
            $retirementDirectory -ceq 'workers/packs' -or
            $packA.root.StartsWith($retirementDirectory + '/', [StringComparison]::Ordinal) -or
            $retirementDirectory -ceq $packA.root -or
            $retirementDirectory.StartsWith($packA.root + '/', [StringComparison]::Ordinal)
        ) "Retirement directory escaped A's canonical chain: $retirementDirectory"
    }
}

Invoke-TestCase 'A to empty retirement' {
    $history = Convert-TestDocument (New-TestDocument @($releaseA))
    $current = Convert-TestDocument (New-TestDocument @($emptyRelease))
    $plan = Get-WindowsGpuPackRetirementPlan `
        -HistoryDocument $history -CurrentDocument $current
    Assert-TestTrue ($plan.RetiredFiles.Count -eq 3) 'A to empty did not retire A inventory.'
    Assert-TestTrue ($plan.CurrentDirectories.Count -eq 0) 'A to empty retained current pack directories.'
    Assert-TestTrue ($plan.RetiredDirectories -ccontains "workers/packs/$($packA.pack_id)") 'A to empty omitted the pack-ID ancestor.'
    Assert-TestTrue ($plan.RetiredDirectories -ccontains "workers/packs/$($packA.pack_id)/$($packA.pack_version)") 'A to empty omitted the pack-version ancestor.'
    Assert-TestTrue ($plan.RetiredDirectories[-2] -ceq 'workers/packs' -and
        $plan.RetiredDirectories[-1] -ceq 'workers') 'A to empty omitted or reordered reviewed structural ancestors.'
}

Invoke-TestCase 'skipped A to C retirement' {
    $history = Convert-TestDocument (New-TestDocument @($releaseA))
    $current = Convert-TestDocument (New-TestDocument @($releaseC))
    $plan = Get-WindowsGpuPackRetirementPlan `
        -HistoryDocument $history -CurrentDocument $current
    Assert-TestTrue ($plan.RetiredPackRoots.Count -eq 1 -and
        $plan.CurrentPackRoots.Count -eq 1) 'Skipped A to C transition was not deterministic.'
    Assert-TestTrue ($plan.RetiredPackRoots[0] -ceq $packA.root -and
        $plan.CurrentPackRoots[0] -ceq $packC.root) 'Skipped A to C selected the wrong roots.'
}

Invoke-TestCase 'same repair is not retired' {
    $repairRelease = New-TestRelease `
        -ReleaseId '1.0.0-repair' -RevisionCharacter '5' -CatalogCharacter 'e' -Packs @($packA)
    $history = Convert-TestDocument (New-TestDocument @($releaseA))
    $current = Convert-TestDocument (New-TestDocument @($repairRelease))
    $plan = Get-WindowsGpuPackRetirementPlan `
        -HistoryDocument $history -CurrentDocument $current
    Assert-TestTrue ($plan.RetiredFiles.Count -eq 0) 'Same repair retired its identical immutable root.'
    Assert-TestTrue ($plan.UniquePackFileCount -eq 3) 'Same repair double-counted immutable files.'
}

Invoke-TestCase 'identical-root reintroduction excludes that root' {
    $reintroduced = New-TestRelease `
        -ReleaseId '3.0.0-reintroduce-a' -RevisionCharacter '6' -CatalogCharacter 'f' -Packs @($packA)
    $history = Convert-TestDocument (New-TestDocument @($releaseA, $releaseB))
    $current = Convert-TestDocument (New-TestDocument @($reintroduced))
    $plan = Get-WindowsGpuPackRetirementPlan `
        -HistoryDocument $history -CurrentDocument $current
    Assert-TestTrue ($plan.RetiredPackRoots.Count -eq 1 -and
        $plan.RetiredPackRoots[0] -ceq $packB.root) 'Reintroduction did not exclude A or retire B.'
    Assert-TestTrue ($plan.RetiredFiles.Path -cnotcontains "$($packA.root)/pack-manifest.json") 'Reintroduced A file was marked retired.'
}

Invoke-TestCase 'duplicate unknown and tampered inventory are rejected' {
    Assert-TestFailure {
        ConvertFrom-WindowsGpuPackHistoryJson -Json '{"schema_version":1'
    } 'malformed JSON'

    $validJson = (New-TestDocument @($releaseA)) | ConvertTo-Json -Depth 16 -Compress
    $duplicateKeyJson = $validJson.Replace(
        '"schema_version":1',
        '"schema_version":1,"Schema_Version":1'
    )
    Assert-TestFailure {
        ConvertFrom-WindowsGpuPackHistoryJson -Json $duplicateKeyJson
    } 'duplicate or case-colliding JSON key'

    $unknownJson = $validJson.Replace('"releases":', '"unknown":true,"releases":')
    Assert-TestFailure {
        ConvertFrom-WindowsGpuPackHistoryJson -Json $unknownJson
    } 'unknown or missing fields'

    $duplicateFilePack = Copy-TestValue $packA
    $duplicateFilePack.files = [object[]]@($duplicateFilePack.files + $duplicateFilePack.files[0])
    $duplicateFileRelease = New-TestRelease `
        -ReleaseId 'duplicate-file' -RevisionCharacter '7' -CatalogCharacter '7' `
        -Packs @($duplicateFilePack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($duplicateFileRelease))
    } 'duplicate or case-colliding file paths'

    $tamperedPack = New-TestPack `
        -Backend cuda -Version '1.0.0' -DigestCharacter 'a' -FileHashCharacter 'd' `
        -AdditionalRelativeFiles @('bin/worker.exe')
    $tamperedRelease = New-TestRelease `
        -ReleaseId 'tampered-reuse' -RevisionCharacter '8' -CatalogCharacter '8' `
        -Packs @($tamperedPack)
    $history = Convert-TestDocument (New-TestDocument @($releaseA))
    $current = Convert-TestDocument (New-TestDocument @($tamperedRelease))
    Assert-TestFailure {
        Get-WindowsGpuPackRetirementPlan -HistoryDocument $history -CurrentDocument $current
    } 'different exact inventory'

    $unknownPack = Copy-TestValue $packA
    $unknownPack.unknown = $true
    $unknownPackRelease = New-TestRelease `
        -ReleaseId 'unknown-pack' -RevisionCharacter '9' -CatalogCharacter '9' `
        -Packs @($unknownPack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($unknownPackRelease))
    } 'unknown or missing fields'

    $wrongTypePack = Copy-TestValue $packA
    $wrongTypePack.security_epoch = '1'
    $wrongTypeRelease = New-TestRelease `
        -ReleaseId 'wrong-type' -RevisionCharacter '9' -CatalogCharacter '1' `
        -Packs @($wrongTypePack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($wrongTypeRelease))
    } 'must be an unsigned integer'

    $duplicateRootRelease = New-TestRelease `
        -ReleaseId 'duplicate-root' -RevisionCharacter '9' -CatalogCharacter '1' `
        -Packs @($packA, $packA)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($duplicateRootRelease))
    } 'duplicate or case-colliding catalog pack roots'

    $upperRootPack = New-TestPack `
        -Backend cuda -Version 'CaseRoot' -DigestCharacter 'e'
    $lowerRootPack = New-TestPack `
        -Backend cuda -Version 'caseroot' -DigestCharacter 'e'
    $caseRootRelease = New-TestRelease `
        -ReleaseId 'case-root' -RevisionCharacter '9' -CatalogCharacter '1' `
        -Packs @($upperRootPack, $lowerRootPack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($caseRootRelease))
    } 'duplicate or case-colliding catalog pack roots'
}

Invoke-TestCase 'missing audit metadata and control files are rejected' {
    $missingRevision = Copy-TestValue $releaseA
    $null = $missingRevision.Remove('source_revision')
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($missingRevision))
    } 'unknown or missing fields'

    $missingControlPack = New-TestPack `
        -Backend cuda -Version 'missing-control' -DigestCharacter 'a' `
        -AdditionalRelativeFiles @('bin/helper.dll', 'bin/worker.exe')
    $missingControlPack.files = [object[]]@(
        $missingControlPack.files | Where-Object { $_.path -cnotlike '*/pack-manifest.sig' }
    )
    $missingControlRelease = New-TestRelease `
        -ReleaseId 'missing-control' -RevisionCharacter 'a' -CatalogCharacter '1' `
        -Packs @($missingControlPack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($missingControlRelease))
    } 'pack-manifest.json and pack-manifest.sig'

    $wrongRootsRelease = Copy-TestValue $releaseA
    $wrongRootsRelease.catalog_pack_roots = [string[]]@()
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($wrongRootsRelease))
    } 'catalog pack roots do not match'

    $unknownIdentityPack = Copy-TestValue $packA
    $unknownIdentityPack.pack_id = 'scribe-directml-windows-x64'
    $unknownIdentityRelease = New-TestRelease `
        -ReleaseId 'unknown-identity' -RevisionCharacter 'a' -CatalogCharacter '1' `
        -Packs @($unknownIdentityPack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($unknownIdentityRelease))
    } 'not an approved Windows GPU pack identity'

    $privateRootPack = Copy-TestValue $packA
    $privateRootPack.root = 'AppData/Local/Scribe/installed-arbitrary'
    $privateRootRelease = New-TestRelease `
        -ReleaseId 'private-root' -RevisionCharacter 'a' -CatalogCharacter '1' `
        -Packs @($privateRootPack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($privateRootRelease))
    } 'canonical immutable Windows GPU pack layout'
}

Invoke-TestCase 'zero-byte payloads are admitted but empty control envelopes are rejected' {
    $zeroPayloadPack = New-TestPack `
        -Backend cuda -Version 'zero-payload' -DigestCharacter 'b' `
        -AdditionalRelativeFiles @('bin/empty-native.dll', 'bin/worker.exe')
    $zeroPayloadPath = "$($zeroPayloadPack.root)/bin/empty-native.dll"
    $zeroPayloadEntry = @($zeroPayloadPack.files | Where-Object {
        $_.path -ceq $zeroPayloadPath
    })
    Assert-TestTrue ($zeroPayloadEntry.Count -eq 1) 'Zero-payload fixture did not select one ordinary file.'
    $zeroPayloadEntry[0].size_bytes = 0
    $zeroPayloadEntry[0].sha256 = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'
    $zeroPayloadRelease = New-TestRelease `
        -ReleaseId 'zero-payload' -RevisionCharacter 'b' -CatalogCharacter 'b' `
        -Packs @($zeroPayloadPack)
    $history = Convert-TestDocument (New-TestDocument @($zeroPayloadRelease))
    $current = Convert-TestDocument (New-TestDocument @($emptyRelease))
    $plan = Get-WindowsGpuPackRetirementPlan `
        -HistoryDocument $history -CurrentDocument $current
    $plannedZeroFile = @($plan.RetiredFiles | Where-Object { $_.Path -ceq $zeroPayloadPath })
    Assert-TestTrue ($plannedZeroFile.Count -eq 1 -and
        $plannedZeroFile[0].SizeBytes -eq 0 -and
        $plannedZeroFile[0].Sha256 -ceq $zeroPayloadEntry[0].sha256) `
        'Retirement planning did not preserve the zero-byte payload identity.'

    foreach ($controlName in @('pack-manifest.json', 'pack-manifest.sig')) {
        $emptyControlPack = Copy-TestValue $zeroPayloadPack
        $controlPath = "$($emptyControlPack.root)/$controlName"
        $controlEntry = @($emptyControlPack.files | Where-Object { $_.path -ceq $controlPath })
        Assert-TestTrue ($controlEntry.Count -eq 1) "Control fixture did not select $controlName."
        $controlEntry[0].size_bytes = 0
        $emptyControlRelease = New-TestRelease `
            -ReleaseId ('empty-' + $controlName.Replace('.', '-')) `
            -RevisionCharacter 'c' -CatalogCharacter 'c' -Packs @($emptyControlPack)
        Assert-TestFailure {
            Convert-TestDocument (New-TestDocument @($emptyControlRelease))
        } 'outside its bounded range'
    }

    $nestedControlPack = New-TestPack `
        -Backend cuda -Version 'nested-control' -DigestCharacter 'd' `
        -AdditionalRelativeFiles @('nested/pack-manifest.json')
    $nestedControlRelease = New-TestRelease `
        -ReleaseId 'nested-control' -RevisionCharacter 'd' -CatalogCharacter 'd' `
        -Packs @($nestedControlPack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($nestedControlRelease))
    } 'reserved pack control-envelope name'

    $aggregatePack = New-TestPack `
        -Backend vulkan -Version 'aggregate-limit' -DigestCharacter 'c' `
        -AdditionalRelativeFiles @('one.bin', 'two.bin')
    foreach ($payloadEntry in @($aggregatePack.files | Where-Object {
        $_.path -cnotlike '*/pack-manifest.*'
    })) {
        $payloadEntry.size_bytes = [uint64](2GB)
    }
    $aggregateRelease = New-TestRelease `
        -ReleaseId 'aggregate-limit' -RevisionCharacter 'c' -CatalogCharacter 'd' `
        -Packs @($aggregatePack)
    $null = Convert-TestDocument (New-TestDocument @($aggregateRelease))
    $aggregatePack.files += [ordered]@{
        path = "$($aggregatePack.root)/zero-overflow.bin"
        size_bytes = 1
        sha256 = Get-TestDigest 'd'
    }
    $aggregatePack.files = [object[]]@($aggregatePack.files | Sort-Object -Property path -CaseSensitive)
    $aggregateOverflowRelease = New-TestRelease `
        -ReleaseId 'aggregate-overflow' -RevisionCharacter 'd' -CatalogCharacter 'e' `
        -Packs @($aggregatePack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($aggregateOverflowRelease))
    } 'aggregate payload bound'
}

Invoke-TestCase 'finite file directory release and value bounds fail closed' {
    $fileLimitPacks = [System.Collections.Generic.List[object]]::new()
    foreach ($packIndex in 1..4) {
        $backend = if (($packIndex % 2) -eq 1) { 'cuda' } else { 'vulkan' }
        $paths = [string[]]@(1..254 | ForEach-Object {
            "f$($_.ToString('D4')).bin"
        })
        $fileLimitPacks.Add((New-TestPack `
            -Backend $backend -Version "files-$packIndex" `
            -DigestCharacter ([char]([string]$packIndex)) `
            -AdditionalRelativeFiles $paths))
    }
    $fileLimitHistoryRelease = New-TestRelease `
        -ReleaseId 'files-history' -RevisionCharacter 'b' -CatalogCharacter '2' `
        -Packs @($fileLimitPacks[0], $fileLimitPacks[1], $fileLimitPacks[2])
    $fileLimitCurrentRelease = New-TestRelease `
        -ReleaseId 'files-current' -RevisionCharacter 'c' -CatalogCharacter '3' `
        -Packs @($fileLimitPacks[3])
    $fileLimitPlan = Get-WindowsGpuPackRetirementPlan `
        -HistoryDocument (Convert-TestDocument (New-TestDocument @($fileLimitHistoryRelease))) `
        -CurrentDocument (Convert-TestDocument (New-TestDocument @($fileLimitCurrentRelease)))
    Assert-TestTrue ($fileLimitPlan.UniquePackFileCount -eq 1024) 'The exact 1,024-file bound was not admitted.'

    $fileOverflowPack = New-TestPack `
        -Backend vulkan -Version 'files-4-overflow' -DigestCharacter '5' `
        -AdditionalRelativeFiles ([string[]]@(1..255 | ForEach-Object {
            "f$($_.ToString('D4')).bin"
        }))
    $fileOverflowRelease = New-TestRelease `
        -ReleaseId 'files-current-overflow' -RevisionCharacter 'd' -CatalogCharacter '4' `
        -Packs @($fileOverflowPack)
    Assert-TestFailure {
        Get-WindowsGpuPackRetirementPlan `
            -HistoryDocument (Convert-TestDocument (New-TestDocument @($fileLimitHistoryRelease))) `
            -CurrentDocument (Convert-TestDocument (New-TestDocument @($fileOverflowRelease)))
    } 'installer handle bound'

    $directoryLimitPacks = [System.Collections.Generic.List[object]]::new()
    $directoryOverflowPacks = [System.Collections.Generic.List[object]]::new()
    $directoryLimitDigests = @('5', '6', '7', '8')
    $directoryOverflowDigests = @('9', 'a', 'b', 'c')
    foreach ($packIndex in 1..4) {
        $backend = if (($packIndex % 2) -eq 1) { 'cuda' } else { 'vulkan' }
        $directoryCount = 222
        $directoryLimitPaths = [string[]]@(1..$directoryCount | ForEach-Object {
            "d$($_.ToString('D4'))/file.bin"
        })
        $directoryLimitPacks.Add((New-TestPack `
            -Backend $backend -Version "directories-$packIndex" `
            -DigestCharacter ([char]$directoryLimitDigests[$packIndex - 1]) `
            -AdditionalRelativeFiles $directoryLimitPaths))
        $overflowDirectoryCount = if ($packIndex -eq 4) { 223 } else { 222 }
        $overflowDirectoryPaths = [string[]]@(1..$overflowDirectoryCount | ForEach-Object {
            "d$($_.ToString('D4'))/file.bin"
        })
        $directoryOverflowPacks.Add((New-TestPack `
            -Backend $backend -Version "directories-overflow-$packIndex" `
            -DigestCharacter ([char]$directoryOverflowDigests[$packIndex - 1]) `
            -AdditionalRelativeFiles $overflowDirectoryPaths))
    }
    $directoryLimitRelease = New-TestRelease `
        -ReleaseId 'directory-limit' -RevisionCharacter 'd' -CatalogCharacter '4' `
        -Packs $directoryLimitPacks.ToArray()
    $directoryLimitPlan = Get-WindowsGpuPackRetirementPlan `
        -HistoryDocument (Convert-TestDocument (New-TestDocument)) `
        -CurrentDocument (Convert-TestDocument (New-TestDocument @($directoryLimitRelease)))
    Assert-TestTrue ($directoryLimitPlan.UniqueAncestorDirectoryCount -eq 900) 'The exact 900-directory bound was not admitted.'
    $directoryOverflowRelease = New-TestRelease `
        -ReleaseId 'directory-overflow' -RevisionCharacter 'e' -CatalogCharacter '5' `
        -Packs $directoryOverflowPacks.ToArray()
    Assert-TestFailure {
        Get-WindowsGpuPackRetirementPlan `
            -HistoryDocument (Convert-TestDocument (New-TestDocument)) `
            -CurrentDocument (Convert-TestDocument (New-TestDocument @($directoryOverflowRelease)))
    } 'installer handle bound'

    $controlsOnlyPack = Copy-TestValue $packA
    $controlsOnlyPack.files = [object[]]@($controlsOnlyPack.files | Where-Object {
        $_.path -clike '*/pack-manifest.*'
    })
    $controlsOnlyRelease = New-TestRelease `
        -ReleaseId 'controls-only' -RevisionCharacter 'e' -CatalogCharacter '6' `
        -Packs @($controlsOnlyPack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($controlsOnlyRelease))
    } 'bounded inventory size'

    $complete3Pack = New-TestPack `
        -Backend cuda -Version 'complete-3' -DigestCharacter 'c'
    Assert-TestTrue (@($complete3Pack.files).Count -eq 3) 'Minimum complete inventory fixture is not exactly three files.'
    $complete3Release = New-TestRelease `
        -ReleaseId 'complete-3' -RevisionCharacter 'e' -CatalogCharacter '7' `
        -Packs @($complete3Pack)
    $null = Convert-TestDocument (New-TestDocument @($complete3Release))

    $complete258Pack = New-TestPack `
        -Backend cuda -Version 'complete-258' -DigestCharacter 'd' `
        -AdditionalRelativeFiles ([string[]]@(1..256 | ForEach-Object {
            "f$($_.ToString('D4')).bin"
        }))
    $complete258Release = New-TestRelease `
        -ReleaseId 'complete-258' -RevisionCharacter 'e' -CatalogCharacter '7' `
        -Packs @($complete258Pack)
    $null = Convert-TestDocument (New-TestDocument @($complete258Release))
    $complete259Pack = New-TestPack `
        -Backend cuda -Version 'complete-259' -DigestCharacter 'e' `
        -AdditionalRelativeFiles ([string[]]@(1..257 | ForEach-Object {
            "f$($_.ToString('D4')).bin"
        }))
    $complete259Release = New-TestRelease `
        -ReleaseId 'complete-259' -RevisionCharacter 'e' -CatalogCharacter '8' `
        -Packs @($complete259Pack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($complete259Release))
    } 'bounded inventory size'

    $catalogAtLimit = Copy-TestValue $releaseA
    $catalogAtLimit.catalog_size_bytes = 512KB
    $null = Convert-TestDocument (New-TestDocument @($catalogAtLimit))
    $catalogOverLimit = Copy-TestValue $releaseA
    $catalogOverLimit.release_id = 'catalog-over-limit'
    $catalogOverLimit.catalog_size_bytes = 512KB + 1
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($catalogOverLimit))
    } 'outside its bounded range'

    $boundedReleases = [System.Collections.Generic.List[object]]::new()
    foreach ($index in 1..128) {
        $boundedReleases.Add((New-TestRelease `
            -ReleaseId "release-$index" -RevisionCharacter 'e' -CatalogCharacter '5'))
    }
    $boundedHistory = Convert-TestDocument (New-TestDocument $boundedReleases.ToArray())
    Assert-TestTrue ($boundedHistory.releases.Count -eq 128) 'The exact 128-release bound was not admitted.'
    $boundedReleases.Add((New-TestRelease `
        -ReleaseId 'release-129' -RevisionCharacter 'e' -CatalogCharacter '5'))
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument $boundedReleases.ToArray())
    } 'reviewed history-epoch and upgrade design'

    $oversizedPack = Copy-TestValue $packA
    $oversizedPack.files[0].size_bytes = [uint64](1TB) + 1
    $oversizedRelease = New-TestRelease `
        -ReleaseId 'oversized' -RevisionCharacter 'f' -CatalogCharacter '6' `
        -Packs @($oversizedPack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($oversizedRelease))
    } 'outside its bounded range'
}

Invoke-TestCase 'path device Unicode case and control ambiguity are rejected' {
    $unsafeRelativePaths = @(
        '../escape.bin',
        'bad:stream',
        'wild*.bin',
        'CON.txt',
        "caf$([char]0x00E9).bin",
        "bad$([char]1).bin"
    )
    foreach ($relativePath in $unsafeRelativePaths) {
        $unsafePack = New-TestPack `
            -Backend cuda -Version 'unsafe' -DigestCharacter '4' `
            -AdditionalRelativeFiles @($relativePath)
        $unsafeRelease = New-TestRelease `
            -ReleaseId ('unsafe-' + [Array]::IndexOf($unsafeRelativePaths, $relativePath)) `
            -RevisionCharacter '1' -CatalogCharacter '7' -Packs @($unsafePack)
        Assert-TestFailure {
            Convert-TestDocument (New-TestDocument @($unsafeRelease))
        } 'path'
    }

    $casePack = New-TestPack `
        -Backend cuda -Version 'case' -DigestCharacter '5' `
        -AdditionalRelativeFiles @('Case.bin', 'case.bin')
    $caseRelease = New-TestRelease `
        -ReleaseId 'case' -RevisionCharacter '2' -CatalogCharacter '8' -Packs @($casePack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($caseRelease))
    } 'duplicate or case-colliding file paths'

    $directoryCasePack = New-TestPack `
        -Backend cuda -Version 'directory-case' -DigestCharacter '7' `
        -AdditionalRelativeFiles @('Dir/a.bin', 'dir/b.bin')
    $directoryCaseRelease = New-TestRelease `
        -ReleaseId 'directory-case' -RevisionCharacter '2' -CatalogCharacter '8' `
        -Packs @($directoryCasePack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($directoryCaseRelease))
    } 'case-colliding directory identity'

    $overlapPack = New-TestPack `
        -Backend cuda -Version 'overlap' -DigestCharacter '6' `
        -AdditionalRelativeFiles @('dir', 'dir/child.bin')
    $overlapRelease = New-TestRelease `
        -ReleaseId 'overlap' -RevisionCharacter '3' -CatalogCharacter '9' -Packs @($overlapPack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($overlapRelease))
    } 'ambiguous file/descendant overlap'

    $ambiguousControlPack = Copy-TestValue $packA
    $ambiguousControlPack.files[1].path = "$($packA.root)/pack-manifest.JSON"
    $ambiguousControlPack.files = [object[]]@(
        $ambiguousControlPack.files | Sort-Object -Property path -CaseSensitive
    )
    $ambiguousControlRelease = New-TestRelease `
        -ReleaseId 'ambiguous-control' -RevisionCharacter '4' -CatalogCharacter 'a' `
        -Packs @($ambiguousControlPack)
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($ambiguousControlRelease))
    } 'reserved pack control-envelope name'
}

Invoke-TestCase 'signed pack version grammar and catalog identities fail closed' {
    foreach ($invalidVersion in @('CaseVersion', 'version+metadata', 'version-')) {
        $invalidVersionPack = New-TestPack `
            -Backend cuda -Version $invalidVersion -DigestCharacter '8'
        $invalidVersionRelease = New-TestRelease `
            -ReleaseId ('invalid-version-' + $invalidVersion.Replace('+', '-')) `
            -RevisionCharacter '5' -CatalogCharacter 'b' -Packs @($invalidVersionPack)
        Assert-TestFailure {
            Convert-TestDocument (New-TestDocument @($invalidVersionRelease))
        } 'canonical signed-pack store component'
    }

    $catalogCollisionRelease = Copy-TestValue $releaseB
    $catalogCollisionRelease.catalog_size_bytes = $releaseA.catalog_size_bytes
    $catalogCollisionRelease.catalog_sha256 = $releaseA.catalog_sha256
    Assert-TestFailure {
        Get-WindowsGpuPackRetirementPlan `
            -HistoryDocument (Convert-TestDocument (New-TestDocument @($releaseA))) `
            -CurrentDocument (Convert-TestDocument (New-TestDocument @($catalogCollisionRelease)))
    } 'catalog byte identity'
}

Invoke-TestCase 'append-only history accepts append and rejects modification removal and reorder' {
    $previous = Convert-TestDocument (New-TestDocument @($releaseA, $releaseB))
    $appended = Convert-TestDocument (New-TestDocument @($releaseA, $releaseB, $releaseC))
    Assert-TestTrue (
        (Assert-WindowsGpuPackHistoryAppendOnly `
            -PreviousDocument $previous -NewDocument $appended)
    ) 'Valid append-only history was rejected.'

    $modifiedReleaseA = Copy-TestValue $releaseA
    $modifiedReleaseA.catalog_sha256 = Get-TestDigest 'd'
    $modified = Convert-TestDocument (New-TestDocument @($modifiedReleaseA, $releaseB))
    Assert-TestFailure {
        Assert-WindowsGpuPackHistoryAppendOnly `
            -PreviousDocument $previous -NewDocument $modified
    } 'modified or reordered'

    $removed = Convert-TestDocument (New-TestDocument @($releaseA))
    Assert-TestFailure {
        Assert-WindowsGpuPackHistoryAppendOnly `
            -PreviousDocument $previous -NewDocument $removed
    } 'removed a previously published release row'

    $reordered = Convert-TestDocument (New-TestDocument @($releaseB, $releaseA))
    Assert-TestFailure {
        Assert-WindowsGpuPackHistoryAppendOnly `
            -PreviousDocument $previous -NewDocument $reordered
    } 'modified or reordered'
}

$epochPack1 = New-TestPack cuda 'epoch-1' '1' -SecurityEpoch 1
$epochPack2 = New-TestPack cuda 'epoch-2' '2' -SecurityEpoch 2
$epochPack3 = New-TestPack cuda 'epoch-3' '3' -SecurityEpoch 3
$epochPackEqual = New-TestPack cuda 'epoch-2-equal' '4' -SecurityEpoch 2
$epochRelease1 = New-TestRelease 'epoch-1' '1' '1' @($epochPack1)
$epochRelease2 = New-TestRelease 'epoch-2' '2' '2' @($epochPack2)
$epochRelease3 = New-TestRelease 'epoch-3' '3' '3' @($epochPack3)
$epochReleaseEqual = New-TestRelease 'epoch-2-equal' '4' '4' @($epochPackEqual)
$epochError = 'security epoch is below the historical high-water mark'

Invoke-TestCase 'history epochs accept equality and increases but reject decreases' {
    $valid = Convert-TestDocument (New-TestDocument @($epochRelease2, $epochReleaseEqual, $epochRelease3))
    Assert-TestTrue ($valid.releases.Count -eq 3) 'Valid epoch progression lost rows.'
    Assert-TestFailure {
        Convert-TestDocument (New-TestDocument @($epochRelease2, $epochRelease1))
    } $epochError
    Assert-TestFailure {
        Assert-WindowsGpuPackHistoryAppendOnly `
            -PreviousDocument (New-TestDocument @($epochRelease2)) `
            -NewDocument (New-TestDocument @($epochRelease2, $epochRelease1))
    } $epochError
}

Invoke-TestCase 'same-ID roots require one epoch independent of lexical root assignment' {
    foreach ($reverse in @($false, $true)) {
        $left = New-TestPack cuda 'epoch-left' '5' -SecurityEpoch 2
        $right = New-TestPack cuda 'epoch-right' '6' -SecurityEpoch 2
        $inputPacks = if ($reverse) { @($right, $left) } else { @($left, $right) }
        $equalRow = New-TestRelease 'equal-roots' '5' '5' $inputPacks
        $equal = Convert-TestDocument (New-TestDocument @($equalRow))
        Assert-TestTrue ($equal.releases[0].packs.Count -eq 2) 'Equal-epoch roots were rejected.'
        if ($reverse) { $left.security_epoch = [uint64]3 }
        else { $right.security_epoch = [uint64]3 }
        $mixedRow = New-TestRelease 'mixed-roots' '6' '6' $inputPacks
        Assert-TestFailure {
            Convert-TestDocument (New-TestDocument @($mixedRow))
        } 'mixes security epochs for one pack ID'
    }
}

Invoke-TestCase 'CPU-only releases and backend absence never reset epoch floors' {
    foreach ($intervening in @($emptyRelease, $releaseC)) {
        Assert-TestFailure {
            Convert-TestDocument (New-TestDocument @($epochRelease2, $intervening, $epochRelease1))
        } $epochError
        $valid = Convert-TestDocument (New-TestDocument @($epochRelease2, $intervening, $epochReleaseEqual))
        Assert-TestTrue ($valid.releases.Count -eq 3) 'Reappearance at the floor was rejected.'
    }
}

Invoke-TestCase 'current staging respects historical floors and accepts CPU-only current' {
    $history = New-TestDocument @($epochRelease2, $emptyRelease)
    Assert-TestFailure {
        Get-WindowsGpuPackRetirementPlan $history (New-TestDocument @($epochRelease1))
    } $epochError
    foreach ($allowed in @($epochReleaseEqual, $epochRelease3)) {
        $plan = Get-WindowsGpuPackRetirementPlan $history (New-TestDocument @($allowed))
        Assert-TestTrue ($plan.CurrentPackRoots.Count -eq 1) 'Allowed current epoch lost its pack.'
    }
    $cpu = Get-WindowsGpuPackRetirementPlan $history (New-TestDocument @($emptyRelease))
    Assert-TestTrue ($cpu.CurrentPackRoots.Count -eq 0) 'CPU-only current was not allowed.'
}

Invoke-TestCase 'exact latest-row repair passes but exact older-row replay cannot downgrade' {
    $history = New-TestDocument @($epochRelease2, $epochRelease3)
    $latest = Get-WindowsGpuPackRetirementPlan $history (New-TestDocument @($epochRelease3))
    Assert-TestTrue ($latest.CurrentPackRoots[0] -ceq $epochPack3.root) 'Exact latest row could not be repaired.'
    Assert-TestFailure {
        Get-WindowsGpuPackRetirementPlan $history (New-TestDocument @($epochRelease2))
    } $epochError
    $changedEpoch = Copy-TestValue $epochPack3
    $changedEpoch.security_epoch = [uint64]4
    $changedRootRow = New-TestRelease 'changed-immutable-epoch' '7' '7' @($changedEpoch)
    Assert-TestFailure {
        Get-WindowsGpuPackRetirementPlan $history (New-TestDocument @($changedRootRow))
    } 'different exact inventory'
}

Invoke-TestCase 'CUDA and Vulkan epoch floors are independent' {
    $vulkan1 = New-TestPack vulkan 'epoch-1' '7' -SecurityEpoch 1
    $vulkan2 = New-TestPack vulkan 'epoch-2' '8' -SecurityEpoch 2
    $first = New-TestRelease 'both-first' '7' '7' @($epochPack3, $vulkan1)
    $second = New-TestRelease 'both-second' '8' '8' @($epochPack3, $vulkan2)
    $valid = Convert-TestDocument (New-TestDocument @($first, $second))
    Assert-TestTrue ($valid.releases.Count -eq 2) 'One backend raised the other backend floor.'
    $plan = Get-WindowsGpuPackRetirementPlan (New-TestDocument @($first)) (New-TestDocument @($second))
    Assert-TestTrue ($plan.CurrentPackRoots.Count -eq 2) 'Independent current floors rejected.'
}

Invoke-TestCase 'security epochs preserve full UInt64 precision across signed and maximum boundaries' {
    $values = @(
        [uint64]::Parse('9223372036854775807'),
        [uint64]::Parse('9223372036854775808'),
        [uint64]::Parse('18446744073709551614'),
        [uint64]::MaxValue
    )
    $rows = @()
    for ($index = 0; $index -lt $values.Count; $index++) {
        $digit = [char]([int][char]'1' + $index)
        $pack = New-TestPack cuda "uint64-$index" $digit -SecurityEpoch $values[$index]
        $rows += New-TestRelease "uint64-$index" $digit $digit @($pack)
    }
    $history = Convert-TestDocument (New-TestDocument $rows)
    for ($index = 0; $index -lt $values.Count; $index++) {
        $actual = $history.releases[$index].packs[0].security_epoch
        Assert-TestTrue ($actual -is [uint64] -and $actual -eq $values[$index]) 'Epoch lost UInt64 precision.'
    }
    $latest = Get-WindowsGpuPackRetirementPlan $history (New-TestDocument @($rows[3]))
    Assert-TestTrue ($latest.CurrentPackRoots.Count -eq 1) 'UInt64 maximum equality failed.'
    Assert-TestFailure {
        Get-WindowsGpuPackRetirementPlan $history (New-TestDocument @($rows[2]))
    } $epochError
    $regression = New-TestRelease 'uint64-regression' '5' '3' @($rows[2].packs[0])
    Assert-TestFailure { Convert-TestDocument (New-TestDocument @($rows + $regression)) } $epochError
}

Invoke-TestCase 'failed rows and empty rows cannot partially advance shared epoch state' {
    $floors = [System.Collections.Generic.Dictionary[string, uint64]]::new([StringComparer]::OrdinalIgnoreCase)
    $floors.Add('scribe-cuda-windows-x64', [uint64]2)
    $floors.Add('scribe-vulkan-windows-x64', [uint64]2)
    $vulkanLow = New-TestPack vulkan 'low' '8' -SecurityEpoch 1
    $normalizedLow = (Convert-TestDocument (New-TestDocument @(
        (New-TestRelease 'atomic-low' '8' '8' @($epochPack3, $vulkanLow))
    ))).releases[0].packs
    Assert-TestFailure {
        Update-WindowsGpuPackHistorySecurityEpochHighWater $normalizedLow $floors 'Test row'
    } $epochError
    Assert-TestTrue ($floors.Count -eq 2 -and $floors['scribe-cuda-windows-x64'] -eq [uint64]2 -and
        $floors['scribe-vulkan-windows-x64'] -eq [uint64]2) 'Failed row partially advanced a floor.'
    $normalized2 = (Convert-TestDocument (New-TestDocument @($epochRelease2))).releases[0].packs[0]
    Assert-TestFailure {
        Update-WindowsGpuPackHistorySecurityEpochHighWater @($normalizedLow[0], $normalized2) $floors 'Mixed row'
    } 'mixes security epochs for one pack ID'
    Update-WindowsGpuPackHistorySecurityEpochHighWater @() $floors 'Empty row'
    Assert-TestTrue ($floors.Count -eq 2 -and $floors['scribe-cuda-windows-x64'] -eq [uint64]2 -and
        $floors['scribe-vulkan-windows-x64'] -eq [uint64]2) 'Mixed or empty row changed an epoch floor.'
}

Invoke-TestCase 'bounded regular parser input rejects BOM oversize and reparse ancestors' {
    $testRoot = Join-Path `
        ([System.IO.Path]::GetTempPath()) `
        "scribe-gpu-pack-history-test-$([guid]::NewGuid().ToString('N'))"
    $junction = Join-Path $testRoot 'history-link'
    try {
        New-Item -ItemType Directory -Path $testRoot | Out-Null
        $validPath = Join-Path $testRoot 'history.json'
        $emptyJson = (New-TestDocument | ConvertTo-Json -Depth 4)
        [System.IO.File]::WriteAllText(
            $validPath,
            $emptyJson,
            [System.Text.UTF8Encoding]::new($false)
        )
        $read = Read-WindowsGpuPackHistory -LiteralPath $validPath
        Assert-TestTrue ($read.releases.Count -eq 0) 'Regular parser input changed empty history.'

        $bomPath = Join-Path $testRoot 'history-bom.json'
        $bomBytes = [byte[]](0xEF, 0xBB, 0xBF) + [System.Text.Encoding]::UTF8.GetBytes($emptyJson)
        [System.IO.File]::WriteAllBytes($bomPath, $bomBytes)
        Assert-TestFailure {
            Read-WindowsGpuPackHistory -LiteralPath $bomPath
        } 'without a byte-order mark'

        $oversizedPath = Join-Path $testRoot 'history-oversized.json'
        $oversizedStream = [System.IO.File]::OpenWrite($oversizedPath)
        try {
            $oversizedStream.SetLength(4MB + 1)
        }
        finally {
            $oversizedStream.Dispose()
        }
        Assert-TestFailure {
            Read-WindowsGpuPackHistory -LiteralPath $oversizedPath
        } 'exceeds the bounded JSON size'

        $realDirectory = Join-Path $testRoot 'history-real'
        New-Item -ItemType Directory -Path $realDirectory | Out-Null
        [System.IO.File]::WriteAllText(
            (Join-Path $realDirectory 'history.json'),
            $emptyJson,
            [System.Text.UTF8Encoding]::new($false)
        )
        New-Item -ItemType Junction -Path $junction -Target $realDirectory | Out-Null
        Assert-TestFailure {
            Read-WindowsGpuPackHistory -LiteralPath (Join-Path $junction 'history.json')
        } 'link or reparse point'
    }
    finally {
        if (Test-Path -LiteralPath $junction) {
            $junctionItem = Get-Item -LiteralPath $junction -Force
            if (($junctionItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0) {
                throw 'Refused to clean a fixture path that is no longer the expected junction.'
            }
            Remove-Item -LiteralPath $junction -Force
        }
        $temporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
        $resolvedTestRoot = [System.IO.Path]::GetFullPath($testRoot)
        if (-not $resolvedTestRoot.StartsWith(
            $temporaryRoot,
            [System.StringComparison]::OrdinalIgnoreCase
        ) -or
            (Split-Path -Leaf $resolvedTestRoot) -cnotmatch '^scribe-gpu-pack-history-test-[0-9a-f]{32}$') {
            throw 'Refused parser-test cleanup outside its narrowly owned temporary root.'
        }
        if (Test-Path -LiteralPath $resolvedTestRoot) {
            Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force
        }
    }
}

Invoke-TestCase 'history helper remains parse validate and plan only' {
    $source = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'windows-gpu-pack-history.ps1') -Raw
    foreach ($forbiddenToken in @(
        'Remove-Item', 'Copy-Item', 'Move-Item', 'New-Item',
        '[System.IO.File]::Delete', '[System.IO.Directory]::Delete'
    )) {
        Assert-TestTrue (-not $source.Contains($forbiddenToken)) "History helper gained mutating token: $forbiddenToken"
    }
}

if ($script:WindowsGpuPackHistoryTestCount -ne 23) {
    throw 'Expected Windows GPU pack history test cases were not all executed.'
}
Write-Output "Windows GPU pack history fail-closed tests passed ($script:WindowsGpuPackHistoryTestCount cases)."
