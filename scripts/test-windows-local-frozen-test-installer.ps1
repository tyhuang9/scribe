$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-local-frozen-installer-test-$([guid]::NewGuid().ToString('N'))"
$fixtureRoot = Join-Path $testRoot 'fixture'
$freezeRoot = Join-Path $testRoot 'freeze'
$bundleRoot = Join-Path $testRoot 'bundle'
$outputRoot = Join-Path $testRoot 'installer-output'
$compilerRoot = Join-Path $testRoot 'compiler'
$script:Assertions = 0

function Assert-True([bool]$Value, [string]$Description) {
    $script:Assertions++
    if (-not $Value) { throw $Description }
}

function Assert-Equal([object]$Actual, [object]$Expected, [string]$Description) {
    $script:Assertions++
    if ($Actual -cne $Expected) { throw "$Description expected '$Expected', got '$Actual'." }
}

function Invoke-ExpectedFailure([scriptblock]$Action, [string]$ExpectedText) {
    $script:Assertions++
    try {
        $null = @(& $Action)
    }
    catch {
        if (-not $_.Exception.Message.Contains($ExpectedText)) {
            throw "Expected failure containing '$ExpectedText', got: $($_.Exception.Message)"
        }
        return
    }
    throw "Expected failure containing '$ExpectedText', but the action succeeded."
}

function Copy-FixtureSource([string]$RelativePath) {
    $source = Join-Path $repositoryRoot ($RelativePath -replace '/', '\')
    $destination = Join-Path $fixtureRoot ($RelativePath -replace '/', '\')
    New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
    Copy-Item -LiteralPath $source -Destination $destination
}

function Set-UInt16([byte[]]$Bytes, [int]$Offset, [uint16]$Value) {
    [BitConverter]::GetBytes($Value).CopyTo($Bytes, $Offset)
}

function Set-UInt32([byte[]]$Bytes, [int]$Offset, [uint32]$Value) {
    [BitConverter]::GetBytes($Value).CopyTo($Bytes, $Offset)
}

function Write-TestReviewedPe([string]$Path, [uint16]$Subsystem) {
    $bytes = [byte[]]::new(1024)
    $bytes[0] = 0x4D; $bytes[1] = 0x5A
    Set-UInt32 $bytes 0x3C 0x80
    Set-UInt32 $bytes 0x80 0x00004550
    Set-UInt16 $bytes 0x84 0x8664
    Set-UInt16 $bytes 0x86 1
    Set-UInt16 $bytes 0x94 0x00F0
    Set-UInt16 $bytes 0x98 0x020B
    Set-UInt16 $bytes 0xDC $Subsystem
    Set-UInt32 $bytes 0xD4 0x200
    Set-UInt32 $bytes 0x104 16
    Set-UInt32 $bytes 0x110 0x1000
    Set-UInt32 $bytes 0x114 40
    [Text.Encoding]::ASCII.GetBytes('.rdata').CopyTo($bytes, 0x188)
    Set-UInt32 $bytes 0x190 0x200
    Set-UInt32 $bytes 0x194 0x1000
    Set-UInt32 $bytes 0x198 0x200
    Set-UInt32 $bytes 0x19C 0x200
    Set-UInt32 $bytes 0x20C 0x1040
    [Text.Encoding]::ASCII.GetBytes('kernel32.dll').CopyTo($bytes, 0x240)
    $bytes[0x240 + 'kernel32.dll'.Length] = 0
    New-Item -ItemType Directory -Path (Split-Path -Parent $Path) -Force | Out-Null
    [IO.File]::WriteAllBytes($Path, $bytes)
}

function Write-Utf8([string]$Path, [string]$Text) {
    New-Item -ItemType Directory -Path (Split-Path -Parent $Path) -Force | Out-Null
    [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($false))
}

function Refresh-BundleInventory([string]$Root) {
    $files = @(Get-ChildItem -LiteralPath $Root -Recurse -File -Force |
        Where-Object { $_.Name -cne 'bundle-inventory.json' } |
        ForEach-Object {
            [ordered]@{
                path = Get-WindowsLocalFrozenRelativePath $Root $_.FullName
                size_bytes = [int64]$_.Length
                sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        } | Sort-Object path)
    $inventory = [ordered]@{
        schema_version = 1
        platform_triple = 'x86_64-pc-windows-msvc'
        files = $files
    }
    Write-Utf8 (Join-Path $Root 'bundle-inventory.json') ($inventory | ConvertTo-Json -Depth 6)
}

function Copy-Bundle([string]$Name) {
    $destination = Join-Path $testRoot $Name
    Copy-Item -LiteralPath $bundleRoot -Destination $destination -Recurse
    return $destination
}

function Invoke-Builder([string]$Bundle, [string]$Output, [string]$Compiler) {
    & (Join-Path $fixtureRoot 'scripts\build-windows-frozen-test-installer.ps1') `
        -BundlePath $Bundle `
        -FrozenCpuWorkerRecordPath (Join-Path $freezeRoot 'windows-frozen-cpu-worker-record.json') `
        -InnoCompilerPath $Compiler `
        -OutputDirectory $Output
}

function Remove-TestRootSafely([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $resolved = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
    if ((Split-Path -Parent $resolved) -cne $temp -or
        (Split-Path -Leaf $resolved) -cnotmatch '^scribe-local-frozen-installer-test-[0-9a-f]{32}$') {
        throw 'Refused local frozen installer test cleanup outside its exact temporary root.'
    }
    foreach ($item in @((Get-Item -LiteralPath $resolved -Force)) + @(Get-ChildItem -LiteralPath $resolved -Recurse -Force)) {
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refused local frozen installer test cleanup through a reparse point: $($item.FullName)"
        }
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}

$previousCi = $env:CI
$previousGitHubActions = $env:GITHUB_ACTIONS
$previousMode = $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE
$previousCapture = $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE
$previousSourceMutation = $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_SOURCE_MUTATION
$previousGitEnvironment = @{}
foreach ($entry in @(Get-ChildItem Env:GIT_*)) { $previousGitEnvironment[$entry.Name] = $entry.Value }
try {
    $env:CI = $null
    $env:GITHUB_ACTIONS = $null
    foreach ($entry in @(Get-ChildItem Env:GIT_*)) { Remove-Item -LiteralPath "Env:$($entry.Name)" }
    $isolatedGitConfig = Join-Path $testRoot 'empty-git-config'
    $isolatedGitTemplate = Join-Path $testRoot 'empty-git-template'
    $isolatedGitHooks = Join-Path $testRoot 'empty-git-hooks'
    New-Item -ItemType Directory -Path $isolatedGitTemplate, $isolatedGitHooks | Out-Null
    Write-Utf8 $isolatedGitConfig ''
    $env:GIT_CONFIG_NOSYSTEM = '1'
    $env:GIT_CONFIG_GLOBAL = $isolatedGitConfig
    $env:GIT_TEMPLATE_DIR = $isolatedGitTemplate
    New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
    foreach ($path in @(
        'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'build.rs', 'src/worker_identity.rs',
        'scripts/windows-frozen-cpu-worker-integrity.ps1', 'scripts/windows-pe-imports.ps1',
        'scripts/windows-local-frozen-installer-integrity.ps1', 'scripts/build-windows-frozen-test-installer.ps1',
        'installer/scribe-local-frozen.iss', 'resources/licenses/Apache-2.0.txt',
        'resources/licenses/OpenAI-Whisper-MIT.txt', 'resources/licenses/Whisper-Base-En-NOTICE.txt',
        'resources/licenses/THIRD-PARTY-NOTICES.txt', 'native/transcribe-cpp-v0.1.3/LICENSE',
        'native/transcribe-cpp-v0.1.3/PROVENANCE.md', 'native/whisper-f049fff/LICENSE',
        'native/whisper-f049fff/PROVENANCE.md', 'native/sherpa-onnx-v1.13.5/PROVENANCE.md',
        'resources/silero-vad/LICENSE', 'resources/silero-vad/PROVENANCE.md'
    )) { Copy-FixtureSource $path }
    $modelBytes = [byte[]](1, 2, 3, 4, 5)
    $modelSha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $modelHash = ([BitConverter]::ToString($modelSha256.ComputeHash($modelBytes))).Replace('-', '').ToLowerInvariant()
    } finally {
        $modelSha256.Dispose()
    }
    $modelManifest = [ordered]@{
        model_id = 'fixture-model'; artifact_filename = 'whisper-base.en-Q8_0.gguf';
        size_bytes = [int64]$modelBytes.Length; sha256 = $modelHash;
        platform_triple = 'x86_64-pc-windows-msvc'
    }
    Write-Utf8 (Join-Path $fixtureRoot 'runtime-manifests\whisper-base-en-q8_0-windows-x64.json') ($modelManifest | ConvertTo-Json -Depth 4)

    New-Item -ItemType Directory -Path $compilerRoot | Out-Null
    $fakeCompiler = Join-Path $compilerRoot 'ISCC.exe'
    $fakeCompilerSource = Join-Path $compilerRoot 'FakeIscc.cs'
    Write-Utf8 $fakeCompilerSource @'
using System;
using System.IO;
using System.Linq;
using System.Threading;
public static class FakeIscc {
  static string Arg(string[] args, string name) { var value = args.FirstOrDefault(x => x.StartsWith(name, StringComparison.Ordinal)); return value == null ? null : value.Substring(name.Length); }
  public static int Main(string[] args) {
    string output = Arg(args, "/DLocalFrozenInstallerOutputRoot=");
    string token = Arg(args, "/DLocalFrozenTestToken=");
    string payload = Arg(args, "/DLocalFrozenBundleRoot=");
    string mode = Environment.GetEnvironmentVariable("SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE") ?? "";
    string capture = Environment.GetEnvironmentVariable("SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE") ?? "";
    if (!String.IsNullOrEmpty(capture)) File.WriteAllText(capture, String.Join("\n", args));
    if (mode == "mutate-staged") {
      try { File.WriteAllText(Path.Combine(payload, "README.txt"), "mutated"); if (!String.IsNullOrEmpty(capture)) File.AppendAllText(capture, "\nmutation=written"); }
      catch (IOException) { if (!String.IsNullOrEmpty(capture)) File.AppendAllText(capture, "\nmutation=blocked"); }
    }
    if (mode == "mutate-source") File.WriteAllText(Environment.GetEnvironmentVariable("SCRIBE_LOCAL_FROZEN_TEST_ISCC_SOURCE_MUTATION"), "source-drift");
    if (mode == "fail") return 19;
    if (mode == "hang") { Thread.Sleep(60000); return 0; }
    Directory.CreateDirectory(output);
    byte[] bytes = new byte[1024]; bytes[0] = 0x4d; bytes[1] = 0x5a;
    File.WriteAllBytes(Path.Combine(output, "Scribe-LOCAL-Frozen-Test-" + token + ".exe"), bytes);
    return 0;
  }
}
'@
    $csharpCompiler = 'C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe'
    if (-not (Test-Path -LiteralPath $csharpCompiler -PathType Leaf)) {
        throw 'The Windows .NET Framework C# compiler is required for the local installer test seam.'
    }
    & $csharpCompiler '/nologo' '/target:exe' "/out:$fakeCompiler" $fakeCompilerSource
    if ($LASTEXITCODE -ne 0) { throw 'Could not build the local installer test compiler seam.' }
    $fakeCompilerItem = Get-Item -LiteralPath $fakeCompiler
    $fakeCompilerHash = (Get-FileHash -LiteralPath $fakeCompiler -Algorithm SHA256).Hash.ToLowerInvariant()
    $provenance = [ordered]@{
        schema_version = 1; product = 'Inno Setup'; product_version = '6.7.1'; reviewed_utc_date = '2000-01-01';
        package_url = 'https://fixture.invalid/inno'; package_size_bytes = 1; package_sha256 = ('0' * 64);
        embedded_installer_path = 'tools/innosetup.exe'; embedded_installer_size_bytes = 1; embedded_installer_sha256 = ('0' * 64);
        compiler_relative_path = 'ISCC.exe'; compiler_size_bytes = [int64]$fakeCompilerItem.Length; compiler_sha256 = $fakeCompilerHash;
        embedded_package_verification_path = 'legal/VERIFICATION.txt'; upstream_installer_url = 'https://fixture.invalid/inno.exe';
        verification_method = 'fixture only'; trust_scope = 'fixture only'
    }
    Write-Utf8 (Join-Path $fixtureRoot 'installer\inno-setup-6.7.1-provenance.json') ($provenance | ConvertTo-Json -Depth 4)
    & git -c safe.directory=$fixtureRoot -c core.hooksPath=$isolatedGitHooks -c commit.gpgSign=false -C $fixtureRoot init -q
    & git -c safe.directory=$fixtureRoot -c core.hooksPath=$isolatedGitHooks -C $fixtureRoot add .
    & git -c safe.directory=$fixtureRoot -c core.hooksPath=$isolatedGitHooks -c commit.gpgSign=false -C $fixtureRoot -c user.email=fixture@example.invalid -c user.name='Scribe fixture' commit -qm fixture
    if ($LASTEXITCODE -ne 0) { throw 'Could not commit local frozen installer fixture source.' }

    . (Join-Path $fixtureRoot 'scripts\windows-frozen-cpu-worker-integrity.ps1')
    . (Join-Path $fixtureRoot 'scripts\windows-local-frozen-installer-integrity.ps1')
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'hang'
    $timeoutStopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    Invoke-ExpectedFailure {
        Invoke-WindowsLocalFrozenBoundedProcess `
            -Executable $fakeCompiler `
            -Arguments @() `
            -Description 'fixture hung compiler' `
            -TimeoutMilliseconds 250 `
            -StreamDrainMilliseconds 250
    } 'timed out after 250 milliseconds'
    $timeoutStopwatch.Stop()
    Assert-True ($timeoutStopwatch.ElapsedMilliseconds -lt 5000) 'Bounded local frozen process timeout did not return promptly after terminating the process tree.'
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = ''
    $context = Get-WindowsFrozenCpuWorkerSourceContext $fixtureRoot
    New-Item -ItemType Directory -Path $freezeRoot | Out-Null
    $frozenWorker = Join-Path $freezeRoot 'scribe-inference-worker.exe'
    Write-TestReviewedPe $frozenWorker 3
    $frozenHash = Get-WindowsFrozenCpuWorkerFileSha256 $frozenWorker
    $frozenRecord = New-WindowsFrozenCpuWorkerRecord $context (Get-Item -LiteralPath $frozenWorker).Length $frozenHash
    $frozenRecordPath = Join-Path $freezeRoot 'windows-frozen-cpu-worker-record.json'
    Write-Utf8 $frozenRecordPath ($frozenRecord | ConvertTo-Json -Depth 5)
    $frozenRecordHash = Get-WindowsFrozenCpuWorkerFileSha256 $frozenRecordPath
    Write-Utf8 (Join-Path $freezeRoot 'WINDOWS-FROZEN-CPU-WORKER-LOCAL-ONLY.txt') (Get-WindowsFrozenCpuWorkerMarkerText $frozenRecordHash ([pscustomobject]$frozenRecord))

    New-Item -ItemType Directory -Path $bundleRoot | Out-Null
    foreach ($path in $script:WindowsLocalFrozenBaseFiles) {
        $destination = Join-Path $bundleRoot ($path -replace '/', '\')
        switch ($path) {
            'local-transcriber.exe' { Write-TestReviewedPe $destination 2 }
            'scribe-inference-worker.exe' { Copy-Item -LiteralPath $frozenWorker -Destination $destination }
            'whisper-base.en-Q8_0.gguf' { New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null; [IO.File]::WriteAllBytes($destination, $modelBytes) }
            'bundled-model-manifest.json' { Copy-Item -LiteralPath (Join-Path $fixtureRoot 'runtime-manifests\whisper-base-en-q8_0-windows-x64.json') -Destination $destination }
            'worker-pack-catalog.json' { Write-Utf8 $destination '{"schema_version":1,"packs":[]}' }
            'WINDOWS-FROZEN-CPU-WORKER-LOCAL-ONLY.txt' { Write-Utf8 $destination (Get-WindowsFrozenCpuWorkerBundleMarkerText $frozenRecordHash ([pscustomobject]$frozenRecord)) }
            default {
                $sourceRelative = switch ($path) {
                    'licenses/transcribe.cpp-MIT.txt' { 'native/transcribe-cpp-v0.1.3/LICENSE' }
                    'licenses/transcribe.cpp-PROVENANCE.md' { 'native/transcribe-cpp-v0.1.3/PROVENANCE.md' }
                    'licenses/whisper.cpp-MIT.txt' { 'native/whisper-f049fff/LICENSE' }
                    'licenses/whisper.cpp-PROVENANCE.md' { 'native/whisper-f049fff/PROVENANCE.md' }
                    'licenses/sherpa-onnx-PROVENANCE.md' { 'native/sherpa-onnx-v1.13.5/PROVENANCE.md' }
                    'licenses/Silero-VAD-MIT.txt' { 'resources/silero-vad/LICENSE' }
                    'licenses/Silero-VAD-PROVENANCE.md' { 'resources/silero-vad/PROVENANCE.md' }
                    'README.txt' { $null }
                    default { 'resources/licenses/' + (Split-Path -Leaf $path) }
                }
                if ($null -eq $sourceRelative) { Write-Utf8 $destination 'fixture local frozen bundle' }
                else { New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null; Copy-Item -LiteralPath (Join-Path $fixtureRoot ($sourceRelative -replace '/', '\')) -Destination $destination }
            }
        }
    }
    Refresh-BundleInventory $bundleRoot
    $openedFrozen = Open-ValidatedWindowsFrozenCpuWorker $frozenRecordPath $fixtureRoot
    try { $fixtureBundle = Assert-WindowsLocalFrozenBundle $bundleRoot $openedFrozen } finally { $openedFrozen.WorkerStream.Dispose() }

    $template = Get-Content -LiteralPath (Join-Path $repositoryRoot 'installer\scribe-local-frozen.iss') -Raw
    foreach ($required in @('CloseApplications=no', 'CreateUninstallRegKey=no', 'UsePreviousAppDir=no', 'DisableDirPage=yes', 'ArchitecturesAllowed=x64compatible', 'ArchitecturesInstallIn64BitMode=x64compatible', "ExpandConstant('{param:DIR|}')", 'HasNoReparseAncestors', 'RejectLocalFrozenWizardDestination')) {
        Assert-True $template.Contains($required) "Local frozen installer template omitted $required."
    }
    foreach ($forbidden in @('[Icons]', '[Run]', '[Tasks]', 'CloseApplications=yes', 'StableAppIdGuid')) {
        Assert-True (-not $template.Contains($forbidden)) "Local frozen installer template unexpectedly contains $forbidden."
    }
    $builderSource = Get-Content -LiteralPath (Join-Path $repositoryRoot 'scripts\build-windows-frozen-test-installer.ps1') -Raw
    Assert-True (-not $builderSource.Contains('InnoProvenancePath')) 'Local frozen builder accepted a caller-provided Inno provenance path.'
    Assert-True $builderSource.Contains("Join-Path `$repositoryRoot 'installer\inno-setup-6.7.1-provenance.json'") 'Local frozen builder did not use the repository-pinned Inno provenance.'
    $productionVerifierSource = Get-Content -LiteralPath (Join-Path $repositoryRoot 'scripts\verify-windows-release-package.ps1') -Raw
    Assert-True (-not $productionVerifierSource.Contains('LocalFrozen')) 'Production package verifier was changed to admit local frozen markers.'

    $capture = Join-Path $testRoot 'compiler-arguments.txt'
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = ''
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE = $capture
    $result = @(Invoke-Builder $bundleRoot $outputRoot $fakeCompiler)
    Assert-Equal $result.Count 1 'Local frozen installer builder result count'
    $result = $result[0]
    Assert-True (Test-Path -LiteralPath $result.InstallerPath -PathType Leaf) 'Local frozen installer builder did not publish its installer.'
    Assert-True (Test-Path -LiteralPath $result.RecordPath -PathType Leaf) 'Local frozen installer builder did not publish its record.'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $outputRoot 'payload'))) 'Local frozen installer builder retained a duplicate payload in final output.'
    Assert-True ((Get-Content -LiteralPath $capture -Raw).Contains('/DLocalFrozenBundleRoot=')) 'Local frozen installer compiler missed staged payload input.'
    Assert-True ((Get-Content -LiteralPath $capture -Raw).Contains('/DLocalFrozenTestToken=')) 'Local frozen installer compiler missed generated local token.'
    $publishedRecord = Get-Content -LiteralPath $result.RecordPath -Raw | ConvertFrom-Json
    Assert-Equal $publishedRecord.bundle_inventory_sha256 ((Get-FileHash -LiteralPath (Join-Path $bundleRoot 'bundle-inventory.json') -Algorithm SHA256).Hash.ToLowerInvariant()) 'Published local frozen installer inventory binding'
    $verifiedInstallerRecord = Assert-WindowsLocalFrozenInstallerRecord $result.RecordPath $openedFrozen $fixtureBundle $result.InstallerPath
    Assert-Equal $verifiedInstallerRecord.local_test_token $publishedRecord.local_test_token 'Published local frozen installer record token binding'

    $originalInstallerBytes = [IO.File]::ReadAllBytes($result.InstallerPath)
    $tamperedInstallerBytes = [byte[]]$originalInstallerBytes.Clone()
    $tamperedInstallerBytes[100] = $tamperedInstallerBytes[100] -bxor 1
    [IO.File]::WriteAllBytes($result.InstallerPath, $tamperedInstallerBytes)
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenInstallerRecord $result.RecordPath $openedFrozen $fixtureBundle $result.InstallerPath } 'bytes do not match'
    [IO.File]::WriteAllBytes($result.InstallerPath, $originalInstallerBytes)

    $originalInstallerRecordText = Get-Content -LiteralPath $result.RecordPath -Raw
    $tamperedInstallerRecord = $originalInstallerRecordText | ConvertFrom-Json
    $tamperedInstallerRecord.bundle_inventory_sha256 = ('0' * 64)
    Write-Utf8 $result.RecordPath ($tamperedInstallerRecord | ConvertTo-Json -Depth 5)
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenInstallerRecord $result.RecordPath $openedFrozen $fixtureBundle $result.InstallerPath } 'does not bind'
    Write-Utf8 $result.RecordPath $originalInstallerRecordText

    $tamperedInstallerRecord = $originalInstallerRecordText | ConvertFrom-Json
    $tamperedInstallerRecord.local_test_token = ('a' * 31)
    Write-Utf8 $result.RecordPath ($tamperedInstallerRecord | ConvertTo-Json -Depth 5)
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenInstallerRecord $result.RecordPath $openedFrozen $fixtureBundle $result.InstallerPath } 'invalid installer or local-test identity'
    Write-Utf8 $result.RecordPath $originalInstallerRecordText

    $tamperedInstallerRecord = $originalInstallerRecordText | ConvertFrom-Json
    $tamperedInstallerRecord.local_test_token = ('b' * 32)
    $tamperedInstallerRecord.install_relative_path = "Scribe/LOCAL-Frozen-Test/$($tamperedInstallerRecord.local_test_token)"
    Write-Utf8 $result.RecordPath ($tamperedInstallerRecord | ConvertTo-Json -Depth 5)
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenInstallerRecord $result.RecordPath $openedFrozen $fixtureBundle $result.InstallerPath } 'invalid installer or local-test identity'
    Write-Utf8 $result.RecordPath $originalInstallerRecordText

    $installedParityFixture = Join-Path $testRoot 'installed-parity-fixture'
    Copy-Item -LiteralPath $bundleRoot -Destination $installedParityFixture -Recurse
    [IO.File]::WriteAllBytes((Join-Path $installedParityFixture 'unins000.exe'), [byte[]](0x4D, 0x5A))
    [IO.File]::WriteAllBytes((Join-Path $installedParityFixture 'unins000.dat'), [byte[]](1, 2, 3))
    Assert-WindowsLocalFrozenPayloadParity $bundleRoot $installedParityFixture
    $parityReadme = Join-Path $installedParityFixture 'README.txt'
    [IO.File]::WriteAllText($parityReadme, 'tampered installed payload', [Text.UTF8Encoding]::new($false))
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenPayloadParity $bundleRoot $installedParityFixture } 'payload parity mismatch'
    $missingParityFixture = Join-Path $testRoot 'missing-parity-fixture'
    Copy-Item -LiteralPath $bundleRoot -Destination $missingParityFixture -Recurse
    [IO.File]::WriteAllBytes((Join-Path $missingParityFixture 'unins000.exe'), [byte[]](0x4D, 0x5A))
    [IO.File]::WriteAllBytes((Join-Path $missingParityFixture 'unins000.dat'), [byte[]](1, 2, 3))
    Remove-Item -LiteralPath (Join-Path $missingParityFixture 'README.txt')
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenPayloadParity $bundleRoot $missingParityFixture } 'payload differs from its exact declared inventory'
    $unexpectedParityFixture = Join-Path $testRoot 'unexpected-parity-fixture'
    Copy-Item -LiteralPath $bundleRoot -Destination $unexpectedParityFixture -Recurse
    [IO.File]::WriteAllBytes((Join-Path $unexpectedParityFixture 'unins000.exe'), [byte[]](0x4D, 0x5A))
    [IO.File]::WriteAllBytes((Join-Path $unexpectedParityFixture 'unins000.dat'), [byte[]](1, 2, 3))
    [IO.File]::WriteAllBytes((Join-Path $unexpectedParityFixture 'unexpected.bin'), [byte[]](4))
    Invoke-ExpectedFailure { Assert-WindowsLocalFrozenPayloadParity $bundleRoot $unexpectedParityFixture } 'payload differs from its exact declared inventory'

    $validSmokeDiagnostics = [pscustomobject]@{
        cancellation_verified = $true
        capabilities = [pscustomobject]@{ cancellation = $true }
        detected_architecture = 'whisper'
    }
    Assert-WindowsLocalFrozenSmokeDiagnostics $validSmokeDiagnostics
    Invoke-ExpectedFailure {
        Assert-WindowsLocalFrozenSmokeDiagnostics ([pscustomobject]@{
            cancellation_verified = $true
            capabilities = [pscustomobject]@{}
            detected_architecture = 'whisper'
        })
    } 'cancellation contract'
    Invoke-ExpectedFailure {
        Assert-WindowsLocalFrozenSmokeDiagnostics ([pscustomobject]@{
            cancellation_verified = $false
            capabilities = [pscustomobject]@{ cancellation = $true }
            detected_architecture = 'whisper'
        })
    } 'cancellation contract'
    Invoke-ExpectedFailure {
        Assert-WindowsLocalFrozenSmokeDiagnostics ([pscustomobject]@{
            cancellation_verified = $true
            capabilities = [pscustomobject]@{ cancellation = $false }
            detected_architecture = 'whisper'
        })
    } 'cancellation contract'
    Invoke-ExpectedFailure {
        Assert-WindowsLocalFrozenSmokeDiagnostics ([pscustomobject]@{
            cancellation_verified = 'false'
            capabilities = [pscustomobject]@{ cancellation = $true }
            detected_architecture = 'whisper'
        })
    } 'cancellation contract'
    Invoke-ExpectedFailure {
        Assert-WindowsLocalFrozenSmokeDiagnostics ([pscustomobject]@{
            cancellation_verified = $true
            capabilities = [pscustomobject]@{ cancellation = 1 }
            detected_architecture = 'whisper'
        })
    } 'cancellation contract'
    $delayedRemovalRoot = Join-Path $testRoot 'delayed-uninstall-removal'
    $delayedRemovalReady = Join-Path $testRoot 'delayed-uninstall-ready'
    New-Item -ItemType Directory -Path $delayedRemovalRoot | Out-Null
    $delayedRemovalJob = Start-Job -ScriptBlock {
        param([string]$Path, [string]$ReadyPath)
        [IO.File]::WriteAllText($ReadyPath, 'ready')
        Start-Sleep -Milliseconds 250
        Remove-Item -LiteralPath $Path
    } -ArgumentList $delayedRemovalRoot, $delayedRemovalReady
    try {
        $jobStartDeadline = [System.Diagnostics.Stopwatch]::StartNew()
        while (-not (Test-Path -LiteralPath $delayedRemovalReady -PathType Leaf)) {
            if ($jobStartDeadline.ElapsedMilliseconds -ge 5000) {
                throw 'Delayed local frozen uninstaller-removal fixture did not become ready within five seconds.'
            }
            Start-Sleep -Milliseconds 25
        }
        Wait-WindowsLocalFrozenInstallRootRemoved $delayedRemovalRoot 2000
        Assert-True (-not (Test-Path -LiteralPath $delayedRemovalRoot)) 'Bounded local frozen uninstaller removal wait returned before delayed deletion completed.'
    }
    finally {
        if ($delayedRemovalJob.State -eq 'Running') {
            Stop-Job -Job $delayedRemovalJob
        }
        $null = Wait-Job -Job $delayedRemovalJob -Timeout 5
        Receive-Job -Job $delayedRemovalJob | Out-Null
        Remove-Job -Job $delayedRemovalJob -Force
    }

    $existingOutput = Join-Path $testRoot 'existing-output'
    New-Item -ItemType Directory -Path $existingOutput | Out-Null
    Invoke-ExpectedFailure { Invoke-Builder $bundleRoot $existingOutput $fakeCompiler } 'already exists'
    Invoke-ExpectedFailure { Invoke-Builder $bundleRoot (Join-Path $bundleRoot 'overlap') $fakeCompiler } 'cannot overlap'

    $tamperedBundle = Copy-Bundle 'tampered-cpu-bundle'
    $tamperedWorker = Join-Path $tamperedBundle 'scribe-inference-worker.exe'
    $tamperedBytes = [IO.File]::ReadAllBytes($tamperedWorker); $tamperedBytes[900] = $tamperedBytes[900] -bxor 1; [IO.File]::WriteAllBytes($tamperedWorker, $tamperedBytes)
    Refresh-BundleInventory $tamperedBundle
    Invoke-ExpectedFailure { Invoke-Builder $tamperedBundle (Join-Path $testRoot 'tampered-output') $fakeCompiler } 'does not preserve'

    $wrongCompiler = Join-Path $compilerRoot 'wrong-ISCC.exe'
    Copy-Item -LiteralPath $fakeCompiler -Destination $wrongCompiler
    Invoke-ExpectedFailure { Invoke-Builder $bundleRoot (Join-Path $testRoot 'wrong-compiler-output') $wrongCompiler } 'reviewed ISCC.exe filename'

    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'fail'
    Invoke-ExpectedFailure { Invoke-Builder $bundleRoot (Join-Path $testRoot 'compiler-failure-output') $fakeCompiler } 'compilation failed'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $testRoot 'compiler-failure-output'))) 'Failed local frozen compiler run left final output.'

    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'mutate-staged'
    $stagedCapture = Join-Path $testRoot 'staged-mutation-arguments.txt'
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE = $stagedCapture
    $stagedMutationResult = @(Invoke-Builder $bundleRoot (Join-Path $testRoot 'staged-mutation-output') $fakeCompiler)[0]
    Assert-True ((Get-Content -LiteralPath $stagedCapture -Raw).Contains('mutation=blocked')) 'Staged bundle read handles did not block compiler-time payload mutation.'
    Assert-True (Test-Path -LiteralPath $stagedMutationResult.InstallerPath) 'Blocked staged mutation prevented valid local installer publication.'

    $driftBundle = Copy-Bundle 'source-drift-bundle'
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = 'mutate-source'
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_SOURCE_MUTATION = Join-Path $driftBundle 'README.txt'
    Invoke-ExpectedFailure { Invoke-Builder $driftBundle (Join-Path $testRoot 'source-drift-output') $fakeCompiler } 'revalidation failed after compilation; refusing to publish'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $testRoot 'source-drift-output'))) 'Source-drift failure left final installer output.'
}
finally {
    $env:CI = $previousCi
    $env:GITHUB_ACTIONS = $previousGitHubActions
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_MODE = $previousMode
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_CAPTURE = $previousCapture
    $env:SCRIBE_LOCAL_FROZEN_TEST_ISCC_SOURCE_MUTATION = $previousSourceMutation
    foreach ($entry in @(Get-ChildItem Env:GIT_*)) { Remove-Item -LiteralPath "Env:$($entry.Name)" }
    foreach ($name in $previousGitEnvironment.Keys) { Set-Item -LiteralPath "Env:$name" -Value $previousGitEnvironment[$name] }
    Remove-TestRootSafely $testRoot
}

Write-Output "Windows local frozen test-installer tests passed ($script:Assertions assertions; real ISCC/install/uninstall is a separate manual gate)."
