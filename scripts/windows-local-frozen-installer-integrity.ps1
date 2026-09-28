$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# This helper validates a local frozen bundle only.  It is deliberately not a
# release trust format and does not alter the production package verifier.

. (Join-Path $PSScriptRoot 'windows-frozen-cpu-worker-integrity.ps1')
. (Join-Path $PSScriptRoot 'windows-pe-imports.ps1')

$script:WindowsLocalFrozenBaseFiles = @(
    'bundled-model-manifest.json',
    'licenses/Apache-2.0.txt',
    'licenses/OpenAI-Whisper-MIT.txt',
    'licenses/Silero-VAD-MIT.txt',
    'licenses/Silero-VAD-PROVENANCE.md',
    'licenses/THIRD-PARTY-NOTICES.txt',
    'licenses/Whisper-Base-En-NOTICE.txt',
    'licenses/sherpa-onnx-PROVENANCE.md',
    'licenses/transcribe.cpp-MIT.txt',
    'licenses/transcribe.cpp-PROVENANCE.md',
    'licenses/whisper.cpp-MIT.txt',
    'licenses/whisper.cpp-PROVENANCE.md',
    'local-transcriber.exe',
    'README.txt',
    'scribe-inference-worker.exe',
    'worker-pack-catalog.json',
    'whisper-base.en-Q8_0.gguf',
    (Get-WindowsFrozenCpuWorkerMarkerFileName)
)

function Get-WindowsLocalFrozenNormalizedFullPath([string]$Path) {
    return Get-WindowsFrozenCpuWorkerNormalizedFullPath $Path
}

function Test-WindowsLocalFrozenPathIsWithin([string]$CandidatePath, [string]$RootPath) {
    $candidate = Get-WindowsLocalFrozenNormalizedFullPath $CandidatePath
    $root = Get-WindowsLocalFrozenNormalizedFullPath $RootPath
    return [string]::Equals($candidate, $root, [System.StringComparison]::OrdinalIgnoreCase) -or
        $candidate.StartsWith($root + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)
}

function Assert-WindowsLocalFrozenRegularDirectory([string]$Path, [string]$Description) {
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $Path
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "$Description directory is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Description directory must be regular and non-reparse: $Path"
    }
    Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $Path
    return $item
}

function Get-WindowsLocalFrozenRelativePath([string]$Root, [string]$Path) {
    $normalizedRoot = Get-WindowsLocalFrozenNormalizedFullPath $Root
    $normalizedPath = Get-WindowsLocalFrozenNormalizedFullPath $Path
    $rootUri = [System.Uri]::new($normalizedRoot.TrimEnd([char[]]@('\', '/')) + '\')
    $pathUri = [System.Uri]::new($normalizedPath)
    return [System.Uri]::UnescapeDataString($rootUri.MakeRelativeUri($pathUri).ToString()).Replace('\', '/')
}

function Assert-WindowsLocalFrozenSafeRelativePath([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path) -or
        [System.IO.Path]::IsPathRooted($Path) -or
        $Path.Contains('\') -or $Path.Contains(':')) {
        throw "Local frozen installer payload path is unsafe: $Path"
    }
    $segments = @($Path.Split('/'))
    if ($segments.Count -eq 0 -or $segments.Count -gt 32) {
        throw "Local frozen installer payload path depth is unsafe: $Path"
    }
    foreach ($segment in $segments) {
        if ([string]::IsNullOrWhiteSpace($segment) -or $segment -in @('.', '..') -or
            $segment.Length -gt 128 -or $segment.EndsWith('.') -or $segment.EndsWith(' ')) {
            throw "Local frozen installer payload path segment is unsafe: $Path"
        }
        $stem = $segment.Split('.')[0].ToUpperInvariant()
        if ($stem -in @('CON', 'PRN', 'AUX', 'NUL', 'CLOCK$') -or $stem -match '^(COM|LPT)[1-9]$') {
            throw "Local frozen installer payload path uses a reserved Windows name: $Path"
        }
    }
}

function Assert-WindowsLocalFrozenExactProperties(
    [psobject]$Value,
    [string[]]$ExpectedProperties,
    [string]$Description
) {
    if ($null -eq $Value) {
        throw "$Description is missing."
    }
    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $expected = @($ExpectedProperties | Sort-Object)
    if ($actual.Count -ne $expected.Count -or
        (Compare-Object -ReferenceObject $expected -DifferenceObject $actual -CaseSensitive)) {
        throw "$Description has unexpected or missing fields."
    }
}

function Assert-WindowsLocalFrozenInt64([object]$Value, [string]$Description) {
    if ($Value -is [double] -or $Value -is [single] -or $Value -is [decimal]) {
        throw "$Description must be an integer."
    }
    try {
        return [int64]$Value
    }
    catch {
        throw "$Description must be an integer."
    }
}

function Stop-WindowsLocalFrozenProcessTree(
    [System.Diagnostics.Process]$Process,
    [string]$Description,
    [int]$TerminationGraceMilliseconds = 5000
) {
    if ($TerminationGraceMilliseconds -lt 1 -or $TerminationGraceMilliseconds -gt 30000) {
        throw 'Local frozen process termination grace period is outside the supported bounds.'
    }
    if (-not $Process.HasExited) {
        try {
            $Process.Kill($true)
        }
        catch {
            try {
                $Process.Kill()
            }
            catch {
                throw "Could not terminate $Description after its bounded timeout."
            }
        }
    }
    if (-not $Process.WaitForExit($TerminationGraceMilliseconds)) {
        throw "$Description did not exit within $TerminationGraceMilliseconds milliseconds after termination."
    }
}

function Invoke-WindowsLocalFrozenBoundedProcess(
    [string]$Executable,
    [string[]]$Arguments,
    [string]$Description,
    [int]$TimeoutMilliseconds = 30000,
    [int]$StreamDrainMilliseconds = 5000
) {
    if ($TimeoutMilliseconds -lt 1 -or $TimeoutMilliseconds -gt 120000 -or
        $StreamDrainMilliseconds -lt 1 -or $StreamDrainMilliseconds -gt 30000) {
        throw 'Local frozen process timeout configuration is outside the supported bounds.'
    }
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $startInfo.ArgumentList.Add($argument) }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            throw "Could not start $Description."
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutMilliseconds)) {
            Stop-WindowsLocalFrozenProcessTree $process $Description
            throw "$Description timed out after $TimeoutMilliseconds milliseconds."
        }
        $streamTasks = [System.Threading.Tasks.Task[]]@($stdout, $stderr)
        if (-not [System.Threading.Tasks.Task]::WaitAll($streamTasks, $StreamDrainMilliseconds)) {
            throw "$Description output streams did not close within $StreamDrainMilliseconds milliseconds after process exit."
        }
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout.GetAwaiter().GetResult()
            Stderr = $stderr.GetAwaiter().GetResult()
        }
    }
    finally {
        $process.Dispose()
    }
}

function Assert-WindowsLocalFrozenInstallerRecord(
    [string]$Path,
    [psobject]$FrozenCpuWorker,
    [psobject]$Bundle,
    [string]$Installer
) {
    $recordFile = Read-WindowsFrozenCpuWorkerBoundedUtf8File $Path 65536
    try {
        $record = $recordFile.Text | ConvertFrom-Json -Depth 5
    }
    catch {
        throw "Local frozen installer record is not valid JSON: $($_.Exception.Message)"
    }
    Assert-WindowsLocalFrozenExactProperties $record @(
        'schema_version', 'kind', 'local_only', 'release_approved',
        'source_revision', 'app_version', 'target_triple',
        'frozen_record_sha256', 'bundle_inventory_sha256',
        'installer_filename', 'installer_size_bytes', 'installer_sha256',
        'local_test_token', 'install_relative_path'
    ) 'Local frozen installer record'
    if ((($record.schema_version -isnot [int64] -and $record.schema_version -isnot [int32]) -or
        [int]$record.schema_version -ne 1 -or $record.kind -cne 'windows-local-frozen-test-installer' -or
        $record.local_only -isnot [bool] -or -not $record.local_only -or
        $record.release_approved -isnot [bool] -or $record.release_approved -or
        $record.source_revision -cne $FrozenCpuWorker.Record.source_revision -or
        $record.app_version -cne $FrozenCpuWorker.Record.app_version -or
        $record.target_triple -cne $FrozenCpuWorker.Record.target_triple -or
        $record.frozen_record_sha256 -cne $FrozenCpuWorker.RecordSha256 -or
        $record.bundle_inventory_sha256 -cne $Bundle.InventorySha256)) {
        throw 'Local frozen installer record does not bind the exact local frozen bundle and worker identities.'
    }
    if ($record.installer_filename -isnot [string] -or
        $record.installer_filename -cnotmatch '^Scribe-LOCAL-Frozen-Test-[0-9a-f]{32}\.exe$' -or
        (Split-Path -Leaf $Installer) -cne $record.installer_filename -or
        $record.installer_sha256 -isnot [string] -or $record.installer_sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        $record.local_test_token -isnot [string] -or $record.local_test_token -cnotmatch '^[0-9a-f]{32}$' -or
        $record.installer_filename -cne "Scribe-LOCAL-Frozen-Test-$($record.local_test_token).exe" -or
        $record.install_relative_path -cne "Scribe/LOCAL-Frozen-Test/$($record.local_test_token)") {
        throw 'Local frozen installer record has an invalid installer or local-test identity.'
    }
    $installerItem = Assert-WindowsFrozenCpuWorkerRegularFile $Installer
    $installerSize = Assert-WindowsLocalFrozenInt64 $record.installer_size_bytes 'Local frozen installer record installer size'
    if ($installerItem.Length -ne $installerSize -or
        (Get-WindowsFrozenCpuWorkerFileSha256 $Installer) -cne $record.installer_sha256) {
        throw 'Local frozen installer bytes do not match the local installer record.'
    }
    return $record
}

function Assert-WindowsLocalFrozenSmokeDiagnostics([psobject]$Smoke) {
    if ($null -eq $Smoke) {
        throw 'Installed local frozen CPU smoke did not verify the expected worker cancellation contract.'
    }
    $smokeProperties = @($Smoke.PSObject.Properties | ForEach-Object { $_.Name })
    if ('cancellation_verified' -notin $smokeProperties -or
        'capabilities' -notin $smokeProperties -or
        'detected_architecture' -notin $smokeProperties) {
        throw 'Installed local frozen CPU smoke did not verify the expected worker cancellation contract.'
    }
    $capabilities = $Smoke.capabilities
    if ($null -eq $capabilities -or
        'cancellation' -notin @($capabilities.PSObject.Properties | ForEach-Object { $_.Name }) -or
        $Smoke.cancellation_verified -isnot [bool] -or $Smoke.cancellation_verified -ne $true -or
        $capabilities.cancellation -isnot [bool] -or $capabilities.cancellation -ne $true -or
        [string]$Smoke.detected_architecture -cne 'whisper') {
        throw 'Installed local frozen CPU smoke did not verify the expected worker cancellation contract.'
    }
}

function Wait-WindowsLocalFrozenInstallRootRemoved(
    [string]$Path,
    [int]$TimeoutMilliseconds = 5000
) {
    if ($TimeoutMilliseconds -lt 1 -or $TimeoutMilliseconds -gt 30000) {
        throw 'Local frozen installer removal timeout is outside the supported bounds.'
    }
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        Assert-WindowsFrozenCpuWorkerNoReparseAncestors $Path
        if (-not (Test-Path -LiteralPath $Path)) {
            return
        }
        $remaining = $TimeoutMilliseconds - [int]$stopwatch.ElapsedMilliseconds
        if ($remaining -le 0) {
            throw "Local frozen installer uninstaller left its token-bound program directory after $TimeoutMilliseconds milliseconds."
        }
        Start-Sleep -Milliseconds ([Math]::Min(100, $remaining))
    }
}

function Get-WindowsLocalFrozenDeclaredPackFiles(
    [string]$Root,
    [switch]$VerifyCompiledDescriptors
) {
    $catalogPath = Join-Path $Root 'worker-pack-catalog.json'
    $catalogText = Read-WindowsFrozenCpuWorkerBoundedUtf8File $catalogPath (4MB)
    try {
        $catalog = $catalogText.Text | ConvertFrom-Json -Depth 10
    }
    catch {
        throw "Local frozen installer worker-pack catalog is not valid JSON: $($_.Exception.Message)"
    }
    Assert-WindowsLocalFrozenExactProperties $catalog @('schema_version', 'packs') 'Local frozen installer worker-pack catalog'
    if ($catalog.schema_version -isnot [int64] -and $catalog.schema_version -isnot [int32]) {
        throw 'Local frozen installer worker-pack catalog schema must be an integer.'
    }
    if ([int]$catalog.schema_version -ne 1) {
        throw 'Local frozen installer worker-pack catalog has an unsupported schema.'
    }
    $packs = @($catalog.packs)
    if ($packs.Count -gt 8) {
        throw 'Local frozen installer worker-pack catalog exceeds the eight-pack bound.'
    }

    $files = [System.Collections.Generic.List[string]]::new()
    $identities = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($pack in $packs) {
        Assert-WindowsLocalFrozenExactProperties $pack @(
            'pack_id', 'pack_version', 'pack_digest', 'security_epoch',
            'runtime_abi_version', 'backend', 'provider', 'target_os',
            'target_arch', 'worker_relative_path', 'root',
            'installed_size_bytes', 'compressed_size_bytes', 'files'
        ) 'Local frozen installer worker-pack catalog entry'
        foreach ($property in @('pack_id', 'pack_version', 'provider')) {
            if ($pack.$property -isnot [string] -or [string]$pack.$property -cnotmatch '^[A-Za-z0-9._:-]{1,96}$') {
                throw "Local frozen installer worker-pack catalog has an invalid $property."
            }
        }
        if ($pack.pack_digest -isnot [string] -or [string]$pack.pack_digest -cnotmatch '^[0-9a-f]{64}$') {
            throw 'Local frozen installer worker-pack catalog has a non-canonical digest.'
        }
        $expectedRoot = "workers/packs/$($pack.pack_id)/$($pack.pack_version)/$($pack.pack_digest)"
        if ([string]$pack.root -cne $expectedRoot) {
            throw 'Local frozen installer worker-pack root does not match the immutable layout.'
        }
        Assert-WindowsLocalFrozenSafeRelativePath $expectedRoot
        $packFiles = @($pack.files)
        if ($packFiles.Count -lt 3 -or $packFiles.Count -gt 258) {
            throw 'Local frozen installer worker-pack catalog file count is outside the supported bound.'
        }
        $actualSize = [int64]0
        foreach ($file in $packFiles) {
            if ($file -isnot [string]) {
                throw 'Local frozen installer worker-pack catalog paths must be strings.'
            }
            Assert-WindowsLocalFrozenSafeRelativePath $file
            if (-not $file.StartsWith($expectedRoot + '/', [System.StringComparison]::Ordinal)) {
                throw 'Local frozen installer worker-pack catalog file escapes its immutable root.'
            }
            if (-not $identities.Add($file)) {
                throw "Local frozen installer worker-pack catalog contains a duplicate case-insensitive path: $file"
            }
            $item = Assert-WindowsFrozenCpuWorkerRegularFile (Join-Path $Root ($file -replace '/', '\'))
            $actualSize += [int64]$item.Length
            $files.Add($file)
        }
        if ((Assert-WindowsLocalFrozenInt64 $pack.installed_size_bytes 'Local frozen installer worker-pack installed size') -ne $actualSize -or
            (Assert-WindowsLocalFrozenInt64 $pack.compressed_size_bytes 'Local frozen installer worker-pack compressed size') -lt 0) {
            throw 'Local frozen installer worker-pack catalog size evidence is invalid.'
        }

        if ($VerifyCompiledDescriptors) {
            $verifier = Join-Path $Root 'local-transcriber.exe'
            $verification = Invoke-WindowsLocalFrozenBoundedProcess `
                -Executable $verifier `
                -Arguments @('--scribe-verify-worker-pack', (Join-Path $Root ($expectedRoot -replace '/', '\'))) `
                -Description 'Frozen bundle compiled worker-pack verification'
            if ($verification.ExitCode -ne 0) {
                throw "Frozen bundle compiled worker-pack verification failed closed: $($verification.Stderr.Trim())"
            }
            try {
                $descriptor = $verification.Stdout | ConvertFrom-Json -Depth 8
            }
            catch {
                throw "Frozen bundle compiled worker-pack verifier returned invalid JSON: $($_.Exception.Message)"
            }
            foreach ($field in @(
                'pack_id', 'pack_version', 'pack_digest', 'security_epoch',
                'runtime_abi_version', 'backend', 'provider', 'target_os',
                'target_arch', 'worker_relative_path'
            )) {
                if ([string]$descriptor.$field -cne [string]$pack.$field) {
                    throw "Frozen bundle compiled worker-pack verifier differs at '$field'."
                }
            }
        }
    }
    if ($files.Count -gt 1024) {
        throw 'Local frozen installer worker-pack catalog exceeds the 1,024-file bound.'
    }
    return $files.ToArray()
}

function Assert-WindowsLocalFrozenPayloadTree([string]$Root, [string[]]$ExpectedFiles) {
    $null = Assert-WindowsLocalFrozenRegularDirectory $Root 'Local frozen installer payload'
    $expected = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $ExpectedFiles) {
        Assert-WindowsLocalFrozenSafeRelativePath $path
        if (-not $expected.Add($path)) {
            throw "Local frozen installer expected payload contains a case-insensitive collision: $path"
        }
    }
    $actual = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($item in @(Get-ChildItem -LiteralPath $Root -Recurse -File -Force)) {
        Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $item.FullName
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Local frozen installer payload contains a link or reparse point: $($item.FullName)"
        }
        $relative = Get-WindowsLocalFrozenRelativePath $Root $item.FullName
        Assert-WindowsLocalFrozenSafeRelativePath $relative
        if (-not $actual.Add($relative)) {
            throw "Local frozen installer payload contains a case-insensitive file collision: $relative"
        }
    }
    foreach ($item in @(Get-ChildItem -LiteralPath $Root -Recurse -Directory -Force)) {
        Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $item.FullName
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Local frozen installer payload contains a directory link or reparse point: $($item.FullName)"
        }
        Assert-WindowsLocalFrozenSafeRelativePath (Get-WindowsLocalFrozenRelativePath $Root $item.FullName)
    }
    if ($actual.Count -ne $expected.Count -or -not $expected.SetEquals($actual)) {
        throw 'Local frozen installer payload differs from its exact declared inventory.'
    }
}

function Assert-WindowsLocalFrozenBundle(
    [Parameter(Mandatory = $true)]
    [string]$BundlePath,
    [psobject]$FrozenCpuWorker = $null
) {
    $root = Get-WindowsLocalFrozenNormalizedFullPath $BundlePath
    $null = Assert-WindowsLocalFrozenRegularDirectory $root 'Local frozen installer bundle'
    $packFiles = @(Get-WindowsLocalFrozenDeclaredPackFiles $root)
    $expectedFiles = @($script:WindowsLocalFrozenBaseFiles) + $packFiles
    Assert-WindowsLocalFrozenPayloadTree $root (@($expectedFiles) + @('bundle-inventory.json'))

    if ($null -ne $FrozenCpuWorker) {
        $marker = Read-WindowsFrozenCpuWorkerBoundedUtf8File (Join-Path $root (Get-WindowsFrozenCpuWorkerMarkerFileName)) 8192
        $expectedMarker = Get-WindowsFrozenCpuWorkerBundleMarkerText $FrozenCpuWorker.RecordSha256 $FrozenCpuWorker.Record
        if ($marker.Text -cne $expectedMarker) {
            throw 'Local frozen installer bundle marker does not bind the supplied frozen CPU worker record.'
        }
        $workerPath = Join-Path $root (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
        $worker = Assert-WindowsFrozenCpuWorkerRegularFile $workerPath
        if ($worker.Length -ne [int64]$FrozenCpuWorker.Record.worker_size_bytes -or
            (Get-WindowsFrozenCpuWorkerFileSha256 $workerPath) -cne [string]$FrozenCpuWorker.Record.worker_sha256) {
            throw 'Local frozen installer bundle CPU worker does not preserve the supplied frozen worker bytes.'
        }
    }

    $inventoryText = Read-WindowsFrozenCpuWorkerBoundedUtf8File (Join-Path $root 'bundle-inventory.json') (4MB)
    try {
        $inventory = $inventoryText.Text | ConvertFrom-Json -Depth 8
    }
    catch {
        throw "Local frozen installer bundle inventory is not valid JSON: $($_.Exception.Message)"
    }
    Assert-WindowsLocalFrozenExactProperties $inventory @('schema_version', 'platform_triple', 'files') 'Local frozen installer bundle inventory'
    if ((($inventory.schema_version -isnot [int64] -and $inventory.schema_version -isnot [int32]) -or
        [int]$inventory.schema_version -ne 1 -or
        [string]$inventory.platform_triple -cne (Get-WindowsFrozenCpuWorkerTargetTriple))) {
        throw 'Local frozen installer bundle inventory has an unsupported schema or platform.'
    }
    $inventoryEntries = @($inventory.files)
    if ($inventoryEntries.Count -ne $expectedFiles.Count) {
        throw 'Local frozen installer bundle inventory does not contain the exact payload entry count.'
    }
    $paths = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $inventoryEntries) {
        Assert-WindowsLocalFrozenExactProperties $entry @('path', 'size_bytes', 'sha256') 'Local frozen installer bundle inventory entry'
        if ($entry.path -isnot [string] -or $entry.sha256 -isnot [string] -or $entry.sha256 -cnotmatch '^[0-9a-f]{64}$') {
            throw 'Local frozen installer bundle inventory contains non-canonical path or SHA-256 data.'
        }
        Assert-WindowsLocalFrozenSafeRelativePath $entry.path
        if (-not $paths.Add($entry.path)) {
            throw "Local frozen installer bundle inventory contains a case-insensitive collision: $($entry.path)"
        }
        $size = Assert-WindowsLocalFrozenInt64 $entry.size_bytes 'Local frozen installer bundle inventory size'
        if ($size -lt 0) {
            throw 'Local frozen installer bundle inventory contains a negative size.'
        }
        $path = Join-Path $root ($entry.path -replace '/', '\')
        $item = Assert-WindowsFrozenCpuWorkerRegularFile $path
        if ($item.Length -ne $size -or (Get-WindowsFrozenCpuWorkerFileSha256 $path) -cne $entry.sha256) {
            throw "Local frozen installer bundle inventory does not match $($entry.path)."
        }
    }
    $expected = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($path in $expectedFiles) { $null = $expected.Add($path) }
    if ($paths.Count -ne $expected.Count -or -not $expected.SetEquals($paths)) {
        throw 'Local frozen installer bundle inventory paths differ from the exact payload allowlist.'
    }
    $repositoryRoot = Get-WindowsLocalFrozenNormalizedFullPath (Split-Path -Parent $PSScriptRoot)
    $sourceModelManifestPath = Join-Path $repositoryRoot 'runtime-manifests\whisper-base-en-q8_0-windows-x64.json'
    $sourceModelManifest = Read-WindowsFrozenCpuWorkerBoundedUtf8File $sourceModelManifestPath 65536
    $bundledModelManifestPath = Join-Path $root 'bundled-model-manifest.json'
    $bundledModelManifest = Assert-WindowsFrozenCpuWorkerRegularFile $bundledModelManifestPath
    if ($bundledModelManifest.Length -ne $sourceModelManifest.Bytes.Length -or
        (Get-WindowsFrozenCpuWorkerFileSha256 $bundledModelManifestPath) -cne $sourceModelManifest.Sha256) {
        throw 'Local frozen installer bundled model manifest differs from the pinned source manifest.'
    }
    try {
        $model = $sourceModelManifest.Text | ConvertFrom-Json -Depth 5
    }
    catch {
        throw "Pinned local frozen installer model manifest is not valid JSON: $($_.Exception.Message)"
    }
    if ($model.artifact_filename -isnot [string] -or $model.artifact_filename -cne 'whisper-base.en-Q8_0.gguf' -or
        $model.sha256 -isnot [string] -or $model.sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        [string]$model.platform_triple -cne (Get-WindowsFrozenCpuWorkerTargetTriple)) {
        throw 'Pinned local frozen installer model manifest has an unsupported identity.'
    }
    $modelSize = Assert-WindowsLocalFrozenInt64 $model.size_bytes 'Pinned local frozen installer model size'
    $bundleModelPath = Join-Path $root $model.artifact_filename
    $bundleModel = Assert-WindowsFrozenCpuWorkerRegularFile $bundleModelPath
    if ($bundleModel.Length -ne $modelSize -or
        (Get-WindowsFrozenCpuWorkerFileSha256 $bundleModelPath) -cne [string]$model.sha256) {
        throw 'Local frozen installer bundled model differs from the pinned source manifest.'
    }
    foreach ($relativePath in @(
        'licenses/Apache-2.0.txt', 'licenses/OpenAI-Whisper-MIT.txt',
        'licenses/Whisper-Base-En-NOTICE.txt', 'licenses/THIRD-PARTY-NOTICES.txt',
        'licenses/transcribe.cpp-MIT.txt', 'licenses/transcribe.cpp-PROVENANCE.md',
        'licenses/whisper.cpp-MIT.txt', 'licenses/whisper.cpp-PROVENANCE.md',
        'licenses/sherpa-onnx-PROVENANCE.md', 'licenses/Silero-VAD-MIT.txt',
        'licenses/Silero-VAD-PROVENANCE.md'
    )) {
        $sourceRelativePath = switch ($relativePath) {
            'licenses/transcribe.cpp-MIT.txt' { 'native/transcribe-cpp-v0.1.3/LICENSE' }
            'licenses/transcribe.cpp-PROVENANCE.md' { 'native/transcribe-cpp-v0.1.3/PROVENANCE.md' }
            'licenses/whisper.cpp-MIT.txt' { 'native/whisper-f049fff/LICENSE' }
            'licenses/whisper.cpp-PROVENANCE.md' { 'native/whisper-f049fff/PROVENANCE.md' }
            'licenses/sherpa-onnx-PROVENANCE.md' { 'native/sherpa-onnx-v1.13.5/PROVENANCE.md' }
            'licenses/Silero-VAD-MIT.txt' { 'resources/silero-vad/LICENSE' }
            'licenses/Silero-VAD-PROVENANCE.md' { 'resources/silero-vad/PROVENANCE.md' }
            default { 'resources/licenses/' + (Split-Path -Leaf $relativePath) }
        }
        $sourcePath = Join-Path $repositoryRoot ($sourceRelativePath -replace '/', '\')
        $source = Assert-WindowsFrozenCpuWorkerRegularFile $sourcePath
        $candidatePath = Join-Path $root ($relativePath -replace '/', '\')
        $candidate = Assert-WindowsFrozenCpuWorkerRegularFile $candidatePath
        if ($candidate.Length -ne $source.Length -or
            (Get-WindowsFrozenCpuWorkerFileSha256 $candidatePath) -cne (Get-WindowsFrozenCpuWorkerFileSha256 $sourcePath)) {
            throw "Local frozen installer legal payload differs from the pinned source file: $relativePath"
        }
    }
    $null = Assert-ReviewedWindowsPe (Join-Path $root 'local-transcriber.exe') 2
    $null = Assert-ReviewedWindowsPe (Join-Path $root 'scribe-inference-worker.exe') 3
    $null = Get-WindowsLocalFrozenDeclaredPackFiles $root -VerifyCompiledDescriptors
    return [pscustomobject]@{
        Root = $root
        InventorySha256 = $inventoryText.Sha256
        Files = @($expectedFiles | Sort-Object)
        PackFiles = $packFiles
    }
}

function Assert-WindowsLocalFrozenPayloadParity(
    [Parameter(Mandatory = $true)]
    [string]$ReferenceBundlePath,
    [Parameter(Mandatory = $true)]
    [string]$InstalledPath,
    [string[]]$AllowedAdditionalRootFiles = @('unins000.exe', 'unins000.dat')
) {
    $reference = Assert-WindowsLocalFrozenBundle $ReferenceBundlePath
    $installed = Get-WindowsLocalFrozenNormalizedFullPath $InstalledPath
    $expected = @($reference.Files) + @('bundle-inventory.json') + @($AllowedAdditionalRootFiles)
    Assert-WindowsLocalFrozenPayloadTree $installed $expected
    foreach ($path in @($reference.Files) + @('bundle-inventory.json')) {
        $referencePath = Join-Path $reference.Root ($path -replace '/', '\')
        $installedPath = Join-Path $installed ($path -replace '/', '\')
        $referenceItem = Assert-WindowsFrozenCpuWorkerRegularFile $referencePath
        $installedItem = Assert-WindowsFrozenCpuWorkerRegularFile $installedPath
        if ($installedItem.Length -ne $referenceItem.Length -or
            (Get-WindowsFrozenCpuWorkerFileSha256 $installedPath) -cne (Get-WindowsFrozenCpuWorkerFileSha256 $referencePath)) {
            throw "Local frozen installer payload parity mismatch for $path."
        }
    }
}
