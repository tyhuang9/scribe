$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-pe-imports.ps1')

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-windows-frozen-cpu-worker-test-$([guid]::NewGuid().ToString('N'))"
$fixtureRoot = Join-Path $testRoot 'fixture'
$fixtureTarget = Join-Path $testRoot 'cargo-target'
$modelSource = Join-Path $testRoot 'fixture-model.gguf'
$producerOutput = Join-Path $testRoot 'frozen-worker'
$installerAllowlist = Join-Path $testRoot 'worker-pack-allowlist.iss'
$script:FocusedFrozenCpuWorkerAssertions = 0
$global:WindowsFrozenCpuWorkerTestBaselineTargetRoots = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
$policyFixtureGlobalNames = @(
    'WindowsFrozenCpuWorkerTestPolicyCalls',
    'WindowsFrozenCpuWorkerTestPolicyResponse',
    'WindowsFrozenCpuWorkerTestPolicyResponses'
)
$savedPolicyFixtureGlobals = @{}
foreach ($name in $policyFixtureGlobalNames) {
    $saved = Get-Variable -Name $name -Scope Global -ErrorAction SilentlyContinue
    $savedPolicyFixtureGlobals[$name] = [pscustomobject]@{
        Exists = $null -ne $saved
        Value = if ($null -ne $saved) { $saved.Value } else { $null }
    }
}

function Invoke-ExpectedFailure([scriptblock]$Action, [string]$ExpectedText) {
    $script:FocusedFrozenCpuWorkerAssertions++
    try {
        $unexpectedOutput = @(& $Action)
        foreach ($item in $unexpectedOutput) {
            if ($null -ne $item -and $null -ne $item.PSObject.Properties['WorkerStream'] -and
                $null -ne $item.WorkerStream) {
                $item.WorkerStream.Dispose()
            }
        }
    }
    catch {
        if (-not $_.Exception.Message.Contains($ExpectedText)) {
            throw "Expected failure containing '$ExpectedText', got: $($_.Exception.Message)"
        }
        return
    }
    throw "Expected failure containing '$ExpectedText', but the action succeeded."
}

function Assert-Equal([object]$Actual, [object]$Expected, [string]$Description) {
    $script:FocusedFrozenCpuWorkerAssertions++
    if ($Actual -cne $Expected) {
        throw "$Description expected '$Expected', got '$Actual'."
    }
}

function Assert-True([bool]$Value, [string]$Description) {
    $script:FocusedFrozenCpuWorkerAssertions++
    if (-not $Value) {
        throw $Description
    }
}

function Copy-FixtureSourceFile([string]$RelativePath) {
    $source = Join-Path $repositoryRoot ($RelativePath -replace '/', '\')
    $destination = Join-Path $fixtureRoot ($RelativePath -replace '/', '\')
    $parent = Split-Path -Parent $destination
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
    Copy-Item -LiteralPath $source -Destination $destination
}

function Set-TestUInt16([byte[]]$Bytes, [int]$Offset, [uint16]$Value) {
    [System.BitConverter]::GetBytes($Value).CopyTo($Bytes, $Offset)
}

function Set-TestUInt32([byte[]]$Bytes, [int]$Offset, [uint32]$Value) {
    [System.BitConverter]::GetBytes($Value).CopyTo($Bytes, $Offset)
}

function New-TestReviewedPe([string]$Path, [uint16]$Subsystem) {
    $bytes = [byte[]]::new(1024)
    $bytes[0] = 0x4D
    $bytes[1] = 0x5A
    Set-TestUInt32 $bytes 0x3C 0x80
    Set-TestUInt32 $bytes 0x80 0x00004550
    Set-TestUInt16 $bytes 0x84 0x8664
    Set-TestUInt16 $bytes 0x86 1
    Set-TestUInt16 $bytes 0x94 0x00F0
    Set-TestUInt16 $bytes 0x98 0x020B
    Set-TestUInt16 $bytes 0xDC $Subsystem
    Set-TestUInt32 $bytes 0xD4 0x200
    Set-TestUInt32 $bytes 0x104 16
    Set-TestUInt32 $bytes 0x110 0x1000
    Set-TestUInt32 $bytes 0x114 40
    $sectionName = [System.Text.Encoding]::ASCII.GetBytes('.rdata')
    [System.Array]::Copy($sectionName, 0, $bytes, 0x188, $sectionName.Length)
    Set-TestUInt32 $bytes 0x190 0x200
    Set-TestUInt32 $bytes 0x194 0x1000
    Set-TestUInt32 $bytes 0x198 0x200
    Set-TestUInt32 $bytes 0x19C 0x200
    Set-TestUInt32 $bytes 0x20C 0x1040
    $dllName = [System.Text.Encoding]::ASCII.GetBytes('kernel32.dll')
    [System.Array]::Copy($dllName, 0, $bytes, 0x240, $dllName.Length)
    $bytes[0x240 + $dllName.Length] = 0
    $parent = Split-Path -Parent $Path
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
    [System.IO.File]::WriteAllBytes($Path, $bytes)
}

function Reset-TestCalls {
    $global:WindowsFrozenCpuWorkerTestCargoCalls = [System.Collections.Generic.List[object]]::new()
    $global:WindowsFrozenCpuWorkerTestNativeCalls = [System.Collections.Generic.List[object]]::new()
    if (Test-Path -LiteralPath 'Variable:global:WindowsFrozenCpuWorkerTestAdmissionCalls') {
        $global:WindowsFrozenCpuWorkerTestAdmissionCalls.Clear()
    }
    if (Test-Path -LiteralPath 'Variable:global:WindowsFrozenCpuWorkerTestPolicyCalls') {
        $global:WindowsFrozenCpuWorkerTestPolicyCalls.Clear()
    }
    if (Test-Path -LiteralPath 'Variable:global:WindowsFrozenCpuWorkerTestPolicyResponses') {
        $global:WindowsFrozenCpuWorkerTestPolicyResponses.Clear()
    }
}

function Assert-DesktopCargoArguments([psobject]$Call, [string]$Features, [string]$Description) {
    $expected = @(
        'build', '--locked', '--offline', '--release', '--bin', 'local-transcriber',
        '--features', $Features, '--target', 'x86_64-pc-windows-msvc',
        '--manifest-path', (Join-Path $fixtureRoot 'Cargo.toml')
    )
    # Compare the complete argv, not a feature substring: additional feature
    # flags or worker/provider features must not slip into the desktop build.
    Assert-Equal (ConvertTo-Json -InputObject @($Call.Arguments) -Compress) `
        (ConvertTo-Json -InputObject $expected -Compress) $Description
}

function Assert-WorkerCargoArguments([psobject]$Call, [string]$Description) {
    $expected = @(
        'build', '--locked', '--offline', '--release', '--bin', 'scribe-inference-worker',
        '--features', 'inference-worker', '--target', 'x86_64-pc-windows-msvc',
        '--manifest-path', (Join-Path $fixtureRoot 'Cargo.toml')
    )
    Assert-Equal (ConvertTo-Json -InputObject @($Call.Arguments) -Compress) `
        (ConvertTo-Json -InputObject $expected -Compress) $Description
}

function Write-TestCpuWorkerBaselineEvidence(
    [string]$TargetRoot,
    [string]$Flags = "C_FLAGS = /O2`nCXX_FLAGS = /O2"
) {
    $nativeEvidenceRoot = Join-Path $TargetRoot 'x86_64-pc-windows-msvc\release\build\transcribe-cpp-sys-a1b2c3d4\out\build'
    $flagsPath = Join-Path $nativeEvidenceRoot 'ggml\src\CMakeFiles\ggml-cpu.dir\flags.make'
    New-Item -ItemType Directory -Path (Split-Path -Parent $flagsPath) -Force | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $nativeEvidenceRoot 'CMakeCache.txt'), @'
TRANSCRIBE_X86_CONSERVATIVE:BOOL=ON
TRANSCRIBE_GGML_BACKEND_DL:BOOL=OFF
GGML_NATIVE:BOOL=OFF
GGML_BACKEND_DL:BOOL=OFF
GGML_OPENMP:BOOL=OFF
GGML_CPU_ALL_VARIANTS:BOOL=OFF
GGML_SSE42:BOOL=OFF
GGML_AVX:BOOL=OFF
GGML_AVX_VNNI:BOOL=OFF
GGML_AVX2:BOOL=OFF
GGML_BMI2:BOOL=OFF
GGML_FMA:BOOL=OFF
GGML_F16C:BOOL=OFF
GGML_AVX512:BOOL=OFF
GGML_AVX512_VBMI:BOOL=OFF
GGML_AVX512_VNNI:BOOL=OFF
GGML_AVX512_BF16:BOOL=OFF
CMAKE_C_FLAGS:STRING=
CMAKE_CXX_FLAGS:STRING=
CMAKE_C_FLAGS_RELEASE:STRING=/O2 /Ob2 /DNDEBUG
CMAKE_CXX_FLAGS_RELEASE:STRING=/O2 /Ob2 /DNDEBUG
CMAKE_GENERATOR:INTERNAL=NMake Makefiles
'@, [System.Text.UTF8Encoding]::new($false))
    [System.IO.File]::WriteAllText($flagsPath, $Flags, [System.Text.UTF8Encoding]::new($false))
}

function Write-CanonicalFrozenRecord([string]$Root, [psobject]$Context, [int64]$Size, [string]$Sha256) {
    $record = New-WindowsFrozenCpuWorkerRecord $Context $Size $Sha256
    $recordPath = Join-Path $Root (Get-WindowsFrozenCpuWorkerRecordFileName)
    [System.IO.File]::WriteAllText($recordPath, ($record | ConvertTo-Json -Depth 5), [System.Text.UTF8Encoding]::new($false))
    $recordHash = Get-WindowsFrozenCpuWorkerFileSha256 $recordPath
    [System.IO.File]::WriteAllText(
        (Join-Path $Root (Get-WindowsFrozenCpuWorkerMarkerFileName)),
        (Get-WindowsFrozenCpuWorkerMarkerText $recordHash ([pscustomobject]$record)),
        [System.Text.UTF8Encoding]::new($false)
    )
}

function Copy-FrozenFixture([string]$Name) {
    $destination = Join-Path $testRoot $Name
    Copy-Item -LiteralPath $producerOutput -Destination $destination -Recurse
    return $destination
}

function Invoke-FrozenConsumer(
    [string]$RecordPath,
    [string]$BundlePath,
    [string]$FrozenCpuWorkerSourceRoot
) {
    $parameters = @{
        ModelSource = $modelSource
        BundlePath = $BundlePath
        InstallerPackAllowlistPath = $installerAllowlist
        FrozenCpuWorkerRecordPath = $RecordPath
    }
    if ($PSBoundParameters.ContainsKey('FrozenCpuWorkerSourceRoot')) {
        $parameters.FrozenCpuWorkerSourceRoot = $FrozenCpuWorkerSourceRoot
    }
    & $fixtureBuilder @parameters
}

function New-ForeignFrozenWorkerSource([string]$Destination) {
    New-Item -ItemType Directory -Path $Destination | Out-Null
    foreach ($item in @(Get-ChildItem -LiteralPath $fixtureRoot -Force)) {
        if ($item.Name -ceq '.git') { continue }
        Copy-Item -LiteralPath $item.FullName -Destination $Destination -Recurse -Force
    }
    # The M builder must treat R as data. If it ever invokes R's builder rather
    # than merely deriving its source context, this fixture fails immediately.
    [System.IO.File]::WriteAllText(
        (Join-Path $Destination 'scripts\build-windows-release.ps1'),
        "throw 'foreign frozen worker source must not be executed'`n",
        [System.Text.UTF8Encoding]::new($false)
    )
    & git -C $Destination init --quiet
    if ($LASTEXITCODE -ne 0) { throw 'Could not initialize foreign frozen worker source fixture.' }
    & git -C $Destination add --all
    if ($LASTEXITCODE -ne 0) { throw 'Could not stage foreign frozen worker source fixture.' }
    & git -C $Destination commit --quiet -m foreign-fixture
    if ($LASTEXITCODE -ne 0) { throw 'Could not commit foreign frozen worker source fixture.' }
    return $Destination
}

function Set-FixtureNativeSmokeSeam([string]$BuilderPath) {
    $source = Get-Content -LiteralPath $BuilderPath -Raw
    $start = $source.IndexOf('function Invoke-NativeProcess')
    $end = $source.IndexOf('function Assert-NoReparseAncestors', $start)
    if ($start -lt 0 -or $end -le $start) {
        throw 'Could not isolate the fixture-only native smoke seam.'
    }
    $fixtureSmoke = @'
function Invoke-NativeProcess(
    [string]$ExecutablePath,
    [string[]]$Arguments
) {
    $global:WindowsFrozenCpuWorkerTestNativeCalls.Add([pscustomobject]@{
        ExecutablePath = $ExecutablePath
        Arguments = @($Arguments)
    })
    return [pscustomobject]@{
        ExitCode = 0
        Stdout = '{"cancellation_verified":true,"capabilities":{"cancellation":true},"detected_architecture":"whisper"}'
        Stderr = ''
    }
}

'@
    $source = $source.Substring(0, $start) + $fixtureSmoke + $source.Substring($end)
    [System.IO.File]::WriteAllText($BuilderPath, $source, [System.Text.UTF8Encoding]::new($false))
}

function Set-FixtureCompiledAdmissionSeam([string]$IntegrityPath) {
    $source = Get-Content -LiteralPath $IntegrityPath -Raw
    $start = $source.IndexOf('function Invoke-WindowsFrozenCpuWorkerAdmissionProcess')
    $end = $source.IndexOf('function Assert-WindowsFrozenCpuWorkerCompiledAdmission', $start)
    if ($start -lt 0 -or $end -le $start) {
        throw 'Could not isolate the fixture-only compiled admission seam.'
    }
    $fixtureAdmission = @'
function Invoke-WindowsFrozenCpuWorkerAdmissionProcess(
    [string]$Executable,
    [ValidateSet('--scribe-frozen-worker-admission', '--scribe-windows-gpu-auto-policy-identity')]
    [string]$Command = '--scribe-frozen-worker-admission'
) {
    if ($Command -ceq '--scribe-frozen-worker-admission') {
        if ($null -eq $global:WindowsFrozenCpuWorkerTestAdmissionResponse) {
            throw 'Fixture compiled admission response was not configured.'
        }
        $global:WindowsFrozenCpuWorkerTestAdmissionCalls.Add($Executable)
        return $global:WindowsFrozenCpuWorkerTestAdmissionResponse
    }
    $global:WindowsFrozenCpuWorkerTestPolicyCalls.Add([pscustomobject]@{ Executable = $Executable; Command = $Command })
    if ($global:WindowsFrozenCpuWorkerTestPolicyResponses.Count -gt 0) {
        $response = $global:WindowsFrozenCpuWorkerTestPolicyResponses[0]
        $global:WindowsFrozenCpuWorkerTestPolicyResponses.RemoveAt(0)
        return $response
    }
    if ($null -eq $global:WindowsFrozenCpuWorkerTestPolicyResponse) {
        throw 'Fixture GPU Auto policy identity response was not configured.'
    }
    return $global:WindowsFrozenCpuWorkerTestPolicyResponse
}

'@
    $source = $source.Substring(0, $start) + $fixtureAdmission + $source.Substring($end)
    [System.IO.File]::WriteAllText($IntegrityPath, $source, [System.Text.UTF8Encoding]::new($false))
}

function Set-FixtureCompiledAdmissionResponse(
    [psobject]$DesktopContext,
    [psobject]$WorkerContext,
    [psobject]$Record
) {
    $kind = if (Test-WindowsFrozenCpuWorkerSameSourceContext $DesktopContext $WorkerContext) {
        'strict_legacy_same_source'
    }
    else {
        'compiled_foreign_approval'
    }
    $global:WindowsFrozenCpuWorkerTestAdmissionResponse = [pscustomobject]@{
        ExitCode = 0
        Stdout = ([ordered]@{
            schema_version = 1
            desktop_build_id = $DesktopContext.DesktopBuildId
            bundled_worker_sha256 = $Record.worker_sha256
            protocol_version = $DesktopContext.ProtocolVersion
            worker_abi_version = $DesktopContext.WorkerAbiVersion
            worker_origin_app_build = $WorkerContext.DesktopBuildId
            worker_build_id = $WorkerContext.WorkerBuildId
            admission_kind = $kind
        } | ConvertTo-Json -Compress)
        Stderr = ''
    }
}

function Set-FixturePolicyIdentityResponse([string]$DesktopBuildId, [string]$Mutation = '') {
    $policyIdentity = Open-WindowsGpuAutoPolicyIdentity $fixtureRoot
    try {
        $report = [ordered]@{
            schema_version = [int64]1
            desktop_build_id = $DesktopBuildId
            policy_schema_version = [int64]$policyIdentity.PolicySchemaVersion
            policy_version = [int64]$policyIdentity.PolicyVersion
            target_os = [string]$policyIdentity.TargetOs
            target_arch = [string]$policyIdentity.TargetArch
            mode = [string]$policyIdentity.Mode
            entry_count = [int64]$policyIdentity.EntryCount
            embedded_manifest_size_bytes = [int64]$policyIdentity.EmbeddedManifestSizeBytes
            embedded_manifest_sha256 = [string]$policyIdentity.EmbeddedManifestSha256
            runtime_manifest_sha256 = [string]$policyIdentity.RuntimeManifestSha256
        }
        $response = [pscustomobject]@{ ExitCode = 0; Stdout = ''; Stderr = '' }
        switch ($Mutation) {
            '' { }
            'malformed' { $response.Stdout = '{fixture-malformed' }
            'wrong-digest' { $report.embedded_manifest_sha256 = '0' * 64 }
            default { throw "Unknown frozen-fixture GPU Auto policy mutation: $Mutation" }
        }
        if ($Mutation -cne 'malformed') { $response.Stdout = $report | ConvertTo-Json -Compress }
        $global:WindowsFrozenCpuWorkerTestPolicyResponses.Clear()
        $global:WindowsFrozenCpuWorkerTestPolicyResponse = $response
    }
    finally {
        $policyIdentity.ManifestStream.Dispose()
    }
}

function Remove-TestRootSafely([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $resolved = [System.IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    $temporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
    if (-not $resolved.StartsWith($temporaryRoot + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path -Leaf $resolved) -cnotmatch '^scribe-windows-frozen-cpu-worker-test-[0-9a-f]{32}$') {
        throw "Refused frozen worker test cleanup outside its exact temporary root: $resolved"
    }
    $current = $resolved
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refused frozen worker test cleanup through a reparse point: $current"
        }
        if ([string]::Equals($current, $temporaryRoot, [System.StringComparison]::OrdinalIgnoreCase)) { break }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) { throw 'Could not prove frozen worker test cleanup ancestry.' }
        $current = $parent
    }
    foreach ($item in @(Get-ChildItem -LiteralPath $resolved -Recurse -Force)) {
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refused frozen worker test cleanup containing a reparse point: $($item.FullName)"
        }
        if (($item.Attributes -band [System.IO.FileAttributes]::ReadOnly) -ne 0) {
            $item.Attributes = $item.Attributes -band (-bnot [System.IO.FileAttributes]::ReadOnly)
        }
    }
    # The fixture deliberately creates literal trailing-dot entries. Remove the
    # already-validated exact root verbatim so cleanup does not normalize them.
    [System.IO.Directory]::Delete("\\?\$resolved", $true)
}

function Get-CommandIdentity([string]$Name) {
    $command = Get-Command -Name $Name -ErrorAction Stop
    return "$($command.CommandType)|$($command.Source)|$($command.Definition)"
}

$previousTargetDirectory = $env:CARGO_TARGET_DIR
$previousRevision = $env:SCRIBE_BUILD_REVISION
$previousWorkerDigest = $env:SCRIBE_BUNDLED_WORKER_SHA256
$previousBuildingWorker = $env:SCRIBE_BUILDING_WORKER
$previousGitHubActions = $env:GITHUB_ACTIONS
$previousCi = $env:CI
$cpuBaselineAmbientNamePattern = '^(?i:CMAKE_ARGS|TRANSCRIBE_CMAKE_ARGS|CMAKE_TOOLCHAIN_FILE(?:_.+)?|HOST_CMAKE_TOOLCHAIN_FILE|CMAKE_X86_64_PC_WINDOWS_MSVC(?:_.+)?|(?:(?:HOST|TARGET)_)?(?:C|CXX|CPP)FLAGS(?:_.+)?|(?:(?:HOST|TARGET)_)?(?:CC|CXX)(?:_.+)?|CL|_CL_|RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_BUILD_RUSTFLAGS|CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS)$'
$previousCpuBaselineAmbient = @{}
foreach ($entry in [Environment]::GetEnvironmentVariables().GetEnumerator()) {
    if ([string]$entry.Key -match $cpuBaselineAmbientNamePattern) {
        $previousCpuBaselineAmbient[[string]$entry.Key] = [string]$entry.Value
    }
}

function Remove-TestCpuWorkerBaselineTarget([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $resolved = [System.IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    $temporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
    if (-not $resolved.StartsWith($temporaryRoot + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path -Leaf $resolved) -cnotmatch '^scribe-windows-cpu-worker-baseline-[0-9]+-[0-9a-f]{32}$') {
        throw "Refused CPU baseline test cleanup outside its exact temporary target: $resolved"
    }
    $current = $resolved
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refused CPU baseline test cleanup through a reparse point: $current"
        }
        if ([string]::Equals($current, $temporaryRoot, [System.StringComparison]::OrdinalIgnoreCase)) { break }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) { throw 'Could not prove CPU baseline test cleanup ancestry.' }
        $current = $parent
    }
    foreach ($item in @(Get-ChildItem -LiteralPath $resolved -Recurse -Force)) {
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refused CPU baseline test cleanup containing a reparse point: $($item.FullName)"
        }
    }
    [System.IO.Directory]::Delete("\\?\$resolved", $true)
}
$previousGitEnvironment = @{}
foreach ($entry in @(Get-ChildItem Env:GIT_*)) {
    $previousGitEnvironment[$entry.Name] = $entry.Value
}
$previousGlobalCargo = Get-Item -LiteralPath Function:\cargo -ErrorAction SilentlyContinue
$previousGlobalCargoScriptBlock = if ($null -ne $previousGlobalCargo) { $previousGlobalCargo.ScriptBlock } else { $null }
$originalCargoCommandIdentity = Get-CommandIdentity 'cargo'
$originalGetChildItemCommandIdentity = Get-CommandIdentity 'Get-ChildItem'
try {
    foreach ($name in $previousCpuBaselineAmbient.Keys) {
        if (Test-Path -LiteralPath "Env:$name") { Remove-Item -LiteralPath "Env:$name" -ErrorAction Stop }
    }
    New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
    # Fixture commits must never invoke the developer's hooks/signing tools or
    # inherit repository routing, config injection, attributes, or templates.
    foreach ($entry in @(Get-ChildItem Env:GIT_*)) {
        Remove-Item -LiteralPath "Env:$($entry.Name)"
    }
    $gitConfigPath = Join-Path $testRoot 'gitconfig'
    $gitEmptyDirectory = Join-Path $testRoot 'git-empty'
    $gitEmptyFile = Join-Path $testRoot 'git-empty-file'
    New-Item -ItemType Directory -Path $gitEmptyDirectory | Out-Null
    [System.IO.File]::WriteAllText($gitEmptyFile, '')
    [System.IO.File]::WriteAllText($gitConfigPath, '')
    $env:GIT_CONFIG_NOSYSTEM = '1'
    $env:GIT_ATTR_NOSYSTEM = '1'
    $env:GIT_CONFIG_GLOBAL = $gitConfigPath
    $env:GIT_TERMINAL_PROMPT = '0'
    $env:GIT_AUTHOR_DATE = '2000-01-01T00:00:00Z'
    $env:GIT_COMMITTER_DATE = '2000-01-01T00:00:00Z'
    foreach ($setting in @(
        @('core.hooksPath', $gitEmptyDirectory),
        @('core.attributesFile', $gitEmptyFile),
        @('core.excludesFile', $gitEmptyFile),
        @('core.autocrlf', 'false'),
        @('commit.gpgsign', 'false'),
        @('tag.gpgsign', 'false'),
        @('init.templateDir', $gitEmptyDirectory),
        @('init.defaultBranch', 'fixture'),
        @('user.email', 'fixture@example.invalid'),
        @('user.name', 'Scribe fixture')
    )) {
        & git config --file $gitConfigPath $setting[0] $setting[1]
        if ($LASTEXITCODE -ne 0) { throw 'Could not configure isolated fixture Git settings.' }
    }
    foreach ($relativePath in @(
        'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/config.toml', 'build.rs', 'src/worker_identity.rs',
        'scripts/build-windows-release.ps1', 'scripts/new-windows-frozen-cpu-worker.ps1',
        'scripts/windows-frozen-cpu-worker-integrity.ps1', 'scripts/windows-gpu-auto-policy-identity.ps1',
        'scripts/report-windows-gpu-auto-qualification.ps1', 'scripts/windows-cpu-worker-native-baseline.ps1',
        'scripts/windows-pe-imports.ps1',
        'scripts/stage-verified-worker-packs.ps1',
        'resources/licenses/Apache-2.0.txt', 'resources/licenses/OpenAI-Whisper-MIT.txt',
        'resources/licenses/Whisper-Base-En-NOTICE.txt', 'resources/licenses/THIRD-PARTY-NOTICES.txt',
        'native/transcribe-cpp-v0.1.3/LICENSE', 'native/transcribe-cpp-v0.1.3/PROVENANCE.md',
        'native/whisper-f049fff/LICENSE', 'native/whisper-f049fff/PROVENANCE.md',
        'native/sherpa-onnx-v1.13.5/PROVENANCE.md', 'resources/silero-vad/LICENSE',
        'resources/silero-vad/PROVENANCE.md',
        'runtime-manifests/gpu-auto-qualification-windows-x64.json'
    )) {
        Copy-FixtureSourceFile $relativePath
    }
    [System.IO.File]::WriteAllBytes($modelSource, [byte[]](0x01, 0x02, 0x03, 0x04))
    $modelHash = (Get-FileHash -LiteralPath $modelSource -Algorithm SHA256).Hash.ToLowerInvariant()
    $fixtureManifest = [ordered]@{
        model_id = 'fixture-model'
        # Keep the production inventory name; only the model bytes/hash are synthetic.
        artifact_filename = 'whisper-base.en-Q8_0.gguf'
        size_bytes = 4
        sha256 = $modelHash
        platform_triple = 'x86_64-pc-windows-msvc'
        attribution_files = @(
            'resources/licenses/Apache-2.0.txt',
            'resources/licenses/OpenAI-Whisper-MIT.txt',
            'resources/licenses/Whisper-Base-En-NOTICE.txt'
        )
    }
    $fixtureManifestPath = Join-Path $fixtureRoot 'runtime-manifests\whisper-base-en-q8_0-windows-x64.json'
    New-Item -ItemType Directory -Path (Split-Path -Parent $fixtureManifestPath) -Force | Out-Null
    [System.IO.File]::WriteAllText($fixtureManifestPath, ($fixtureManifest | ConvertTo-Json -Depth 4), [System.Text.UTF8Encoding]::new($false))
    [System.IO.File]::WriteAllText((Join-Path $fixtureRoot '.gitignore'), "target/`r`n", [System.Text.UTF8Encoding]::new($false))
    Set-FixtureNativeSmokeSeam (Join-Path $fixtureRoot 'scripts\build-windows-release.ps1')
    Set-FixtureCompiledAdmissionSeam (Join-Path $fixtureRoot 'scripts\windows-frozen-cpu-worker-integrity.ps1')
    & git -C $fixtureRoot init --quiet
    if ($LASTEXITCODE -ne 0) { throw 'Could not initialize frozen worker test fixture Git repository.' }
    & git -C $fixtureRoot add --all
    if ($LASTEXITCODE -ne 0) { throw 'Could not stage frozen worker test fixture files.' }
    & git -C $fixtureRoot commit --quiet -m fixture
    if ($LASTEXITCODE -ne 0) { throw 'Could not commit frozen worker test fixture files.' }

    $fixtureBuilder = Join-Path $fixtureRoot 'scripts\build-windows-release.ps1'
    $fixtureProducer = Join-Path $fixtureRoot 'scripts\new-windows-frozen-cpu-worker.ps1'
    . (Join-Path $fixtureRoot 'scripts\windows-frozen-cpu-worker-integrity.ps1')
    . (Join-Path $fixtureRoot 'scripts\windows-gpu-auto-policy-identity.ps1')
    . (Join-Path $fixtureRoot 'scripts\windows-cpu-worker-native-baseline.ps1')

    $baselineEvidenceTarget = Join-Path $testRoot 'baseline-evidence'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget
    $baselineEvidence = [pscustomobject]@{ TargetRoot = $baselineEvidenceTarget; TargetTriple = 'x86_64-pc-windows-msvc' }
    Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence
    $baselineNativeRoot = Join-Path $baselineEvidenceTarget 'x86_64-pc-windows-msvc\release\build\transcribe-cpp-sys-a1b2c3d4\out\build'
    Assert-WindowsCpuWorkerBaselineNativeBuildEvidence $baselineNativeRoot
    $baselineCache = Join-Path $baselineEvidenceTarget 'x86_64-pc-windows-msvc\release\build\transcribe-cpp-sys-a1b2c3d4\out\build\CMakeCache.txt'
    $baselineCacheText = Get-Content -LiteralPath $baselineCache -Raw
    [System.IO.File]::WriteAllText($baselineCache, $baselineCacheText.Replace('GGML_AVX2:BOOL=OFF', 'GGML_AVX2:BOOL=ON'), [System.Text.UTF8Encoding]::new($false))
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'GGML_AVX2=OFF'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget
    [System.IO.File]::WriteAllText($baselineCache, $baselineCacheText.Replace('CMAKE_C_FLAGS_RELEASE:STRING=/O2 /Ob2 /DNDEBUG', 'CMAKE_C_FLAGS_RELEASE:STRING=/O2 /arch:AVX2'), [System.Text.UTF8Encoding]::new($false))
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'CMake compiler flags contain a native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget
    $extraCache = Join-Path $baselineEvidenceTarget 'x86_64-pc-windows-msvc\release\build\transcribe-cpp-sys-e1f2a3b4\out\build\CMakeCache.txt'
    New-Item -ItemType Directory -Path (Split-Path -Parent $extraCache) -Force | Out-Null
    [System.IO.File]::WriteAllText($extraCache, 'GGML_NATIVE:BOOL=OFF', [System.Text.UTF8Encoding]::new($false))
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'exactly one transcribe-cpp-sys Cargo build directory'
    Remove-Item -LiteralPath (Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $extraCache))) -Recurse -Force
    $baselineFlags = Join-Path $baselineEvidenceTarget 'x86_64-pc-windows-msvc\release\build\transcribe-cpp-sys-a1b2c3d4\out\build\ggml\src\CMakeFiles\ggml-cpu.dir\flags.make'
    Remove-Item -LiteralPath $baselineFlags -Force
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'evidence file is missing'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'C_FLAGS = /arch:AVX2'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'C_FLAGS = /arch:AVX10.1'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'CXX_FLAGS = -msse4.2'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'CXX_FLAGS = GGML_AVX2'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'CXX_FLAGS = @hidden.rsp'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'CXX_FLAGS = @hidden.txt'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'CXX_FLAGS = -arch:AVX10.1'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'CXX_FLAGS = -DGGML_AVX2'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'CXX_FLAGS = /DGGML_AVX2=1'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'CXX_FLAGS = /D__AVX2__=0'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'CXX_FLAGS = /D "__AVX2__=0"'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'CXX_FLAGS = -D__AVX10_1__=1'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget 'CXX_FLAGS = /FIhidden.h'
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'native or higher ISA requirement'
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget
    $visualStudioProject = Join-Path $baselineEvidenceTarget 'x86_64-pc-windows-msvc\release\build\transcribe-cpp-sys-a1b2c3d4\out\build\ggml\src\ggml-cpu.vcxproj'
    $visualStudioTlog = Join-Path $baselineEvidenceTarget 'x86_64-pc-windows-msvc\release\build\transcribe-cpp-sys-a1b2c3d4\out\build\ggml\src\ggml-cpu.dir\Release\ggml-cpu.tlog\CL.command.1.tlog'
    [System.IO.File]::WriteAllText($baselineCache, $baselineCacheText.Replace('CMAKE_GENERATOR:INTERNAL=NMake Makefiles', 'CMAKE_GENERATOR:INTERNAL=Visual Studio 17 2022'), [System.Text.UTF8Encoding]::new($false))
    $safeVisualStudioProject = '<Project xmlns="http://schemas.microsoft.com/developer/msbuild/2003"><ItemDefinitionGroup><ClCompile><AdditionalIncludeDirectories>%(AdditionalIncludeDirectories)</AdditionalIncludeDirectories><PreprocessorDefinitions>%(PreprocessorDefinitions);NDEBUG</PreprocessorDefinitions><ObjectFileName>$(IntDir)</ObjectFileName><AdditionalOptions>/O2 %(AdditionalOptions)</AdditionalOptions><EnableEnhancedInstructionSet>NotSet</EnableEnhancedInstructionSet></ClCompile></ItemDefinitionGroup></Project>'
    [System.IO.File]::WriteAllText($visualStudioProject, $safeVisualStudioProject, [System.Text.UTF8Encoding]::new($false))
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'evidence file is missing'
    New-Item -ItemType Directory -Path (Split-Path -Parent $visualStudioTlog) -Force | Out-Null
    [System.IO.File]::WriteAllText($visualStudioTlog, "^C:\fixture\ggml-cpu.c`r`n/c /O2", [System.Text.Encoding]::Unicode)
    Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence
    [System.IO.File]::WriteAllText($visualStudioTlog, "^C:\fixture\ggml-cpu.c`r`n/c /O2", [System.Text.UTF8Encoding]::new($false))
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'UTF-16 with a byte-order mark'
    [System.IO.File]::WriteAllText($visualStudioTlog, "^C:\fixture\ggml-cpu.c`r`n/c /arch:AVX10.1", [System.Text.Encoding]::Unicode)
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'command log contains an unresolved or higher ISA requirement'
    [System.IO.File]::WriteAllText($visualStudioTlog, "^C:\fixture\ggml-cpu.c`r`n/c %(AdditionalOptions)", [System.Text.Encoding]::Unicode)
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'command log contains an unresolved or higher ISA requirement'
    [System.IO.File]::WriteAllText($visualStudioTlog, "^C:\fixture\ggml-cpu.c`r`n/c @hidden.txt", [System.Text.Encoding]::Unicode)
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'command log contains an unresolved or higher ISA requirement'
    [System.IO.File]::WriteAllText($visualStudioTlog, "^C:\fixture\ggml-cpu.c`r`n/c /D__AVX512F__=1", [System.Text.Encoding]::Unicode)
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'command log contains an unresolved or higher ISA requirement'
    [System.IO.File]::WriteAllText($visualStudioTlog, "^C:\fixture\ggml-cpu.c`r`n/c /D `"__AVX2__=0`"", [System.Text.Encoding]::Unicode)
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'command log contains an unresolved or higher ISA requirement'
    [System.IO.File]::WriteAllText($visualStudioTlog, "^C:\fixture\ggml-cpu.c`r`n/c -imacros hidden.h", [System.Text.Encoding]::Unicode)
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'command log contains an unresolved or higher ISA requirement'
    [System.IO.File]::WriteAllText($visualStudioTlog, "^C:\fixture\ggml-cpu.c`r`n/c /O2", [System.Text.Encoding]::Unicode)
    [System.IO.File]::WriteAllText($visualStudioProject, $safeVisualStudioProject.Replace('NotSet', 'AdvancedVectorExtensions2'), [System.Text.UTF8Encoding]::new($false))
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'Visual Studio ggml-cpu flags contain an unrecognized or higher ISA requirement'
    [System.IO.File]::WriteAllText($visualStudioProject, '<Project><ItemDefinitionGroup><ClCompile><AdditionalOptions>@hidden.rsp %(AdditionalOptions)</AdditionalOptions><EnhancedInstructionSet>NotSet</EnhancedInstructionSet></ClCompile></ItemDefinitionGroup></Project>', [System.Text.UTF8Encoding]::new($false))
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'Visual Studio ggml-cpu flags contain an unrecognized or higher ISA requirement'
    [System.IO.File]::WriteAllText($visualStudioProject, '<Project><ItemDefinitionGroup><ClCompile><AdditionalOptions>$(InjectedOptions)</AdditionalOptions><EnhancedInstructionSet>NotSet</EnhancedInstructionSet></ClCompile></ItemDefinitionGroup></Project>', [System.Text.UTF8Encoding]::new($false))
    [System.IO.File]::WriteAllText($visualStudioTlog, "^C:\fixture\ggml-cpu.c`r`n/c /arch:AVX2", [System.Text.Encoding]::Unicode)
    Invoke-ExpectedFailure { Assert-WindowsCpuWorkerBaselineEvidence $baselineEvidence } 'command log contains an unresolved or higher ISA requirement'
    Remove-Item -LiteralPath $visualStudioProject -Force
    Write-TestCpuWorkerBaselineEvidence $baselineEvidenceTarget

    foreach ($name in $previousCpuBaselineAmbient.Keys) {
        if (Test-Path -LiteralPath "Env:$name") { Remove-Item -LiteralPath "Env:$name" -ErrorAction Stop }
    }
    # RUSTFLAGS itself is now a presence-based ambient compiler override, so
    # New-WindowsCpuWorkerBaselineBuild must reject it. Its direct restore
    # contract is covered separately below; these variables retain the
    # construction-and-restoration coverage.
    $baselineEnvironmentNames = @(Get-WindowsCpuWorkerBaselineEnvironmentNames | Where-Object { $_ -cne 'RUSTFLAGS' })
    $baselineBeforeRestorationTests = [Environment]::GetEnvironmentVariables()
    try {
        foreach ($name in $baselineEnvironmentNames) {
            if (Test-Path -LiteralPath "Env:$name") { Remove-Item -LiteralPath "Env:$name" -ErrorAction Stop }
        }
        $absentEnvironmentBuild = New-WindowsCpuWorkerBaselineBuild 'x86_64-pc-windows-msvc'
        foreach ($attempt in @(1, 2)) {
            Restore-WindowsCpuWorkerBaselineEnvironment $absentEnvironmentBuild
            foreach ($name in $baselineEnvironmentNames) {
                Assert-True (-not (Test-Path -LiteralPath "Env:$name")) "Originally absent $name became present after restore $attempt."
                Assert-Equal ([Environment]::GetEnvironmentVariable($name)) $null "Originally absent $name value after restore $attempt"
            }
            Assert-True (-not (Test-Path -LiteralPath $absentEnvironmentBuild.TargetRoot)) 'Environment-only restoration test created a target directory.'
        }
        foreach ($name in $baselineEnvironmentNames) { [Environment]::SetEnvironmentVariable($name, '') }
        # Older .NET runtimes cannot create present-empty values this way.
        # Preserve the actual supported pre-build state, never assume absence
        # and an empty override are equivalent on runtimes that distinguish them.
        $emptyEnvironmentBefore = [Environment]::GetEnvironmentVariables()
        $emptyEnvironmentBuild = New-WindowsCpuWorkerBaselineBuild 'x86_64-pc-windows-msvc'
        foreach ($attempt in @(1, 2)) {
            Restore-WindowsCpuWorkerBaselineEnvironment $emptyEnvironmentBuild
            $after = [Environment]::GetEnvironmentVariables()
            foreach ($name in $baselineEnvironmentNames) {
                Assert-Equal (Test-Path -LiteralPath "Env:$name") ($emptyEnvironmentBefore.Contains($name)) "Empty-state $name presence after restore $attempt"
                Assert-Equal ($after[$name]) ($emptyEnvironmentBefore[$name]) "Empty-state $name value after restore $attempt"
            }
            Assert-True (-not (Test-Path -LiteralPath $emptyEnvironmentBuild.TargetRoot)) 'Empty-state restoration test created a target directory.'
        }
        $env:CARGO_TARGET_DIR = 'C:\fixture\original-target'
        $env:TRANSCRIBE_CMAKE_ARGS = ' '
        $presentEnvironmentBefore = [Environment]::GetEnvironmentVariables()
        $presentEnvironmentBuild = New-WindowsCpuWorkerBaselineBuild 'x86_64-pc-windows-msvc'
        foreach ($attempt in @(1, 2)) {
            Restore-WindowsCpuWorkerBaselineEnvironment $presentEnvironmentBuild
            $after = [Environment]::GetEnvironmentVariables()
            foreach ($name in $baselineEnvironmentNames) {
                Assert-True (Test-Path -LiteralPath "Env:$name") "Present $name disappeared after restore $attempt."
                Assert-Equal ($after[$name]) ($presentEnvironmentBefore[$name]) "Present $name value after restore $attempt"
            }
            Assert-True (-not (Test-Path -LiteralPath $presentEnvironmentBuild.TargetRoot)) 'Present-state restoration test created a target directory.'
        }
    }
    finally {
        foreach ($name in $baselineEnvironmentNames) {
            if ($baselineBeforeRestorationTests.Contains($name)) {
                [Environment]::SetEnvironmentVariable($name, [string]$baselineBeforeRestorationTests[$name])
            }
            elseif (Test-Path -LiteralPath "Env:$name") { Remove-Item -LiteralPath "Env:$name" -ErrorAction Stop }
        }
    }
    $env:CMAKE_ARGS = '-DGGML_AVX2=ON'
    Invoke-ExpectedFailure { New-WindowsCpuWorkerBaselineBuild 'x86_64-pc-windows-msvc' } 'does not accept ambient CMake arguments'
    Remove-Item Env:CMAKE_ARGS
    foreach ($name in @('CMAKE_TOOLCHAIN_FILE', 'CMAKE_TOOLCHAIN_FILE_x86_64-pc-windows-msvc', 'HOST_CMAKE_TOOLCHAIN_FILE', 'CMAKE_X86_64_PC_WINDOWS_MSVC_TOOLCHAIN_FILE')) {
        [Environment]::SetEnvironmentVariable($name, 'C:\fixture\toolchain.cmake')
        try {
            Invoke-ExpectedFailure { New-WindowsCpuWorkerBaselineBuild 'x86_64-pc-windows-msvc' } "does not accept ambient CMake toolchain overrides: $name"
        }
        finally { if (Test-Path -LiteralPath "Env:$name") { Remove-Item -LiteralPath "Env:$name" -ErrorAction Stop } }
    }
    $env:CL = '/arch:AVX2'
    Invoke-ExpectedFailure { New-WindowsCpuWorkerBaselineBuild 'x86_64-pc-windows-msvc' } 'does not accept ambient compiler flags: CL'
    Remove-Item Env:CL
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = '-C target-cpu=native'
    Invoke-ExpectedFailure { New-WindowsCpuWorkerBaselineBuild 'x86_64-pc-windows-msvc' } 'does not accept ambient compiler flags: CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS'
    Remove-Item Env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS
    [Environment]::SetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', '')
    try {
        Assert-True (Test-Path -LiteralPath 'Env:CARGO_ENCODED_RUSTFLAGS') 'Present-empty CARGO_ENCODED_RUSTFLAGS fixture was not preserved by this runtime.'
        Invoke-ExpectedFailure { New-WindowsCpuWorkerBaselineBuild 'x86_64-pc-windows-msvc' } 'does not accept ambient compiler flags: CARGO_ENCODED_RUSTFLAGS'
    }
    finally { if (Test-Path -LiteralPath 'Env:CARGO_ENCODED_RUSTFLAGS') { Remove-Item -LiteralPath 'Env:CARGO_ENCODED_RUSTFLAGS' -ErrorAction Stop } }
    $directRustFlagsBefore = [Environment]::GetEnvironmentVariables()
    try {
        foreach ($case in @(
            [pscustomobject]@{ Name = 'absent'; Value = $null; Exists = $false },
            [pscustomobject]@{ Name = 'empty'; Value = ''; Exists = $true },
            [pscustomobject]@{ Name = 'value'; Value = '-C target-feature=+crt-static'; Exists = $true }
        )) {
            [Environment]::SetEnvironmentVariable('RUSTFLAGS', 'mutated-before-direct-restore', 'Process')
            $previous = @{}
            foreach ($name in Get-WindowsCpuWorkerBaselineEnvironmentNames) {
                $previous[$name] = if ($directRustFlagsBefore.Contains($name)) { [string]$directRustFlagsBefore[$name] } else { $null }
            }
            $previous['RUSTFLAGS'] = $case.Value
            $directRestoreBuild = [pscustomobject]@{ PreviousEnvironment = $previous; Restored = $false }
            foreach ($attempt in @(1, 2)) {
                Restore-WindowsCpuWorkerBaselineEnvironment $directRestoreBuild
                $observed = Get-Item -LiteralPath 'Env:RUSTFLAGS' -ErrorAction SilentlyContinue
                Assert-Equal ($null -ne $observed) $case.Exists "Direct $($case.Name) RUSTFLAGS presence after restore $attempt"
                if ($case.Exists) {
                    Assert-Equal ([string]$observed.Value) ([string]$case.Value) "Direct $($case.Name) RUSTFLAGS value after restore $attempt"
                }
            }
        }
    }
    finally {
        if ($directRustFlagsBefore.Contains('RUSTFLAGS')) {
            [Environment]::SetEnvironmentVariable('RUSTFLAGS', [string]$directRustFlagsBefore['RUSTFLAGS'], 'Process')
        }
        elseif (Test-Path -LiteralPath 'Env:RUSTFLAGS') {
            Remove-Item -LiteralPath 'Env:RUSTFLAGS' -ErrorAction Stop
        }
    }
    [Environment]::SetEnvironmentVariable('RUSTFLAGS', '')
    try {
        Assert-True (Test-Path -LiteralPath 'Env:RUSTFLAGS') 'Present-empty RUSTFLAGS fixture was not preserved by this runtime.'
        Invoke-ExpectedFailure { New-WindowsCpuWorkerBaselineBuild 'x86_64-pc-windows-msvc' } 'does not accept ambient compiler flags: RUSTFLAGS'
    }
    finally { if (Test-Path -LiteralPath 'Env:RUSTFLAGS') { Remove-Item -LiteralPath 'Env:RUSTFLAGS' -ErrorAction Stop } }
    foreach ($name in @('HOST_CFLAGS', 'TARGET_CXXFLAGS', 'HOST_CC', 'CC_x86_64_pc_windows_msvc')) {
        [Environment]::SetEnvironmentVariable($name, 'fixture-compiler-override')
        try {
            Invoke-ExpectedFailure { New-WindowsCpuWorkerBaselineBuild 'x86_64-pc-windows-msvc' } "does not accept ambient compiler flags: $name"
        }
        finally { if (Test-Path -LiteralPath "Env:$name") { Remove-Item -LiteralPath "Env:$name" -ErrorAction Stop } }
    }

    # Use verbatim creation only to make a test-owned physical path that exceeds
    # MAX_PATH. The helper itself receives its ordinary absolute identity.
    $longStreamDirectory = $testRoot
    $nonBmpPathComponent = [char]::ConvertFromUtf32(0x1F9EA)
    foreach ($component in @(
        "stream-path-路径-$('a' * 52)",
        "stream-path-$nonBmpPathComponent-$('b' * 52)",
        "stream-path-данные-$('c' * 52)",
        "stream-path-δοκιμή-$('d' * 52)"
    )) {
        $longStreamDirectory = Join-Path $longStreamDirectory $component
        [System.IO.Directory]::CreateDirectory("\\?\$longStreamDirectory") | Out-Null
    }
    $longStreamFile = Join-Path $longStreamDirectory 'scribe-inference-worker-長い.exe'
    [System.IO.File]::WriteAllText(
        "\\?\$longStreamFile",
        'long stream enumeration fixture',
        [System.Text.UTF8Encoding]::new($false)
    )
    Assert-True (([System.Text.Encoding]::Unicode.GetByteCount($longStreamDirectory) / 2) -gt 260) `
        'Long stream directory did not exceed 260 UTF-16 code units.'
    Assert-True (([System.Text.Encoding]::Unicode.GetByteCount($longStreamFile) / 2) -gt 260) `
        'Long stream file did not exceed 260 UTF-16 code units.'
    Assert-True $longStreamDirectory.Contains($nonBmpPathComponent) `
        'Long stream fixture did not include its non-BMP component.'
    Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $longStreamDirectory
    Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $longStreamFile
    $longStreamDirectoryVerbatim = "\\?\$longStreamDirectory"
    $longStreamFileVerbatim = "\\?\$longStreamFile"
    [System.IO.File]::WriteAllText("$longStreamDirectoryVerbatim`:frozen-test", 'directory ads')
    try {
        Invoke-ExpectedFailure { Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $longStreamDirectory } 'alternate data stream'
    }
    finally {
        [System.IO.File]::Delete("$longStreamDirectoryVerbatim`:frozen-test")
    }
    [System.IO.File]::WriteAllText("$longStreamFileVerbatim`:frozen-test", 'file ads')
    try {
        Invoke-ExpectedFailure { Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $longStreamFile } 'alternate data stream'
        Invoke-ExpectedFailure { Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams "$longStreamFileVerbatim`:frozen-test" } 'alternate data stream'
    }
    finally {
        [System.IO.File]::Delete("$longStreamFileVerbatim`:frozen-test")
    }

    # Trailing-dot spelling resolves to a clean normalized neighbor in the
    # ordinary parser; trailing-space spelling varies by Windows API. Both
    # have distinct literal verbatim targets carrying ADSs, so the helper must
    # inspect the explicit object or fail closed for the ordinary spelling.
    $normalizationDecoyRoot = Join-Path $testRoot 'stream-normalization-decoy'
    foreach ($decoy in @(
        [pscustomobject]@{ NormalizedComponent = 'dotted-target'; LiteralComponent = 'dotted-target.'; Label = 'trailing-dot'; OrdinaryReadsNormalized = $true },
        [pscustomobject]@{ NormalizedComponent = 'spaced-target'; LiteralComponent = 'spaced-target '; Label = 'trailing-space'; OrdinaryReadsNormalized = $false }
    )) {
        $normalizedDirectory = Join-Path $normalizationDecoyRoot $decoy.NormalizedComponent
        [System.IO.Directory]::CreateDirectory($normalizedDirectory) | Out-Null
        $normalizedFile = Join-Path $normalizedDirectory 'marker'
        [System.IO.File]::WriteAllText($normalizedFile, "normalized $($decoy.Label) neighbor")
        $literalDirectory = "$normalizationDecoyRoot\$($decoy.LiteralComponent)"
        $literalDirectoryVerbatim = "\\?\$literalDirectory"
        [System.IO.Directory]::CreateDirectory($literalDirectoryVerbatim) | Out-Null
        $literalFile = "$literalDirectory\marker"
        $literalFileVerbatim = "\\?\$literalFile"
        [System.IO.File]::WriteAllText($literalFileVerbatim, "literal $($decoy.Label) target")
        Assert-Equal ([System.IO.File]::ReadAllText($normalizedFile)) "normalized $($decoy.Label) neighbor" `
            "Normalized $($decoy.Label) neighbor did not retain its physical identity."
        if ($decoy.OrdinaryReadsNormalized) {
            Assert-Equal ([System.IO.File]::ReadAllText($literalFile)) "normalized $($decoy.Label) neighbor" `
                "Ordinary $($decoy.Label) path did not resolve to its normalized neighbor."
        }
        Assert-Equal ([System.IO.File]::ReadAllText($literalFileVerbatim)) "literal $($decoy.Label) target" `
            "Verbatim $($decoy.Label) path did not retain its physical identity."
        [System.IO.File]::WriteAllText("$literalFileVerbatim`:frozen-test", "$($decoy.Label) ads")
        try {
            Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $normalizedFile
            Invoke-ExpectedFailure { Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $literalFileVerbatim } 'alternate data stream'
            Invoke-ExpectedFailure { Assert-WindowsFrozenCpuWorkerNoAlternateDataStreams $literalFile } 'safe absolute Windows stream-enumeration path'
        }
        finally {
            [System.IO.File]::Delete("$literalFileVerbatim`:frozen-test")
        }
    }

    Assert-Equal (ConvertTo-WindowsFrozenCpuWorkerStreamEnumerationPath 'C:\') `
        '\\?\C:\' 'Ordinary drive-root stream enumeration path'
    Assert-Equal (ConvertTo-WindowsFrozenCpuWorkerStreamEnumerationPath 'C:\workers\packs\worker.exe') `
        '\\?\C:\workers\packs\worker.exe' 'Ordinary drive stream enumeration path'
    Assert-Equal (ConvertTo-WindowsFrozenCpuWorkerStreamEnumerationPath 'C:/workers/packs/worker.exe') `
        '\\?\C:\workers\packs\worker.exe' 'Slash-separated drive stream enumeration path'
    Assert-Equal (ConvertTo-WindowsFrozenCpuWorkerStreamEnumerationPath '\\server\share\packs\worker.exe') `
        '\\?\UNC\server\share\packs\worker.exe' 'Ordinary UNC stream enumeration path'
    Assert-Equal (ConvertTo-WindowsFrozenCpuWorkerStreamEnumerationPath '\\server\share') `
        '\\?\UNC\server\share' 'Ordinary UNC-root stream enumeration path'
    Assert-Equal (ConvertTo-WindowsFrozenCpuWorkerStreamEnumerationPath '\\?\C:\workers\victim.\marker') `
        '\\?\C:\workers\victim.\marker' 'Explicit drive stream identity preservation'
    Assert-Equal (ConvertTo-WindowsFrozenCpuWorkerStreamEnumerationPath '\\?\UNC\server\share\victim.\marker') `
        '\\?\UNC\server\share\victim.\marker' 'Explicit UNC stream identity preservation'
    Assert-Equal (ConvertTo-WindowsFrozenCpuWorkerStreamEnumerationPath '\\?\UNC\server\share') `
        '\\?\UNC\server\share' 'Explicit UNC-root stream identity preservation'
    foreach ($unsafePath in @(
        'packs\worker.exe',
        'C:packs\worker.exe',
        '\packs\worker.exe',
        'C:\workers\.\worker.exe',
        'C:\workers\..\worker.exe',
        'C:\workers\victim.\marker',
        'C:\workers\victim \marker',
        'C:\workers\CON\marker',
        'C:\workers\com1.txt\marker',
        '\\server..\share\worker.exe',
        '\\server\share.\worker.exe',
        '\\server\share\\worker.exe',
        '\\.\C:\workers\packs\worker.exe',
        '\\?\GLOBALROOT\Device\HarddiskVolume1\worker.exe',
        '\\?\C:/workers/packs/worker.exe'
    )) {
        Invoke-ExpectedFailure { ConvertTo-WindowsFrozenCpuWorkerStreamEnumerationPath $unsafePath } 'safe absolute Windows stream-enumeration path'
    }
    foreach ($adsPath in @(
        'C:\workers\marker:untrusted',
        '\\server\share\marker:untrusted',
        '\\?\C:\workers\marker:untrusted',
        '\\?\UNC\server\share\marker:untrusted'
    )) {
        Invoke-ExpectedFailure { ConvertTo-WindowsFrozenCpuWorkerStreamEnumerationPath $adsPath } 'alternate data stream'
    }

    $fixtureContext = Get-WindowsFrozenCpuWorkerSourceContext $fixtureRoot
    $env:CARGO_TARGET_DIR = $fixtureTarget
    $env:SCRIBE_BUILD_REVISION = 'inherited-test-revision'
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = 'f' * 64 -join ''
    $env:SCRIBE_BUILDING_WORKER = 'inherited-worker-flag'
    $env:GITHUB_ACTIONS = $null
    $env:CI = $null
    Reset-TestCalls
    $global:WindowsFrozenCpuWorkerTestAdmissionCalls = [System.Collections.Generic.List[string]]::new()
    $global:WindowsFrozenCpuWorkerTestAdmissionResponse = $null
    $global:WindowsFrozenCpuWorkerTestPolicyCalls = [System.Collections.Generic.List[object]]::new()
    $global:WindowsFrozenCpuWorkerTestPolicyResponses = [System.Collections.Generic.List[object]]::new()
    $global:WindowsFrozenCpuWorkerTestPolicyResponse = $null
    $global:WindowsFrozenCpuWorkerTestFailWorkerBuild = $false
    $global:WindowsFrozenCpuWorkerTestFailDesktopBuild = $false
    $global:WindowsFrozenCpuWorkerTestRaceFreezeOutput = $null
    $global:WindowsFrozenCpuWorkerTestRaceBundleOutput = $null
    $global:WindowsFrozenCpuWorkerTestSourceDriftPath = $null
    $global:WindowsFrozenCpuWorkerTestMutateWorkerPath = $null
    $global:WindowsFrozenCpuWorkerTestMutationWasBlocked = $false
    Set-FixturePolicyIdentityResponse (Get-WindowsGpuAutoPolicyDesktopBuildId $fixtureRoot $env:SCRIBE_BUILD_REVISION)

    function global:cargo {
        param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments)
        $binIndex = [array]::IndexOf($Arguments, '--bin')
        if ($binIndex -lt 0 -or $binIndex -ge ($Arguments.Count - 1)) {
            throw 'Synthetic Cargo seam did not receive an exact --bin argument.'
        }
        $binary = $Arguments[$binIndex + 1]
        $global:WindowsFrozenCpuWorkerTestCargoCalls.Add([pscustomobject]@{
            Binary = $binary
            Arguments = @($Arguments)
            Revision = $env:SCRIBE_BUILD_REVISION
            WorkerDigest = $env:SCRIBE_BUNDLED_WORKER_SHA256
            BuildingWorker = $env:SCRIBE_BUILDING_WORKER
            CargoTargetDirectory = $env:CARGO_TARGET_DIR
            TranscribeCmakeArgs = $env:TRANSCRIBE_CMAKE_ARGS
            RustFlags = $env:RUSTFLAGS
        })
        if (($global:WindowsFrozenCpuWorkerTestFailWorkerBuild -and $binary -ceq 'scribe-inference-worker') -or
            ($global:WindowsFrozenCpuWorkerTestFailDesktopBuild -and $binary -ceq 'local-transcriber')) {
            $global:LASTEXITCODE = 1
            return
        }
        $output = Join-Path $env:CARGO_TARGET_DIR "x86_64-pc-windows-msvc\release\$binary.exe"
        if ($binary -ceq 'scribe-inference-worker') {
            $null = $global:WindowsFrozenCpuWorkerTestBaselineTargetRoots.Add($env:CARGO_TARGET_DIR)
            New-TestReviewedPe $output 3
            Write-TestCpuWorkerBaselineEvidence $env:CARGO_TARGET_DIR
            if ($global:WindowsFrozenCpuWorkerTestRaceFreezeOutput) {
                New-Item -ItemType Directory -Path $global:WindowsFrozenCpuWorkerTestRaceFreezeOutput | Out-Null
            }
            if ($global:WindowsFrozenCpuWorkerTestSourceDriftPath) {
                [System.IO.File]::WriteAllText($global:WindowsFrozenCpuWorkerTestSourceDriftPath, 'drift')
            }
        }
        elseif ($binary -ceq 'local-transcriber') {
            New-TestReviewedPe $output 2
            if ($global:WindowsFrozenCpuWorkerTestMutateWorkerPath) {
                try {
                    [System.IO.File]::WriteAllBytes($global:WindowsFrozenCpuWorkerTestMutateWorkerPath, [byte[]](0x66))
                    $global:WindowsFrozenCpuWorkerTestMutationWasBlocked = $false
                }
                catch {
                    $global:WindowsFrozenCpuWorkerTestMutationWasBlocked = $true
                }
            }
            if ($global:WindowsFrozenCpuWorkerTestRaceBundleOutput) {
                New-Item -ItemType Directory -Path $global:WindowsFrozenCpuWorkerTestRaceBundleOutput | Out-Null
            }
            if ($global:WindowsFrozenCpuWorkerTestSourceDriftPath) {
                [System.IO.File]::WriteAllText($global:WindowsFrozenCpuWorkerTestSourceDriftPath, 'drift')
            }
        }
        else {
            throw "Synthetic Cargo seam received an unsupported binary: $binary"
        }
        $global:LASTEXITCODE = 0
    }

    $nonFrozenObservationBundle = Join-Path $testRoot 'rejected-non-frozen-observation'
    Invoke-ExpectedFailure {
        & $fixtureBuilder -ModelSource $modelSource -BundlePath $nonFrozenObservationBundle `
            -InstallerPackAllowlistPath $installerAllowlist -LocalFrozenGpuObservation
    } 'available only with a local frozen CPU worker record'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 'Non-frozen GPU observation invoked Cargo'
    Assert-True (-not (Test-Path -LiteralPath $nonFrozenObservationBundle)) 'Non-frozen GPU observation rejection left an output.'

    $normalBundle = Join-Path $testRoot 'normal-bundle'
    & $fixtureBuilder `
        -ModelSource $modelSource `
        -BundlePath $normalBundle `
        -InstallerPackAllowlistPath $installerAllowlist
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 2 'Normal packaging Cargo call count'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].Binary 'scribe-inference-worker' 'Normal packaging worker-first order'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[1].Binary 'local-transcriber' 'Normal packaging desktop-second order'
    Assert-WorkerCargoArguments $global:WindowsFrozenCpuWorkerTestCargoCalls[0] 'Normal CPU worker exact Cargo argv'
    Assert-DesktopCargoArguments $global:WindowsFrozenCpuWorkerTestCargoCalls[1] 'ui-harness' 'Normal desktop exact feature argv'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].BuildingWorker '1' 'Normal packaging worker build marker'
    Assert-True ($global:WindowsFrozenCpuWorkerTestCargoCalls[0].CargoTargetDirectory -cne $fixtureTarget) 'Normal CPU worker did not use a fresh isolated Cargo target.'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].TranscribeCmakeArgs (Get-WindowsCpuWorkerBaselineCmakeArgs) 'Normal CPU worker exact native CMake baseline'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].RustFlags '-C target-feature=+crt-static' 'Normal CPU worker exact Rust ISA baseline'
    Assert-True ([string]::IsNullOrEmpty($global:WindowsFrozenCpuWorkerTestCargoCalls[0].WorkerDigest)) 'Normal worker build inherited a desktop digest.'
    Assert-True ([string]::IsNullOrEmpty($global:WindowsFrozenCpuWorkerTestCargoCalls[1].BuildingWorker)) 'Normal desktop build inherited the worker build marker.'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[1].WorkerDigest (Get-FileHash -LiteralPath (Join-Path $normalBundle 'scribe-inference-worker.exe') -Algorithm SHA256).Hash.ToLowerInvariant() 'Normal desktop embeds the exact packaged worker digest'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $normalBundle (Get-WindowsFrozenCpuWorkerMarkerFileName)))) 'Normal packaging unexpectedly staged the local-only marker.'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestPolicyCalls.Count 3 'Normal packaging did not verify compiled GPU Auto policy identity at built, staged, and pre-activation boundaries.'
    foreach ($call in $global:WindowsFrozenCpuWorkerTestPolicyCalls) {
        Assert-Equal $call.Command '--scribe-windows-gpu-auto-policy-identity' 'Normal packaging policy identity command'
    }
    Assert-Equal $env:SCRIBE_BUILD_REVISION 'inherited-test-revision' 'Normal packaging revision environment restoration'
    Assert-Equal $env:SCRIBE_BUNDLED_WORKER_SHA256 ('f' * 64 -join '') 'Normal packaging worker digest environment restoration'
    Assert-Equal $env:SCRIBE_BUILDING_WORKER 'inherited-worker-flag' 'Normal packaging worker marker environment restoration'

    Reset-TestCalls
    & $fixtureProducer -OutputDirectory $producerOutput
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 1 'Freeze producer Cargo call count'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].Binary 'scribe-inference-worker' 'Freeze producer worker-only build'
    Assert-WorkerCargoArguments $global:WindowsFrozenCpuWorkerTestCargoCalls[0] 'Freeze producer exact CPU worker Cargo argv'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].Revision $fixtureContext.SourceRevision 'Freeze producer exact build revision'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].BuildingWorker '1' 'Freeze producer worker build marker'
    Assert-True ($global:WindowsFrozenCpuWorkerTestCargoCalls[0].CargoTargetDirectory -cne $fixtureTarget) 'Freeze producer did not use a fresh isolated Cargo target.'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].TranscribeCmakeArgs (Get-WindowsCpuWorkerBaselineCmakeArgs) 'Freeze producer exact native CMake baseline'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].RustFlags '-C target-feature=+crt-static' 'Freeze producer exact Rust ISA baseline'
    Assert-True ([string]::IsNullOrEmpty($global:WindowsFrozenCpuWorkerTestCargoCalls[0].WorkerDigest)) 'Freeze producer did not clear the desktop worker digest.'
    Assert-Equal $env:SCRIBE_BUILD_REVISION 'inherited-test-revision' 'Freeze producer revision environment restoration'
    Assert-Equal $env:SCRIBE_BUNDLED_WORKER_SHA256 ('f' * 64 -join '') 'Freeze producer digest environment restoration'
    Assert-Equal $env:SCRIBE_BUILDING_WORKER 'inherited-worker-flag' 'Freeze producer worker marker environment restoration'
    Assert-WindowsFrozenCpuWorkerDirectoryInventory $producerOutput
    $producerRecordPath = Join-Path $producerOutput (Get-WindowsFrozenCpuWorkerRecordFileName)
    $validatedFrozenWorker = Open-ValidatedWindowsFrozenCpuWorker $producerRecordPath $fixtureRoot
    try {
        Assert-Equal $validatedFrozenWorker.Record.source_revision $fixtureContext.SourceRevision 'Freeze record source revision'
        Assert-Equal $validatedFrozenWorker.Record.protocol_version 5 'Freeze record protocol version'
        Assert-Equal $validatedFrozenWorker.Record.worker_abi_version 1 'Freeze record worker ABI version'
    }
    finally {
        $validatedFrozenWorker.WorkerStream.Dispose()
    }
    $producerRecord = Get-Content -LiteralPath $producerRecordPath -Raw | ConvertFrom-Json
    Set-FixtureCompiledAdmissionResponse $fixtureContext $fixtureContext $producerRecord
    Set-FixturePolicyIdentityResponse $fixtureContext.DesktopBuildId
    $producerCallsBeforeExistingOutput = $global:WindowsFrozenCpuWorkerTestCargoCalls.Count
    Invoke-ExpectedFailure { & $fixtureProducer -OutputDirectory $producerOutput } 'already exists'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count $producerCallsBeforeExistingOutput 'Existing freeze output invoked Cargo'

    $nestedBundleParent = Join-Path $producerOutput 'must-not-be-created'
    Invoke-ExpectedFailure {
        Invoke-FrozenConsumer $producerRecordPath (Join-Path $nestedBundleParent 'bundle')
    } 'inputs cannot contain'
    Assert-True (-not (Test-Path -LiteralPath $nestedBundleParent)) 'Rejected nested bundle output changed the frozen input directory.'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count $producerCallsBeforeExistingOutput 'Overlapping frozen output invoked Cargo'
    Assert-WindowsFrozenCpuWorkerDirectoryInventory $producerOutput

    foreach ($name in @('GITHUB_ACTIONS', 'CI')) {
        Set-Item -LiteralPath "Env:$name" -Value 'true'
        try {
            Invoke-ExpectedFailure {
                & $fixtureProducer -OutputDirectory (Join-Path $testRoot "hosted-producer-$name")
            } 'local-only'
            Invoke-ExpectedFailure {
                Invoke-FrozenConsumer $producerRecordPath (Join-Path $testRoot "hosted-consumer-$name")
            } 'local-only'
            $hostedObservationBundle = Join-Path $testRoot "hosted-observation-$name"
            Invoke-ExpectedFailure {
                & $fixtureBuilder -ModelSource $modelSource -BundlePath $hostedObservationBundle `
                    -InstallerPackAllowlistPath $installerAllowlist `
                    -FrozenCpuWorkerRecordPath $producerRecordPath -LocalFrozenGpuObservation
            } 'local-only'
            Assert-True (-not (Test-Path -LiteralPath $hostedObservationBundle)) 'Hosted GPU observation rejection left an output.'
            Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count $producerCallsBeforeExistingOutput 'Hosted frozen entry point invoked Cargo'
            Assert-Equal ([Environment]::GetEnvironmentVariable($name)) 'true' 'Hosted-entry rejection changed caller environment'
        }
        finally {
            Remove-Item -LiteralPath "Env:$name"
        }
    }

    foreach ($name in @(
        'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR',
        'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_NAMESPACE'
    )) {
        Set-Item -LiteralPath "Env:$name" -Value 'fixture-repository-override'
        try {
            Invoke-ExpectedFailure {
                & $fixtureProducer -OutputDirectory (Join-Path $testRoot "rejected-$name")
            } 'does not accept Git repository overrides'
            Assert-Equal ([Environment]::GetEnvironmentVariable($name)) 'fixture-repository-override' 'Rejected Git override was modified'
            Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count $producerCallsBeforeExistingOutput 'Git override invoked Cargo'
        }
        finally {
            Remove-Item -LiteralPath "Env:$name"
        }
    }
    & git -C $fixtureRoot config --local core.worktree $testRoot
    if ($LASTEXITCODE -ne 0) { throw 'Could not configure the test-owned alternate worktree.' }
    try {
        Invoke-ExpectedFailure { Get-WindowsFrozenCpuWorkerSourceContext $fixtureRoot } 'Git top-level does not match'
    }
    finally {
        & git -C $fixtureRoot config --local --unset core.worktree
        if ($LASTEXITCODE -ne 0) { throw 'Could not restore the test-owned worktree configuration.' }
    }

    $global:WindowsFrozenCpuWorkerTestFailWorkerBuild = $true
    $failedFreezeOutput = Join-Path $testRoot 'failed-freeze'
    Invoke-ExpectedFailure { & $fixtureProducer -OutputDirectory $failedFreezeOutput } 'release build failed'
    Assert-True (-not (Test-Path -LiteralPath $failedFreezeOutput)) 'Failed worker build left a freeze output.'
    $global:WindowsFrozenCpuWorkerTestFailWorkerBuild = $false

    $global:WindowsFrozenCpuWorkerTestRaceFreezeOutput = Join-Path $testRoot 'raced-freeze'
    Invoke-ExpectedFailure { & $fixtureProducer -OutputDirectory $global:WindowsFrozenCpuWorkerTestRaceFreezeOutput } 'appeared during construction'
    Assert-True (Test-Path -LiteralPath $global:WindowsFrozenCpuWorkerTestRaceFreezeOutput) 'Freeze output race fixture was not created.'
    $global:WindowsFrozenCpuWorkerTestRaceFreezeOutput = $null

    $global:WindowsFrozenCpuWorkerTestSourceDriftPath = Join-Path $fixtureRoot 'source-drift.txt'
    $driftFreezeOutput = Join-Path $testRoot 'drift-freeze'
    Invoke-ExpectedFailure { & $fixtureProducer -OutputDirectory $driftFreezeOutput } 'requires a clean source workspace'
    Assert-True (-not (Test-Path -LiteralPath $driftFreezeOutput)) 'Source-drift producer failure left an output.'
    Remove-Item -LiteralPath $global:WindowsFrozenCpuWorkerTestSourceDriftPath -Force
    $global:WindowsFrozenCpuWorkerTestSourceDriftPath = $null

    $record = Get-Content -LiteralPath $producerRecordPath -Raw | ConvertFrom-Json
    $workerPath = Join-Path $producerOutput (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
    foreach ($mutation in @(
        @{ Name = 'source'; Property = 'source_revision'; Value = ('a' * 40); Expected = 'source context' },
        @{ Name = 'protocol'; Property = 'protocol_version'; Value = 4; Expected = 'source context' },
        @{ Name = 'abi'; Property = 'worker_abi_version'; Value = 2; Expected = 'source context' },
        @{ Name = 'schema-type'; Property = 'schema_version'; Value = '1'; Expected = 'non-canonical scalar types' },
        @{ Name = 'protocol-type'; Property = 'protocol_version'; Value = '5'; Expected = 'non-canonical scalar types' },
        @{ Name = 'local-only-type'; Property = 'local_only'; Value = 'true'; Expected = 'non-canonical scalar types' },
        @{ Name = 'size-fraction'; Property = 'worker_size_bytes'; Value = 1.5; Expected = 'non-canonical scalar types' },
        @{ Name = 'hash-type'; Property = 'worker_sha256'; Value = 123; Expected = 'must be a string' }
    )) {
        $mutated = Copy-FrozenFixture "record-$($mutation.Name)"
        $mutatedRecordPath = Join-Path $mutated (Get-WindowsFrozenCpuWorkerRecordFileName)
        $mutatedRecord = Get-Content -LiteralPath $mutatedRecordPath -Raw | ConvertFrom-Json
        $mutatedRecord.($mutation.Property) = $mutation.Value
        [System.IO.File]::WriteAllText($mutatedRecordPath, ($mutatedRecord | ConvertTo-Json -Depth 5), [System.Text.UTF8Encoding]::new($false))
        Invoke-ExpectedFailure { Open-ValidatedWindowsFrozenCpuWorker $mutatedRecordPath $fixtureRoot } $mutation.Expected
    }

    $sizeMismatch = Copy-FrozenFixture 'record-size-mismatch'
    $sizeMismatchWorker = Join-Path $sizeMismatch (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
    $sizeMismatchHash = Get-WindowsFrozenCpuWorkerFileSha256 $sizeMismatchWorker
    Write-CanonicalFrozenRecord $sizeMismatch $fixtureContext ((Get-Item -LiteralPath $sizeMismatchWorker).Length + 1) $sizeMismatchHash
    Invoke-ExpectedFailure { Open-ValidatedWindowsFrozenCpuWorker (Join-Path $sizeMismatch (Get-WindowsFrozenCpuWorkerRecordFileName)) $fixtureRoot } 'recorded size'

    $digestMismatch = Copy-FrozenFixture 'record-digest-mismatch'
    $digestMismatchWorker = Join-Path $digestMismatch (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
    Write-CanonicalFrozenRecord $digestMismatch $fixtureContext (Get-Item -LiteralPath $digestMismatchWorker).Length ('0' * 64)
    Invoke-ExpectedFailure { Open-ValidatedWindowsFrozenCpuWorker (Join-Path $digestMismatch (Get-WindowsFrozenCpuWorkerRecordFileName)) $fixtureRoot } 'recorded SHA-256'

    $byteMutation = Copy-FrozenFixture 'worker-byte-mutation'
    $byteMutationWorker = Join-Path $byteMutation (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
    $byteMutationBytes = [System.IO.File]::ReadAllBytes($byteMutationWorker)
    $byteMutationBytes[800] = $byteMutationBytes[800] -bxor 0x01
    [System.IO.File]::WriteAllBytes($byteMutationWorker, $byteMutationBytes)
    Invoke-ExpectedFailure { Open-ValidatedWindowsFrozenCpuWorker (Join-Path $byteMutation (Get-WindowsFrozenCpuWorkerRecordFileName)) $fixtureRoot } 'recorded SHA-256'

    $duplicateMember = Copy-FrozenFixture 'record-duplicate-member'
    $duplicateRecordPath = Join-Path $duplicateMember (Get-WindowsFrozenCpuWorkerRecordFileName)
    $duplicateJson = Get-Content -LiteralPath $duplicateRecordPath -Raw
    $duplicateJson = $duplicateJson.Replace('"schema_version": 1,', "`"schema_version`": 1,`r`n  `"schema_version`": 1,")
    [System.IO.File]::WriteAllText($duplicateRecordPath, $duplicateJson, [System.Text.UTF8Encoding]::new($false))
    Invoke-ExpectedFailure { Open-ValidatedWindowsFrozenCpuWorker $duplicateRecordPath $fixtureRoot } 'canonical local integrity encoding'

    $extraInput = Copy-FrozenFixture 'extra-input'
    [System.IO.File]::WriteAllText((Join-Path $extraInput 'extra.txt'), 'extra')
    Invoke-ExpectedFailure { Open-ValidatedWindowsFrozenCpuWorker (Join-Path $extraInput (Get-WindowsFrozenCpuWorkerRecordFileName)) $fixtureRoot } 'exactly'

    $adsInput = Copy-FrozenFixture 'ads-input'
    $adsWorker = Join-Path $adsInput (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
    [System.IO.File]::WriteAllText("$adsWorker`:frozen-test", 'ads')
    try {
        Invoke-ExpectedFailure { Open-ValidatedWindowsFrozenCpuWorker (Join-Path $adsInput (Get-WindowsFrozenCpuWorkerRecordFileName)) $fixtureRoot } 'alternate data stream'
    }
    finally {
        [System.IO.File]::Delete("$adsWorker`:frozen-test")
    }

    $reparseInput = Copy-FrozenFixture 'reparse-input'
    $reparseLink = Join-Path $reparseInput 'extra-link'
    New-Item -ItemType Junction -Path $reparseLink -Target $testRoot | Out-Null
    Invoke-ExpectedFailure { Open-ValidatedWindowsFrozenCpuWorker (Join-Path $reparseInput (Get-WindowsFrozenCpuWorkerRecordFileName)) $fixtureRoot } 'non-regular item'
    Remove-Item -LiteralPath $reparseLink -Force

    # Physical NTFS case-sensitive directory coverage is not part of this
    # unprivileged suite; do not change filesystem policy just to run a unit test.
    # Exercise the real inventory validator with a deterministic test-owned
    # enumeration seam instead; every returned FullName remains a real regular
    # file, so its ADS validation still executes against the filesystem.
    Write-Output 'UNRUN: physical case-sensitive-directory collision integration; deterministic inventory-enumeration coverage follows.'
    $caseInput = Copy-FrozenFixture 'synthetic-case-input'
    $caseWorker = Get-Item -LiteralPath (Join-Path $caseInput (Get-WindowsFrozenCpuWorkerExecutableRelativePath))
    $caseRecord = Get-Item -LiteralPath (Join-Path $caseInput (Get-WindowsFrozenCpuWorkerRecordFileName))
    $caseMarker = Get-Item -LiteralPath (Join-Path $caseInput (Get-WindowsFrozenCpuWorkerMarkerFileName))
    $global:WindowsFrozenCpuWorkerSyntheticCollisionRoot = $caseInput
    $global:WindowsFrozenCpuWorkerSyntheticCollisionItems = @(
        [pscustomobject]@{ Name = $caseWorker.Name; FullName = $caseWorker.FullName; PSIsContainer = $false; Attributes = $caseWorker.Attributes },
        [pscustomobject]@{ Name = 'SCRIBE-INFERENCE-WORKER.EXE'; FullName = $caseWorker.FullName; PSIsContainer = $false; Attributes = $caseWorker.Attributes },
        [pscustomobject]@{ Name = $caseRecord.Name; FullName = $caseRecord.FullName; PSIsContainer = $false; Attributes = $caseRecord.Attributes },
        [pscustomobject]@{ Name = $caseMarker.Name; FullName = $caseMarker.FullName; PSIsContainer = $false; Attributes = $caseMarker.Attributes }
    )
    $previousGlobalGetChildItem = Get-Item -LiteralPath Function:\Get-ChildItem -ErrorAction SilentlyContinue
    $previousGlobalGetChildItemScriptBlock = if ($null -ne $previousGlobalGetChildItem) { $previousGlobalGetChildItem.ScriptBlock } else { $null }
    try {
        function global:Get-ChildItem {
            [CmdletBinding()]
            param(
                [string]$LiteralPath,
                [switch]$Force,
                [switch]$Recurse,
                [switch]$File,
                [switch]$Directory,
                [Parameter(ValueFromRemainingArguments = $true)]
                [object[]]$RemainingArguments
            )
            if ($LiteralPath -ceq $global:WindowsFrozenCpuWorkerSyntheticCollisionRoot -and -not $Recurse) {
                return $global:WindowsFrozenCpuWorkerSyntheticCollisionItems
            }
            $forwarded = @{}
            foreach ($name in @('LiteralPath', 'Force', 'Recurse', 'File', 'Directory')) {
                if ($PSBoundParameters.ContainsKey($name)) {
                    $forwarded[$name] = $PSBoundParameters[$name]
                }
            }
            return Microsoft.PowerShell.Management\Get-ChildItem @forwarded
        }
        Invoke-ExpectedFailure { Open-ValidatedWindowsFrozenCpuWorker (Join-Path $caseInput (Get-WindowsFrozenCpuWorkerRecordFileName)) $fixtureRoot } 'case-insensitive filename collision'
    }
    finally {
        if ($null -ne $previousGlobalGetChildItem) {
            Set-Item -LiteralPath Function:\Get-ChildItem -Value $previousGlobalGetChildItemScriptBlock
        }
        else {
            Remove-Item -LiteralPath Function:\Get-ChildItem -ErrorAction SilentlyContinue
        }
        Assert-Equal (Get-CommandIdentity 'Get-ChildItem') $originalGetChildItemCommandIdentity 'Synthetic enumeration seam command restoration'
        $global:WindowsFrozenCpuWorkerSyntheticCollisionRoot = $null
        $global:WindowsFrozenCpuWorkerSyntheticCollisionItems = $null
    }

    Reset-TestCalls
    $frozenBundle = Join-Path $testRoot 'frozen-bundle'
    Invoke-FrozenConsumer $producerRecordPath $frozenBundle
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 1 'Frozen consumer Cargo call count'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].Binary 'local-transcriber' 'Frozen consumer desktop-only build'
    Assert-DesktopCargoArguments $global:WindowsFrozenCpuWorkerTestCargoCalls[0] 'ui-harness' 'Frozen default desktop exact feature argv'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].Revision $fixtureContext.SourceRevision 'Frozen consumer exact build revision'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].BuildingWorker $null 'Frozen consumer worker marker clearing'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].WorkerDigest $record.worker_sha256 'Frozen consumer exact worker anchor'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestAdmissionCalls.Count 1 'Frozen consumer compiled admission invocation count'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestPolicyCalls.Count 3 'Frozen consumer did not retain all three compiled GPU Auto policy identity gates.'
    foreach ($call in $global:WindowsFrozenCpuWorkerTestPolicyCalls) {
        Assert-Equal $call.Command '--scribe-windows-gpu-auto-policy-identity' 'Frozen consumer policy identity command'
    }
    Assert-Equal $env:SCRIBE_BUILD_REVISION 'inherited-test-revision' 'Frozen consumer revision environment restoration'
    Assert-Equal $env:SCRIBE_BUNDLED_WORKER_SHA256 ('f' * 64 -join '') 'Frozen consumer digest environment restoration'
    Assert-Equal $env:SCRIBE_BUILDING_WORKER 'inherited-worker-flag' 'Frozen consumer worker marker environment restoration'
    Assert-True (Test-Path -LiteralPath (Join-Path $frozenBundle (Get-WindowsFrozenCpuWorkerMarkerFileName))) 'Frozen bundle omitted its local-only marker.'
    $stagedWorker = Join-Path $frozenBundle (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
    Assert-Equal (Get-WindowsFrozenCpuWorkerFileSha256 $stagedWorker) $record.worker_sha256 'Frozen consumer staged exact worker bytes'

    # A distinct retained R may have the exact same revision/contracts as M;
    # containment is a checkout-root property, not an identity-label property.
    $sameCommitWorkerSourceRoot = Join-Path $testRoot 'same-commit-worker-source'
    & git clone --quiet --no-local $fixtureRoot $sameCommitWorkerSourceRoot
    if ($LASTEXITCODE -ne 0) { throw 'Could not create same-commit retained worker source fixture.' }
    $previousFixtureTemp = [Environment]::GetEnvironmentVariable('TEMP')
    $sameCommitIgnoredTemp = Join-Path $sameCommitWorkerSourceRoot 'target\temporary-output'
    try {
        [Environment]::SetEnvironmentVariable('TEMP', $sameCommitIgnoredTemp)
        Set-FixtureCompiledAdmissionResponse $fixtureContext $fixtureContext $record
        Reset-TestCalls
        Invoke-ExpectedFailure {
            Invoke-FrozenConsumer $producerRecordPath (Join-Path $testRoot 'same-commit-r-temp-bundle') $sameCommitWorkerSourceRoot
        } 'cannot contain temporary output paths'
        Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 'Same-commit foreign temporary-root rejection invoked Cargo'
        Assert-True (-not (Test-Path -LiteralPath $sameCommitIgnoredTemp)) 'Same-commit foreign temporary-root rejection wrote ignored R output.'
    }
    finally {
        [Environment]::SetEnvironmentVariable('TEMP', $previousFixtureTemp)
    }

    # A relative TEMP/TMP would be rebased by the builder's later Push-Location
    # to M. Reject both relative and drive-relative values before resolving a
    # temporary root, invoking Cargo, or creating an M output directory.
    $previousFixtureTemp = [Environment]::GetEnvironmentVariable('TEMP')
    $previousFixtureTmp = [Environment]::GetEnvironmentVariable('TMP')
    try {
        foreach ($temporaryCase in @(
            @{ Name = 'TEMP'; Value = 'relative-frozen-worker-temp' },
            @{ Name = 'TMP'; Value = 'C:drive-relative-frozen-worker-temp' }
        )) {
            [Environment]::SetEnvironmentVariable('TEMP', $previousFixtureTemp)
            [Environment]::SetEnvironmentVariable('TMP', $previousFixtureTmp)
            [Environment]::SetEnvironmentVariable($temporaryCase.Name, $temporaryCase.Value)
            $relativeTemporaryBundle = Join-Path $testRoot "same-commit-r-$($temporaryCase.Name.ToLowerInvariant())-relative-temp-bundle"
            Reset-TestCalls
            Invoke-ExpectedFailure {
                Invoke-FrozenConsumer $producerRecordPath $relativeTemporaryBundle $sameCommitWorkerSourceRoot
            } 'fully qualified temporary output paths'
            Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 "$($temporaryCase.Name) relative temporary-root rejection invoked Cargo"
            Assert-True (-not (Test-Path -LiteralPath $relativeTemporaryBundle)) "$($temporaryCase.Name) relative temporary-root rejection created an M output directory"
        }
    }
    finally {
        [Environment]::SetEnvironmentVariable('TEMP', $previousFixtureTemp)
        [Environment]::SetEnvironmentVariable('TMP', $previousFixtureTmp)
    }

    # M builds the desktop while R is a separately clean, data-only worker
    # source. Its local builder is deliberately poisoned: any execution of R
    # instead of M would fail this synthetic assembly seam.
    $foreignWorkerSourceRoot = New-ForeignFrozenWorkerSource (Join-Path $testRoot 'foreign-worker-source')
    $foreignWorkerContext = Get-WindowsFrozenCpuWorkerSourceContext $foreignWorkerSourceRoot
    Assert-True (-not (Test-WindowsFrozenCpuWorkerSameSourceContext $fixtureContext $foreignWorkerContext)) 'Foreign worker source did not receive an independent identity.'
    $foreignFreeze = Join-Path $testRoot 'foreign-worker-freeze'
    New-Item -ItemType Directory -Path $foreignFreeze | Out-Null
    $foreignWorkerPath = Join-Path $foreignFreeze (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
    Copy-Item -LiteralPath $workerPath -Destination $foreignWorkerPath
    $foreignWorkerHash = Get-WindowsFrozenCpuWorkerFileSha256 $foreignWorkerPath
    Write-CanonicalFrozenRecord $foreignFreeze $foreignWorkerContext (Get-Item -LiteralPath $foreignWorkerPath).Length $foreignWorkerHash
    $foreignRecordPath = Join-Path $foreignFreeze (Get-WindowsFrozenCpuWorkerRecordFileName)
    $foreignRecord = Get-Content -LiteralPath $foreignRecordPath -Raw | ConvertFrom-Json

    # A distinct R must stay read-only even for Git-ignored locations. Reject
    # all M outputs before creating directories or invoking Cargo.
    $previousFixtureCargoTarget = $env:CARGO_TARGET_DIR
    $foreignIgnoredCargoTarget = Join-Path $foreignWorkerSourceRoot 'target\m-build-output'
    try {
        $env:CARGO_TARGET_DIR = $foreignIgnoredCargoTarget
        Reset-TestCalls
        Invoke-ExpectedFailure {
            Invoke-FrozenConsumer $foreignRecordPath (Join-Path $testRoot 'foreign-r-target-bundle') $foreignWorkerSourceRoot
        } 'cannot contain Cargo build output'
        Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 'Foreign ignored Cargo target rejection invoked Cargo'
        Assert-True (-not (Test-Path -LiteralPath $foreignIgnoredCargoTarget)) 'Foreign ignored Cargo target rejection wrote R.'
    }
    finally { $env:CARGO_TARGET_DIR = $previousFixtureCargoTarget }
    $foreignParentCargoTarget = Split-Path -Parent $foreignWorkerSourceRoot
    $foreignParentTargetBundle = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-foreign-parent-target-$([guid]::NewGuid().ToString('N'))"
    try {
        $env:CARGO_TARGET_DIR = $foreignParentCargoTarget
        Reset-TestCalls
        Invoke-ExpectedFailure {
            Invoke-FrozenConsumer $foreignRecordPath $foreignParentTargetBundle $foreignWorkerSourceRoot
        } 'cannot contain Cargo build output'
        Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 'Foreign parent Cargo target rejection invoked Cargo'
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $foreignWorkerSourceRoot 'x86_64-pc-windows-msvc'))) 'Foreign parent Cargo target rejection wrote R.'
        Assert-True (-not (Test-Path -LiteralPath $foreignParentTargetBundle)) 'Foreign parent Cargo target rejection created its external bundle output.'
    }
    finally { $env:CARGO_TARGET_DIR = $previousFixtureCargoTarget }
    Reset-TestCalls
    $foreignBundleOutput = Join-Path $foreignWorkerSourceRoot 'bundle-output'
    Invoke-ExpectedFailure {
        Invoke-FrozenConsumer $foreignRecordPath $foreignBundleOutput $foreignWorkerSourceRoot
    } 'cannot contain bundle, staging, or installer-allowlist outputs'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 'Foreign bundle output rejection invoked Cargo'
    Assert-True (-not (Test-Path -LiteralPath $foreignBundleOutput)) 'Foreign bundle output rejection wrote R.'
    $foreignAllowlistOutput = Join-Path $foreignWorkerSourceRoot 'worker-pack-allowlist.iss'
    Reset-TestCalls
    Invoke-ExpectedFailure {
        & $fixtureBuilder `
            -ModelSource $modelSource `
            -BundlePath (Join-Path $testRoot 'foreign-r-allowlist-bundle') `
            -InstallerPackAllowlistPath $foreignAllowlistOutput `
            -FrozenCpuWorkerRecordPath $foreignRecordPath `
            -FrozenCpuWorkerSourceRoot $foreignWorkerSourceRoot
    } 'cannot contain bundle, staging, or installer-allowlist outputs'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 'Foreign installer-allowlist rejection invoked Cargo'
    Assert-True (-not (Test-Path -LiteralPath $foreignAllowlistOutput)) 'Foreign installer-allowlist rejection wrote R.'
    Assert-Equal (@(git -C $foreignWorkerSourceRoot status --porcelain=v1 --untracked-files=all)).Count 0 'Foreign worker source changed during rejected M output requests'

    # The retained R checkout must stay read-only even when a hostile ambient
    # temporary root points at its Git-ignored target directory.
    $previousFixtureTemp = [Environment]::GetEnvironmentVariable('TEMP')
    $foreignIgnoredTemp = Join-Path $foreignWorkerSourceRoot 'target\temporary-output'
    try {
        [Environment]::SetEnvironmentVariable('TEMP', $foreignIgnoredTemp)
        Reset-TestCalls
        Invoke-ExpectedFailure {
            Invoke-FrozenConsumer $foreignRecordPath (Join-Path $testRoot 'foreign-r-temp-bundle') $foreignWorkerSourceRoot
        } 'cannot contain temporary output paths'
        Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 'Foreign temporary-root rejection invoked Cargo'
        Assert-True (-not (Test-Path -LiteralPath $foreignIgnoredTemp)) 'Foreign temporary-root rejection wrote ignored R output.'
    }
    finally {
        [Environment]::SetEnvironmentVariable('TEMP', $previousFixtureTemp)
    }

    # Source-context discovery must reject an external Git clean/process filter
    # before status can consult anything defined by R's checkout configuration.
    try {
        & git -C $foreignWorkerSourceRoot config filter.fixture.clean 'C:\fixture-filter-must-not-run.exe'
        if ($LASTEXITCODE -ne 0) { throw 'Could not configure foreign fixture Git filter.' }
        Reset-TestCalls
        Invoke-ExpectedFailure {
            Invoke-FrozenConsumer $foreignRecordPath (Join-Path $testRoot 'foreign-r-filter-bundle') $foreignWorkerSourceRoot
        } 'contains an external clean or process filter'
        Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 'Foreign Git-filter rejection invoked Cargo'
    }
    finally {
        & git -C $foreignWorkerSourceRoot config --unset-all filter.fixture.clean
        if ($LASTEXITCODE -ne 0) { throw 'Could not clear foreign fixture Git filter.' }
    }

    # An empty compiled map reports only M's strict legacy expectation. It
    # cannot admit a foreign R before the desktop build reaches publication.
    Set-FixtureCompiledAdmissionResponse $fixtureContext $fixtureContext $foreignRecord
    Reset-TestCalls
    $defaultEmptyForeignBundle = Join-Path $testRoot 'default-empty-foreign-bundle'
    Invoke-ExpectedFailure {
        Invoke-FrozenConsumer $foreignRecordPath $defaultEmptyForeignBundle $foreignWorkerSourceRoot
    } 'compiled admission does not match'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 1 'Default-empty foreign admission rebuilt a worker or skipped the desktop build'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].Binary 'local-transcriber' 'Default-empty foreign admission did not build only M'
    Assert-True (-not (Test-Path -LiteralPath $defaultEmptyForeignBundle)) 'Default-empty foreign admission published a bundle.'

    # A synthetic report stands in for an exact compiled foreign approval. The
    # fixture proves M owns the build revision and R's worker bytes remain
    # unchanged without running any R helper.
    Set-FixtureCompiledAdmissionResponse $fixtureContext $foreignWorkerContext $foreignRecord
    Reset-TestCalls
    $foreignBundle = Join-Path $testRoot 'foreign-worker-bundle'
    Invoke-FrozenConsumer $foreignRecordPath $foreignBundle $foreignWorkerSourceRoot
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 1 'Foreign frozen assembly did not build exactly one desktop'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].Binary 'local-transcriber' 'Foreign frozen assembly rebuilt the worker'
    Assert-DesktopCargoArguments $global:WindowsFrozenCpuWorkerTestCargoCalls[0] 'ui-harness' 'Foreign frozen assembly built a non-M desktop'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].Revision $fixtureContext.SourceRevision 'Foreign frozen assembly inherited R or caller revision'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestAdmissionCalls.Count 1 'Foreign frozen assembly did not gate publication on compiled admission'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestPolicyCalls.Count 3 'Foreign frozen assembly did not retain all three compiled GPU Auto policy identity gates.'
    foreach ($call in $global:WindowsFrozenCpuWorkerTestPolicyCalls) {
        Assert-Equal $call.Command '--scribe-windows-gpu-auto-policy-identity' 'Foreign frozen assembly policy identity command'
    }
    Assert-Equal (Get-WindowsFrozenCpuWorkerFileSha256 (Join-Path $foreignBundle (Get-WindowsFrozenCpuWorkerExecutableRelativePath))) $foreignRecord.worker_sha256 'Foreign frozen assembly changed R worker bytes'

    foreach ($mismatch in @(
        @{ Name = 'desktop M'; Property = 'desktop_build_id'; Value = 'local-transcriber@0.1.0#wrong-m'; Expected = 'compiled admission does not match' },
        @{ Name = 'worker R'; Property = 'worker_origin_app_build'; Value = 'local-transcriber@0.1.0#wrong-r'; Expected = 'compiled admission does not match' },
        @{ Name = 'worker hash'; Property = 'bundled_worker_sha256'; Value = ('0' * 64); Expected = 'compiled admission does not match' },
        @{ Name = 'protocol'; Property = 'protocol_version'; Value = [int64]4; Expected = 'compiled admission does not match' },
        @{ Name = 'worker hash array'; Property = 'bundled_worker_sha256'; Value = @($foreignRecord.worker_sha256); Expected = 'must be a string' }
    )) {
        $mismatchedReport = $global:WindowsFrozenCpuWorkerTestAdmissionResponse.Stdout | ConvertFrom-Json
        $mismatchedReport.($mismatch.Property) = $mismatch.Value
        $global:WindowsFrozenCpuWorkerTestAdmissionResponse = [pscustomobject]@{
            ExitCode = 0
            Stdout = ($mismatchedReport | ConvertTo-Json -Compress)
            Stderr = ''
        }
        Reset-TestCalls
        $mismatchBundle = Join-Path $testRoot ("foreign-admission-mismatch-" + ($mismatch.Name -replace ' ', '-'))
        Invoke-ExpectedFailure {
            Invoke-FrozenConsumer $foreignRecordPath $mismatchBundle $foreignWorkerSourceRoot
        } $mismatch.Expected
        Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 1 "Foreign $($mismatch.Name) mismatch rebuilt a worker or skipped M"
        Assert-True (-not (Test-Path -LiteralPath $mismatchBundle)) "Foreign $($mismatch.Name) mismatch published a bundle."
        Set-FixtureCompiledAdmissionResponse $fixtureContext $foreignWorkerContext $foreignRecord
    }

    $admissionObjectReport = $global:WindowsFrozenCpuWorkerTestAdmissionResponse.Stdout
    $global:WindowsFrozenCpuWorkerTestAdmissionResponse = [pscustomobject]@{
        ExitCode = 0
        Stdout = "[$admissionObjectReport]"
        Stderr = ''
    }
    Reset-TestCalls
    $arrayReportBundle = Join-Path $testRoot 'foreign-admission-array-report'
    Invoke-ExpectedFailure {
        Invoke-FrozenConsumer $foreignRecordPath $arrayReportBundle $foreignWorkerSourceRoot
    } 'must be one JSON object'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 1 'Array-shaped foreign admission report rebuilt a worker or skipped M'
    Assert-True (-not (Test-Path -LiteralPath $arrayReportBundle)) 'Array-shaped foreign admission report published a bundle.'
    Set-FixtureCompiledAdmissionResponse $fixtureContext $foreignWorkerContext $foreignRecord

    $global:WindowsFrozenCpuWorkerTestSourceDriftPath = Join-Path $foreignWorkerSourceRoot 'source-drift.txt'
    Reset-TestCalls
    $foreignSourceDriftBundle = Join-Path $testRoot 'foreign-source-drift-bundle'
    Invoke-ExpectedFailure {
        Invoke-FrozenConsumer $foreignRecordPath $foreignSourceDriftBundle $foreignWorkerSourceRoot
    } 'requires a clean source workspace'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 1 'Foreign source drift was not rechecked after M desktop build'
    Assert-True (-not (Test-Path -LiteralPath $foreignSourceDriftBundle)) 'Foreign source drift published a bundle.'
    Remove-Item -LiteralPath $global:WindowsFrozenCpuWorkerTestSourceDriftPath -Force
    $global:WindowsFrozenCpuWorkerTestSourceDriftPath = $null
    Set-FixtureCompiledAdmissionResponse $fixtureContext $fixtureContext $record

    Reset-TestCalls
    $observationBundle = Join-Path $testRoot 'frozen-observation-bundle'
    & $fixtureBuilder -ModelSource $modelSource -BundlePath $observationBundle `
        -InstallerPackAllowlistPath $installerAllowlist `
        -FrozenCpuWorkerRecordPath $producerRecordPath -LocalFrozenGpuObservation
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 1 'Frozen observation must not rebuild the CPU worker'
    $observationCall = $global:WindowsFrozenCpuWorkerTestCargoCalls[0]
    Assert-Equal $observationCall.Binary 'local-transcriber' 'Frozen observation desktop-only build'
    Assert-DesktopCargoArguments $observationCall 'ui-harness,windows-gpu-capture-observation' 'Frozen observation exact desktop-only feature argv'
    Assert-Equal $observationCall.WorkerDigest $record.worker_sha256 'Frozen observation exact CPU worker anchor'
    Assert-Equal $observationCall.Revision $fixtureContext.SourceRevision 'Frozen observation exact build revision'
    Assert-Equal $observationCall.BuildingWorker $null 'Frozen observation clears the worker build marker'
    $observedWorker = Join-Path $observationBundle (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
    Assert-Equal (Get-WindowsFrozenCpuWorkerFileSha256 $observedWorker) $record.worker_sha256 'Frozen observation stages unchanged CPU worker bytes'
    Assert-Equal $env:SCRIBE_BUILD_REVISION 'inherited-test-revision' 'Frozen observation revision environment restoration'
    Assert-Equal $env:SCRIBE_BUNDLED_WORKER_SHA256 ('f' * 64 -join '') 'Frozen observation digest environment restoration'
    Assert-Equal $env:SCRIBE_BUILDING_WORKER 'inherited-worker-flag' 'Frozen observation worker marker environment restoration'

    Reset-TestCalls
    $defaultAllowlistBundle = Join-Path $testRoot 'default-allowlist-bundle'
    & $fixtureBuilder -ModelSource $modelSource -BundlePath $defaultAllowlistBundle -FrozenCpuWorkerRecordPath $producerRecordPath
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 1 'Default frozen allowlist path desktop-only build'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $defaultAllowlistBundle 'worker-pack-allowlist.iss'))) 'Transient installer allowlist leaked into the frozen bundle.'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $fixtureRoot 'dist\worker-pack-allowlist.iss'))) 'Frozen packaging wrote the normal release allowlist into the source checkout.'
    Assert-WindowsFrozenCpuWorkerContextUnchanged $fixtureContext

    Reset-TestCalls
    Invoke-ExpectedFailure {
        Invoke-FrozenConsumer (Join-Path $sizeMismatch (Get-WindowsFrozenCpuWorkerRecordFileName)) (Join-Path $testRoot 'invalid-input-bundle')
    } 'recorded size'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 'Invalid frozen input invoked Cargo'

    $global:WindowsFrozenCpuWorkerTestFailDesktopBuild = $true
    try {
        Invoke-ExpectedFailure { Invoke-FrozenConsumer $producerRecordPath (Join-Path $testRoot 'failed-desktop-bundle') } 'desktop release build failed'
    }
    finally {
        $global:WindowsFrozenCpuWorkerTestFailDesktopBuild = $false
    }
    $releasedWorker = [System.IO.File]::Open($workerPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
    $releasedWorker.Dispose()
    Assert-Equal $env:SCRIBE_BUILD_REVISION 'inherited-test-revision' 'Failed frozen desktop build revision restoration'
    Assert-Equal $env:SCRIBE_BUNDLED_WORKER_SHA256 ('f' * 64 -join '') 'Failed frozen desktop build digest restoration'
    Assert-Equal $env:SCRIBE_BUILDING_WORKER 'inherited-worker-flag' 'Failed frozen desktop build worker marker restoration'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $testRoot 'failed-desktop-bundle'))) 'Failed frozen desktop build left a final bundle.'

    $verifierSource = Get-Content -LiteralPath (Join-Path $repositoryRoot 'scripts\verify-windows-release-package.ps1') -Raw
    $preambleStart = $verifierSource.IndexOf('$targetTriple =')
    $helpersStart = $verifierSource.IndexOf('function Get-NormalizedPath')
    $helpersEnd = $verifierSource.IndexOf('$bundle = Get-NormalizedPath')
    if ($preambleStart -lt 0 -or $helpersStart -le $preambleStart -or $helpersEnd -le $helpersStart) {
        throw 'Could not isolate the unchanged production package verifier helpers.'
    }
    $verifierPreamble = $verifierSource.Substring($preambleStart, $helpersStart - $preambleStart)
    $quotedScriptRoot = (Join-Path $repositoryRoot 'scripts').Replace("'", "''")
    $verifierPreamble = $verifierPreamble.Replace('$PSScriptRoot', "'$quotedScriptRoot'")
    Invoke-Expression $verifierPreamble
    Invoke-Expression $verifierSource.Substring($helpersStart, $helpersEnd - $helpersStart)
    $verifierFixtureArguments = @{
        ExpectedModelManifest = [pscustomobject]$fixtureManifest
        ExpectedModelManifestPath = $fixtureManifestPath
    }
    # First prove this otherwise production-shaped fixture is accepted. Then
    # add only the local marker and prove that exact change causes rejection.
    $script:FocusedFrozenCpuWorkerAssertions++
    Assert-Bundle -Root $normalBundle @verifierFixtureArguments
    $normalBundleMarker = Join-Path $normalBundle (Get-WindowsFrozenCpuWorkerMarkerFileName)
    Copy-Item -LiteralPath (Join-Path $frozenBundle (Get-WindowsFrozenCpuWorkerMarkerFileName)) -Destination $normalBundleMarker
    try {
        Invoke-ExpectedFailure { Assert-Bundle -Root $normalBundle @verifierFixtureArguments } 'explicit inventory'
    }
    finally {
        Remove-Item -LiteralPath $normalBundleMarker
    }
    $script:FocusedFrozenCpuWorkerAssertions++
    Assert-Bundle -Root $normalBundle @verifierFixtureArguments
    Invoke-ExpectedFailure { Assert-Bundle -Root $frozenBundle @verifierFixtureArguments } 'explicit inventory'

    $global:WindowsFrozenCpuWorkerTestMutateWorkerPath = $workerPath
    $global:WindowsFrozenCpuWorkerTestMutationWasBlocked = $false
    $mutationBundle = Join-Path $testRoot 'mutation-bundle'
    Reset-TestCalls
    Invoke-FrozenConsumer $producerRecordPath $mutationBundle
    Assert-True $global:WindowsFrozenCpuWorkerTestMutationWasBlocked 'Frozen worker read handle did not block a write during the desktop build.'
    $global:WindowsFrozenCpuWorkerTestMutateWorkerPath = $null

    $global:WindowsFrozenCpuWorkerTestSourceDriftPath = Join-Path $fixtureRoot 'source-drift.txt'
    Reset-TestCalls
    Invoke-ExpectedFailure { Invoke-FrozenConsumer $producerRecordPath (Join-Path $testRoot 'consumer-drift') } 'requires a clean source workspace'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 1 'Source drift was not detected after frozen desktop build.'
    Remove-Item -LiteralPath $global:WindowsFrozenCpuWorkerTestSourceDriftPath -Force
    $global:WindowsFrozenCpuWorkerTestSourceDriftPath = $null

    Reset-TestCalls
    Invoke-ExpectedFailure {
        & $fixtureBuilder `
            -ModelSource $modelSource `
            -BundlePath (Join-Path $testRoot 'blank-frozen-path') `
            -InstallerPackAllowlistPath $installerAllowlist `
            -FrozenCpuWorkerRecordPath ' '
    } 'explicitly supplied'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 'Explicit blank frozen record path invoked Cargo.'

    Reset-TestCalls
    Invoke-ExpectedFailure {
        Invoke-FrozenConsumer $producerRecordPath (Join-Path $testRoot 'blank-frozen-source-root') ' '
    } 'explicitly supplied'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 'Explicit blank frozen worker source root invoked Cargo.'

    $blankObservationBundle = Join-Path $testRoot 'blank-observation-frozen-path'
    Invoke-ExpectedFailure {
        & $fixtureBuilder -ModelSource $modelSource -BundlePath $blankObservationBundle `
            -InstallerPackAllowlistPath $installerAllowlist `
            -FrozenCpuWorkerRecordPath ' ' -LocalFrozenGpuObservation
    } 'explicitly supplied'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 0 'Observation with a blank frozen record invoked Cargo.'
    Assert-True (-not (Test-Path -LiteralPath $blankObservationBundle)) 'Observation with a blank frozen record left an output.'

    $global:WindowsFrozenCpuWorkerTestRaceBundleOutput = Join-Path $testRoot 'raced-bundle'
    Reset-TestCalls
    Invoke-ExpectedFailure { Invoke-FrozenConsumer $producerRecordPath $global:WindowsFrozenCpuWorkerTestRaceBundleOutput } 'appeared during staging'
    Assert-True (Test-Path -LiteralPath $global:WindowsFrozenCpuWorkerTestRaceBundleOutput) 'Frozen bundle race fixture was not created.'
    $global:WindowsFrozenCpuWorkerTestRaceBundleOutput = $null

    $workflow = Get-Content -LiteralPath (Join-Path $repositoryRoot '.github\workflows\release.yml') -Raw
    if ($workflow.Contains('FrozenCpuWorkerRecordPath') -or $workflow.Contains('new-windows-frozen-cpu-worker.ps1')) {
        throw 'The hosted release workflow must not invoke local frozen CPU worker packaging.'
    }
}
finally {
    if ($null -ne $previousGlobalCargo) {
        Set-Item -LiteralPath Function:\cargo -Value $previousGlobalCargoScriptBlock
    }
    else {
        Remove-Item Function:\cargo -ErrorAction SilentlyContinue
    }
    $env:CARGO_TARGET_DIR = $previousTargetDirectory
    $env:SCRIBE_BUILD_REVISION = $previousRevision
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $previousWorkerDigest
    $env:SCRIBE_BUILDING_WORKER = $previousBuildingWorker
    $env:GITHUB_ACTIONS = $previousGitHubActions
    $env:CI = $previousCi
    foreach ($entry in [Environment]::GetEnvironmentVariables().GetEnumerator()) {
        if ([string]$entry.Key -match $cpuBaselineAmbientNamePattern) {
            Remove-Item -LiteralPath "Env:$($entry.Key)"
        }
    }
    foreach ($name in $previousCpuBaselineAmbient.Keys) {
        Set-Item -LiteralPath "Env:$name" -Value $previousCpuBaselineAmbient[$name]
    }
    foreach ($entry in @(Get-ChildItem Env:GIT_*)) {
        Remove-Item -LiteralPath "Env:$($entry.Name)"
    }
    foreach ($name in $previousGitEnvironment.Keys) {
        Set-Item -LiteralPath "Env:$name" -Value $previousGitEnvironment[$name]
    }
    foreach ($name in $policyFixtureGlobalNames) {
        $saved = $savedPolicyFixtureGlobals[$name]
        if ($saved.Exists) {
            Set-Variable -Name $name -Scope Global -Value $saved.Value -Force
        }
        else {
            Remove-Variable -Name $name -Scope Global -Force -ErrorAction SilentlyContinue
        }
    }
    try {
        Assert-Equal (Get-CommandIdentity 'cargo') $originalCargoCommandIdentity 'Synthetic Cargo seam command restoration'
    }
    finally {
        foreach ($targetRoot in $global:WindowsFrozenCpuWorkerTestBaselineTargetRoots) {
            Remove-TestCpuWorkerBaselineTarget $targetRoot
        }
        $global:WindowsFrozenCpuWorkerTestBaselineTargetRoots = $null
        Remove-TestRootSafely $testRoot
    }
}
Write-Output "Windows frozen CPU worker local integrity tests passed ($script:FocusedFrozenCpuWorkerAssertions assertions; physical case-sensitive collision integration UNRUN)."
