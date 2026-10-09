[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$testRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-windows-gpu-auto-policy-identity-$([guid]::NewGuid().ToString('N'))"
$fixtureRoot = Join-Path $testRoot 'fixture'
$script:Assertions = 0
$script:Utf8 = [Text.UTF8Encoding]::new($false, $true)
$script:GitOverrideVariableNames = @(
    'GIT_DIR',
    'GIT_WORK_TREE',
    'GIT_COMMON_DIR',
    'GIT_INDEX_FILE',
    'GIT_OBJECT_DIRECTORY',
    'GIT_ALTERNATE_OBJECT_DIRECTORIES',
    'GIT_NAMESPACE'
)
$savedGitOverrideEnvironment = @{}
foreach ($name in $script:GitOverrideVariableNames) {
    $environmentPath = "Env:$name"
    $exists = Test-Path -LiteralPath $environmentPath
    $savedGitOverrideEnvironment[$name] = [pscustomobject]@{
        Exists = $exists
        Value = if ($exists) { (Get-Item -LiteralPath $environmentPath).Value } else { $null }
    }
}

$fixtureGlobalNames = @(
    'WindowsGpuAutoPolicyIdentityFixtureCalls',
    'WindowsGpuAutoPolicyIdentityFixtureResponse',
    'WindowsGpuAutoPolicyIdentityFixtureMutationPath',
    'WindowsGpuAutoPolicyIdentityFixtureMutationBlocked'
)
$savedFixtureGlobals = @{}
foreach ($name in $fixtureGlobalNames) {
    $saved = Get-Variable -Name $name -Scope Global -ErrorAction SilentlyContinue
    $savedFixtureGlobals[$name] = [pscustomobject]@{
        Exists = $null -ne $saved
        Value = if ($null -ne $saved) { $saved.Value } else { $null }
    }
}
$savedGlobalAdmissionProcess = Get-Item -LiteralPath Function:global:Invoke-WindowsFrozenCpuWorkerAdmissionProcess -ErrorAction SilentlyContinue
$savedGlobalAdmissionProcessBody = if ($null -ne $savedGlobalAdmissionProcess) { $savedGlobalAdmissionProcess.ScriptBlock } else { $null }

function Assert-True([bool]$Condition, [string]$Message) {
    $script:Assertions++
    if (-not $Condition) { throw "TEST FAILED: $Message" }
}

function Assert-Equal($Actual, $Expected, [string]$Message) {
    $script:Assertions++
    if ($Actual -cne $Expected) { throw "TEST FAILED: $Message Expected '$Expected', got '$Actual'." }
}

function Restore-GitOverrideEnvironment([hashtable]$EnvironmentState) {
    foreach ($name in $script:GitOverrideVariableNames) {
        $state = $EnvironmentState[$name]
        if ($state.Exists) {
            [Environment]::SetEnvironmentVariable($name, $state.Value)
        }
        else {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        }
    }
}

function Assert-GitOverrideEnvironmentState([string]$Name, [psobject]$Expected, [string]$Context) {
    $environmentPath = "Env:$Name"
    $exists = Test-Path -LiteralPath $environmentPath
    Assert-Equal $exists ([bool]$Expected.Exists) "Git override environment variable $Name presence $Context"
    if ($Expected.Exists) {
        Assert-Equal (Get-Item -LiteralPath $environmentPath).Value $Expected.Value "Git override environment variable $Name value $Context"
    }
}

function Assert-Rejected([string]$Name, [scriptblock]$Action) {
    $script:Assertions++
    try {
        $result = @(& $Action)
        foreach ($item in $result) {
            if ($null -ne $item -and $null -ne $item.PSObject.Properties['ManifestStream'] -and
                $null -ne $item.ManifestStream) {
                $item.ManifestStream.Dispose()
            }
        }
    }
    catch {
        if ($_.Exception.Message.StartsWith('TEST FAILED:', [StringComparison]::Ordinal)) { throw }
        return
    }
    throw "TEST FAILED: $Name was accepted."
}

function Get-TestHash([byte[]]$Bytes) {
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Bytes)).ToLowerInvariant()
}

function Get-SourceArchiveFallbackHash([string]$Root) {
    $archive = [IO.MemoryStream]::new()
    try {
        foreach ($relativePath in @('Cargo.lock', 'build.rs', 'src/onnx_worker.rs', 'src/worker_contracts.rs')) {
            $path = Join-Path $Root ($relativePath -replace '/', '\\')
            $bytes = [IO.File]::ReadAllBytes($path)
            $archive.Write($bytes, 0, $bytes.Length)
        }
        return Get-TestHash $archive.ToArray()
    }
    finally {
        $archive.Dispose()
    }
}

function Copy-FixtureSourceFile([string]$RelativePath) {
    $source = Join-Path $repositoryRoot ($RelativePath -replace '/', '\')
    $destination = Join-Path $fixtureRoot ($RelativePath -replace '/', '\')
    [IO.Directory]::CreateDirectory((Split-Path -Parent $destination)) | Out-Null
    Copy-Item -LiteralPath $source -Destination $destination
}

function Remove-OwnedTestRoot([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $root = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
    Assert-True ($root.StartsWith($temp + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -and
        (Split-Path -Leaf $root) -cmatch '^scribe-windows-gpu-auto-policy-identity-[0-9a-f]{32}$') `
        'Refused GPU Auto policy identity fixture cleanup outside its exact temporary root.'
    $current = $root
    while ($true) {
        $item = Get-Item -LiteralPath $current -Force
        Assert-True (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Refused GPU Auto policy identity fixture cleanup through a reparse point.'
        if ([string]::Equals($current, $temp, [StringComparison]::OrdinalIgnoreCase)) { break }
        $parent = Split-Path -Parent $current
        Assert-True (-not [string]::IsNullOrWhiteSpace($parent) -and $parent -cne $current) 'Could not prove GPU Auto policy identity fixture cleanup ancestry.'
        $current = $parent
    }
    $ownedItems = @(Get-ChildItem -LiteralPath $root -Recurse -Force)
    foreach ($item in $ownedItems) {
        Assert-True (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Refused GPU Auto policy identity fixture cleanup containing a reparse point.'
        if (($item.Attributes -band [IO.FileAttributes]::ReadOnly) -ne 0) {
            $item.Attributes = $item.Attributes -band (-bnot [IO.FileAttributes]::ReadOnly)
        }
    }
    [IO.Directory]::Delete("\\?\$root", $true)
}

function Set-FixturePolicyProcessSeam([string]$IntegrityPath) {
    $source = Get-Content -LiteralPath $IntegrityPath -Raw
    $start = $source.IndexOf('function Invoke-WindowsFrozenCpuWorkerAdmissionProcess')
    $end = $source.IndexOf('function Assert-WindowsFrozenCpuWorkerCompiledAdmission', $start)
    if ($start -lt 0 -or $end -le $start) {
        throw 'Could not isolate the fixture-only bounded admission-process seam.'
    }
    $fixtureProcess = @'
function Invoke-WindowsFrozenCpuWorkerAdmissionProcess(
    [string]$Executable,
    [ValidateSet('--scribe-frozen-worker-admission', '--scribe-windows-gpu-auto-policy-identity')]
    [string]$Command = '--scribe-frozen-worker-admission'
) {
    if ($Command -cne '--scribe-windows-gpu-auto-policy-identity') {
        throw "Unexpected fixture admission command: $Command"
    }
    $global:WindowsGpuAutoPolicyIdentityFixtureCalls.Add([pscustomobject]@{
        Executable = $Executable
        Command = $Command
    })
    if (-not [string]::IsNullOrWhiteSpace([string]$global:WindowsGpuAutoPolicyIdentityFixtureMutationPath)) {
        try {
            [IO.File]::WriteAllBytes($global:WindowsGpuAutoPolicyIdentityFixtureMutationPath, [byte[]](0x6d))
            $global:WindowsGpuAutoPolicyIdentityFixtureMutationBlocked = $false
        }
        catch {
            $global:WindowsGpuAutoPolicyIdentityFixtureMutationBlocked = $true
        }
    }
    if ($null -eq $global:WindowsGpuAutoPolicyIdentityFixtureResponse) {
        throw 'Fixture GPU Auto policy identity response was not configured.'
    }
    return $global:WindowsGpuAutoPolicyIdentityFixtureResponse
}

'@
    [IO.File]::WriteAllText($IntegrityPath, $source.Substring(0, $start) + $fixtureProcess + $source.Substring($end), [Text.UTF8Encoding]::new($false))
}

function Set-PolicyIdentityResponse([psobject]$PolicyIdentity, [string]$DesktopBuildId, [string]$Mutation = '') {
    $report = [ordered]@{
        schema_version = [int64]1
        desktop_build_id = $DesktopBuildId
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
    $response = [pscustomobject]@{
        ExitCode = 0
        Stdout = ''
        Stderr = ''
    }
    switch ($Mutation) {
        '' { }
        'schema' { $report.schema_version = [int64]2 }
        'policy-schema' { $report.policy_schema_version = [int64]3 }
        'policy-version' { $report.policy_version = [int64]4 }
        'target-os' { $report.target_os = 'linux' }
        'target-arch' { $report.target_arch = 'aarch64' }
        'mode' { $report.mode = 'default_allow' }
        'hash' { $report.embedded_manifest_sha256 = '0' * 64 }
        'runtime-hash' { $report.runtime_manifest_sha256 = '0' * 64 }
        'count' { $report.entry_count = [int64]($report.entry_count + 1) }
        'build' { $report.desktop_build_id = 'local-transcriber@fixture#wrong-build' }
        'type' { $report.entry_count = '0' }
        'string-type' { $report.mode = [int64]1 }
        'extra' { $report.unexpected = 'field' }
        'missing' { $null = $report.Remove('mode') }
        'uppercase' {
            $schema = $report.schema_version
            $null = $report.Remove('schema_version')
            $report.Schema_Version = $schema
        }
        'case-collision' { $report.Schema_Version = [int64]1 }
        'nonzero' {
            $response.ExitCode = 17
            $response.Stderr = 'fixture exit failure'
        }
        'stderr' { $response.Stderr = 'fixture diagnostics are forbidden' }
        'empty' { }
        'malformed' { }
        'array' { }
        'duplicate' { }
        'overflow' { }
        default { throw "Unknown policy identity report mutation: $Mutation" }
    }
    $json = $report | ConvertTo-Json -Compress -Depth 4
    switch ($Mutation) {
        'empty' { $response.Stdout = '' }
        'malformed' { $response.Stdout = '{not-json' }
        'array' { $response.Stdout = "[$json]" }
        'duplicate' { $response.Stdout = $json.Replace('{"schema_version":1,', '{"schema_version":1,"schema_version":1,') }
        'overflow' { $response.Stdout = [string]::new([char]120, 65537) }
        default { $response.Stdout = $json }
    }
    $global:WindowsGpuAutoPolicyIdentityFixtureResponse = $response
}

function Assert-PolicyIdentityRejected(
    [string]$Name,
    [psobject]$PolicyIdentity,
    [string]$Executable,
    [int64]$ExpectedSize,
    [string]$ExpectedSha256,
    [string]$ExpectedDesktopBuildId,
    [string]$Mutation
) {
    Set-PolicyIdentityResponse $PolicyIdentity $ExpectedDesktopBuildId $Mutation
    $global:WindowsGpuAutoPolicyIdentityFixtureCalls.Clear()
    Assert-Rejected $Name {
        Assert-WindowsGpuAutoPolicyCompiledIdentity `
            -Executable $Executable `
            -ExpectedSize $ExpectedSize `
            -ExpectedSha256 $ExpectedSha256 `
            -ExpectedDesktopBuildId $ExpectedDesktopBuildId `
            -PolicyIdentity $PolicyIdentity
    }
}

$primaryFailure = $null
try {
    [IO.Directory]::CreateDirectory($fixtureRoot) | Out-Null
    foreach ($relativePath in @(
        'Cargo.toml', 'Cargo.lock', 'build.rs', 'src/onnx_worker.rs', 'src/worker_contracts.rs',
        'scripts/windows-frozen-cpu-worker-integrity.ps1',
        'scripts/windows-gpu-auto-policy-identity.ps1',
        'scripts/report-windows-gpu-auto-qualification.ps1',
        'runtime-manifests/gpu-auto-qualification-windows-x64.json'
    )) {
        Copy-FixtureSourceFile $relativePath
    }
    Set-FixturePolicyProcessSeam (Join-Path $fixtureRoot 'scripts\windows-frozen-cpu-worker-integrity.ps1')
    . (Join-Path $fixtureRoot 'scripts\windows-frozen-cpu-worker-integrity.ps1')
    . (Join-Path $fixtureRoot 'scripts\windows-gpu-auto-policy-identity.ps1')

    $global:WindowsGpuAutoPolicyIdentityFixtureCalls = [Collections.Generic.List[object]]::new()
    $global:WindowsGpuAutoPolicyIdentityFixtureResponse = $null
    $global:WindowsGpuAutoPolicyIdentityFixtureMutationPath = $null
    $global:WindowsGpuAutoPolicyIdentityFixtureMutationBlocked = $false

    $manifestPath = Join-Path $fixtureRoot 'runtime-manifests\gpu-auto-qualification-windows-x64.json'
    $originalManifestBytes = [IO.File]::ReadAllBytes($manifestPath)
    $originalManifestText = $script:Utf8.GetString($originalManifestBytes)
    Assert-True $originalManifestText.EndsWith("`n", [StringComparison]::Ordinal) 'Fixture manifest must preserve the production trailing LF.'

    $identity = Open-WindowsGpuAutoPolicyIdentity $fixtureRoot
    try {
        Assert-Equal $identity.PolicySchemaVersion ([int64]2) 'Policy schema version'
        Assert-Equal $identity.PolicyVersion ([int64]3) 'Compiled policy version'
        Assert-Equal $identity.TargetOs 'windows' 'Policy target OS'
        Assert-Equal $identity.TargetArch 'x86_64' 'Policy target architecture'
        Assert-Equal $identity.Mode 'default_deny' 'Policy mode'
        Assert-Equal $identity.EntryCount ([int64]0) 'Default-deny entry count'
        Assert-Equal $identity.EmbeddedManifestSizeBytes ([int64]$originalManifestBytes.Length) 'Embedded raw manifest size'
        Assert-Equal $identity.EmbeddedManifestSha256 (Get-TestHash $originalManifestBytes) 'Embedded raw manifest fingerprint'
        Assert-Equal $identity.RuntimeManifestSha256 (Get-TestHash $script:Utf8.GetBytes($originalManifestText.Substring(0, $originalManifestText.Length - 1))) 'Runtime canonical manifest fingerprint'
        Assert-WindowsGpuAutoPolicyIdentitySource $identity

        Assert-Rejected 'held manifest write' {
            $writer = [IO.File]::Open($manifestPath, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
            try { $null = $writer.Length }
            finally { $writer.Dispose() }
        }
    }
    finally { $identity.ManifestStream.Dispose() }
    $releasedManifest = [IO.File]::Open($manifestPath, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    $releasedManifest.Dispose()

    $withoutLf = $originalManifestText.Substring(0, $originalManifestText.Length - 1)
    [IO.File]::WriteAllBytes($manifestPath, $script:Utf8.GetBytes($withoutLf))
    $withoutLfIdentity = Open-WindowsGpuAutoPolicyIdentity $fixtureRoot
    try {
        Assert-Equal $withoutLfIdentity.EmbeddedManifestSizeBytes ([int64]($originalManifestBytes.Length - 1)) 'No-LF raw manifest size'
        Assert-True ($withoutLfIdentity.EmbeddedManifestSha256 -cne (Get-TestHash $originalManifestBytes)) 'No-LF raw manifest fingerprint must differ from LF source bytes.'
        Assert-Equal $withoutLfIdentity.RuntimeManifestSha256 (Get-TestHash $script:Utf8.GetBytes($withoutLf)) 'No-LF canonical runtime fingerprint'
    }
    finally {
        $withoutLfIdentity.ManifestStream.Dispose()
        [IO.File]::WriteAllBytes($manifestPath, $originalManifestBytes)
    }

    $bomBytes = [byte[]]::new($originalManifestBytes.Length + 3)
    [Array]::Copy([byte[]](0xef, 0xbb, 0xbf), 0, $bomBytes, 0, 3)
    [Array]::Copy($originalManifestBytes, 0, $bomBytes, 3, $originalManifestBytes.Length)
    $invalidManifestCases = @(
        [pscustomobject]@{ Name = 'CRLF manifest'; Bytes = $script:Utf8.GetBytes($originalManifestText.Replace("`n", "`r`n")) },
        [pscustomobject]@{ Name = 'BOM manifest'; Bytes = $bomBytes },
        [pscustomobject]@{ Name = 'invalid UTF-8 manifest'; Bytes = [byte[]](0x7b, 0xc3, 0x28, 0x7d) },
        [pscustomobject]@{ Name = 'oversized manifest'; Bytes = [byte[]]::new(512KB + 1) },
        [pscustomobject]@{ Name = 'noncanonical manifest'; Bytes = $script:Utf8.GetBytes($withoutLf + ' ') }
    )
    foreach ($case in $invalidManifestCases) {
        [IO.File]::WriteAllBytes($manifestPath, [byte[]]$case.Bytes)
        try {
            Assert-Rejected $case.Name { Open-WindowsGpuAutoPolicyIdentity $fixtureRoot }
        }
        finally { [IO.File]::WriteAllBytes($manifestPath, $originalManifestBytes) }
    }

    $sourceIdentity = Open-WindowsGpuAutoPolicyIdentity $fixtureRoot
    try {
        $sourceIdentity.EmbeddedManifestSha256 = '0' * 64
        Assert-Rejected 'manifest identity-bound digest drift' { Assert-WindowsGpuAutoPolicyIdentitySource $sourceIdentity }
    }
    finally { $sourceIdentity.ManifestStream.Dispose() }

    $sourceIdentity = Open-WindowsGpuAutoPolicyIdentity $fixtureRoot
    try {
        $changedManifestPath = Join-Path $testRoot 'changed-policy-manifest.json'
        [IO.File]::WriteAllBytes($changedManifestPath, $script:Utf8.GetBytes($withoutLf))
        $sourceIdentity.ManifestPath = $changedManifestPath
        Assert-Rejected 'manifest identity-bound path drift' { Assert-WindowsGpuAutoPolicyIdentitySource $sourceIdentity }
    }
    finally { $sourceIdentity.ManifestStream.Dispose() }

    $desktopBuildId = Get-WindowsGpuAutoPolicyDesktopBuildId $fixtureRoot 'fixture-policy-identity-build'
    Assert-Equal $desktopBuildId 'local-transcriber@0.1.0#fixture-policy-identity-build' 'Build-ID override binding'
    $fallbackBuildId = Get-WindowsGpuAutoPolicyDesktopBuildId $fixtureRoot $null
    $expectedSourceArchiveHash = Get-SourceArchiveFallbackHash $fixtureRoot
    Assert-Equal $fallbackBuildId "local-transcriber@0.1.0#source-$expectedSourceArchiveHash" 'Source-archive fallback build identity'

    $missingSourcePath = Join-Path $fixtureRoot 'src\worker_contracts.rs'
    $missingSourceBackupPath = "$missingSourcePath.fixture-missing"
    [IO.File]::Move($missingSourcePath, $missingSourceBackupPath)
    try {
        Assert-Rejected 'source-archive fallback with missing required source file' {
            Get-WindowsGpuAutoPolicyDesktopBuildId $fixtureRoot $null
        }
    }
    finally {
        [IO.File]::Move($missingSourceBackupPath, $missingSourcePath)
    }

    $fixtureGitDirectory = Join-Path $fixtureRoot '.git'
    [IO.Directory]::CreateDirectory($fixtureGitDirectory) | Out-Null
    $fixtureGitFunctionPath = 'Function:script:Invoke-WindowsFrozenCpuWorkerGit'
    $savedFixtureGitFunction = Get-Item -LiteralPath $fixtureGitFunctionPath -ErrorAction SilentlyContinue
    $savedFixtureGitFunctionBody = if ($null -ne $savedFixtureGitFunction) { $savedFixtureGitFunction.ScriptBlock } else { $null }
    try {
        $script:FixtureGitCalls = [Collections.Generic.List[object]]::new()
        $script:FixtureGitMode = 'success'
        Set-Item -LiteralPath $fixtureGitFunctionPath -Value {
            param(
                [string]$RepositoryRoot,
                [string[]]$Arguments
            )
            $overridesCleared = $true
            foreach ($name in $script:GitOverrideVariableNames) {
                if ($null -ne [Environment]::GetEnvironmentVariable($name)) {
                    $overridesCleared = $false
                }
            }
            $script:FixtureGitCalls.Add([pscustomobject]@{
                RepositoryRoot = $RepositoryRoot
                Arguments = @($Arguments)
                OverridesCleared = $overridesCleared
            })
            if (-not $overridesCleared) {
                throw 'Fixture Git seam observed an inherited Git override environment variable.'
            }
            if ($script:FixtureGitMode -ceq 'failure') {
                throw 'Fixture Git seam failure.'
            }
            return [string]::new([char]97, 40)
        }

        $fixtureGitOverrides = @{}
        foreach ($name in $script:GitOverrideVariableNames) {
            if ($name -ceq 'GIT_DIR') {
                $fixtureGitOverrides[$name] = [pscustomobject]@{ Exists = $false; Value = $null }
                Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
            }
            elseif ($name -ceq 'GIT_WORK_TREE') {
                $fixtureGitOverrides[$name] = [pscustomobject]@{ Exists = $true; Value = '' }
                [Environment]::SetEnvironmentVariable($name, '')
            }
            else {
                $fixtureGitOverrides[$name] = [pscustomobject]@{ Exists = $true; Value = "fixture-git-override-$name" }
                [Environment]::SetEnvironmentVariable($name, $fixtureGitOverrides[$name].Value)
            }
        }

        $gitBuildId = Get-WindowsGpuAutoPolicyDesktopBuildId $fixtureRoot $null
        Assert-Equal $gitBuildId "local-transcriber@0.1.0#$([string]::new([char]97, 40))" 'Git build identity'
        Assert-Equal $script:FixtureGitCalls.Count 1 'Git build identity invocation count'
        Assert-Equal $script:FixtureGitCalls[0].RepositoryRoot $fixtureRoot 'Git build identity repository root'
        Assert-Equal ([string]::Join('|', [string[]]$script:FixtureGitCalls[0].Arguments)) 'rev-parse|--verify|HEAD' 'Git build identity arguments'
        Assert-True $script:FixtureGitCalls[0].OverridesCleared 'Git build identity must clear Git override environment variables before invoking Git.'
        foreach ($name in $script:GitOverrideVariableNames) {
            Assert-GitOverrideEnvironmentState $name $fixtureGitOverrides[$name] 'after successful Git invocation'
        }

        $script:FixtureGitMode = 'failure'
        $script:FixtureGitCalls.Clear()
        Assert-Rejected 'Git build identity failure' {
            Get-WindowsGpuAutoPolicyDesktopBuildId $fixtureRoot $null
        }
        Assert-Equal $script:FixtureGitCalls.Count 1 'Failed Git build identity invocation count'
        Assert-True $script:FixtureGitCalls[0].OverridesCleared 'Failed Git build identity must clear Git override environment variables before invoking Git.'
        foreach ($name in $script:GitOverrideVariableNames) {
            Assert-GitOverrideEnvironmentState $name $fixtureGitOverrides[$name] 'after failed Git invocation'
        }
    }
    finally {
        if ($null -ne $savedFixtureGitFunction) {
            Set-Item -LiteralPath $fixtureGitFunctionPath -Value $savedFixtureGitFunctionBody
        }
        else {
            Remove-Item -LiteralPath $fixtureGitFunctionPath -ErrorAction SilentlyContinue
        }
        if (Test-Path -LiteralPath $fixtureGitDirectory) {
            [IO.Directory]::Delete($fixtureGitDirectory, $false)
        }
    }

    $executablePath = Join-Path $testRoot 'local-transcriber.exe'
    $executableBytes = [byte[]](0x4d, 0x5a, 0x90, 0x00, 0x66)
    [IO.File]::WriteAllBytes($executablePath, $executableBytes)
    $executableSize = [int64]$executableBytes.Length
    $executableHash = Get-TestHash $executableBytes
    $global:WindowsGpuAutoPolicyIdentityFixtureCalls.Clear()
    Assert-Rejected 'unsupported policy admission command' {
        Invoke-WindowsFrozenCpuWorkerAdmissionProcess -Executable $executablePath -Command '--unsupported-fixture-command'
    }
    Assert-Equal $global:WindowsGpuAutoPolicyIdentityFixtureCalls.Count 0 'Unsupported policy admission command reached the fixture process seam.'
    $identity = Open-WindowsGpuAutoPolicyIdentity $fixtureRoot
    try {
        Set-PolicyIdentityResponse $identity $desktopBuildId
        $global:WindowsGpuAutoPolicyIdentityFixtureCalls.Clear()
        $report = Assert-WindowsGpuAutoPolicyCompiledIdentity `
            -Executable $executablePath `
            -ExpectedSize $executableSize `
            -ExpectedSha256 $executableHash `
            -ExpectedDesktopBuildId $desktopBuildId `
            -PolicyIdentity $identity
        Assert-Equal $global:WindowsGpuAutoPolicyIdentityFixtureCalls.Count 1 'Policy identity report invocation count'
        Assert-Equal $global:WindowsGpuAutoPolicyIdentityFixtureCalls[0].Command '--scribe-windows-gpu-auto-policy-identity' 'Policy identity report command'
        Assert-Equal $report.runtime_manifest_sha256 $identity.RuntimeManifestSha256 'Accepted policy identity report binding'

        foreach ($mutation in @(
            'schema', 'policy-schema', 'policy-version', 'target-os', 'target-arch', 'mode',
            'hash', 'runtime-hash', 'count', 'build', 'type', 'string-type', 'extra', 'missing', 'uppercase', 'case-collision', 'duplicate',
            'array', 'empty', 'malformed', 'nonzero', 'stderr', 'overflow'
        )) {
            Assert-PolicyIdentityRejected "policy identity report $mutation" $identity $executablePath $executableSize $executableHash $desktopBuildId $mutation
        }

        $global:WindowsGpuAutoPolicyIdentityFixtureMutationPath = $executablePath
        $global:WindowsGpuAutoPolicyIdentityFixtureMutationBlocked = $false
        Set-PolicyIdentityResponse $identity $desktopBuildId
        $null = Assert-WindowsGpuAutoPolicyCompiledIdentity `
            -Executable $executablePath `
            -ExpectedSize $executableSize `
            -ExpectedSha256 $executableHash `
            -ExpectedDesktopBuildId $desktopBuildId `
            -PolicyIdentity $identity
        Assert-True $global:WindowsGpuAutoPolicyIdentityFixtureMutationBlocked 'Held desktop executable did not block policy-report write mutation.'
        $global:WindowsGpuAutoPolicyIdentityFixtureMutationPath = $null

        [IO.File]::WriteAllBytes($executablePath, [byte[]](0x4d, 0x5a, 0x90, 0x00, 0x67))
        Set-PolicyIdentityResponse $identity $desktopBuildId
        $global:WindowsGpuAutoPolicyIdentityFixtureCalls.Clear()
        Assert-Rejected 'desktop executable changed before policy identity verification' {
            Assert-WindowsGpuAutoPolicyCompiledIdentity `
                -Executable $executablePath `
                -ExpectedSize $executableSize `
                -ExpectedSha256 $executableHash `
                -ExpectedDesktopBuildId $desktopBuildId `
                -PolicyIdentity $identity
        }
        Assert-Equal $global:WindowsGpuAutoPolicyIdentityFixtureCalls.Count 0 'Changed desktop executable reached the policy report process.'
        [IO.File]::WriteAllBytes($executablePath, $executableBytes)
    }
    finally {
        $global:WindowsGpuAutoPolicyIdentityFixtureMutationPath = $null
        $identity.ManifestStream.Dispose()
    }
    $releasedExecutable = [IO.File]::Open($executablePath, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    $releasedExecutable.Dispose()
}
catch {
    $primaryFailure = $_
    throw
}
finally {
    try {
        Restore-GitOverrideEnvironment $savedGitOverrideEnvironment
        foreach ($name in $fixtureGlobalNames) {
            $saved = $savedFixtureGlobals[$name]
            if ($saved.Exists) {
                Set-Variable -Name $name -Scope Global -Value $saved.Value -Force
            }
            else {
                Remove-Variable -Name $name -Scope Global -Force -ErrorAction SilentlyContinue
            }
        }
        if ($null -ne $savedGlobalAdmissionProcess) {
            Set-Item -LiteralPath Function:global:Invoke-WindowsFrozenCpuWorkerAdmissionProcess -Value $savedGlobalAdmissionProcessBody
        }
        else {
            Remove-Item -LiteralPath Function:global:Invoke-WindowsFrozenCpuWorkerAdmissionProcess -ErrorAction SilentlyContinue
        }
        Remove-OwnedTestRoot $testRoot
    }
    catch {
        if ($null -ne $primaryFailure) {
            Write-Warning "GPU Auto policy identity fixture cleanup also failed after the primary test error: $($_.Exception.Message)"
        }
        else {
            throw
        }
    }
}

Write-Output "Windows GPU Auto compiled policy identity tests passed ($script:Assertions assertions)."
