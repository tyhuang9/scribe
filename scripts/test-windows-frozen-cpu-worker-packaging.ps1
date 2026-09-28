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

function Invoke-FrozenConsumer([string]$RecordPath, [string]$BundlePath) {
    & $fixtureBuilder `
        -ModelSource $modelSource `
        -BundlePath $BundlePath `
        -InstallerPackAllowlistPath $installerAllowlist `
        -FrozenCpuWorkerRecordPath $RecordPath
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
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
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
$previousGitEnvironment = @{}
foreach ($entry in @(Get-ChildItem Env:GIT_*)) {
    $previousGitEnvironment[$entry.Name] = $entry.Value
}
$previousGlobalCargo = Get-Item -LiteralPath Function:\cargo -ErrorAction SilentlyContinue
$previousGlobalCargoScriptBlock = if ($null -ne $previousGlobalCargo) { $previousGlobalCargo.ScriptBlock } else { $null }
$originalCargoCommandIdentity = Get-CommandIdentity 'cargo'
$originalGetChildItemCommandIdentity = Get-CommandIdentity 'Get-ChildItem'
try {
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
        'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'build.rs', 'src/worker_identity.rs',
        'scripts/build-windows-release.ps1', 'scripts/new-windows-frozen-cpu-worker.ps1',
        'scripts/windows-frozen-cpu-worker-integrity.ps1', 'scripts/windows-pe-imports.ps1',
        'scripts/stage-verified-worker-packs.ps1',
        'resources/licenses/Apache-2.0.txt', 'resources/licenses/OpenAI-Whisper-MIT.txt',
        'resources/licenses/Whisper-Base-En-NOTICE.txt', 'resources/licenses/THIRD-PARTY-NOTICES.txt',
        'native/transcribe-cpp-v0.1.3/LICENSE', 'native/transcribe-cpp-v0.1.3/PROVENANCE.md',
        'native/whisper-f049fff/LICENSE', 'native/whisper-f049fff/PROVENANCE.md',
        'native/sherpa-onnx-v1.13.5/PROVENANCE.md', 'resources/silero-vad/LICENSE',
        'resources/silero-vad/PROVENANCE.md'
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
    & git -C $fixtureRoot init --quiet
    if ($LASTEXITCODE -ne 0) { throw 'Could not initialize frozen worker test fixture Git repository.' }
    & git -C $fixtureRoot add --all
    if ($LASTEXITCODE -ne 0) { throw 'Could not stage frozen worker test fixture files.' }
    & git -C $fixtureRoot commit --quiet -m fixture
    if ($LASTEXITCODE -ne 0) { throw 'Could not commit frozen worker test fixture files.' }

    $fixtureBuilder = Join-Path $fixtureRoot 'scripts\build-windows-release.ps1'
    $fixtureProducer = Join-Path $fixtureRoot 'scripts\new-windows-frozen-cpu-worker.ps1'
    . (Join-Path $fixtureRoot 'scripts\windows-frozen-cpu-worker-integrity.ps1')
    $fixtureContext = Get-WindowsFrozenCpuWorkerSourceContext $fixtureRoot
    $env:CARGO_TARGET_DIR = $fixtureTarget
    $env:SCRIBE_BUILD_REVISION = 'inherited-test-revision'
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = 'f' * 64 -join ''
    $env:SCRIBE_BUILDING_WORKER = 'inherited-worker-flag'
    $env:GITHUB_ACTIONS = $null
    $env:CI = $null
    Reset-TestCalls
    $global:WindowsFrozenCpuWorkerTestFailWorkerBuild = $false
    $global:WindowsFrozenCpuWorkerTestFailDesktopBuild = $false
    $global:WindowsFrozenCpuWorkerTestRaceFreezeOutput = $null
    $global:WindowsFrozenCpuWorkerTestRaceBundleOutput = $null
    $global:WindowsFrozenCpuWorkerTestSourceDriftPath = $null
    $global:WindowsFrozenCpuWorkerTestMutateWorkerPath = $null
    $global:WindowsFrozenCpuWorkerTestMutationWasBlocked = $false

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
        })
        if (($global:WindowsFrozenCpuWorkerTestFailWorkerBuild -and $binary -ceq 'scribe-inference-worker') -or
            ($global:WindowsFrozenCpuWorkerTestFailDesktopBuild -and $binary -ceq 'local-transcriber')) {
            $global:LASTEXITCODE = 1
            return
        }
        $output = Join-Path $env:CARGO_TARGET_DIR "x86_64-pc-windows-msvc\release\$binary.exe"
        if ($binary -ceq 'scribe-inference-worker') {
            New-TestReviewedPe $output 3
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
    Assert-DesktopCargoArguments $global:WindowsFrozenCpuWorkerTestCargoCalls[1] 'ui-harness' 'Normal desktop exact feature argv'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].BuildingWorker '1' 'Normal packaging worker build marker'
    Assert-True ([string]::IsNullOrEmpty($global:WindowsFrozenCpuWorkerTestCargoCalls[0].WorkerDigest)) 'Normal worker build inherited a desktop digest.'
    Assert-True ([string]::IsNullOrEmpty($global:WindowsFrozenCpuWorkerTestCargoCalls[1].BuildingWorker)) 'Normal desktop build inherited the worker build marker.'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[1].WorkerDigest (Get-FileHash -LiteralPath (Join-Path $normalBundle 'scribe-inference-worker.exe') -Algorithm SHA256).Hash.ToLowerInvariant() 'Normal desktop embeds the exact packaged worker digest'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $normalBundle (Get-WindowsFrozenCpuWorkerMarkerFileName)))) 'Normal packaging unexpectedly staged the local-only marker.'
    Assert-Equal $env:SCRIBE_BUILD_REVISION 'inherited-test-revision' 'Normal packaging revision environment restoration'
    Assert-Equal $env:SCRIBE_BUNDLED_WORKER_SHA256 ('f' * 64 -join '') 'Normal packaging worker digest environment restoration'
    Assert-Equal $env:SCRIBE_BUILDING_WORKER 'inherited-worker-flag' 'Normal packaging worker marker environment restoration'

    Reset-TestCalls
    & $fixtureProducer -OutputDirectory $producerOutput
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls.Count 1 'Freeze producer Cargo call count'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].Binary 'scribe-inference-worker' 'Freeze producer worker-only build'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].Revision $fixtureContext.SourceRevision 'Freeze producer exact build revision'
    Assert-Equal $global:WindowsFrozenCpuWorkerTestCargoCalls[0].BuildingWorker '1' 'Freeze producer worker build marker'
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
    Assert-Equal $env:SCRIBE_BUILD_REVISION 'inherited-test-revision' 'Frozen consumer revision environment restoration'
    Assert-Equal $env:SCRIBE_BUNDLED_WORKER_SHA256 ('f' * 64 -join '') 'Frozen consumer digest environment restoration'
    Assert-Equal $env:SCRIBE_BUILDING_WORKER 'inherited-worker-flag' 'Frozen consumer worker marker environment restoration'
    Assert-True (Test-Path -LiteralPath (Join-Path $frozenBundle (Get-WindowsFrozenCpuWorkerMarkerFileName))) 'Frozen bundle omitted its local-only marker.'
    $stagedWorker = Join-Path $frozenBundle (Get-WindowsFrozenCpuWorkerExecutableRelativePath)
    Assert-Equal (Get-WindowsFrozenCpuWorkerFileSha256 $stagedWorker) $record.worker_sha256 'Frozen consumer staged exact worker bytes'

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
    foreach ($entry in @(Get-ChildItem Env:GIT_*)) {
        Remove-Item -LiteralPath "Env:$($entry.Name)"
    }
    foreach ($name in $previousGitEnvironment.Keys) {
        Set-Item -LiteralPath "Env:$name" -Value $previousGitEnvironment[$name]
    }
    try {
        Assert-Equal (Get-CommandIdentity 'cargo') $originalCargoCommandIdentity 'Synthetic Cargo seam command restoration'
    }
    finally {
        Remove-TestRootSafely $testRoot
    }
}
Write-Output "Windows frozen CPU worker local integrity tests passed ($script:FocusedFrozenCpuWorkerAssertions assertions; physical case-sensitive collision integration UNRUN)."
