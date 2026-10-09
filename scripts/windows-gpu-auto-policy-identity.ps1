$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Build-time byte identity only. This helper cannot supply, approve, or activate
# a policy. Load windows-frozen-cpu-worker-integrity.ps1 before using it.
function Read-WindowsGpuAutoPolicyBoundedBytes([IO.FileStream]$Stream, [int64]$MaximumBytes) {
    if ($Stream.Length -le 0 -or $Stream.Length -gt $MaximumBytes) {
        throw 'GPU Auto policy identity input is empty or oversized.'
    }
    $Stream.Position = 0
    $bytes = [byte[]]::new([int]$Stream.Length)
    $offset = 0
    while ($offset -lt $bytes.Length) {
        $read = $Stream.Read($bytes, $offset, $bytes.Length - $offset)
        if ($read -le 0) { throw 'GPU Auto policy identity input changed during reading.' }
        $offset += $read
    }
    if ($Stream.Length -ne $bytes.Length) { throw 'GPU Auto policy identity input changed during reading.' }
    $Stream.Position = 0
    return ,$bytes
}

function Open-WindowsGpuAutoPolicyIdentity([string]$RepositoryRoot) {
    $manifestPath = Join-Path $RepositoryRoot 'runtime-manifests/gpu-auto-qualification-windows-x64.json'
    $stream = Open-WindowsFrozenCpuWorkerReadHandle $manifestPath
    try {
        $bytes = Read-WindowsGpuAutoPolicyBoundedBytes $stream (512KB)
        $raw = [Text.UTF8Encoding]::new($false, $true).GetString($bytes)
        if ($raw[0] -eq [char]0xfeff) { throw 'GPU Auto policy identity input cannot contain a UTF-8 BOM.' }
        # Reuse the full existing manifest validator while the original file is
        # held against replacement/writes. Never execute a helper from worker R.
        & (Join-Path $RepositoryRoot 'scripts/report-windows-gpu-auto-qualification.ps1') -ManifestPath $manifestPath | Out-Null
        $document = ConvertFrom-Json -InputObject $raw -AsHashtable -Depth 16
        $canonical = if ($raw.EndsWith("`n", [StringComparison]::Ordinal)) { $raw.Substring(0, $raw.Length - 1) } else { $raw }
        $identity = [pscustomobject]@{
            ManifestPath = Get-WindowsFrozenCpuWorkerNormalizedFullPath $manifestPath
            ManifestStream = $stream
            EmbeddedManifestSizeBytes = [int64]$bytes.Length
            EmbeddedManifestSha256 = ConvertTo-WindowsFrozenCpuWorkerSha256 $bytes
            RuntimeManifestSha256 = ConvertTo-WindowsFrozenCpuWorkerSha256 ([Text.Encoding]::UTF8.GetBytes($canonical))
            EntryCount = [int64]$document.entries.Count
            PolicySchemaVersion = [int64]2
            PolicyVersion = [int64]3
            TargetOs = 'windows'
            TargetArch = 'x86_64'
            Mode = 'default_deny'
        }
        Assert-WindowsGpuAutoPolicyIdentitySource $identity
        return $identity
    }
    catch {
        $stream.Dispose()
        throw
    }
}

function Assert-WindowsGpuAutoPolicyIdentitySource([psobject]$PolicyIdentity) {
    if ($PolicyIdentity.ManifestStream.Length -ne $PolicyIdentity.EmbeddedManifestSizeBytes -or
        (Get-WindowsFrozenCpuWorkerOpenStreamSha256 $PolicyIdentity.ManifestStream) -cne $PolicyIdentity.EmbeddedManifestSha256) {
        throw 'The retained GPU Auto policy manifest changed.'
    }
    # Also check the current path: a retained handle is not permission to accept
    # a different filesystem generation under the same name.
    $current = Open-WindowsFrozenCpuWorkerReadHandle $PolicyIdentity.ManifestPath
    try {
        if ($current.Length -ne $PolicyIdentity.EmbeddedManifestSizeBytes -or
            (Get-WindowsFrozenCpuWorkerOpenStreamSha256 $current) -cne $PolicyIdentity.EmbeddedManifestSha256) {
            throw 'The GPU Auto policy manifest path changed.'
        }
    }
    finally { $current.Dispose() }
}

function Get-WindowsGpuAutoPolicyDesktopBuildId(
    [string]$RepositoryRoot,
    [AllowNull()][string]$BuildRevisionOverride
) {
    $cargoStream = Open-WindowsFrozenCpuWorkerReadHandle (Join-Path $RepositoryRoot 'Cargo.toml')
    try {
        $cargo = [Text.UTF8Encoding]::new($false, $true).GetString((Read-WindowsGpuAutoPolicyBoundedBytes $cargoStream (1MB)))
    }
    finally { $cargoStream.Dispose() }
    $versions = @([regex]::Matches($cargo, '(?m)^version\s*=\s*"([^"]+)"'))
    if ($versions.Count -ne 1) { throw 'GPU Auto policy identity could not derive one app version.' }
    $version = $versions[0].Groups[1].Value
    if (-not [string]::IsNullOrWhiteSpace($BuildRevisionOverride)) {
        if ($BuildRevisionOverride.Length -lt 12 -or $BuildRevisionOverride.Length -gt 96 -or
            $BuildRevisionOverride -cmatch '[^\x20-\x7e]') {
            throw 'GPU Auto policy identity build override must be a 12-96 character ASCII build identity.'
        }
        $revision = $BuildRevisionOverride
    }
    elseif (Test-Path -LiteralPath (Join-Path $RepositoryRoot '.git')) {
        # Match build_support/build_revision.rs: query physical Git identity,
        # ignoring repository/index override variables, without requiring a
        # clean developer checkout or changing persistent Git configuration.
        $gitNames = @('GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR', 'GIT_INDEX_FILE', 'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_NAMESPACE')
        $saved = @{}
        try {
            foreach ($name in $gitNames) {
                $saved[$name] = [Environment]::GetEnvironmentVariable($name)
                Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
            }
            $lines = @(Invoke-WindowsFrozenCpuWorkerGit $RepositoryRoot @('rev-parse', '--verify', 'HEAD'))
            if ($lines.Count -ne 1 -or $lines[0] -cnotmatch '\A[0-9a-f]{40}\z') {
                throw 'GPU Auto policy identity could not resolve the physical build revision.'
            }
            $revision = $lines[0]
        }
        finally {
            foreach ($name in $saved.Keys) {
                if ($null -eq $saved[$name]) {
                    Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
                }
                else { [Environment]::SetEnvironmentVariable($name, $saved[$name]) }
            }
        }
    }
    else {
        # Preserve the existing source-archive fallback and its exact order.
        $sourceBytes = [IO.MemoryStream]::new()
        try {
            foreach ($relative in @('Cargo.lock', 'build.rs', 'src/onnx_worker.rs', 'src/worker_contracts.rs')) {
                $sourceStream = Open-WindowsFrozenCpuWorkerReadHandle (Join-Path $RepositoryRoot $relative)
                try {
                    $bytes = Read-WindowsGpuAutoPolicyBoundedBytes $sourceStream (16MB)
                    $sourceBytes.Write($bytes, 0, $bytes.Length)
                }
                finally { $sourceStream.Dispose() }
            }
            $revision = 'source-' + (ConvertTo-WindowsFrozenCpuWorkerSha256 $sourceBytes.ToArray())
        }
        finally { $sourceBytes.Dispose() }
    }
    return "local-transcriber@$version#$revision"
}

function Assert-WindowsGpuAutoPolicyCompiledIdentity(
    [string]$Executable,
    [int64]$ExpectedSize,
    [string]$ExpectedSha256,
    [string]$ExpectedDesktopBuildId,
    [psobject]$PolicyIdentity
) {
    Assert-WindowsFrozenCpuWorkerDigest $ExpectedSha256 'Compiled GPU Auto policy desktop SHA-256'
    Assert-WindowsGpuAutoPolicyIdentitySource $PolicyIdentity
    $stream = Open-WindowsFrozenCpuWorkerReadHandle $Executable
    try {
        if ($stream.Length -ne $ExpectedSize -or
            (Get-WindowsFrozenCpuWorkerOpenStreamSha256 $stream) -cne $ExpectedSha256) {
            throw 'Desktop executable changed before GPU Auto policy identity verification.'
        }
        $result = Invoke-WindowsFrozenCpuWorkerAdmissionProcess `
            -Executable $Executable -Command '--scribe-windows-gpu-auto-policy-identity'
        if ($stream.Length -ne $ExpectedSize -or
            (Get-WindowsFrozenCpuWorkerOpenStreamSha256 $stream) -cne $ExpectedSha256) {
            throw 'Desktop executable changed during GPU Auto policy identity verification.'
        }
    }
    finally { $stream.Dispose() }
    Assert-WindowsGpuAutoPolicyIdentitySource $PolicyIdentity
    if ($result.ExitCode -ne 0 -or -not [string]::IsNullOrEmpty($result.Stderr)) {
        throw 'Compiled GPU Auto policy identity process failed or returned unexpected diagnostics.'
    }
    if ($result.Stdout -isnot [string] -or [string]::IsNullOrWhiteSpace($result.Stdout) -or $result.Stdout.Length -gt 65536) {
        throw 'Compiled GPU Auto policy identity report is empty or oversized.'
    }
    $expected = [ordered]@{
        schema_version = [int64]1
        desktop_build_id = $ExpectedDesktopBuildId
        policy_schema_version = [int64]$PolicyIdentity.PolicySchemaVersion
        policy_version = [int64]$PolicyIdentity.PolicyVersion
        target_os = [string]$PolicyIdentity.TargetOs
        target_arch = [string]$PolicyIdentity.TargetArch
        mode = [string]$PolicyIdentity.Mode
        entry_count = [int64]$PolicyIdentity.EntryCount
        embedded_manifest_size_bytes = [int64]$PolicyIdentity.EmbeddedManifestSizeBytes
        embedded_manifest_sha256 = [string]$PolicyIdentity.EmbeddedManifestSha256
        runtime_manifest_sha256 = [string]$PolicyIdentity.RuntimeManifestSha256
    }
    $document = $null
    try {
        $options = [Text.Json.JsonDocumentOptions]::new()
        $options.MaxDepth = 2
        $document = [Text.Json.JsonDocument]::Parse([string]$result.Stdout, $options)
        if ($document.RootElement.ValueKind -ne [Text.Json.JsonValueKind]::Object) {
            throw 'Compiled GPU Auto policy identity report must be one JSON object.'
        }
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        $expectedNames = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($name in $expected.Keys) { $null = $expectedNames.Add($name) }
        foreach ($property in $document.RootElement.EnumerateObject()) {
            if (-not $seen.Add($property.Name) -or -not $expectedNames.Contains($property.Name)) {
                throw 'Compiled GPU Auto policy identity report has unexpected or duplicate fields.'
            }
            $wanted = $expected[$property.Name]
            if ($wanted -is [string]) {
                if ($property.Value.ValueKind -ne [Text.Json.JsonValueKind]::String -or $property.Value.GetString() -cne $wanted) {
                    throw "Compiled GPU Auto policy identity report mismatches $($property.Name)."
                }
            }
            else {
                $integer = [int64]0
                if ($property.Value.ValueKind -ne [Text.Json.JsonValueKind]::Number -or
                    -not $property.Value.TryGetInt64([ref]$integer) -or $integer -ne $wanted) {
                    throw "Compiled GPU Auto policy identity report mismatches $($property.Name)."
                }
            }
        }
        if ($seen.Count -ne $expected.Count) { throw 'Compiled GPU Auto policy identity report is missing fields.' }
    }
    finally { if ($null -ne $document) { $document.Dispose() } }
    return [pscustomobject]$expected
}
