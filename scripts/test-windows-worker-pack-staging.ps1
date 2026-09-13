$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'windows-gpu-pack-history.ps1')

# Exercise the real post-verification serializer and generator, without executing
# the stager's verifier/copy entry point or granting fixture bytes production trust.
$tokens = $null
$parseErrors = $null
$stagerAst = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'stage-verified-worker-packs.ps1'),
    [ref]$tokens,
    [ref]$parseErrors
)
if ($parseErrors.Count -ne 0) {
    throw 'Worker-pack staging script has parse errors.'
}
foreach ($functionName in @(
    'Assert-RegularNonReparseFile', 'Write-WorkerPackCatalog',
    'Get-InstallerFileIdentityClauses', 'Write-InstallerAllowlist'
)) {
    $definitions = @($stagerAst.FindAll({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -ceq $functionName
    }, $false))
    if ($definitions.Count -ne 1) {
        throw "Expected exactly one staging helper: $functionName"
    }
    Invoke-Expression $definitions[0].Extent.Text
}

function Assert-StagingTrue([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-StagingFailure([scriptblock]$Action, [string]$ExpectedText) {
    try { & $Action }
    catch {
        if (-not $_.Exception.Message.Contains($ExpectedText)) {
            throw "Expected '$ExpectedText', got: $($_.Exception.Message)"
        }
        return
    }
    throw "Expected failure containing '$ExpectedText'."
}

function New-StagingFixturePack([string]$Bundle, [string]$Backend, [string]$Version, [char]$Digest) {
    $packId = "scribe-$Backend-windows-x64"
    $packDigest = ([string]$Digest) * 64
    $root = "workers/packs/$packId/$Version/$packDigest"
    $files = @('alpha.bin', 'pack-manifest.sig', 'Zulu.bin', 'pack-manifest.json', 'empty.txt')
    New-Item -ItemType Directory -Path (Join-Path $Bundle $root) -Force | Out-Null
    foreach ($relative in $files) {
        $bytes = [byte[]]@()
        if ($relative -cne 'empty.txt') {
            $bytes = [System.Text.Encoding]::UTF8.GetBytes("fixture:$Backend/$Version/$relative")
        }
        [System.IO.File]::WriteAllBytes((Join-Path $Bundle "$root/$relative"), [byte[]]$bytes)
    }
    return [ordered]@{
        pack_id = $packId
        pack_version = $Version
        pack_digest = $packDigest
        security_epoch = 1
        runtime_abi_version = 1
        backend = $Backend
        provider = "fixture-$Backend"
        target_os = 'windows'
        target_arch = 'x86_64'
        worker_relative_path = 'Zulu.bin'
        root = $root
        installed_size_bytes = 123
        compressed_size_bytes = 99
        files = @($files | ForEach-Object { "$root/$_" })
    }
}

function Get-StagingGeneratedFunction([string]$Source, [string]$Name) {
    $pattern = '(?ms)^function ' + [regex]::Escape($Name) + '\(.*?^end;'
    $matches = [regex]::Matches($Source, $pattern)
    if ($matches.Count -ne 1) { throw "Expected exactly one generated function: $Name" }
    return $matches[0].Value
}

$testParent = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\', '/')
$testRoot = Join-Path $testParent "scribe-worker-pack-staging-test-$([guid]::NewGuid().ToString('N'))"
$savedCulture = [System.Globalization.CultureInfo]::CurrentCulture
$testCount = 0
try {
    New-Item -ItemType Directory -Path $testRoot | Out-Null
    $emptyHistory = ConvertFrom-WindowsGpuPackHistoryJson `
        -Json '{"schema_version":1,"history_epoch":1,"releases":[]}'
    $baselineCatalog = $null
    $baselineAllowlist = $null
    $current = $null
    $plan = $null
    $bundle = $null
    foreach ($cultureName in @('en-US', 'tr-TR', 'sv-SE')) {
        [System.Globalization.CultureInfo]::CurrentCulture = [System.Globalization.CultureInfo]::GetCultureInfo($cultureName)
        $bundle = Join-Path $testRoot $cultureName
        New-Item -ItemType Directory -Path $bundle | Out-Null
        $cuda = New-StagingFixturePack $bundle 'cuda' '2.0.0' 'b'
        $vulkan = New-StagingFixturePack $bundle 'vulkan' '2.0.0' 'c'
        $packs = if ($cultureName -ceq 'tr-TR') { @($cuda, $vulkan) } else { @($vulkan, $cuda) }
        $current = Write-WorkerPackCatalog $bundle $packs ('2' * 40)
        $catalogBytes = [Convert]::ToBase64String([System.IO.File]::ReadAllBytes($current.Path))
        $plan = Get-WindowsGpuPackRetirementPlan $emptyHistory $current.History
        Assert-StagingTrue ($plan.CurrentFiles.Count -eq 10) 'Nonempty staging lost expected files.'
        Assert-StagingTrue ($current.History.releases[0].packs[0].pack_id -ceq $cuda.pack_id) 'Pack order is not ordinal.'
        Assert-StagingTrue ($current.History.releases[0].packs[0].files[0].path.EndsWith('/Zulu.bin')) 'File order is not ordinal.'
        foreach ($file in $plan.CurrentFiles) {
            $physicalPath = Join-Path $bundle $file.Path
            $hash = (Get-FileHash -LiteralPath $physicalPath -Algorithm SHA256).Hash.ToLowerInvariant()
            Assert-StagingTrue ($file.Sha256 -ceq $hash -and
                $file.SizeBytes -eq (Get-Item -LiteralPath $physicalPath).Length) 'Current file identity does not bind staged bytes.'
        }
        $allowlist = Join-Path $testRoot "$cultureName.iss"
        Write-InstallerAllowlist $allowlist $plan.CurrentFiles.Path $plan $current.SizeBytes $current.Sha256
        $allowlistSource = Get-Content -LiteralPath $allowlist -Raw
        if ($null -eq $baselineCatalog) {
            $baselineCatalog = $catalogBytes
            $baselineAllowlist = $allowlistSource
        }
        else {
            Assert-StagingTrue ($catalogBytes -ceq $baselineCatalog) 'Catalog bytes changed with caller order or culture.'
            Assert-StagingTrue ($allowlistSource -ceq $baselineAllowlist) 'Installer identities changed with caller order or culture.'
        }
    }
    $testCount++
    Write-Output 'PASS: nonempty staged identities are byte-exact and culture/order independent'

    $identitySource = Get-StagingGeneratedFunction $baselineAllowlist 'GetGeneratedCurrentWorkerPackFileIdentity'
    Assert-StagingTrue ($identitySource.Contains("Result := False;") -and
        $identitySource.Contains('FileSize := -1;') -and
        $identitySource.Contains("Sha256 := '';")) 'Unknown current files do not default-deny.'
    Assert-StagingTrue ([regex]::Matches($identitySource, 'if SameStr\(RelativePath,').Count -eq 10) 'Generated current identity table has wrong cardinality.'
    foreach ($file in $plan.CurrentFiles) {
        $native = $file.Path.Replace('/', '\')
        $expected = [regex]::Escape("if SameStr(RelativePath, '$native') then") +
            '\s+begin\s+' + [regex]::Escape("FileSize := $($file.SizeBytes);") +
            '\s+' + [regex]::Escape("Sha256 := '$($file.Sha256)';")
        Assert-StagingTrue ($identitySource -cmatch $expected) 'Generated current file lost its size/hash binding.'
    }
    $countSource = Get-StagingGeneratedFunction $baselineAllowlist 'GetGeneratedCurrentWorkerPackFileCount'
    Assert-StagingTrue ($countSource.Contains('Result := 10;')) 'Generated completeness count is wrong.'
    $catalogSource = Get-StagingGeneratedFunction $baselineAllowlist 'IsGeneratedCurrentWorkerCatalog'
    Assert-StagingTrue ($catalogSource.Contains("FileSize = $($current.SizeBytes)") -and
        $catalogSource.Contains($current.Sha256)) 'Generated current catalog lost its exact identity.'
    $testCount++
    Write-Output 'PASS: generated current lookup, completeness count, and catalog are exact and default-deny'

    $oldBundle = Join-Path $testRoot 'old'
    New-Item -ItemType Directory -Path $oldBundle | Out-Null
    $oldPack = New-StagingFixturePack $oldBundle 'cuda' '1.0.0' 'a'
    $old = Write-WorkerPackCatalog $oldBundle @($oldPack) ('1' * 40)
    $upgrade = Get-WindowsGpuPackRetirementPlan $old.History $current.History
    $upgradePath = Join-Path $testRoot 'upgrade.iss'
    Write-InstallerAllowlist $upgradePath $upgrade.CurrentFiles.Path $upgrade $current.SizeBytes $current.Sha256
    $upgradeSource = Get-Content -LiteralPath $upgradePath -Raw
    $retiredSource = Get-StagingGeneratedFunction $upgradeSource 'GetGeneratedRetiredWorkerPackFileIdentity'
    Assert-StagingTrue ([regex]::Matches($retiredSource, 'if SameStr\(RelativePath,').Count -eq 5) 'Retired identity table does not match exact old inventory.'
    $currentPathsSource = Get-StagingGeneratedFunction $upgradeSource 'IsGeneratedWorkerPackFile'
    Assert-StagingTrue (-not $currentPathsSource.Contains($oldPack.root.Replace('/', '\'))) 'History accidentally broadened current-only file admission.'
    $knownSource = Get-StagingGeneratedFunction $upgradeSource 'IsGeneratedKnownWorkerCatalog'
    Assert-StagingTrue ($knownSource.Contains($old.Sha256) -and $knownSource.Contains($current.Sha256)) 'Known catalog set lost current or historical identity.'
    Assert-StagingTrue (-not (Get-StagingGeneratedFunction $upgradeSource 'IsGeneratedCurrentWorkerCatalog').Contains($old.Sha256)) 'Old catalog became current authority.'
    $testCount++
    Write-Output 'PASS: previous identities are emitted separately without broadening current admission'

    $invalidBundle = Join-Path $testRoot 'invalid-revision'
    New-Item -ItemType Directory -Path $invalidBundle | Out-Null
    Assert-StagingFailure { Write-WorkerPackCatalog $invalidBundle @() 'main' } 'exact checked-out source revision'
    Assert-StagingTrue (-not (Test-Path -LiteralPath (Join-Path $invalidBundle 'worker-pack-catalog.json'))) 'Invalid source revision wrote a catalog.'
    $testCount++
    Write-Output 'PASS: invalid source revision is rejected before catalog creation'

    # Synthetic extra metadata tests the serializer byte envelope only. It is
    # not a schema-valid production catalog and never reaches a pack verifier.
    Assert-StagingTrue ($script:WindowsGpuPackHistoryMaximumCatalogSize -eq 512KB) 'History catalog bound drifted from the runtime 512 KiB contract.'
    $runtimeSource = Get-Content -LiteralPath (Join-Path (Split-Path -Parent $PSScriptRoot) 'src/gpu_worker_pack/mod.rs') -Raw
    Assert-StagingTrue ($runtimeSource.Contains('const MAX_PACK_CATALOG_BYTES: u64 = 512 * 1024;')) 'Review the changed runtime catalog bound before updating staging.'
    $sizeBundle = Join-Path $testRoot 'catalog-size'
    New-Item -ItemType Directory -Path $sizeBundle | Out-Null
    $sizePack = New-StagingFixturePack $sizeBundle 'cuda' 'catalog-size' 'd'
    $sizePack.fixture_padding = ''
    $smallCatalog = Write-WorkerPackCatalog $sizeBundle @($sizePack) ('4' * 40)
    $sizePack.fixture_padding = 'x' * (512KB - $smallCatalog.SizeBytes)
    $limitCatalog = Write-WorkerPackCatalog $sizeBundle @($sizePack) ('4' * 40)
    Assert-StagingTrue ($limitCatalog.SizeBytes -eq 512KB) 'Exact catalog byte limit was not accepted.'
    $limitDigest = $limitCatalog.Sha256
    $sizePack.fixture_padding += 'x'
    Assert-StagingFailure {
        Write-WorkerPackCatalog $sizeBundle @($sizePack) ('4' * 40)
    } 'runtime catalog byte bound'
    Assert-StagingTrue ((Get-FileHash -LiteralPath $limitCatalog.Path -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $limitDigest) 'Oversized catalog overwrote the previous output.'
    $testCount++
    Write-Output 'PASS: exact 512 KiB catalog accepted; one byte larger rejected before overwrite'

    $changedFile = Join-Path $oldBundle "$($oldPack.root)/alpha.bin"
    [System.IO.File]::WriteAllBytes($changedFile, [byte[]]@(99))
    $changed = Write-WorkerPackCatalog $oldBundle @($oldPack) ('3' * 40)
    Assert-StagingFailure {
        Get-WindowsGpuPackRetirementPlan $old.History $changed.History
    } 'different exact inventory'
    $testCount++
    Write-Output 'PASS: a reused immutable root cannot change its staged byte identities'
}
finally {
    [System.Globalization.CultureInfo]::CurrentCulture = $savedCulture
    $resolved = [System.IO.Path]::GetFullPath($testRoot)
    if ([System.IO.Path]::GetDirectoryName($resolved) -cne $testParent -or
        [System.IO.Path]::GetFileName($resolved) -cnotmatch '^scribe-worker-pack-staging-test-[0-9a-f]{32}$') {
        throw 'Refused staging-test cleanup outside its exact temporary child.'
    }
    if (Test-Path -LiteralPath $resolved) {
        # No fixture creates a link; refuse unexpected links before recursive cleanup.
        foreach ($item in @((Get-Item -LiteralPath $resolved -Force)) + @(Get-ChildItem -LiteralPath $resolved -Recurse -Force)) {
            if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw 'Refused staging-test cleanup containing an unexpected reparse point.'
            }
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
if ($testCount -ne 6) { throw 'Expected staging test cases were not all executed.' }
Write-Output "Windows worker-pack staging tests passed ($testCount cases)."
