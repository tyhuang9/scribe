# Called only by test-windows-release-packaging.ps1, which supplies its inert PE/model
# fixture and package-verifier helpers. This tests packaging orchestration, NOT
# signatures, compiled verifier trust, Inno execution, or GPU/DLL execution.
param(
    [Parameter(Mandatory = $true)][string]$TestRoot,
    [Parameter(Mandatory = $true)][string]$BaseBundleRoot,
    [Parameter(Mandatory = $true)][psobject]$ModelManifest,
    [Parameter(Mandatory = $true)][string]$ModelManifestPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-SamePaths([string[]]$Actual, [string[]]$Expected, [string]$Label) {
    if ($Actual.Count -ne $Expected.Count -or
        (Compare-Object ($Expected | Sort-Object) ($Actual | Sort-Object) -CaseSensitive)) {
        throw "$Label differs from the exact expected paths."
    }
}

$fixtureRoot = Join-Path $TestRoot 'two-pack-contract'
if (Test-Path -LiteralPath $fixtureRoot) { throw 'Two-pack fixture root must be fresh.' }
$null = New-Item -ItemType Directory -Path $fixtureRoot
$portableRoot = Join-Path $fixtureRoot 'portable'
Copy-Item -LiteralPath $BaseBundleRoot -Destination $portableRoot -Recurse
$utf8 = [System.Text.UTF8Encoding]::new($false)
$fixtures = @('cuda', 'vulkan') | ForEach-Object {
    $backend = $_
    $root = Join-Path $fixtureRoot "source-$backend"
    $null = New-Item -ItemType Directory -Path (Join-Path $root 'bin') -Force
    $descriptor = [ordered]@{
        pack_id = "fixture-$backend-windows-x64"
        pack_version = '1.0.0'
        pack_digest = $(if ($backend -ceq 'cuda') { 'a' * 64 } else { 'b' * 64 })
        security_epoch = 1
        runtime_abi_version = 1
        backend = $backend
        provider = "fixture-$backend"
        target_os = 'windows'
        target_arch = 'x86_64'
        worker_relative_path = 'bin/worker.exe'
        root = $root
    }
    $files = @('pack-manifest.json', 'pack-manifest.sig', 'bin/worker.exe', 'bin/provider.dll')
    foreach ($relative in $files) {
        # Deliberately not a real signature or executable. No fixture key is created.
        [System.IO.File]::WriteAllText((Join-Path $root $relative), "inert-$backend-$relative", $utf8)
    }
    $hashes = @{}
    foreach ($relative in $files) {
        $hashes[$relative] = (Get-FileHash -LiteralPath (Join-Path $root $relative) -Algorithm SHA256).Hash
    }
    [pscustomobject]@{ Root = $root; Descriptor = $descriptor; Files = $files; Hashes = $hashes }
}

# Keep the real staging body and helpers; replace only its compiled verifier
# boundary inside a test-local scope. No production switch or trust bypass exists.
$parseErrors = $null
$stageAst = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'stage-verified-worker-packs.ps1'), [ref]$null, [ref]$parseErrors)
if ($parseErrors.Count -ne 0 -or
    ($stageAst.ParamBlock.Parameters.Name.VariablePath.UserPath -join ',') -cne
        'BundleRoot,VerifierExecutable,PackRoot,InstallerAllowlistPath') {
    throw 'Worker-pack staging test seam changed.'
}
$stageFunctions = @($stageAst.EndBlock.Statements | Where-Object {
    $_ -is [System.Management.Automation.Language.FunctionDefinitionAst]
})
if (@($stageFunctions | Where-Object { $_.Name -ceq 'Invoke-PackVerifier' }).Count -ne 1) {
    throw 'Expected exactly one compiled staging verifier boundary.'
}
$stageDefinitions = [scriptblock]::Create(($stageFunctions.Extent.Text -join "`n"))
$stageBody = [scriptblock]::Create((@($stageAst.EndBlock.Statements | Where-Object {
    $_ -isnot [System.Management.Automation.Language.FunctionDefinitionAst]
}).Extent.Text -join "`n"))
$verificationCalls = [System.Collections.Generic.List[string]]::new()

function Get-SyntheticPackDescriptor([string]$Root) {
    $manifest = Join-Path $Root 'pack-manifest.json'
    $manifestHash = (Get-FileHash -LiteralPath $manifest -Algorithm SHA256).Hash
    $matching = @($fixtures | Where-Object { $_.Hashes['pack-manifest.json'] -ceq $manifestHash })
    if ($matching.Count -ne 1) { throw 'Synthetic verifier received an unknown fixture.' }
    $fixture = $matching[0]
    $actualFiles = @(Get-ChildItem -LiteralPath $Root -File -Recurse -Force | ForEach-Object {
        [System.IO.Path]::GetRelativePath($Root, $_.FullName).Replace('\', '/')
    })
    Assert-SamePaths $actualFiles $fixture.Files 'Synthetic fixture inventory'
    foreach ($relative in $fixture.Files) {
        if ((Get-FileHash -LiteralPath (Join-Path $Root $relative) -Algorithm SHA256).Hash -cne $fixture.Hashes[$relative]) {
            throw 'Synthetic verifier found changed payload bytes.'
        }
    }
    $verificationCalls.Add([System.IO.Path]::GetFullPath($Root))
    $result = [pscustomobject]($fixture.Descriptor | ConvertTo-Json | ConvertFrom-Json -AsHashtable)
    $result.root = [System.IO.Path]::GetFullPath($Root)
    return $result
}

function Invoke-TestStage([string]$BundleRoot, [string]$InstallerAllowlistPath, [switch]$Mismatch) {
    $VerifierExecutable = Join-Path $BaseBundleRoot 'local-transcriber.exe'
    $PackRoot = @($fixtures.Root)
    . $stageDefinitions
    function Invoke-PackVerifier([string]$Executable, [string]$Root) {
        if ($Executable -cne $VerifierExecutable) { throw 'Unexpected staging verifier executable.' }
        $descriptor = Get-SyntheticPackDescriptor $Root
        if ($Mismatch -and $Root.StartsWith($BundleRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
            $descriptor.pack_version = 'changed-after-copy'
        }
        return $descriptor
    }
    . $stageBody
}

function Invoke-NativeProcess([string]$Executable, [string[]]$Arguments) {
    # Fail closed on every native request except the exact pack-verifier contract.
    if ($Arguments.Count -ne 2 -or $Arguments[0] -cne '--scribe-verify-worker-pack' -or
        [System.IO.Path]::GetFileName($Executable) -cne 'local-transcriber.exe' -or
        -not $Arguments[1].StartsWith((Split-Path -Parent $Executable) + '\workers\packs\', [StringComparison]::OrdinalIgnoreCase) -or
        -not $Executable.StartsWith($fixtureRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Two-pack test refused an unexpected native invocation.'
    }
    return [pscustomobject]@{
        ExitCode = 0
        Stdout = (Get-SyntheticPackDescriptor $Arguments[1] | ConvertTo-Json -Compress)
        Stderr = ''
    }
}

function Assert-TestBundle([string]$Root) {
    Assert-Bundle -Root $Root -ExpectedModelManifest $ModelManifest `
        -ExpectedModelManifestPath $ModelManifestPath -ExpectedLegalFiles @()
}

$allowlistPath = Join-Path $fixtureRoot 'two-pack-allowlist.iss'
$stage = @(Invoke-TestStage $portableRoot $allowlistPath)
if ($stage.Count -ne 1 -or $stage[0].PackCount -ne 2) { throw 'Both packs must stage as one result.' }
$expectedPackFiles = @()
$expectedCalls = @()
$catalogPath = Join-Path $portableRoot 'worker-pack-catalog.json'
$catalogBytes = [System.IO.File]::ReadAllBytes($catalogPath)
$catalog = Get-Content -LiteralPath $catalogPath -Raw | ConvertFrom-Json
if ($catalog.schema_version -ne 1 -or @($catalog.packs).Count -ne 2) { throw 'Expected a two-pack catalog.' }
for ($index = 0; $index -lt 2; $index++) {
    $fixture = $fixtures[$index]
    $descriptor = $fixture.Descriptor
    $entry = $catalog.packs[$index]
    $relativeRoot = "workers/packs/$($descriptor.pack_id)/$($descriptor.pack_version)/$($descriptor.pack_digest)"
    if ($entry.root -cne $relativeRoot) { throw 'Staging changed the immutable pack root.' }
    foreach ($field in @($descriptor.Keys | Where-Object { $_ -cne 'root' })) {
        if ([string]$entry.$field -cne [string]$descriptor.$field) { throw "Catalog changed '$field'." }
    }
    $expectedFiles = @($fixture.Files | ForEach-Object { "$relativeRoot/$_" })
    Assert-SamePaths @($entry.files) $expectedFiles 'Catalog pack inventory'
    $expectedBytes = [int64](($fixture.Files | ForEach-Object {
        (Get-Item -LiteralPath (Join-Path $fixture.Root $_)).Length
    } | Measure-Object -Sum).Sum)
    if ($entry.installed_size_bytes -ne $expectedBytes -or $entry.compressed_size_bytes -le 0) {
        throw 'Two-pack catalog lost per-pack size evidence.'
    }
    $expectedPackFiles += $expectedFiles
    $expectedCalls += @($fixture.Root, (Join-Path $portableRoot $relativeRoot).Replace('/', '\'))
}
Assert-SamePaths @($stage[0].PackFiles) $expectedPackFiles 'Staged pack union'
if (($verificationCalls -join "`n") -cne ($expectedCalls -join "`n")) {
    throw 'Both source and staged pack trees must be verified in order.'
}

$allowlist = Get-Content -LiteralPath $allowlistPath -Raw
foreach ($kind in @('File', 'Directory')) {
    $body = [regex]::Match($allowlist, "(?s)function IsGeneratedWorkerPack$kind\(RelativePath: String\): Boolean;\s*begin\s*(.*?)\s*end;")
    if (-not $body.Success) { throw "Missing Inno $kind allowlist function." }
    $clauses = @([regex]::Matches($body.Groups[1].Value, "SameStr\(RelativePath, '([^']+)'\)") | ForEach-Object {
        $_.Groups[1].Value.Replace('\', '/')
    })
    $expected = if ($kind -ceq 'File') { $expectedPackFiles } else { @(Get-ExpectedDirectories $expectedPackFiles) }
    Assert-SamePaths $clauses $expected "Inno $kind allowlist"
    $expectedExpression = 'Result :=' + (@($expected | Sort-Object | ForEach-Object {
        "SameStr(RelativePath, '$($_.Replace('/', '\'))')"
    }) -join ' or ') + ';'
    if (($body.Groups[1].Value -replace '\s+', '') -cne ($expectedExpression -replace '\s+', '')) {
        throw "Inno $kind allowlist changed its exact disjunction or introduced extra code."
    }
}

# Rebuild only the fixture inventory, using the independently expected union.
$inventoryPaths = @($baseExpectedInventoryPaths) + $expectedPackFiles
$inventory = [ordered]@{
    schema_version = 1
    platform_triple = 'x86_64-pc-windows-msvc'
    files = @($inventoryPaths | Sort-Object | ForEach-Object {
        $path = Join-Path $portableRoot $_
        [ordered]@{
            path = $_
            size_bytes = [int64](Get-Item -LiteralPath $path).Length
            sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    })
}
[System.IO.File]::WriteAllText((Join-Path $portableRoot 'bundle-inventory.json'), ($inventory | ConvertTo-Json -Depth 6), $utf8)
Assert-TestBundle $portableRoot
Assert-SamePaths @(Get-DeclaredWorkerPackFiles $portableRoot) $expectedPackFiles 'Declared pack union'
$cpuWorker = Get-Item -LiteralPath (Join-Path $BaseBundleRoot 'scribe-inference-worker.exe')
Assert-ExactFile (Join-Path $portableRoot $cpuWorker.Name) $cpuWorker.Length `
    (Get-FileHash -LiteralPath $cpuWorker.FullName -Algorithm SHA256).Hash.ToLowerInvariant()

$zipPath = Join-Path $fixtureRoot 'portable.zip'
[System.IO.Compression.ZipFile]::CreateFromDirectory($portableRoot, $zipPath)
Assert-SafePortableZip $zipPath
$extractedRoot = Join-Path $fixtureRoot 'extracted'
[System.IO.Compression.ZipFile]::ExtractToDirectory($zipPath, $extractedRoot)
Assert-TestBundle $extractedRoot
Assert-PayloadParity $portableRoot $extractedRoot 'Two-pack extracted fixture'

# Negative cases restore bytes/state independently; no real installation occurs.
$catalog.packs = @($catalog.packs[0])
try {
    [System.IO.File]::WriteAllText($catalogPath, ($catalog | ConvertTo-Json -Depth 8), $utf8)
    Invoke-ExpectedFailure { Assert-TestBundle $portableRoot } 'Release payload'
} finally { [System.IO.File]::WriteAllBytes($catalogPath, $catalogBytes) }

$listedPayload = Join-Path $portableRoot $expectedPackFiles[0]
$listedBytes = [System.IO.File]::ReadAllBytes($listedPayload)
try {
    Remove-Item -LiteralPath $listedPayload
    Invoke-ExpectedFailure { Assert-TestBundle $portableRoot } 'Required release file is missing'
} finally { [System.IO.File]::WriteAllBytes($listedPayload, $listedBytes) }

$extraPayload = Join-Path $portableRoot 'workers/packs/unlisted.bin'
try {
    [System.IO.File]::WriteAllText($extraPayload, 'not declared', $utf8)
    Invoke-ExpectedFailure { Assert-TestBundle $portableRoot } 'Release payload differs from its explicit inventory'
} finally { Remove-Item -LiteralPath $extraPayload }

$copiedPayload = Join-Path $extractedRoot $expectedPackFiles[0]
$copiedBytes = [System.IO.File]::ReadAllBytes($copiedPayload)
try {
    $changedBytes = [byte[]]$copiedBytes.Clone()
    $changedBytes[0] = $changedBytes[0] -bxor 1
    [System.IO.File]::WriteAllBytes($copiedPayload, $changedBytes)
    Invoke-ExpectedFailure { Assert-PayloadParity $portableRoot $extractedRoot 'Two-pack extracted fixture' } 'payload parity mismatch'
} finally { [System.IO.File]::WriteAllBytes($copiedPayload, $copiedBytes) }

$failedRoot = Join-Path $fixtureRoot 'staged-mismatch'
$failedAllowlist = Join-Path $fixtureRoot 'staged-mismatch.iss'
$null = New-Item -ItemType Directory -Path $failedRoot
Invoke-ExpectedFailure { Invoke-TestStage $failedRoot $failedAllowlist -Mismatch } "Staged worker-pack descriptor changed field 'pack_version'"
if ((Test-Path -LiteralPath (Join-Path $failedRoot 'worker-pack-catalog.json')) -or
    (Test-Path -LiteralPath $failedAllowlist)) {
    throw 'Failed staging published a catalog or installer allowlist.'
}
Assert-TestBundle $portableRoot
Assert-TestBundle $extractedRoot
Write-Output 'Two-pack synthetic staging/catalog/Inno-allowlist/portable/parity contracts passed (no signature or installer execution claim).'
