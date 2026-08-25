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
$verifierHelpersStart = $verifierSource.IndexOf("function Get-NormalizedPath")
$verifierHelpersEnd = $verifierSource.IndexOf("`$bundle = Get-NormalizedPath")
if ($verifierHelpersStart -lt 0 -or $verifierHelpersEnd -le $verifierHelpersStart) {
    throw "Could not isolate Windows release package verifier helpers for testing."
}
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
    $inventoryHash = (Get-FileHash -LiteralPath $inventoryFile -Algorithm SHA256).Hash
    Assert-ExactFile $inventoryFile $inventoryItem.Length $inventoryHash
    [System.IO.File]::WriteAllBytes($inventoryFile, [byte[]](9))
    Invoke-ExpectedFailure {
        Assert-ExactFile $inventoryFile $inventoryItem.Length $inventoryHash
    } "SHA-256 mismatch"

    $targetBundle = Join-Path $repositoryRoot "target\scribe-release-probe-$PID"
    Invoke-ExpectedFailure {
        & $releaseScript -ModelSource "missing-model" -RuntimeSource "missing-runtime" -BundlePath $targetBundle
    } "Cargo target directories"
    if (Test-Path -LiteralPath $targetBundle) {
        throw "Rejected Cargo-target bundle path was mutated."
    }

    $existingFinal = Join-Path $testRoot "existing-final"
    New-Item -ItemType Directory -Path $existingFinal | Out-Null
    $existingMarker = Join-Path $existingFinal "keep.bin"
    [System.IO.File]::WriteAllBytes($existingMarker, [byte[]](7))
    Invoke-ExpectedFailure {
        & $releaseScript -ModelSource "missing-model" -RuntimeSource "missing-runtime" -BundlePath $existingFinal
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
        & $releaseScript -ModelSource "missing-model" -RuntimeSource "missing-runtime" -BundlePath $staleFinal
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
        $workflow -match "Copy-Item target\\release\\local-transcriber\.exe") {
        throw "Windows release workflow must package the validated full bundle, not a bare executable."
    }
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
        'rulesets?per_page=100',
        "`$ruleset.target -cne 'tag'",
        "`$ruleset.source_type -cne 'Repository'",
        "`$ruleset.source -cne `$env:GITHUB_REPOSITORY",
        "`$ruleset.enforcement -cne 'active'",
        "`$includedRefs[0] -cne 'refs/tags/v*'",
        "`$excludedRefs.Count -ne 0",
        "`$ruleset.bypass_actors",
        "`$_ -ceq 'update'",
        "`$_ -ceq 'deletion'",
        "`$_ -ceq 'creation'",
        '--draft=false',
        '--latest',
        '--prerelease=false',
        '--verify-tag'
    )) {
        if (-not $workflow.Contains($requiredPublicationGuard)) {
            throw "Windows release workflow must retain publication guard: $requiredPublicationGuard"
        }
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
    $contractTestPosition = $workflow.IndexOf('& .\scripts\test-windows-release-packaging.ps1')
    $releaseInputPosition = $workflow.IndexOf('prepare-windows-release-inputs.ps1')
    $releaseBuildPosition = $workflow.IndexOf('build-windows-release.ps1')
    if ($contractTestPosition -lt 0 -or
        $contractTestPosition -ge $releaseInputPosition -or
        $contractTestPosition -ge $releaseBuildPosition) {
        throw "Windows release packaging contracts must run before release input preparation and build."
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
        $installer -notmatch "createallsubdirs") {
        throw "Windows installer must recursively install the validated portable payload."
    }

    $verificationBundle = Join-Path $testRoot "verification-bundle"
    New-Item -ItemType Directory -Path $verificationBundle | Out-Null
    $verificationReadme = Join-Path $verificationBundle "README.txt"
    [System.IO.File]::WriteAllText($verificationReadme, "verified portable payload", [System.Text.UTF8Encoding]::new($false))
    $verificationItem = Get-Item -LiteralPath $verificationReadme
    $verificationInventory = [ordered]@{
        schema_version = 1
        platform_triple = "x86_64-pc-windows-msvc"
        files = @([ordered]@{
            path = "README.txt"
            size_bytes = [int64]$verificationItem.Length
            sha256 = (Get-FileHash -LiteralPath $verificationReadme -Algorithm SHA256).Hash.ToLowerInvariant()
        })
    }
    [System.IO.File]::WriteAllText(
        (Join-Path $verificationBundle "bundle-inventory.json"),
        ($verificationInventory | ConvertTo-Json -Depth 5),
        [System.Text.UTF8Encoding]::new($false)
    )
    & $packageVerifier -BundlePath $verificationBundle

    $installedVerificationBundle = Join-Path $testRoot "installed-verification-bundle"
    Copy-Item -LiteralPath $verificationBundle -Destination $installedVerificationBundle -Recurse
    [System.IO.File]::WriteAllBytes((Join-Path $installedVerificationBundle "unins000.exe"), [byte[]](0x4D, 0x5A))
    [System.IO.File]::WriteAllBytes((Join-Path $installedVerificationBundle "unins000.dat"), [byte[]](1, 2, 3))
    Assert-Bundle -Root $installedVerificationBundle -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts

    [System.IO.File]::WriteAllBytes((Join-Path $installedVerificationBundle "unexpected-installer-payload.bin"), [byte[]](4))
    Invoke-ExpectedFailure {
        Assert-Bundle -Root $installedVerificationBundle -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts
    } "Release payload differs from its explicit inventory"

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
