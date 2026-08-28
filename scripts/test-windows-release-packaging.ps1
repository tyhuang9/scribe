$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$releaseScript = Join-Path $PSScriptRoot "build-windows-release.ps1"
$modelScript = Join-Path $PSScriptRoot "bundle-base-model.ps1"
$packageVerifier = Join-Path $PSScriptRoot "verify-windows-release-package.ps1"
$source = Get-Content -LiteralPath $releaseScript -Raw
$helpersStart = $source.IndexOf("function Get-NormalizedFullPath")
$helpersEnd = $source.IndexOf("if (-not [Environment]::Is64BitOperatingSystem")
if ($helpersStart -lt 0 -or $helpersEnd -le $helpersStart) {
    throw "Could not isolate Windows release helper functions for testing."
}
$expectedPeMachine = 0x8664
Invoke-Expression $source.Substring($helpersStart, $helpersEnd - $helpersStart)

$verifierSource = Get-Content -LiteralPath $packageVerifier -Raw
$verifierPreambleStart = $verifierSource.IndexOf("`$targetTriple =")
$verifierHelpersStart = $verifierSource.IndexOf("function Get-NormalizedPath")
$verifierHelpersEnd = $verifierSource.IndexOf("`$bundle = Get-NormalizedPath")
if ($verifierPreambleStart -lt 0 -or
    $verifierHelpersStart -le $verifierPreambleStart -or
    $verifierHelpersEnd -le $verifierHelpersStart) {
    throw "Could not isolate Windows release package verifier helpers for testing."
}
$verifierPreamble = $verifierSource.Substring($verifierPreambleStart, $verifierHelpersStart - $verifierPreambleStart)
$quotedScriptRoot = $PSScriptRoot.Replace("'", "''")
$verifierPreamble = $verifierPreamble.Replace('$PSScriptRoot', "'$quotedScriptRoot'")
Invoke-Expression $verifierPreamble
Invoke-Expression $verifierSource.Substring($verifierHelpersStart, $verifierHelpersEnd - $verifierHelpersStart)

function Invoke-ExpectedFailure([scriptblock]$Action, [string]$ExpectedText) {
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

function Write-TestPe([string]$Path, [uint16]$Machine) {
    $bytes = [byte[]]::new(256)
    $bytes[0] = 0x4D
    $bytes[1] = 0x5A
    [BitConverter]::GetBytes([uint32]0x40).CopyTo($bytes, 0x3C)
    [BitConverter]::GetBytes([uint32]0x00004550).CopyTo($bytes, 0x40)
    [BitConverter]::GetBytes($Machine).CopyTo($bytes, 0x44)
    [BitConverter]::GetBytes([uint16]0x20B).CopyTo($bytes, 0x58)
    [BitConverter]::GetBytes([uint16]2).CopyTo($bytes, 0x9C)
    [System.IO.File]::WriteAllBytes($Path, $bytes)
}

function New-TestReleaseRuleset {
    @'
{
  "id": 21505050,
  "node_id": "RRS_lACqUmVwb3NpdG9yec5L6WbnzgFIJBo",
  "updated_at": "2026-08-25T18:57:12.727-05:00",
  "name": "Protect release tags",
  "target": "tag",
  "source_type": "Repository",
  "source": "tyhuang9/scribe",
  "enforcement": "active",
  "current_user_can_bypass": "never",
  "conditions": {
    "ref_name": {
      "include": ["refs/tags/v*"],
      "exclude": []
    }
  },
  "rules": [
    { "type": "update" },
    { "type": "deletion" }
  ]
}
'@ | ConvertFrom-Json -Depth 20
}

function Assert-ReleaseRulesetContract([psobject]$Ruleset) {
    $requiredRulesetName = 'Protect release tags'
    $requiredRulesetId = 21505050
    $requiredRulesetNodeId = 'RRS_lACqUmVwb3NpdG9yec5L6WbnzgFIJBo'
    $requiredRulesetUpdatedAt = '2026-08-25T18:57:12.727-05:00'
    foreach ($requiredProperty in @(
        'id', 'node_id', 'updated_at', 'name', 'target', 'source_type', 'source',
        'enforcement', 'current_user_can_bypass', 'conditions', 'rules'
    )) {
        if ($null -eq $Ruleset.PSObject.Properties[$requiredProperty]) {
            throw "Ruleset is missing required property $requiredProperty"
        }
    }
    if ($Ruleset.id -ne $requiredRulesetId -or
        $Ruleset.node_id -cne $requiredRulesetNodeId -or
        $Ruleset.updated_at -cne $requiredRulesetUpdatedAt -or
        $Ruleset.name -cne $requiredRulesetName -or
        $Ruleset.target -cne 'tag' -or
        $Ruleset.source_type -cne 'Repository' -or
        $Ruleset.source -cne 'tyhuang9/scribe' -or
        $Ruleset.enforcement -cne 'active' -or
        $Ruleset.current_user_can_bypass -cne 'never') {
        throw "Ruleset identity, ownership, enforcement, or bypass contract changed"
    }
    $conditionNames = @($Ruleset.conditions.PSObject.Properties.Name)
    if ($conditionNames.Count -ne 1 -or $conditionNames[0] -cne 'ref_name') {
        throw "Ruleset must define only ref-name conditions"
    }
    $refName = $Ruleset.conditions.ref_name
    $refNameProperties = @($refName.PSObject.Properties.Name | Sort-Object)
    if ($refNameProperties.Count -ne 2 -or
        $refNameProperties[0] -cne 'exclude' -or
        $refNameProperties[1] -cne 'include' -or
        $refName.include -isnot [System.Array] -or
        $refName.exclude -isnot [System.Array]) {
        throw "Ruleset must define unambiguous ref includes and exclusions"
    }
    if (@($refName.include).Count -ne 1 -or
        @($refName.include)[0] -cne 'refs/tags/v*' -or
        @($refName.exclude).Count -ne 0) {
        throw "Ruleset ref conditions changed"
    }
    if ($null -ne $Ruleset.PSObject.Properties['bypass_actors'] -and
        ($Ruleset.bypass_actors -isnot [System.Array] -or @($Ruleset.bypass_actors).Count -ne 0)) {
        throw "Ruleset must not allow bypass actors"
    }
    if ($Ruleset.rules -isnot [System.Array]) {
        throw "Ruleset must define an unambiguous rules array"
    }
    $ruleTypes = @($Ruleset.rules | ForEach-Object {
        if ($null -eq $_.PSObject.Properties['type'] -or $_.type -isnot [string]) {
            throw "Ruleset contains a rule without a valid type"
        }
        $_.type
    })
    if ($ruleTypes.Count -ne 2 -or
        @($ruleTypes | Where-Object { $_ -ceq 'update' }).Count -ne 1 -or
        @($ruleTypes | Where-Object { $_ -ceq 'deletion' }).Count -ne 1) {
        throw "Ruleset must contain exactly update and deletion rules"
    }
}

$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-release-script-$PID-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $testRoot | Out-Null
try {
    $amd64 = Join-Path $testRoot "amd64.exe"
    $x86 = Join-Path $testRoot "x86.exe"
    Write-TestPe $amd64 0x8664
    Write-TestPe $x86 0x014C
    Assert-Amd64Pe $amd64
    Assert-WindowsGuiSubsystem $amd64
    Invoke-ExpectedFailure { Assert-Amd64Pe $x86 } "PE Machine mismatch"
    $consoleSubsystem = Join-Path $testRoot "console-subsystem.exe"
    Write-TestPe $consoleSubsystem 0x8664
    $consoleBytes = [System.IO.File]::ReadAllBytes($consoleSubsystem)
    [BitConverter]::GetBytes([uint16]3).CopyTo($consoleBytes, 0x9C)
    [System.IO.File]::WriteAllBytes($consoleSubsystem, $consoleBytes)
    Invoke-ExpectedFailure { Assert-WindowsGuiSubsystem $consoleSubsystem } "PE subsystem mismatch"

    $pwshPath = (Get-Process -Id $PID).Path
    $nativeProcess = Invoke-NativeProcess $pwshPath @(
        "-NoProfile",
        "-Command",
        "[Console]::Out.Write('captured-output'); [Console]::Error.Write('captured-error'); exit 7"
    )
    if ($nativeProcess.ExitCode -ne 7 -or
        $nativeProcess.Stdout -ne "captured-output" -or
        $nativeProcess.Stderr -ne "captured-error") {
        throw "Synchronous native-process capture did not preserve exit, stdout, and stderr evidence."
    }

    $final = Join-Path $testRoot "Scribe-windows-x64"
    $validStaging = "$final.staging-$PID-$([guid]::NewGuid().ToString('N'))"
    New-Item -ItemType Directory -Path $validStaging | Out-Null
    [System.IO.File]::WriteAllBytes((Join-Path $validStaging "marker.bin"), [byte[]](1, 2, 3))
    Remove-ValidatedStaging $validStaging $final
    if (Test-Path -LiteralPath $validStaging) {
        throw "Validated staging cleanup did not remove its bounded target."
    }

    $outsideParent = Join-Path $testRoot "outside-parent"
    New-Item -ItemType Directory -Path $outsideParent | Out-Null
    $outside = Join-Path $outsideParent "Scribe-windows-x64.staging-$PID-$([guid]::NewGuid().ToString('N'))"
    New-Item -ItemType Directory -Path $outside | Out-Null
    $outsideMarker = Join-Path $outside "keep.bin"
    [System.IO.File]::WriteAllBytes($outsideMarker, [byte[]](4, 5, 6))
    Invoke-ExpectedFailure { Remove-ValidatedStaging $outside $final } "direct sibling"
    if (-not (Test-Path -LiteralPath $outsideMarker -PathType Leaf)) {
        throw "Out-of-bounds cleanup touched an unrelated marker."
    }

    $allowlist = Join-Path $testRoot "allowlist"
    New-Item -ItemType Directory -Path (Join-Path $allowlist "nested") -Force | Out-Null
    [System.IO.File]::WriteAllBytes((Join-Path $allowlist "one.bin"), [byte[]](1))
    [System.IO.File]::WriteAllBytes((Join-Path $allowlist "nested\two.bin"), [byte[]](2))
    Assert-ExactAllowlist $allowlist @("one.bin", "nested/two.bin")
    [System.IO.File]::WriteAllBytes((Join-Path $allowlist "unexpected.bin"), [byte[]](3))
    Invoke-ExpectedFailure {
        Assert-ExactAllowlist $allowlist @("one.bin", "nested/two.bin")
    } "outside the explicit allowlist"

    $inventoryFile = Join-Path $allowlist "one.bin"
    $inventoryItem = Get-Item -LiteralPath $inventoryFile
    $inventoryHash = (Get-FileHash -LiteralPath $inventoryFile -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-ExactFile $inventoryFile $inventoryItem.Length $inventoryHash
    [System.IO.File]::WriteAllBytes($inventoryFile, [byte[]](9))
    Invoke-ExpectedFailure {
        Assert-ExactFile $inventoryFile $inventoryItem.Length $inventoryHash
    } "SHA-256 mismatch"

    $targetBundle = Join-Path $repositoryRoot "target\scribe-release-probe-$PID"
    Invoke-ExpectedFailure {
        & $releaseScript -ModelSource "missing-model" -BundlePath $targetBundle
    } "Cargo target directories"
    if (Test-Path -LiteralPath $targetBundle) {
        throw "Rejected Cargo-target bundle path was mutated."
    }

    $previousCargoTargetDirectory = $env:CARGO_TARGET_DIR
    $externalCargoTarget = Join-Path $testRoot "external-cargo-target"
    try {
        $env:CARGO_TARGET_DIR = $externalCargoTarget
        Invoke-ExpectedFailure {
            & $releaseScript -ModelSource "missing-model" -BundlePath (Join-Path $externalCargoTarget "portable")
        } "Cargo target directories"
    }
    finally {
        $env:CARGO_TARGET_DIR = $previousCargoTargetDirectory
    }
    if (Test-Path -LiteralPath $externalCargoTarget) {
        throw "Rejected external Cargo-target bundle path was mutated."
    }

    $existingFinal = Join-Path $testRoot "existing-final"
    New-Item -ItemType Directory -Path $existingFinal | Out-Null
    $existingMarker = Join-Path $existingFinal "keep.bin"
    [System.IO.File]::WriteAllBytes($existingMarker, [byte[]](7))
    Invoke-ExpectedFailure {
        & $releaseScript -ModelSource "missing-model" -BundlePath $existingFinal
    } "already exists"
    if (-not (Test-Path -LiteralPath $existingMarker -PathType Leaf)) {
        throw "Existing final bundle was mutated."
    }

    $staleFinal = Join-Path $testRoot "stale-final"
    $stale = "$staleFinal.staging-old"
    New-Item -ItemType Directory -Path $stale | Out-Null
    $staleMarker = Join-Path $stale "keep.bin"
    [System.IO.File]::WriteAllBytes($staleMarker, [byte[]](8))
    Invoke-ExpectedFailure {
        & $releaseScript -ModelSource "missing-model" -BundlePath $staleFinal
    } "stale release staging sibling"
    if (-not (Test-Path -LiteralPath $staleMarker -PathType Leaf)) {
        throw "Stale staging refusal mutated the stale directory."
    }

    $modelDestination = Join-Path $testRoot "model-destination"
    $otherDestination = Join-Path $testRoot "other-destination"
    New-Item -ItemType Directory -Path $modelDestination | Out-Null
    New-Item -ItemType Directory -Path $otherDestination | Out-Null
    Invoke-ExpectedFailure {
        & $modelScript -Source "missing-model" -Destination $modelDestination -Executable (Join-Path $modelDestination "renamed.exe")
    } "exact executable name"
    Invoke-ExpectedFailure {
        & $modelScript -Source "missing-model" -Destination $modelDestination -Executable (Join-Path $otherDestination "local-transcriber.exe")
    } "canonical executable parent"

    $realDestination = Join-Path $testRoot "real-destination"
    $junctionDestination = Join-Path $testRoot "junction-destination"
    New-Item -ItemType Directory -Path $realDestination | Out-Null
    New-Item -ItemType Junction -Path $junctionDestination -Target $realDestination | Out-Null
    Invoke-ExpectedFailure {
        & $modelScript -Source "missing-model" -Destination $junctionDestination -Executable (Join-Path $junctionDestination "local-transcriber.exe")
    } "reparse point"

    $workflow = Get-Content -LiteralPath (Join-Path $repositoryRoot ".github\workflows\release.yml") -Raw
    if ($workflow -notmatch "prepare-windows-release-inputs\.ps1" -or
        $workflow -notmatch "build-windows-release\.ps1" -or
        $workflow -notmatch "choco install innosetup --version=6\.7\.1" -or
        $workflow -notmatch "-PortableZipPath dist\\Scribe-windows-x64\.zip" -or
        $workflow -match "-RuntimeSource" -or
        $workflow -match "Copy-Item target\\release\\local-transcriber\.exe") {
        throw "Windows release workflow must package the validated full bundle, not a bare executable."
    }
    $releaseRulesetFixture = New-TestReleaseRuleset
    Assert-ReleaseRulesetContract $releaseRulesetFixture
    foreach ($mutation in @(
        @{ Name = 'id'; Action = { param($ruleset) $ruleset.id = 1 }; Expected = 'identity' },
        @{ Name = 'node id'; Action = { param($ruleset) $ruleset.node_id = 'RRS_wrong' }; Expected = 'identity' },
        @{ Name = 'revision'; Action = { param($ruleset) $ruleset.updated_at = '2026-08-25T18:57:12.728-05:00' }; Expected = 'identity' },
        @{ Name = 'current-user bypass'; Action = { param($ruleset) $ruleset.current_user_can_bypass = 'always' }; Expected = 'identity' },
        @{ Name = 'ref condition'; Action = { param($ruleset) $ruleset.conditions.ref_name.include = @('refs/tags/*') }; Expected = 'ref conditions' },
        @{ Name = 'extra rule'; Action = { param($ruleset) $ruleset.rules += [pscustomobject]@{ type = 'creation' } }; Expected = 'exactly update and deletion' }
    )) {
        $mutatedRuleset = New-TestReleaseRuleset
        & $mutation.Action $mutatedRuleset
        Invoke-ExpectedFailure {
            Assert-ReleaseRulesetContract $mutatedRuleset
        } $mutation.Expected
    }
    $bypassActorRuleset = New-TestReleaseRuleset
    $bypassActorRuleset | Add-Member -NotePropertyName bypass_actors -NotePropertyValue @([pscustomobject]@{ actor_type = 'RepositoryRole' })
    Invoke-ExpectedFailure {
        Assert-ReleaseRulesetContract $bypassActorRuleset
    } 'must not allow bypass actors'
    foreach ($requiredPublicationGuard in @(
        'publish_release:',
        'type: boolean',
        'default: false',
        "inputs.publish_release == true",
        "github.event.repository.default_branch",
        "github.ref == format('refs/heads/{0}', github.event.repository.default_branch)",
        "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/')",
        "if: github.event_name == 'workflow_dispatch'",
        'git ls-remote --exit-code --tags origin',
        'Could not confirm that tag',
        'Could not confirm that release',
        '$savedNativeErrorPreference = $PSNativeCommandUseErrorActionPreference',
        '$PSNativeCommandUseErrorActionPreference = $false',
        '$global:LASTEXITCODE = 0',
        '$PSNativeCommandUseErrorActionPreference = $savedNativeErrorPreference',
        'needs: build',
        'name: windows-release-assets',
        '& .\scripts\test-windows-release-packaging.ps1',
        'queue: max',
        'gh api --method POST',
        'git/refs',
        'ref=refs/tags/$env:RELEASE_TAG',
        'sha=$env:RELEASE_SHA',
        'git/ref/tags/$env:RELEASE_TAG',
        'refs/tags/$env:RELEASE_TAG^{}',
        "`$requiredRulesetName = 'Protect release tags'",
        "`$requiredRulesetId = 21505050",
        "`$requiredRulesetNodeId = 'RRS_lACqUmVwb3NpdG9yec5L6WbnzgFIJBo'",
        "`$requiredRulesetUpdatedAt = '2026-08-25T18:57:12.727-05:00'",
        "-H 'X-GitHub-Api-Version: 2026-03-10'",
        'rulesets/$requiredRulesetId',
        "`$ruleset.id -ne `$requiredRulesetId",
        "`$ruleset.node_id -cne `$requiredRulesetNodeId",
        "`$ruleset.updated_at -cne `$requiredRulesetUpdatedAt",
        "`$ruleset.target -cne 'tag'",
        "`$ruleset.source_type -cne 'Repository'",
        "`$ruleset.source -cne `$env:GITHUB_REPOSITORY",
        "`$ruleset.enforcement -cne 'active'",
        "`$ruleset.current_user_can_bypass -cne 'never'",
        "`$conditionNames.Count -ne 1",
        "`$conditionNames[0] -cne 'ref_name'",
        "`$refNamePropertyNames.Count -ne 2",
        "`$refNamePropertyNames[0] -cne 'exclude'",
        "`$refNamePropertyNames[1] -cne 'include'",
        "`$includedRefs[0] -cne 'refs/tags/v*'",
        "`$excludedRefs.Count -ne 0",
        "`$ruleset.bypass_actors",
        "`$null -ne `$ruleset.PSObject.Properties['bypass_actors']",
        "`$_ -ceq 'update'",
        "`$_ -ceq 'deletion'",
        "`$ruleTypes.Count -ne 2",
        'must contain exactly update and deletion rules',
        '--draft=false',
        '--latest',
        '--prerelease=false',
        '--verify-tag'
    )) {
        if (-not $workflow.Contains($requiredPublicationGuard)) {
            throw "Windows release workflow must retain publication guard: $requiredPublicationGuard"
        }
    }
    if ($workflow.Contains('rulesets?per_page=100') -or
        $workflow -match "requiredProperty in @\([^)]*bypass_actors") {
        throw "Windows release workflow must not discover or require hidden bypass actor fields"
    }

    $readme = Get-Content -LiteralPath (Join-Path $repositoryRoot "README.md") -Raw
    $canonicalReleaseAssets = @('Scribe-Setup.exe', 'Scribe-windows-x64.zip')
    $latestDownloadMatches = @(
        [regex]::Matches($readme, 'releases/latest/download/(?<asset>[^"?#<]+)')
    )
    $readmeReleaseAssets = @($latestDownloadMatches | ForEach-Object { $_.Groups['asset'].Value })
    if ($readmeReleaseAssets.Count -ne $canonicalReleaseAssets.Count) {
        throw "README must link exactly the canonical installer and portable ZIP release assets."
    }
    foreach ($canonicalAsset in $canonicalReleaseAssets) {
        if (@($readmeReleaseAssets | Where-Object { $_ -ceq $canonicalAsset }).Count -ne 1) {
            throw "README must link exactly once to canonical release asset $canonicalAsset."
        }
        if (-not $workflow.Contains("dist/$canonicalAsset") -or
            -not $workflow.Contains("'$canonicalAsset'")) {
            throw "Windows release workflow must upload and publish canonical README asset $canonicalAsset."
        }
    }
    if ($workflow.Contains('--target $env:RELEASE_TARGET_SHA') -or
        $workflow.Contains('cancel-in-progress:')) {
        throw "Windows release publication must use verified atomic tags and non-cancelling queued concurrency."
    }
    $duplicateGuardStepStart = $workflow.IndexOf('      - name: Refuse duplicate manual release')
    $duplicateGuardRunMarker = $workflow.IndexOf('        run: |', $duplicateGuardStepStart)
    $duplicateGuardScriptStart = $workflow.IndexOf("`n", $duplicateGuardRunMarker) + 1
    $duplicateGuardScriptEnd = $workflow.IndexOf('      - name: Download verified release assets', $duplicateGuardScriptStart)
    if ($duplicateGuardStepStart -lt 0 -or
        $duplicateGuardRunMarker -lt $duplicateGuardStepStart -or
        $duplicateGuardScriptStart -le $duplicateGuardRunMarker -or
        $duplicateGuardScriptEnd -le $duplicateGuardScriptStart) {
        throw "Could not isolate the duplicate manual release guard for executable testing."
    }
    $duplicateGuardScriptLines = @(
        $workflow.Substring(
            $duplicateGuardScriptStart,
            $duplicateGuardScriptEnd - $duplicateGuardScriptStart
        ) -split '\r?\n' | ForEach-Object {
            if ($_.StartsWith('          ', [System.StringComparison]::Ordinal)) {
                $_.Substring(10)
            } else {
                $_
            }
        }
    )
    $duplicateGuardScript = $duplicateGuardScriptLines -join "`r`n"
    $duplicateGuardOrder = @(
        '$savedNativeErrorPreference = $PSNativeCommandUseErrorActionPreference',
        'try {',
        '$PSNativeCommandUseErrorActionPreference = $false',
        '& git ls-remote --exit-code --tags origin',
        '$tagLookupExit = $LASTEXITCODE',
        '& gh api "repos/$env:GITHUB_REPOSITORY/releases/tags/$env:RELEASE_TAG"',
        '$releaseLookupExit = $LASTEXITCODE',
        '$global:LASTEXITCODE = 0',
        'finally {',
        '$PSNativeCommandUseErrorActionPreference = $savedNativeErrorPreference'
    )
    $previousGuardPosition = -1
    foreach ($guardFragment in $duplicateGuardOrder) {
        $guardPosition = $duplicateGuardScript.IndexOf($guardFragment, [System.StringComparison]::Ordinal)
        if ($guardPosition -le $previousGuardPosition) {
            throw "Duplicate manual release guard must preserve probe handling order at: $guardFragment"
        }
        $previousGuardPosition = $guardPosition
    }
    if ($duplicateGuardScript -notmatch '(?m)^\s*\$null = & git ls-remote[^\r\n]+\r?\n\s*\$tagLookupExit = \$LASTEXITCODE\r?$' -or
        $duplicateGuardScript -notmatch '(?m)^\s*\$releaseLookup = @\(& gh api[^\r\n]+\r?\n\s*\$releaseLookupExit = \$LASTEXITCODE\r?$') {
        throw "Duplicate manual release probes must capture native exit codes immediately."
    }

    $absenceProbeBin = Join-Path $testRoot "absence-probe-bin"
    New-Item -ItemType Directory -Path $absenceProbeBin | Out-Null
    [System.IO.File]::WriteAllText(
        (Join-Path $absenceProbeBin 'git.cmd'),
        "@exit /b 2`r`n",
        [System.Text.UTF8Encoding]::new($false)
    )
    [System.IO.File]::WriteAllText(
        (Join-Path $absenceProbeBin 'gh.cmd'),
        "@echo gh: release not found (HTTP 404) 1^>^&2`r`n@exit /b 1`r`n",
        [System.Text.UTF8Encoding]::new($false)
    )
    $absenceProbeScript = Join-Path $testRoot 'test-absence-probes.ps1'
    $quotedAbsenceProbeBin = $absenceProbeBin.Replace("'", "''")
    $absenceProbePrelude = @"
`$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
`$PSNativeCommandUseErrorActionPreference = `$true
`$env:PATH = '$quotedAbsenceProbeBin;' + `$env:PATH
`$env:RELEASE_TAG = 'v0.1.0'
`$env:GITHUB_REPOSITORY = 'tyhuang9/scribe'
"@
    [System.IO.File]::WriteAllText(
        $absenceProbeScript,
        "$absenceProbePrelude`r`n$duplicateGuardScript`r`nexit `$LASTEXITCODE`r`n",
        [System.Text.UTF8Encoding]::new($false)
    )
    $absenceProbeResult = Invoke-NativeProcess $pwshPath @('-NoProfile', '-File', $absenceProbeScript)
    if ($absenceProbeResult.ExitCode -ne 0) {
        throw "Expected absent tag and release probes to survive the GitHub PowerShell wrapper; exit $($absenceProbeResult.ExitCode): $($absenceProbeResult.Stderr)"
    }

    $contractTestPosition = $workflow.IndexOf('& .\scripts\test-windows-release-packaging.ps1')
    $releaseInputPosition = $workflow.IndexOf('prepare-windows-release-inputs.ps1')
    $releaseBuildPosition = $workflow.IndexOf('build-windows-release.ps1')
    if ($contractTestPosition -lt 0 -or
        $contractTestPosition -ge $releaseInputPosition -or
        $contractTestPosition -ge $releaseBuildPosition) {
        throw "Windows release packaging contracts must run before release input preparation and build."
    }
    $portableZipPosition = $workflow.IndexOf('Compress-Archive -Path dist\portable\*')
    $payloadParityPosition = $workflow.IndexOf('-PortableZipPath dist\Scribe-windows-x64.zip')
    if ($portableZipPosition -lt 0 -or
        $payloadParityPosition -le $portableZipPosition) {
        throw "Portable ZIP creation must precede portable/installer parity verification."
    }
    $assetValidationPosition = $workflow.IndexOf("`$assetRoot =")
    $rulesetPreflightPosition = $workflow.IndexOf("`$requiredRulesetName = 'Protect release tags'")
    $atomicTagPosition = $workflow.IndexOf('gh api --method POST')
    $releaseCreationPosition = $workflow.IndexOf('gh release create')
    if ($assetValidationPosition -lt 0 -or
        $rulesetPreflightPosition -le $assetValidationPosition -or
        $atomicTagPosition -le $rulesetPreflightPosition -or
        $atomicTagPosition -le $assetValidationPosition -or
        $releaseCreationPosition -le $atomicTagPosition) {
        throw "Release-tag rules must be verified before atomic tag creation and release publication."
    }
    if ($workflow -notmatch '(?ms)release:\s+name: Create GitHub release.*?permissions:\s+contents: write' -or
        $workflow -notmatch '(?ms)^permissions:\s+contents: read') {
        throw "GitHub contents write permission must remain scoped to the release job."
    }
    $installer = Get-Content -LiteralPath (Join-Path $repositoryRoot "installer\scribe.iss") -Raw
    if ($installer -notmatch 'Source: "\.\.\\dist\\portable\\\*"' -or
        $installer -notmatch "recursesubdirs" -or
        $installer -notmatch "createallsubdirs" -or
        $installer -notmatch '#define StableAppIdGuid "8E0F1935-8E3D-4B1D-9A42-7C7D7C3D5E7A"' -or
        $installer -notmatch 'DefaultDirName=\{localappdata\}\\Programs\\Scribe' -or
        $installer -notmatch 'AppId=\{code:ResolveAppId\}' -or
        $installer -notmatch '\{param:SCRIBEVERIFY\|\}' -or
        $installer -match '(?m)^\[InstallDelete\]') {
        throw "Windows installer must recursively install the validated portable payload."
    }

    $verificationBundle = Join-Path $testRoot "verification-bundle"
    New-Item -ItemType Directory -Path $verificationBundle | Out-Null
    $fixtureModelBytes = [byte[]](0x47, 0x47, 0x55, 0x46, 1, 2, 3, 4)
    $fixtureModelHash = [Convert]::ToHexString([System.Security.Cryptography.SHA256]::HashData($fixtureModelBytes)).ToLowerInvariant()
    $fixtureModelManifest = [pscustomobject]@{
        schema_version = 1
        platform_triple = "x86_64-pc-windows-msvc"
        artifact_filename = "whisper-base.en-Q8_0.gguf"
        size_bytes = [int64]$fixtureModelBytes.Length
        sha256 = $fixtureModelHash
    }
    $fixtureManifestSource = Join-Path $testRoot "fixture-model-manifest.json"
    [System.IO.File]::WriteAllText(
        $fixtureManifestSource,
        ($fixtureModelManifest | ConvertTo-Json -Depth 5),
        [System.Text.UTF8Encoding]::new($false)
    )

    foreach ($relativePath in $expectedInventoryPaths) {
        $path = Join-Path $verificationBundle ($relativePath -replace '/', '\')
        New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force | Out-Null
        switch ($relativePath) {
            "local-transcriber.exe" { Write-TestPe $path 0x8664 }
            "whisper-base.en-Q8_0.gguf" { [System.IO.File]::WriteAllBytes($path, $fixtureModelBytes) }
            "bundled-model-manifest.json" { Copy-Item -LiteralPath $fixtureManifestSource -Destination $path }
            default {
                [System.IO.File]::WriteAllText(
                    $path,
                    "verified fixture for $relativePath",
                    [System.Text.UTF8Encoding]::new($false)
                )
            }
        }
    }

    $verificationInventory = [ordered]@{
        schema_version = 1
        platform_triple = "x86_64-pc-windows-msvc"
        files = @($expectedInventoryPaths | Sort-Object | ForEach-Object {
            $relativePath = $_
            $path = Join-Path $verificationBundle ($relativePath -replace '/', '\')
            $item = Get-Item -LiteralPath $path
            [ordered]@{
                path = $relativePath
                size_bytes = [int64]$item.Length
                sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        })
    }
    [System.IO.File]::WriteAllText(
        (Join-Path $verificationBundle "bundle-inventory.json"),
        ($verificationInventory | ConvertTo-Json -Depth 5),
        [System.Text.UTF8Encoding]::new($false)
    )
    Assert-Bundle `
        -Root $verificationBundle `
        -ExpectedModelManifest $fixtureModelManifest `
        -ExpectedModelManifestPath $fixtureManifestSource `
        -ExpectedLegalFiles @()

    foreach ($forbiddenPath in @(
        "RUNTIMES/whisper/whisper.dll",
        "nested/runtime-manifest.JSON",
        "nested/GGML.DLL",
        "nested/SHERPA-helper.exe",
        "nested/onnxruntime.dll",
        "WHISPER-CLI.EXE",
        "main.exe",
        "python/runner.py",
        ".venv/module.pyd",
        "nested/model.ONNX",
        "nested/model.ORT"
    )) {
        Invoke-ExpectedFailure {
            Assert-AllowedPayloadFile $forbiddenPath
        } "Release payload contains"
    }
    foreach ($unsafePath in @("../escape.txt", "nested/../escape.txt", "C:/escape.txt", "nested\escape.txt")) {
        Invoke-ExpectedFailure {
            Assert-SafeRelativePayloadPath $unsafePath
        } "unsafe"
    }

    $portableZip = Join-Path $testRoot "verification-portable.zip"
    Compress-Archive -Path (Join-Path $verificationBundle '*') -DestinationPath $portableZip
    Assert-SafePortableZip $portableZip

    $traversalZip = Join-Path $testRoot "traversal.zip"
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::Open($traversalZip, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        $null = $zip.CreateEntry("../escape.txt")
    }
    finally {
        $zip.Dispose()
    }
    Invoke-ExpectedFailure {
        Assert-SafePortableZip $traversalZip
    } "unsafe"

    $caseCollisionZip = Join-Path $testRoot "case-collision.zip"
    $zip = [System.IO.Compression.ZipFile]::Open($caseCollisionZip, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        $null = $zip.CreateEntry("README.txt")
        $null = $zip.CreateEntry("readme.txt")
    }
    finally {
        $zip.Dispose()
    }
    Invoke-ExpectedFailure {
        Assert-SafePortableZip $caseCollisionZip
    } "duplicate case-insensitive"

    $installedVerificationBundle = Join-Path $testRoot "installed-verification-bundle"
    Copy-Item -LiteralPath $verificationBundle -Destination $installedVerificationBundle -Recurse
    [System.IO.File]::WriteAllBytes((Join-Path $installedVerificationBundle "unins000.exe"), [byte[]](0x4D, 0x5A))
    [System.IO.File]::WriteAllBytes((Join-Path $installedVerificationBundle "unins000.dat"), [byte[]](1, 2, 3))
    Assert-Bundle `
        -Root $installedVerificationBundle `
        -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts `
        -ExpectedModelManifest $fixtureModelManifest `
        -ExpectedModelManifestPath $fixtureManifestSource `
        -ExpectedLegalFiles @()
    Assert-PayloadParity $verificationBundle $installedVerificationBundle "Installed fixture"

    [System.IO.File]::WriteAllBytes((Join-Path $installedVerificationBundle "unexpected-installer-payload.bin"), [byte[]](4))
    Invoke-ExpectedFailure {
        Assert-Bundle `
            -Root $installedVerificationBundle `
            -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts `
            -ExpectedModelManifest $fixtureModelManifest `
            -ExpectedModelManifestPath $fixtureManifestSource `
            -ExpectedLegalFiles @()
    } "Release payload differs from its explicit inventory"

    Remove-Item -LiteralPath (Join-Path $installedVerificationBundle "unexpected-installer-payload.bin")
    $installedReadme = Join-Path $installedVerificationBundle "README.txt"
    $readmeBytes = [System.IO.File]::ReadAllBytes($installedReadme)
    $readmeBytes[0] = $readmeBytes[0] -bxor 0x01
    [System.IO.File]::WriteAllBytes($installedReadme, $readmeBytes)
    Invoke-ExpectedFailure {
        Assert-PayloadParity $verificationBundle $installedVerificationBundle "Installed fixture"
    } "payload parity mismatch"

    Write-Output "Windows release packaging fail-closed tests passed."
}
finally {
    $tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    $resolvedTestRoot = [System.IO.Path]::GetFullPath($testRoot)
    if (-not $resolvedTestRoot.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refused test cleanup outside the system temporary directory."
    }
    if (Test-Path -LiteralPath $resolvedTestRoot) {
        Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force
    }
}
