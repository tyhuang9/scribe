$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# This helper deliberately describes local byte integrity only.  It is not a
# signing or release-provenance format, and its records are rejected by the
# production package verifier when copied into a portable bundle.

function Get-WindowsFrozenCpuWorkerRecordFileName {
    return 'windows-frozen-cpu-worker-record.json'
}

function Get-WindowsFrozenCpuWorkerMarkerFileName {
    return 'WINDOWS-FROZEN-CPU-WORKER-LOCAL-ONLY.txt'
}

function Get-WindowsFrozenCpuWorkerExecutableRelativePath {
    return 'scribe-inference-worker.exe'
}

function Get-WindowsFrozenCpuWorkerTargetTriple {
    return 'x86_64-pc-windows-msvc'
}

function Get-WindowsFrozenCpuWorkerMaximumBytes {
    return [int64](2GB)
}

function Assert-WindowsFrozenCpuWorkerLocalOnlyEnvironment {
    if ($env:GITHUB_ACTIONS -ceq 'true' -or $env:CI -ceq 'true') {
        throw 'Frozen CPU worker packaging is local-only and cannot run in hosted CI.'
    }
}

function Get-WindowsFrozenCpuWorkerExpectedRecordProperties {
    return @(
        'schema_version',
        'kind',
        'local_only',
        'release_approved',
        'source_revision',
        'app_version',
        'target_triple',
        'protocol_version',
        'worker_abi_version',
        'desktop_build_id',
        'worker_build_id',
        'cargo_lock_sha256',
        'rust_toolchain_sha256',
        'cargo_manifest_sha256',
        'worker_identity_sha256',
        'build_rs_sha256',
        'build_contract_sha256',
        'worker_relative_path',
        'worker_size_bytes',
        'worker_sha256'
    )
}

function Get-WindowsFrozenCpuWorkerNormalizedFullPath([string]$Path) {
    $full = [System.IO.Path]::GetFullPath($Path)
    $root = [System.IO.Path]::GetPathRoot($full)
    if ([string]::Equals($full, $root, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $root
    }
    return $full.TrimEnd([char[]]@('\', '/'))
}

function Assert-WindowsFrozenCpuWorkerNoReparseAncestors([string]$Path) {
    $current = Get-WindowsFrozenCpuWorkerNormalizedFullPath $Path
    while (-not (Test-Path -LiteralPath $current)) {
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            throw "Could not resolve an existing ancestor for frozen CPU worker path: $Path"
        }
        $current = $parent
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Frozen CPU worker paths cannot cross a symbolic link or reparse point: $current"
        }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            break
        }
        $current = $parent
    }
}

function Test-WindowsFrozenCpuWorkerSafeStreamPathComponent([string]$Component) {
    if ([string]::IsNullOrEmpty($Component) -or $Component -ceq '.' -or $Component -ceq '..' -or
        $Component.EndsWith(' ') -or $Component.EndsWith('.') -or
        $Component -match '[\x00-\x1f"*:<>?|]') {
        return $false
    }
    $stem = $Component.Split('.')[0].TrimEnd(' ').ToUpperInvariant()
    return $stem -notmatch '^(?:CON|PRN|AUX|NUL|COM[1-9¹²³]|LPT[1-9¹²³]|CONIN\$|CONOUT\$)$'
}

function ConvertTo-WindowsFrozenCpuWorkerStreamEnumerationPath([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path) -or $Path.IndexOf([char]0) -ge 0) {
        throw "Frozen CPU worker paths must use a safe absolute Windows stream-enumeration path: $Path"
    }

    # Do not call GetFullPath here: it can normalize ordinary dotted or
    # space-suffixed components to a different physical object before stream
    # enumeration. Explicit verbatim paths retain their caller-supplied identity.
    if ($Path.StartsWith('\\?\', [System.StringComparison]::Ordinal)) {
        if ($Path.Contains('/')) {
            throw "Frozen CPU worker paths must use a safe absolute Windows stream-enumeration path: $Path"
        }
        $suffix = $Path.Substring(4)
        if ($suffix.Length -ge 3 -and $suffix[0] -match '^[A-Za-z]$' -and
            $suffix[1] -eq ':' -and $suffix[2] -eq '\') {
            if ($suffix.Substring(2).Contains(':')) {
                throw "Frozen CPU worker paths cannot name an alternate data stream: $Path"
            }
            return $Path
        }
        if ($suffix.StartsWith('UNC\', [System.StringComparison]::OrdinalIgnoreCase)) {
            if ($suffix.Contains(':')) {
                throw "Frozen CPU worker paths cannot name an alternate data stream: $Path"
            }
            $parts = @($suffix.Substring(4).Split('\'))
            if ($parts.Count -lt 2 -or
                -not (Test-WindowsFrozenCpuWorkerSafeStreamPathComponent $parts[0]) -or
                $parts[0] -notmatch '^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$' -or
                $parts[0].Contains('..') -or
                -not (Test-WindowsFrozenCpuWorkerSafeStreamPathComponent $parts[1])) {
                throw "Frozen CPU worker paths must use a safe absolute Windows stream-enumeration path: $Path"
            }
            return $Path
        }
        throw "Frozen CPU worker paths must use a safe absolute Windows stream-enumeration path: $Path"
    }

    if ($Path.StartsWith('\\', [System.StringComparison]::Ordinal)) {
        if ($Path.StartsWith('\\.\', [System.StringComparison]::Ordinal)) {
            throw "Frozen CPU worker paths must use a safe absolute Windows stream-enumeration path: $Path"
        }
        $suffix = $Path.Substring(2)
        if ($suffix.Contains(':')) {
            throw "Frozen CPU worker paths cannot name an alternate data stream: $Path"
        }
        $parts = @($suffix -split '[\\/]')
        if ($parts.Count -lt 2 -or
            -not (Test-WindowsFrozenCpuWorkerSafeStreamPathComponent $parts[0]) -or
            $parts[0] -notmatch '^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$' -or
            $parts[0].Contains('..') -or
            -not (Test-WindowsFrozenCpuWorkerSafeStreamPathComponent $parts[1])) {
            throw "Frozen CPU worker paths must use a safe absolute Windows stream-enumeration path: $Path"
        }
        foreach ($component in @($parts | Select-Object -Skip 2)) {
            if (-not (Test-WindowsFrozenCpuWorkerSafeStreamPathComponent $component)) {
                throw "Frozen CPU worker paths must use a safe absolute Windows stream-enumeration path: $Path"
            }
        }
        return "\\?\UNC\$($parts -join '\')"
    }

    if ($Path.Length -lt 3 -or $Path[0] -notmatch '^[A-Za-z]$' -or $Path[1] -ne ':' -or
        ($Path[2] -ne '\' -and $Path[2] -ne '/')) {
        throw "Frozen CPU worker paths must use a safe absolute Windows stream-enumeration path: $Path"
    }
    $suffix = $Path.Substring(3)
    if ($suffix.Contains(':')) {
        throw "Frozen CPU worker paths cannot name an alternate data stream: $Path"
    }
    $parts = if ($suffix.Length -eq 0) { @() } else { @($suffix -split '[\\/]') }
    foreach ($component in $parts) {
        if (-not (Test-WindowsFrozenCpuWorkerSafeStreamPathComponent $component)) {
            throw "Frozen CPU worker paths must use a safe absolute Windows stream-enumeration path: $Path"
        }
    }
    return "\\?\$($Path[0]):\$($parts -join '\')"
}

function Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams([string]$Path) {
    $streamPath = ConvertTo-WindowsFrozenCpuWorkerStreamEnumerationPath $Path
    $streams = @(Get-Item -LiteralPath $streamPath -Stream * -ErrorAction Stop)
    foreach ($stream in $streams) {
        if ($stream.Stream -cne ':$DATA') {
            throw "Frozen CPU worker inputs cannot contain an alternate data stream: $Path"
        }
    }
}

function Assert-WindowsFrozenCpuWorkerRegularFile([string]$Path) {
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $Path
    Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $Path
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Frozen CPU worker file is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Frozen CPU worker files must be regular non-reparse files: $Path"
    }
    return $item
}

function Open-WindowsFrozenCpuWorkerReadHandle([string]$Path) {
    $null = Assert-WindowsFrozenCpuWorkerRegularFile $Path
    return [System.IO.File]::Open(
        (Get-WindowsFrozenCpuWorkerNormalizedFullPath $Path),
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
}

function ConvertTo-WindowsFrozenCpuWorkerSha256([byte[]]$Bytes) {
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([System.BitConverter]::ToString($sha256.ComputeHash($Bytes))).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $sha256.Dispose()
    }
}

function Get-WindowsFrozenCpuWorkerOpenStreamSha256([System.IO.FileStream]$Stream) {
    $Stream.Position = 0
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([System.BitConverter]::ToString($sha256.ComputeHash($Stream))).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $sha256.Dispose()
        $Stream.Position = 0
    }
}

function Get-WindowsFrozenCpuWorkerFileSha256([string]$Path) {
    $stream = Open-WindowsFrozenCpuWorkerReadHandle $Path
    try {
        return Get-WindowsFrozenCpuWorkerOpenStreamSha256 $stream
    }
    finally {
        $stream.Dispose()
    }
}

function Read-WindowsFrozenCpuWorkerBoundedUtf8File([string]$Path, [int]$MaximumBytes = 65536) {
    $stream = Open-WindowsFrozenCpuWorkerReadHandle $Path
    try {
        if ($stream.Length -le 0 -or $stream.Length -gt $MaximumBytes) {
            throw "Frozen CPU worker record must be between 1 and $MaximumBytes bytes."
        }
        $bytes = [byte[]]::new([int]$stream.Length)
        $offset = 0
        while ($offset -lt $bytes.Length) {
            $read = $stream.Read($bytes, $offset, $bytes.Length - $offset)
            if ($read -le 0) {
                throw 'Frozen CPU worker record ended before its declared file length.'
            }
            $offset += $read
        }
        $encoding = [System.Text.UTF8Encoding]::new($false, $true)
        try {
            $text = $encoding.GetString($bytes)
        }
        catch {
            throw "Frozen CPU worker record must be valid UTF-8: $($_.Exception.Message)"
        }
        return [pscustomobject]@{
            Bytes = $bytes
            Text = $text
            Sha256 = ConvertTo-WindowsFrozenCpuWorkerSha256 $bytes
        }
    }
    finally {
        $stream.Dispose()
    }
}

function Assert-WindowsFrozenCpuWorkerExactProperties(
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

function Assert-WindowsFrozenCpuWorkerInt64([object]$Value, [string]$Description) {
    if ($Value -isnot [int32] -and $Value -isnot [int64]) {
        throw "$Description must be an integer."
    }
    return [int64]$Value
}

function Assert-WindowsFrozenCpuWorkerDigest([string]$Value, [string]$Description) {
    if ($Value -isnot [string] -or $Value -cnotmatch '^[0-9a-f]{64}$') {
        throw "$Description must be a lowercase SHA-256 digest."
    }
}

function Test-WindowsFrozenCpuWorkerPathIsWithin([string]$CandidatePath, [string]$RootPath) {
    $candidate = Get-WindowsFrozenCpuWorkerNormalizedFullPath $CandidatePath
    $root = Get-WindowsFrozenCpuWorkerNormalizedFullPath $RootPath
    return [string]::Equals($candidate, $root, [System.StringComparison]::OrdinalIgnoreCase) -or
        $candidate.StartsWith($root + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)
}

function Get-WindowsFrozenCpuWorkerSafeTemporaryRoot(
    [psobject]$DesktopContext,
    [psobject]$FrozenCpuWorker
) {
    $desktopSourceRoot = Get-WindowsFrozenCpuWorkerNormalizedFullPath $DesktopContext.RepositoryRoot
    $workerSourceRoot = Get-WindowsFrozenCpuWorkerNormalizedFullPath $FrozenCpuWorker.Context.RepositoryRoot
    if ([string]::Equals($desktopSourceRoot, $workerSourceRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        return Get-WindowsFrozenCpuWorkerNormalizedFullPath ([System.IO.Path]::GetTempPath())
    }
    # A relative TEMP/TMP resolves against the caller's current directory. In
    # distinct-source assembly that is not a stable, independently verified
    # location, so reject it before resolving a scratch root or inspecting any
    # ancestor.
    foreach ($name in @('TEMP', 'TMP')) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if ([string]::IsNullOrWhiteSpace($value)) { continue }
        if (-not [System.IO.Path]::IsPathFullyQualified($value)) {
            throw 'A separately retained frozen worker source requires fully qualified temporary output paths.'
        }
    }
    $temporaryRoot = Get-WindowsFrozenCpuWorkerNormalizedFullPath ([System.IO.Path]::GetTempPath())
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $temporaryRoot
    foreach ($name in @('TEMP', 'TMP')) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if ([string]::IsNullOrWhiteSpace($value)) { continue }
        Assert-WindowsFrozenCpuWorkerNoReparseAncestors $value
        if (Test-WindowsFrozenCpuWorkerPathIsWithin $value $workerSourceRoot) {
            throw 'A separately retained frozen worker source cannot contain temporary output paths.'
        }
    }
    if (Test-WindowsFrozenCpuWorkerPathIsWithin $temporaryRoot $workerSourceRoot) {
        throw 'A separately retained frozen worker source cannot contain temporary output paths.'
    }
    return $temporaryRoot
}

function Invoke-WindowsFrozenCpuWorkerGitProcess([string]$RepositoryRoot, [string[]]$Arguments) {
    $gitCommand = @(Get-Command -Name git -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1)
    if ($gitCommand.Count -ne 1 -or [string]::IsNullOrWhiteSpace($gitCommand[0].Path)) {
        throw 'Could not resolve the Git executable for frozen CPU worker source context.'
    }
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = Get-WindowsFrozenCpuWorkerNormalizedFullPath $gitCommand[0].Path
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in @('--no-optional-locks', '--no-lazy-fetch', '-c', 'core.fsmonitor=false', '--no-pager', '-C', $RepositoryRoot) + $Arguments) {
        $startInfo.ArgumentList.Add($argument)
    }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $started = $false
    try {
        if (-not $process.Start()) {
            throw 'Frozen CPU worker Git query could not start.'
        }
        $started = $true
        $stdoutBuffer = [char[]]::new(4096)
        $stderrBuffer = [char[]]::new(4096)
        $stdout = [Text.StringBuilder]::new()
        $stderr = [Text.StringBuilder]::new()
        $stdoutTask = $process.StandardOutput.ReadAsync($stdoutBuffer, 0, $stdoutBuffer.Length)
        $stderrTask = $process.StandardError.ReadAsync($stderrBuffer, 0, $stderrBuffer.Length)
        $stdoutClosed = $false
        $stderrClosed = $false
        $clock = [Diagnostics.Stopwatch]::StartNew()
        $exitedAt = $null
        while ($true) {
            foreach ($stream in @(
                [pscustomobject]@{ Task = $stdoutTask; Buffer = $stdoutBuffer; Text = $stdout; Closed = $stdoutClosed; Name = 'stdout' },
                [pscustomobject]@{ Task = $stderrTask; Buffer = $stderrBuffer; Text = $stderr; Closed = $stderrClosed; Name = 'stderr' }
            )) {
                if ($stream.Closed -or -not $stream.Task.IsCompleted) { continue }
                if ($stream.Task.IsFaulted -or $stream.Task.IsCanceled) {
                    Stop-WindowsFrozenCpuWorkerAdmissionProcess $process 'Frozen CPU worker Git query'
                    throw "Frozen CPU worker Git query $($stream.Name) capture failed."
                }
                $count = $stream.Task.GetAwaiter().GetResult()
                if ($count -eq 0) {
                    if ($stream.Name -ceq 'stdout') { $stdoutClosed = $true } else { $stderrClosed = $true }
                    continue
                }
                if ($stream.Text.Length -gt 65536 - $count) {
                    Stop-WindowsFrozenCpuWorkerAdmissionProcess $process 'Frozen CPU worker Git query'
                    throw "Frozen CPU worker Git query $($stream.Name) exceeded the fixed 65536-character bound."
                }
                $null = $stream.Text.Append($stream.Buffer, 0, $count)
                if ($stream.Name -ceq 'stdout') {
                    $stdoutTask = $process.StandardOutput.ReadAsync($stdoutBuffer, 0, $stdoutBuffer.Length)
                }
                else {
                    $stderrTask = $process.StandardError.ReadAsync($stderrBuffer, 0, $stderrBuffer.Length)
                }
            }
            if ($process.HasExited -and $null -eq $exitedAt) {
                $exitedAt = $clock.ElapsedMilliseconds
            }
            if ($process.HasExited -and $stdoutClosed -and $stderrClosed) { break }
            if ($clock.ElapsedMilliseconds -ge 10000) {
                if (-not $process.HasExited) {
                    Stop-WindowsFrozenCpuWorkerAdmissionProcess $process 'Frozen CPU worker Git query'
                    throw 'Frozen CPU worker Git query timed out after the fixed 10000-millisecond deadline.'
                }
                throw 'Frozen CPU worker Git query output streams did not close within the fixed deadline.'
            }
            if ($process.HasExited -and ($clock.ElapsedMilliseconds - $exitedAt) -ge 5000) {
                throw 'Frozen CPU worker Git query output streams did not close within the fixed post-exit drain deadline.'
            }
            [Threading.Thread]::Sleep(10)
        }
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout.ToString()
            Stderr = $stderr.ToString()
        }
    }
    catch {
        $originalError = $_
        if ($started -and -not $process.HasExited) {
            try {
                Stop-WindowsFrozenCpuWorkerAdmissionProcess $process 'Frozen CPU worker Git query'
            }
            catch {
                throw 'Frozen CPU worker Git query failed and process termination could not be confirmed.'
            }
        }
        throw $originalError
    }
    finally {
        $process.Dispose()
    }
}

function ConvertFrom-WindowsFrozenCpuWorkerGitOutputLines([string]$Text) {
    if ([string]::IsNullOrEmpty($Text)) { return @() }
    return @($Text -split "`r?`n" | Where-Object { $_.Length -ne 0 })
}

function Invoke-WindowsFrozenCpuWorkerGit([string]$RepositoryRoot, [string[]]$Arguments) {
    $result = Invoke-WindowsFrozenCpuWorkerGitProcess $RepositoryRoot $Arguments
    if ($result.ExitCode -ne 0) {
        throw 'Could not run Git for frozen CPU worker source context; Git with --no-lazy-fetch support is required.'
    }
    return ConvertFrom-WindowsFrozenCpuWorkerGitOutputLines $result.Stdout
}

function Assert-WindowsFrozenCpuWorkerNoExternalGitFilters([string]$RepositoryRoot) {
    # `status` must not consult an externally configured filter from R. Ask Git
    # for names only so an unsafe command value is never echoed into diagnostics.
    $result = Invoke-WindowsFrozenCpuWorkerGitProcess $RepositoryRoot @(
        'config', '--name-only', '--get-regexp', '^(filter\..*\.(clean|process))$'
    )
    $filterNames = @(ConvertFrom-WindowsFrozenCpuWorkerGitOutputLines $result.Stdout)
    if ($result.ExitCode -eq 1 -and $filterNames.Count -eq 0) { return }
    if ($result.ExitCode -ne 0) {
        throw 'Could not inspect frozen CPU worker Git filters; Git with --no-lazy-fetch support is required.'
    }
    if ($filterNames.Count -ne 0) {
        throw 'Frozen CPU worker Git configuration contains an external clean or process filter.'
    }
}

function Get-WindowsFrozenCpuWorkerSourceContext([string]$RepositoryRoot) {
    $root = Get-WindowsFrozenCpuWorkerNormalizedFullPath $RepositoryRoot
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $root
    # `git -C` does not override these variables. Refuse an alternate repository
    # or index instead of recording its clean HEAD beside this checkout's files.
    foreach ($name in @(
        'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR',
        'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_NAMESPACE'
    )) {
        if (-not [string]::IsNullOrEmpty([Environment]::GetEnvironmentVariable($name))) {
            throw "Frozen CPU worker packaging does not accept Git repository overrides: $name"
        }
    }
    Assert-WindowsFrozenCpuWorkerNoExternalGitFilters $root
    $topLevelLines = @(Invoke-WindowsFrozenCpuWorkerGit $root @('rev-parse', '--show-toplevel'))
    if ($topLevelLines.Count -ne 1 -or
        -not [string]::Equals((Get-WindowsFrozenCpuWorkerNormalizedFullPath $topLevelLines[0]), $root, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Frozen CPU worker Git top-level does not match the source checkout.'
    }
    foreach ($path in @(
        'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/config.toml', 'build.rs', 'src/worker_identity.rs',
        'scripts/windows-cpu-worker-native-baseline.ps1',
        'scripts/new-windows-frozen-cpu-worker.ps1',
        'scripts/build-windows-release.ps1'
    )) {
        $null = Assert-WindowsFrozenCpuWorkerRegularFile (Join-Path $root $path)
    }
    $status = @(Invoke-WindowsFrozenCpuWorkerGit $root @('status', '--porcelain=v1', '--untracked-files=all'))
    if ($status.Count -ne 0) {
        throw 'Frozen CPU worker packaging requires a clean source workspace.'
    }
    $revisionLines = @(Invoke-WindowsFrozenCpuWorkerGit $root @('rev-parse', '--verify', 'HEAD'))
    if ($revisionLines.Count -ne 1 -or $revisionLines[0] -cnotmatch '^[0-9a-f]{40}$') {
        throw 'Frozen CPU worker packaging requires a canonical lowercase Git HEAD revision.'
    }
    $cargoToml = [System.IO.File]::ReadAllText((Join-Path $root 'Cargo.toml'), [System.Text.UTF8Encoding]::new($false, $true))
    $versions = @([regex]::Matches($cargoToml, '(?m)^version\s*=\s*"([^"]+)"'))
    if ($versions.Count -ne 1 -or $versions[0].Groups[1].Value -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$') {
        throw 'Frozen CPU worker packaging could not derive one canonical app version from Cargo.toml.'
    }
    $identity = [System.IO.File]::ReadAllText((Join-Path $root 'src\worker_identity.rs'), [System.Text.UTF8Encoding]::new($false, $true))
    $protocols = @([regex]::Matches($identity, 'pub\(crate\)\s+const\s+PROTOCOL_VERSION\s*:\s*u8\s*=\s*([0-9]+)\s*;'))
    $abis = @([regex]::Matches($identity, 'pub\(crate\)\s+const\s+WORKER_ABI_VERSION\s*:\s*u16\s*=\s*([0-9]+)\s*;'))
    $desktopDefinitions = @([regex]::Matches($identity, 'pub\(crate\)\s+const\s+DESKTOP_BUILD_ID\s*:\s*&str\s*=\s*concat!\(\s*"local-transcriber@"\s*,\s*env!\("CARGO_PKG_VERSION"\)\s*,\s*"#"\s*,\s*env!\("SCRIBE_BUILD_REVISION"\)\s*\)\s*;'))
    $workerDefinitions = @([regex]::Matches($identity, 'pub\(crate\)\s+const\s+INFERENCE_WORKER_BUILD_ID\s*:\s*&str\s*=\s*concat!\(\s*"scribe-inference-worker@"\s*,\s*env!\("CARGO_PKG_VERSION"\)\s*,\s*"#"\s*,\s*env!\("SCRIBE_BUILD_REVISION"\)\s*\)\s*;'))
    if ($protocols.Count -ne 1 -or $abis.Count -ne 1 -or
        $desktopDefinitions.Count -ne 1 -or $workerDefinitions.Count -ne 1 -or
        [int]$protocols[0].Groups[1].Value -ne 5 -or [int]$abis[0].Groups[1].Value -ne 1) {
        throw 'Frozen CPU worker packaging found an unsupported worker identity definition.'
    }
    $version = $versions[0].Groups[1].Value
    $digests = [ordered]@{}
    foreach ($path in @(
        'Cargo.lock', 'rust-toolchain.toml', 'Cargo.toml', '.cargo/config.toml', 'src/worker_identity.rs', 'build.rs',
        'scripts/windows-cpu-worker-native-baseline.ps1',
        'scripts/new-windows-frozen-cpu-worker.ps1',
        'scripts/build-windows-release.ps1'
    )) {
        $relative = $path.Replace('\', '/')
        $digests[$relative] = Get-WindowsFrozenCpuWorkerFileSha256 (Join-Path $root $path)
    }
    $contractBytes = [System.Text.Encoding]::UTF8.GetBytes((@(
        'windows-frozen-cpu-worker-contract-v1',
        "Cargo.toml=$($digests['Cargo.toml'])",
        ".cargo/config.toml=$($digests['.cargo/config.toml'])",
        "build.rs=$($digests['build.rs'])",
        "src/worker_identity.rs=$($digests['src/worker_identity.rs'])",
        "scripts/windows-cpu-worker-native-baseline.ps1=$($digests['scripts/windows-cpu-worker-native-baseline.ps1'])",
        "scripts/new-windows-frozen-cpu-worker.ps1=$($digests['scripts/new-windows-frozen-cpu-worker.ps1'])",
        "scripts/build-windows-release.ps1=$($digests['scripts/build-windows-release.ps1'])"
    ) -join "`n"))
    return [pscustomobject]@{
        RepositoryRoot = $root
        SourceRevision = $revisionLines[0]
        AppVersion = $version
        TargetTriple = Get-WindowsFrozenCpuWorkerTargetTriple
        ProtocolVersion = [int]$protocols[0].Groups[1].Value
        WorkerAbiVersion = [int]$abis[0].Groups[1].Value
        DesktopBuildId = "local-transcriber@$version#$($revisionLines[0])"
        WorkerBuildId = "scribe-inference-worker@$version#$($revisionLines[0])"
        CargoLockSha256 = $digests['Cargo.lock']
        RustToolchainSha256 = $digests['rust-toolchain.toml']
        CargoManifestSha256 = $digests['Cargo.toml']
        WorkerIdentitySha256 = $digests['src/worker_identity.rs']
        BuildRsSha256 = $digests['build.rs']
        BuildContractSha256 = ConvertTo-WindowsFrozenCpuWorkerSha256 $contractBytes
    }
}

function Assert-WindowsFrozenCpuWorkerContextUnchanged([psobject]$ExpectedContext) {
    $actual = Get-WindowsFrozenCpuWorkerSourceContext $ExpectedContext.RepositoryRoot
    foreach ($property in @(
        'SourceRevision', 'AppVersion', 'TargetTriple', 'ProtocolVersion', 'WorkerAbiVersion',
        'DesktopBuildId', 'WorkerBuildId', 'CargoLockSha256', 'RustToolchainSha256',
        'CargoManifestSha256', 'WorkerIdentitySha256', 'BuildRsSha256', 'BuildContractSha256'
    )) {
        if ([string]$actual.$property -cne [string]$ExpectedContext.$property) {
            throw "Frozen CPU worker source context changed before publication: $property"
        }
    }
}

function Test-WindowsFrozenCpuWorkerSameSourceContext(
    [psobject]$DesktopContext,
    [psobject]$WorkerContext
) {
    foreach ($property in @(
        'SourceRevision', 'AppVersion', 'TargetTriple', 'ProtocolVersion', 'WorkerAbiVersion',
        'DesktopBuildId', 'WorkerBuildId', 'CargoLockSha256', 'RustToolchainSha256',
        'CargoManifestSha256', 'WorkerIdentitySha256', 'BuildRsSha256', 'BuildContractSha256'
    )) {
        if ([string]$DesktopContext.$property -cne [string]$WorkerContext.$property) {
            return $false
        }
    }
    return $true
}

function Assert-WindowsFrozenCpuWorkerCompatibleSourceContexts(
    [psobject]$DesktopContext,
    [psobject]$WorkerContext
) {
    foreach ($property in @('TargetTriple', 'ProtocolVersion', 'WorkerAbiVersion')) {
        if ([string]$DesktopContext.$property -cne [string]$WorkerContext.$property) {
            throw "Frozen CPU worker source contexts are incompatible: $property"
        }
    }
}

function Stop-WindowsFrozenCpuWorkerAdmissionProcess(
    [System.Diagnostics.Process]$Process,
    [string]$Description
) {
    if ($Process.HasExited) { return }
    try { $Process.Kill($true) }
    catch {
        try { $Process.Kill() }
        catch { throw "$Description could not be terminated after its bounded deadline." }
    }
    if (-not $Process.WaitForExit(5000)) {
        throw "$Description did not exit within the fixed termination deadline."
    }
}

function Invoke-WindowsFrozenCpuWorkerAdmissionProcess(
    [string]$Executable,
    [ValidateSet('--scribe-frozen-worker-admission', '--scribe-windows-gpu-auto-policy-identity')]
    [string]$Command = '--scribe-frozen-worker-admission'
) {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = Get-WindowsFrozenCpuWorkerNormalizedFullPath $Executable
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.ArgumentList.Add($Command)
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $started = $false
    try {
        if (-not $process.Start()) {
            throw 'Frozen CPU worker admission report process could not start.'
        }
        $started = $true
        $stdoutBuffer = [char[]]::new(4096)
        $stderrBuffer = [char[]]::new(4096)
        $stdout = [Text.StringBuilder]::new()
        $stderr = [Text.StringBuilder]::new()
        $stdoutTask = $process.StandardOutput.ReadAsync($stdoutBuffer, 0, $stdoutBuffer.Length)
        $stderrTask = $process.StandardError.ReadAsync($stderrBuffer, 0, $stderrBuffer.Length)
        $stdoutClosed = $false
        $stderrClosed = $false
        $clock = [Diagnostics.Stopwatch]::StartNew()
        $exitedAt = $null
        while ($true) {
            foreach ($stream in @(
                [pscustomobject]@{ Task = $stdoutTask; Buffer = $stdoutBuffer; Text = $stdout; Closed = $stdoutClosed; Name = 'stdout' },
                [pscustomobject]@{ Task = $stderrTask; Buffer = $stderrBuffer; Text = $stderr; Closed = $stderrClosed; Name = 'stderr' }
            )) {
                if ($stream.Closed -or -not $stream.Task.IsCompleted) { continue }
                if ($stream.Task.IsFaulted -or $stream.Task.IsCanceled) {
                    Stop-WindowsFrozenCpuWorkerAdmissionProcess $process 'Frozen CPU worker admission report process'
                    throw "Frozen CPU worker admission report $($stream.Name) capture failed."
                }
                $count = $stream.Task.GetAwaiter().GetResult()
                if ($count -eq 0) {
                    if ($stream.Name -ceq 'stdout') { $stdoutClosed = $true } else { $stderrClosed = $true }
                    continue
                }
                if ($stream.Text.Length -gt 65536 - $count) {
                    Stop-WindowsFrozenCpuWorkerAdmissionProcess $process 'Frozen CPU worker admission report process'
                    throw "Frozen CPU worker admission report $($stream.Name) exceeded the fixed 65536-character bound."
                }
                $null = $stream.Text.Append($stream.Buffer, 0, $count)
                if ($stream.Name -ceq 'stdout') {
                    $stdoutTask = $process.StandardOutput.ReadAsync($stdoutBuffer, 0, $stdoutBuffer.Length)
                }
                else {
                    $stderrTask = $process.StandardError.ReadAsync($stderrBuffer, 0, $stderrBuffer.Length)
                }
            }
            if ($process.HasExited -and $null -eq $exitedAt) {
                $exitedAt = $clock.ElapsedMilliseconds
            }
            if ($process.HasExited -and $stdoutClosed -and $stderrClosed) { break }
            if ($clock.ElapsedMilliseconds -ge 30000) {
                if (-not $process.HasExited) {
                    Stop-WindowsFrozenCpuWorkerAdmissionProcess $process 'Frozen CPU worker admission report process'
                    throw 'Frozen CPU worker admission report process timed out after the fixed 30000-millisecond deadline.'
                }
                throw 'Frozen CPU worker admission report output streams did not close within the fixed deadline.'
            }
            if ($process.HasExited -and ($clock.ElapsedMilliseconds - $exitedAt) -ge 5000) {
                throw 'Frozen CPU worker admission report output streams did not close within the fixed post-exit drain deadline.'
            }
            [Threading.Thread]::Sleep(10)
        }
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout.ToString()
            Stderr = $stderr.ToString()
        }
    }
    catch {
        $originalError = $_
        if ($started -and -not $process.HasExited) {
            try {
                Stop-WindowsFrozenCpuWorkerAdmissionProcess $process 'Frozen CPU worker admission report process'
            }
            catch {
                throw "Frozen CPU worker admission report process failed and parent termination could not be confirmed: $($originalError.Exception.Message)"
            }
        }
        throw $originalError
    }
    finally {
        $process.Dispose()
    }
}

function Assert-WindowsFrozenCpuWorkerCompiledAdmission(
    [string]$Executable,
    [int64]$ExpectedSize,
    [string]$ExpectedSha256,
    [psobject]$DesktopContext,
    [psobject]$FrozenCpuWorker
) {
    Assert-WindowsFrozenCpuWorkerDigest $ExpectedSha256 'Frozen desktop executable SHA-256'
    Assert-WindowsFrozenCpuWorkerCompatibleSourceContexts $DesktopContext $FrozenCpuWorker.Context
    $executablePath = Get-WindowsFrozenCpuWorkerNormalizedFullPath $Executable
    $stream = Open-WindowsFrozenCpuWorkerReadHandle $executablePath
    try {
        if ($stream.Length -ne $ExpectedSize -or
            (Get-WindowsFrozenCpuWorkerOpenStreamSha256 $stream) -cne $ExpectedSha256) {
            throw 'Frozen desktop executable changed before compiled admission verification.'
        }
        $result = Invoke-WindowsFrozenCpuWorkerAdmissionProcess $executablePath
    }
    finally {
        $stream.Dispose()
    }
    if ($result.ExitCode -ne 0) {
        throw "Frozen desktop compiled admission failed with exit code $($result.ExitCode): $($result.Stderr.Trim())"
    }
    if ([string]::IsNullOrWhiteSpace($result.Stdout)) {
        throw 'Frozen desktop compiled admission returned no report.'
    }
    try {
        $report = ConvertFrom-Json -InputObject $result.Stdout -Depth 5 -NoEnumerate
    }
    catch {
        throw "Frozen desktop compiled admission returned malformed JSON: $($_.Exception.Message)"
    }
    if ($report -is [array] -or $report -isnot [pscustomobject]) {
        throw 'Frozen desktop compiled admission report must be one JSON object.'
    }
    Assert-WindowsFrozenCpuWorkerExactProperties $report @(
        'schema_version', 'desktop_build_id', 'bundled_worker_sha256',
        'protocol_version', 'worker_abi_version', 'worker_origin_app_build',
        'worker_build_id', 'admission_kind'
    ) 'Frozen desktop compiled admission report'
    foreach ($field in @(
        'desktop_build_id', 'bundled_worker_sha256', 'worker_origin_app_build',
        'worker_build_id', 'admission_kind'
    )) {
        if ($report.$field -isnot [string]) {
            throw "Frozen desktop compiled admission report field '$field' must be a string."
        }
    }
    Assert-WindowsFrozenCpuWorkerDigest $report.bundled_worker_sha256 'Frozen desktop admission bundled CPU worker SHA-256'
    $expectedKind = if (Test-WindowsFrozenCpuWorkerSameSourceContext $DesktopContext $FrozenCpuWorker.Context) {
        'strict_legacy_same_source'
    }
    else {
        'compiled_foreign_approval'
    }
    if (($report.schema_version -isnot [int64] -and $report.schema_version -isnot [int32]) -or
        [int]$report.schema_version -ne 1 -or
        $report.desktop_build_id -cne $DesktopContext.DesktopBuildId -or
        $report.bundled_worker_sha256 -cne $FrozenCpuWorker.Record.worker_sha256 -or
        (Assert-WindowsFrozenCpuWorkerInt64 $report.protocol_version 'Frozen desktop admission protocol version') -ne [int64]$DesktopContext.ProtocolVersion -or
        (Assert-WindowsFrozenCpuWorkerInt64 $report.worker_abi_version 'Frozen desktop admission worker ABI version') -ne [int64]$DesktopContext.WorkerAbiVersion -or
        $report.worker_origin_app_build -cne $FrozenCpuWorker.Context.DesktopBuildId -or
        $report.worker_build_id -cne $FrozenCpuWorker.Context.WorkerBuildId -or
        $report.admission_kind -cne $expectedKind) {
        throw 'Frozen desktop compiled admission does not match the expected desktop and frozen worker identities.'
    }
    return $report
}

function New-WindowsFrozenCpuWorkerRecord([psobject]$Context, [int64]$WorkerSize, [string]$WorkerSha256) {
    Assert-WindowsFrozenCpuWorkerDigest $WorkerSha256 'Frozen CPU worker digest'
    if ($WorkerSize -le 0 -or $WorkerSize -gt (Get-WindowsFrozenCpuWorkerMaximumBytes)) {
        throw 'Frozen CPU worker size is outside the supported local freeze bound.'
    }
    return [ordered]@{
        schema_version = 1
        kind = 'windows-frozen-cpu-worker'
        local_only = $true
        release_approved = $false
        source_revision = $Context.SourceRevision
        app_version = $Context.AppVersion
        target_triple = $Context.TargetTriple
        protocol_version = $Context.ProtocolVersion
        worker_abi_version = $Context.WorkerAbiVersion
        desktop_build_id = $Context.DesktopBuildId
        worker_build_id = $Context.WorkerBuildId
        cargo_lock_sha256 = $Context.CargoLockSha256
        rust_toolchain_sha256 = $Context.RustToolchainSha256
        cargo_manifest_sha256 = $Context.CargoManifestSha256
        worker_identity_sha256 = $Context.WorkerIdentitySha256
        build_rs_sha256 = $Context.BuildRsSha256
        build_contract_sha256 = $Context.BuildContractSha256
        worker_relative_path = Get-WindowsFrozenCpuWorkerExecutableRelativePath
        worker_size_bytes = $WorkerSize
        worker_sha256 = $WorkerSha256
    }
}

function Get-WindowsFrozenCpuWorkerMarkerText([string]$RecordSha256, [psobject]$Record) {
    Assert-WindowsFrozenCpuWorkerDigest $RecordSha256 'Frozen CPU worker record digest'
    return (@(
        'WINDOWS FROZEN CPU WORKER - LOCAL ONLY',
        'This directory records local byte integrity only; it is not approved for release publication.',
        "record_sha256=$RecordSha256",
        "source_revision=$($Record.source_revision)",
        "target_triple=$($Record.target_triple)",
        "desktop_build_id=$($Record.desktop_build_id)",
        "worker_build_id=$($Record.worker_build_id)",
        "worker_sha256=$($Record.worker_sha256)",
        'local_only=true',
        'release_approved=false',
        ''
    ) -join "`r`n")
}

function Get-WindowsFrozenCpuWorkerBundleMarkerText([string]$RecordSha256, [psobject]$Record) {
    return (@(
        'WINDOWS FROZEN CPU WORKER - LOCAL ONLY',
        'This bundle consumed a locally frozen CPU worker and is intentionally rejected by the production package verifier.',
        "frozen_record_sha256=$RecordSha256",
        "source_revision=$($Record.source_revision)",
        "target_triple=$($Record.target_triple)",
        "desktop_build_id=$($Record.desktop_build_id)",
        "worker_build_id=$($Record.worker_build_id)",
        "worker_size_bytes=$($Record.worker_size_bytes)",
        "worker_sha256=$($Record.worker_sha256)",
        'local_only=true',
        'release_approved=false',
        ''
    ) -join "`r`n")
}

function Write-WindowsFrozenCpuWorkerAtomicUtf8File([string]$Path, [string]$Text) {
    $full = Get-WindowsFrozenCpuWorkerNormalizedFullPath $Path
    $parent = Split-Path -Parent $full
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $parent
    if (Test-Path -LiteralPath $full) {
        throw "Refusing to overwrite frozen CPU worker output: $full"
    }
    $temporary = Join-Path $parent (".$([System.IO.Path]::GetFileName($full)).tmp-$PID-$([guid]::NewGuid().ToString('N'))")
    try {
        [System.IO.File]::WriteAllText($temporary, $Text, [System.Text.UTF8Encoding]::new($false))
        [System.IO.File]::Move($temporary, $full)
    }
    finally {
        if (Test-Path -LiteralPath $temporary -PathType Leaf) {
            Remove-Item -LiteralPath $temporary -Force
        }
    }
}

function Assert-WindowsFrozenCpuWorkerDirectoryInventory([string]$Root) {
    $root = Get-WindowsFrozenCpuWorkerNormalizedFullPath $Root
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $root
    Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $root
    if (-not (Test-Path -LiteralPath $root -PathType Container)) {
        throw "Frozen CPU worker directory is missing: $root"
    }
    $rootItem = Get-Item -LiteralPath $root -Force
    if (($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Frozen CPU worker directory cannot be a reparse point: $root"
    }
    $expected = @(
        $(Get-WindowsFrozenCpuWorkerRecordFileName)
        $(Get-WindowsFrozenCpuWorkerMarkerFileName)
        $(Get-WindowsFrozenCpuWorkerExecutableRelativePath)
    )
    $items = @(Get-ChildItem -LiteralPath $root -Force)
    $names = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($item in $items) {
        if ($item.PSIsContainer -or ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Frozen CPU worker directory contains a non-regular item: $($item.FullName)"
        }
        Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $item.FullName
        if (-not $names.Add($item.Name)) {
            throw "Frozen CPU worker directory contains a case-insensitive filename collision: $($item.Name)"
        }
    }
    if ($items.Count -ne $expected.Count) {
        throw 'Frozen CPU worker directory must contain exactly its worker, record, and local-only marker.'
    }
    foreach ($name in $expected) {
        $matches = @($items | Where-Object { $_.Name -ceq $name })
        if ($matches.Count -ne 1) {
            throw "Frozen CPU worker directory is missing its exact required file name: $name"
        }
    }
}

function Assert-WindowsFrozenCpuWorkerRecordMatchesContext([psobject]$Record, [psobject]$Context) {
    Assert-WindowsFrozenCpuWorkerExactProperties $Record (Get-WindowsFrozenCpuWorkerExpectedRecordProperties) 'Frozen CPU worker record'
    if ($Record.schema_version -isnot [long] -or $Record.protocol_version -isnot [long] -or
        $Record.worker_abi_version -isnot [long] -or $Record.worker_size_bytes -isnot [long] -or
        $Record.local_only -isnot [bool] -or $Record.release_approved -isnot [bool]) {
        throw 'Frozen CPU worker record contains non-canonical scalar types.'
    }
    foreach ($property in @(
        'kind', 'source_revision', 'app_version', 'target_triple', 'desktop_build_id', 'worker_build_id',
        'cargo_lock_sha256', 'rust_toolchain_sha256', 'cargo_manifest_sha256', 'worker_identity_sha256',
        'build_rs_sha256', 'build_contract_sha256', 'worker_relative_path', 'worker_sha256'
    )) {
        if ($Record.$property -isnot [string]) {
            throw "Frozen CPU worker record $property must be a string."
        }
    }
    if ($Record.schema_version -ne 1 -or $Record.kind -cne 'windows-frozen-cpu-worker' -or
        $Record.local_only -ne $true -or $Record.release_approved -ne $false) {
        throw 'Frozen CPU worker record does not declare the only supported local-only schema.'
    }
    foreach ($property in @(
        'source_revision', 'app_version', 'target_triple', 'protocol_version', 'worker_abi_version',
        'desktop_build_id', 'worker_build_id', 'cargo_lock_sha256', 'rust_toolchain_sha256',
        'cargo_manifest_sha256', 'worker_identity_sha256', 'build_rs_sha256', 'build_contract_sha256'
    )) {
        $expectedProperty = switch ($property) {
            'source_revision' { 'SourceRevision' }
            'app_version' { 'AppVersion' }
            'target_triple' { 'TargetTriple' }
            'protocol_version' { 'ProtocolVersion' }
            'worker_abi_version' { 'WorkerAbiVersion' }
            'desktop_build_id' { 'DesktopBuildId' }
            'worker_build_id' { 'WorkerBuildId' }
            'cargo_lock_sha256' { 'CargoLockSha256' }
            'rust_toolchain_sha256' { 'RustToolchainSha256' }
            'cargo_manifest_sha256' { 'CargoManifestSha256' }
            'worker_identity_sha256' { 'WorkerIdentitySha256' }
            'build_rs_sha256' { 'BuildRsSha256' }
            'build_contract_sha256' { 'BuildContractSha256' }
        }
        if ([string]$Record.$property -cne [string]$Context.$expectedProperty) {
            throw "Frozen CPU worker record does not match current source context: $property"
        }
    }
    foreach ($property in @('cargo_lock_sha256', 'rust_toolchain_sha256', 'cargo_manifest_sha256', 'worker_identity_sha256', 'build_rs_sha256', 'build_contract_sha256', 'worker_sha256')) {
        Assert-WindowsFrozenCpuWorkerDigest ([string]$Record.$property) "Frozen CPU worker record $property"
    }
    if ($Record.worker_relative_path -cne (Get-WindowsFrozenCpuWorkerExecutableRelativePath)) {
        throw 'Frozen CPU worker record names an unsupported worker path.'
    }
    try {
        $workerSize = [int64]$Record.worker_size_bytes
    }
    catch {
        throw 'Frozen CPU worker record worker_size_bytes is invalid.'
    }
    if ($workerSize -le 0 -or $workerSize -gt (Get-WindowsFrozenCpuWorkerMaximumBytes)) {
        throw 'Frozen CPU worker record worker_size_bytes is outside the supported local freeze bound.'
    }
}

function Open-ValidatedWindowsFrozenCpuWorker([string]$RecordPath, [string]$RepositoryRoot) {
    $recordPath = Get-WindowsFrozenCpuWorkerNormalizedFullPath $RecordPath
    if ((Split-Path -Leaf $recordPath) -cne (Get-WindowsFrozenCpuWorkerRecordFileName)) {
        throw 'Frozen CPU worker record must use the exact supported record file name.'
    }
    $root = Split-Path -Parent $recordPath
    Assert-WindowsFrozenCpuWorkerDirectoryInventory $root
    $recordFile = Read-WindowsFrozenCpuWorkerBoundedUtf8File $recordPath
    try {
        $record = $recordFile.Text | ConvertFrom-Json -Depth 8
    }
    catch {
        throw "Frozen CPU worker record is not valid JSON: $($_.Exception.Message)"
    }
    $context = Get-WindowsFrozenCpuWorkerSourceContext $RepositoryRoot
    Assert-WindowsFrozenCpuWorkerRecordMatchesContext $record $context
    $canonicalRecord = (New-WindowsFrozenCpuWorkerRecord `
        -Context $context `
        -WorkerSize ([int64]$record.worker_size_bytes) `
        -WorkerSha256 ([string]$record.worker_sha256) | ConvertTo-Json -Depth 5)
    if ($recordFile.Text -cne $canonicalRecord) {
        throw 'Frozen CPU worker record is not the exact canonical local integrity encoding.'
    }
    $expectedMarker = Get-WindowsFrozenCpuWorkerMarkerText $recordFile.Sha256 $record
    $markerPath = Join-Path $root (Get-WindowsFrozenCpuWorkerMarkerFileName)
    $marker = Read-WindowsFrozenCpuWorkerBoundedUtf8File $markerPath 8192
    if ($marker.Text -cne $expectedMarker) {
        throw 'Frozen CPU worker local-only marker does not bind the exact record and source identity.'
    }
    $workerPath = Join-Path $root (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
    $workerStream = Open-WindowsFrozenCpuWorkerReadHandle $workerPath
    try {
        if ($workerStream.Length -gt (Get-WindowsFrozenCpuWorkerMaximumBytes)) {
            throw 'Frozen CPU worker bytes exceed the supported local freeze bound.'
        }
        if ($workerStream.Length -ne [int64]$record.worker_size_bytes) {
            throw 'Frozen CPU worker bytes do not match the recorded size.'
        }
        $workerHash = Get-WindowsFrozenCpuWorkerOpenStreamSha256 $workerStream
        if ($workerHash -cne [string]$record.worker_sha256) {
            throw 'Frozen CPU worker bytes do not match the recorded SHA-256.'
        }
        return [pscustomobject]@{
            Root = $root
            RecordPath = $recordPath
            RecordSha256 = $recordFile.Sha256
            Record = $record
            Context = $context
            WorkerPath = $workerPath
            WorkerStream = $workerStream
        }
    }
    catch {
        $workerStream.Dispose()
        throw
    }
}

function Copy-WindowsFrozenCpuWorkerOpenHandle([System.IO.FileStream]$Source, [string]$Destination) {
    Assert-WindowsFrozenCpuWorkerNoReparseAncestors $Destination
    if (Test-Path -LiteralPath $Destination) {
        throw "Refusing to overwrite staged frozen CPU worker: $Destination"
    }
    $Source.Position = 0
    $destinationStream = [System.IO.File]::Open($Destination, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    try {
        $Source.CopyTo($destinationStream)
    }
    finally {
        $destinationStream.Dispose()
        $Source.Position = 0
    }
}
