$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$releaseScript = Join-Path $PSScriptRoot "build-windows-release.ps1"
$modelScript = Join-Path $PSScriptRoot "bundle-base-model.ps1"
$packageVerifier = Join-Path $PSScriptRoot "verify-windows-release-package.ps1"
. (Join-Path $PSScriptRoot "windows-pe-imports.ps1")
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

function Start-TestLogHandleHolder(
    [string]$PowerShellPath,
    [string]$LogPath,
    [string]$ReadyPath,
    [int]$HoldMilliseconds
) {
    $escapedLogPath = $LogPath.Replace("'", "''")
    $escapedReadyPath = $ReadyPath.Replace("'", "''")
    $holderScript = @"
`$logHandle = [System.IO.File]::Open('$escapedLogPath', [System.IO.FileMode]::Open, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
try {
    [System.IO.File]::WriteAllText('$escapedReadyPath', 'ready')
    Start-Sleep -Milliseconds $HoldMilliseconds
}
finally {
    `$logHandle.Dispose()
}
"@
    $encodedHolderScript = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($holderScript))
    Start-Process -FilePath $PowerShellPath -ArgumentList @(
        '-NoProfile', '-NonInteractive', '-EncodedCommand', $encodedHolderScript
    ) -PassThru
}

function Wait-TestLogHandleHolderReady(
    [System.Diagnostics.Process]$Process,
    [string]$ReadyPath
) {
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while (-not (Test-Path -LiteralPath $ReadyPath -PathType Leaf)) {
        if ($Process.HasExited) {
            throw "Synthetic log-handle holder exited before acquiring its handle."
        }
        if ([DateTime]::UtcNow -ge $deadline) {
            throw "Timed out waiting for synthetic log-handle holder readiness."
        }
        Start-Sleep -Milliseconds 20
    }
}

function Replace-FirstExact([string]$Text, [string]$Old, [string]$New) {
    $index = $Text.IndexOf($Old, [StringComparison]::Ordinal)
    if ($index -lt 0) {
        throw "Could not find the exact workflow fixture text to mutate: $Old"
    }
    $Text.Substring(0, $index) + $New + $Text.Substring($index + $Old.Length)
}

function Replace-ExactAfter(
    [string]$Text,
    [string]$Anchor,
    [string]$Old,
    [string]$New
) {
    $anchorIndex = $Text.IndexOf($Anchor, [StringComparison]::Ordinal)
    if ($anchorIndex -lt 0) {
        throw "Could not find the exact workflow fixture anchor: $Anchor"
    }
    $index = $Text.IndexOf($Old, $anchorIndex, [StringComparison]::Ordinal)
    if ($index -lt 0) {
        throw "Could not find the exact workflow fixture text after '$Anchor': $Old"
    }
    $Text.Substring(0, $index) + $New + $Text.Substring($index + $Old.Length)
}

function Get-WorkflowStepBlock(
    [string]$Workflow,
    [string]$StepName,
    [string]$NextStepName
) {
    $startMarker = "      - name: $StepName"
    $endMarker = "      - name: $NextStepName"
    $start = $Workflow.IndexOf($startMarker, [StringComparison]::Ordinal)
    $end = $Workflow.IndexOf($endMarker, $start + $startMarker.Length, [StringComparison]::Ordinal)
    if ($start -lt 0 -or $end -le $start) {
        throw "Could not isolate workflow step '$StepName'."
    }
    $Workflow.Substring($start, $end - $start)
}

function Assert-OrderedWorkflowTokens(
    [string]$Block,
    [string[]]$Tokens,
    [string]$ContractName
) {
    $previous = -1
    foreach ($token in $Tokens) {
        $position = $Block.IndexOf($token, [StringComparison]::Ordinal)
        if ($position -le $previous) {
            throw "$ContractName is missing or reorders required control: $token"
        }
        $previous = $position
    }
}

function Assert-InnoCompilerWorkflowContract([string]$Workflow) {
    $acquire = Get-WorkflowStepBlock $Workflow 'Acquire digest-pinned Inno Setup' 'Build and normalize Windows installer'
    $build = Get-WorkflowStepBlock $Workflow 'Build and normalize Windows installer' 'Package portable ZIP'
    $quotedDirectoryArgument = '(''"/DIR={0}"'' -f $installRoot)'
    if (-not $acquire.Contains($quotedDirectoryArgument)) {
        throw 'Inno Setup /DIR must remain an explicitly quoted argument so workspace paths with spaces are preserved.'
    }
    $spaceContainingRoot = 'C:\CI workspace\inno setup'
    $formattedDirectoryArgument = ('"/DIR={0}"' -f $spaceContainingRoot)
    if ($formattedDirectoryArgument -cne '"/DIR=C:\CI workspace\inno setup"') {
        throw 'Inno Setup /DIR quoting does not preserve a workspace path containing spaces.'
    }
    Assert-OrderedWorkflowTokens $acquire @(
        '$installArguments = @(',
        $quotedDirectoryArgument,
        'Start-Process -FilePath $installerPath',
        '-ArgumentList $installArguments',
        '-Wait -PassThru -WindowStyle Hidden',
        'if ($installProcess.ExitCode -ne 0)',
        '$iscc = Join-Path $installRoot ''ISCC.exe''',
        '$isccFile = Get-Item -LiteralPath $iscc',
        '[System.IO.FileAttributes]::ReparsePoint',
        '$isccFile.Length -ne [int64]$env:INNO_COMPILER_SIZE',
        'Get-FileHash -Algorithm SHA256 -LiteralPath $iscc',
        '$isccHash -cne $env:INNO_COMPILER_SHA256',
        '"INNO_ISCC=$iscc" >> $env:GITHUB_ENV'
    ) 'Inno compiler acquisition'
    Assert-OrderedWorkflowTokens $build @(
        '$isccFile = Get-Item -LiteralPath $iscc',
        '[System.IO.FileAttributes]::ReparsePoint',
        '$isccFile.Length -ne [int64]$env:INNO_ISCC_SIZE',
        'Get-FileHash -Algorithm SHA256 -LiteralPath $iscc',
        '$isccHash -cne $env:INNO_ISCC_SHA256',
        '& $iscc "/DAppVersion=$env:APP_VERSION" installer\scribe.iss'
    ) 'Inno compiler pre-invocation verification'
    if ($acquire -match 'VersionInfo\.ProductVersion' -or $build -match 'VersionInfo\.ProductVersion') {
        throw 'Inno compiler verification must use pinned bytes instead of unreliable PE version fields.'
    }
}

function Write-TestPe(
    [string]$Path,
    [uint16]$Machine,
    [string]$NormalImport = "kernel32.dll",
    [string]$DelayImport = "user32.dll",
    [uint16]$Subsystem = 2
) {
    $bytes = [byte[]]::new(0x600)
    $bytes[0] = 0x4D
    $bytes[1] = 0x5A
    [BitConverter]::GetBytes([uint32]0x80).CopyTo($bytes, 0x3C)
    [BitConverter]::GetBytes([uint32]0x00004550).CopyTo($bytes, 0x80)
    [BitConverter]::GetBytes($Machine).CopyTo($bytes, 0x84)
    [BitConverter]::GetBytes([uint16]1).CopyTo($bytes, 0x86)
    [BitConverter]::GetBytes([uint16]0xF0).CopyTo($bytes, 0x94)
    $optionalOffset = 0x98
    [BitConverter]::GetBytes([uint16]0x20B).CopyTo($bytes, $optionalOffset)
    [BitConverter]::GetBytes([uint64]0x140000000).CopyTo($bytes, $optionalOffset + 24)
    [BitConverter]::GetBytes([uint32]0x1000).CopyTo($bytes, $optionalOffset + 32)
    [BitConverter]::GetBytes([uint32]0x200).CopyTo($bytes, $optionalOffset + 36)
    [BitConverter]::GetBytes([uint32]0x2000).CopyTo($bytes, $optionalOffset + 56)
    [BitConverter]::GetBytes([uint32]0x200).CopyTo($bytes, $optionalOffset + 60)
    [BitConverter]::GetBytes($Subsystem).CopyTo($bytes, $optionalOffset + 68)
    [BitConverter]::GetBytes([uint32]16).CopyTo($bytes, $optionalOffset + 108)
    [BitConverter]::GetBytes([uint32]0x1000).CopyTo($bytes, $optionalOffset + 120)
    [BitConverter]::GetBytes([uint32]40).CopyTo($bytes, $optionalOffset + 124)
    [BitConverter]::GetBytes([uint32]0x1040).CopyTo($bytes, $optionalOffset + 216)
    [BitConverter]::GetBytes([uint32]64).CopyTo($bytes, $optionalOffset + 220)

    $sectionOffset = $optionalOffset + 0xF0
    [System.Text.Encoding]::ASCII.GetBytes('.rdata').CopyTo($bytes, $sectionOffset)
    [BitConverter]::GetBytes([uint32]0x400).CopyTo($bytes, $sectionOffset + 8)
    [BitConverter]::GetBytes([uint32]0x1000).CopyTo($bytes, $sectionOffset + 12)
    [BitConverter]::GetBytes([uint32]0x400).CopyTo($bytes, $sectionOffset + 16)
    [BitConverter]::GetBytes([uint32]0x200).CopyTo($bytes, $sectionOffset + 20)

    [BitConverter]::GetBytes([uint32]0x1100).CopyTo($bytes, 0x200)
    [BitConverter]::GetBytes([uint32]0x1080).CopyTo($bytes, 0x20C)
    [BitConverter]::GetBytes([uint32]0x1110).CopyTo($bytes, 0x210)
    [BitConverter]::GetBytes([uint32]1).CopyTo($bytes, 0x240)
    [BitConverter]::GetBytes([uint32]0x10A0).CopyTo($bytes, 0x244)
    [BitConverter]::GetBytes([uint32]0x1120).CopyTo($bytes, 0x24C)
    [BitConverter]::GetBytes([uint32]0x1130).CopyTo($bytes, 0x250)
    ([System.Text.Encoding]::ASCII.GetBytes($NormalImport + [char]0)).CopyTo($bytes, 0x280)
    ([System.Text.Encoding]::ASCII.GetBytes($DelayImport + [char]0)).CopyTo($bytes, 0x2A0)
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
    $syntheticImportReport = Assert-ReviewedWindowsPe $amd64
    if ($syntheticImportReport.NormalImports -cnotcontains "kernel32.dll" -or
        $syntheticImportReport.DelayImports -cnotcontains "user32.dll") {
        throw "Synthetic PE fixture did not prove both normal and delay import parsing."
    }
    Invoke-ExpectedFailure { Assert-Amd64Pe $x86 } "PE Machine mismatch"
    $consoleSubsystem = Join-Path $testRoot "console-subsystem.exe"
    Write-TestPe $consoleSubsystem 0x8664 "kernel32.dll" "user32.dll" 3
    Invoke-ExpectedFailure { Assert-WindowsGuiSubsystem $consoleSubsystem } "PE subsystem mismatch"

    $forbiddenNormalImport = Join-Path $testRoot "forbidden-normal-import.exe"
    Write-TestPe $forbiddenNormalImport 0x8664 "whisper.dll" "user32.dll"
    Invoke-ExpectedFailure {
        Assert-ReviewedWindowsPe $forbiddenNormalImport
    } "unreviewed normal import DLL: whisper.dll"
    $forbiddenDelayImport = Join-Path $testRoot "forbidden-delay-import.exe"
    Write-TestPe $forbiddenDelayImport 0x8664 "kernel32.dll" "onnxruntime.dll"
    Invoke-ExpectedFailure {
        Assert-ReviewedWindowsPe $forbiddenDelayImport
    } "unreviewed delay import DLL: onnxruntime.dll"

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

    $sharingViolation = [System.IO.IOException]::new('synthetic sharing violation', -2147024864)
    $accessDenied = [System.IO.IOException]::new('synthetic access denied', -2147024891)
    if (-not (Test-TemporaryCleanupSharingViolation $sharingViolation)) {
        throw 'Temporary cleanup did not classify Win32 sharing violations as retryable.'
    }
    if (Test-TemporaryCleanupSharingViolation $accessDenied) {
        throw 'Temporary cleanup must not retry non-sharing failures.'
    }

    $cleanupSuccessToken = [guid]::NewGuid().ToString('N')
    $cleanupSuccessRoot = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-release-package-verifier-$cleanupSuccessToken"
    $cleanupSuccessLog = Join-Path $cleanupSuccessRoot 'stable-emergency-uninstall.log'
    $cleanupSuccessReady = Join-Path $cleanupSuccessRoot 'holder-ready'
    New-Item -ItemType Directory -Path $cleanupSuccessRoot | Out-Null
    [System.IO.File]::WriteAllText($cleanupSuccessLog, 'synthetic held log')
    $cleanupSuccessHolder = $null
    try {
        $cleanupSuccessHolder = Start-TestLogHandleHolder $pwshPath $cleanupSuccessLog $cleanupSuccessReady 500
        Wait-TestLogHandleHolderReady $cleanupSuccessHolder $cleanupSuccessReady
        Remove-ValidatedTemporaryRoot $cleanupSuccessRoot -MaximumAttempts 8 -MaximumRetryMilliseconds 2000
        if (Test-Path -LiteralPath $cleanupSuccessRoot) {
            throw 'Temporary cleanup did not remove the synthetic token-bound root after the held log was released.'
        }
    }
    finally {
        if ($null -ne $cleanupSuccessHolder) {
            $null = $cleanupSuccessHolder.WaitForExit(5000)
            $cleanupSuccessHolder.Dispose()
        }
        if (Test-Path -LiteralPath $cleanupSuccessRoot) {
            Remove-ValidatedTemporaryRoot $cleanupSuccessRoot
        }
    }

    $cleanupExhaustionToken = [guid]::NewGuid().ToString('N')
    $cleanupExhaustionRoot = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-release-package-verifier-$cleanupExhaustionToken"
    $cleanupExhaustionLog = Join-Path $cleanupExhaustionRoot 'stable-emergency-uninstall.log'
    $cleanupExhaustionReady = Join-Path $cleanupExhaustionRoot 'holder-ready'
    New-Item -ItemType Directory -Path $cleanupExhaustionRoot | Out-Null
    [System.IO.File]::WriteAllText($cleanupExhaustionLog, 'synthetic held log')
    $cleanupExhaustionHolder = $null
    try {
        $cleanupExhaustionHolder = Start-TestLogHandleHolder $pwshPath $cleanupExhaustionLog $cleanupExhaustionReady 1500
        Wait-TestLogHandleHolderReady $cleanupExhaustionHolder $cleanupExhaustionReady
        Invoke-ExpectedFailure {
            Remove-ValidatedTemporaryRoot $cleanupExhaustionRoot -MaximumAttempts 2 -MaximumRetryMilliseconds 250
        } 'being used by another process'
        if (-not (Test-Path -LiteralPath $cleanupExhaustionRoot)) {
            throw 'Temporary cleanup unexpectedly removed the synthetic root while its log remained held.'
        }
    }
    finally {
        if ($null -ne $cleanupExhaustionHolder) {
            $null = $cleanupExhaustionHolder.WaitForExit(5000)
            $cleanupExhaustionHolder.Dispose()
        }
        if (Test-Path -LiteralPath $cleanupExhaustionRoot) {
            Remove-ValidatedTemporaryRoot $cleanupExhaustionRoot
        }
    }

    $validSmoke = [pscustomobject]@{
        cancellation_verified = $true
        capabilities = [pscustomobject]@{ cancellation = $true }
        detected_architecture = "whisper"
    }
    Assert-ReleaseSmokeDiagnostics $validSmoke
    $wrongArchitectureSmoke = $validSmoke.PSObject.Copy()
    $wrongArchitectureSmoke.detected_architecture = "whisper-compatible"
    Invoke-ExpectedFailure {
        Assert-ReleaseSmokeDiagnostics $wrongArchitectureSmoke
    } "expected detected architecture 'whisper'"

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

    $relativeCargoTarget = "relative-cargo-target-$PID"
    $resolvedRelativeCargoTarget = Join-Path $repositoryRoot $relativeCargoTarget
    $differentWorkingDirectory = Join-Path $testRoot "different-cwd"
    New-Item -ItemType Directory -Path $differentWorkingDirectory | Out-Null
    try {
        $env:CARGO_TARGET_DIR = $relativeCargoTarget
        Push-Location $differentWorkingDirectory
        try {
            Invoke-ExpectedFailure {
                & $releaseScript `
                    -ModelSource "missing-model" `
                    -BundlePath (Join-Path $resolvedRelativeCargoTarget "portable")
            } "Cargo target directories"
        }
        finally {
            Pop-Location
        }
    }
    finally {
        $env:CARGO_TARGET_DIR = $previousCargoTargetDirectory
    }
    if (Test-Path -LiteralPath $resolvedRelativeCargoTarget) {
        throw "Rejected repository-relative Cargo target was mutated."
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
        $workflow -notmatch "INNO_NUPKG_SHA256: a0dad33db33099d9cd2b89ac2d08b5d70c589b15118ced3b95f469f044f99950" -or
        $workflow -notmatch "INNO_INSTALLER_SHA256: 4d11e8050b6185e0d49bd9e8cc661a7a59f44959a621d31d11033124c4e8a7b0" -or
        $workflow -match "choco install innosetup" -or
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
    $usesLines = @($workflow -split "`r?`n" | Where-Object { $_ -match '^\s*uses:\s*' })
    foreach ($usesLine in $usesLines) {
        if ($usesLine -notmatch '^\s*uses:\s*[^@\s]+@[0-9a-f]{40}(?:\s+#.*)?$') {
            throw "Every GitHub Action reference must use an immutable full commit SHA: $usesLine"
        }
    }
    if ($workflow -notmatch 'dtolnay/rust-toolchain@01ba1edad32c6f80dbcce879d3e0fa5a00b2a84e\s+# 1\.96\.0' -or
        $workflow -notmatch '-ExerciseStableUpgrade' -or
        $workflow -notmatch '-EvidenceDirectory dist\\installer-verification-logs' -or
        $workflow -notmatch '(?ms)name: Upload installer verification evidence\s+if: always\(\).*?name: windows-installer-verification-logs') {
        throw "Windows release workflow must pin Rust, exercise compiled installer contracts, and retain their logs."
    }
    $innoProvenancePath = Join-Path $repositoryRoot 'installer\inno-setup-6.7.1-provenance.json'
    $innoProvenance = Get-Content -LiteralPath $innoProvenancePath -Raw | ConvertFrom-Json
    if ($innoProvenance.schema_version -ne 1 -or
        $innoProvenance.product_version -cne '6.7.1' -or
        $innoProvenance.package_url -cne 'https://community.chocolatey.org/api/v2/package/InnoSetup/6.7.1' -or
        $innoProvenance.package_size_bytes -ne 10017031 -or
        $innoProvenance.package_sha256 -cne 'a0dad33db33099d9cd2b89ac2d08b5d70c589b15118ced3b95f469f044f99950' -or
        $innoProvenance.embedded_installer_path -cne 'tools/innosetup-6.7.1.exe' -or
        $innoProvenance.embedded_installer_size_bytes -ne 10619024 -or
        $innoProvenance.embedded_installer_sha256 -cne '4d11e8050b6185e0d49bd9e8cc661a7a59f44959a621d31d11033124c4e8a7b0' -or
        $innoProvenance.compiler_relative_path -cne 'ISCC.exe' -or
        $innoProvenance.compiler_size_bytes -ne 1455248 -or
        $innoProvenance.compiler_sha256 -cne 'eb6f4410c8db367a5f74127e8025ad2ccacc0afabbe783959d237df3050f97fb' -or
        $innoProvenance.upstream_installer_url -cne 'https://files.jrsoftware.org/is/6/innosetup-6.7.1.exe') {
        throw "Inno Setup provenance must retain the reviewed source, version, sizes, and digests."
    }
    foreach ($pinnedInnoValue in @(
        "INNO_NUPKG_SHA256: $($innoProvenance.package_sha256)",
        "INNO_NUPKG_SIZE: '$($innoProvenance.package_size_bytes)'",
        "INNO_INSTALLER_SHA256: $($innoProvenance.embedded_installer_sha256)",
        "INNO_INSTALLER_SIZE: '$($innoProvenance.embedded_installer_size_bytes)'",
        "INNO_COMPILER_SHA256: $($innoProvenance.compiler_sha256)",
        "INNO_COMPILER_SIZE: '$($innoProvenance.compiler_size_bytes)'"
    )) {
        if (-not $workflow.Contains($pinnedInnoValue)) {
            throw "Windows release workflow differs from reviewed Inno provenance: $pinnedInnoValue"
        }
    }
    Assert-InnoCompilerWorkflowContract $workflow
    $buildStepAnchor = '      - name: Build and normalize Windows installer'
    $compilerHashLine = '          $isccHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $iscc).Hash.ToLowerInvariant()'
    $compilerInvocationLine = '          & $iscc "/DAppVersion=$env:APP_VERSION" installer\scribe.iss'
    foreach ($mutation in @(
        @{
            Name = 'missing installer wait'
            Expected = 'acquisition'
            Action = {
                param($text)
                Replace-FirstExact $text '-Wait -PassThru -WindowStyle Hidden' '-PassThru -WindowStyle Hidden'
            }
        },
        @{
            Name = 'missing compiler size pin'
            Expected = 'acquisition'
            Action = {
                param($text)
                Replace-FirstExact $text '$isccFile.Length -ne [int64]$env:INNO_COMPILER_SIZE' '$false'
            }
        },
        @{
            Name = 'wrong compiler hash source'
            Expected = 'acquisition'
            Action = {
                param($text)
                Replace-FirstExact $text 'Get-FileHash -Algorithm SHA256 -LiteralPath $iscc' 'Get-FileHash -Algorithm SHA256 -LiteralPath $installerPath'
            }
        },
        @{
            Name = 'compiler invocation before final hash'
            Expected = 'pre-invocation'
            Action = {
                param($text)
                $withoutInvocation = Replace-ExactAfter $text $buildStepAnchor $compilerInvocationLine ''
                Replace-ExactAfter $withoutInvocation $buildStepAnchor $compilerHashLine "$compilerInvocationLine`n$compilerHashLine"
            }
        },
        @{
            Name = 'unquoted installer directory'
            Expected = '/DIR'
            Action = {
                param($text)
                Replace-FirstExact $text '(''"/DIR={0}"'' -f $installRoot)' '("/DIR={0}" -f $installRoot)'
            }
        }
    )) {
        $mutatedWorkflow = & $mutation.Action $workflow
        Invoke-ExpectedFailure {
            Assert-InnoCompilerWorkflowContract $mutatedWorkflow
        } $mutation.Expected
    }
    $installer = Get-Content -LiteralPath (Join-Path $repositoryRoot "installer\scribe.iss") -Raw
    if ($installer -notmatch 'Source: "\.\.\\dist\\portable\\\*"' -or
        $installer -notmatch "recursesubdirs" -or
        $installer -notmatch "createallsubdirs" -or
        $installer -notmatch '#define StableAppIdGuid "8E0F1935-8E3D-4B1D-9A42-7C7D7C3D5E7A"' -or
        $installer -notmatch 'DefaultDirName=\{code:ResolveDefaultDir\}' -or
        $installer -notmatch '\{localappdata\}\\Programs\\Scribe' -or
        $installer -notmatch 'AppId=\{code:ResolveAppId\}' -or
        $installer -notmatch "ReadBoundedToken\('SCRIBEVERIFY'\)" -or
        $installer -notmatch 'function PrepareToInstall' -or
        $installer -notmatch 'function ValidateAndBindInstallTree' -or
        $installer -notmatch 'function QueryExistingAttributes' -or
        $installer -notmatch 'function BindDirectory' -or
        $installer -notmatch 'function BindFileForUpdate' -or
        $installer -notmatch 'FindFirstFileW' -or
        $installer -notmatch 'FindNextFileW' -or
        $installer -notmatch 'FindFirstStreamW' -or
        $installer -notmatch 'FindNextStreamW' -or
        $installer -notmatch 'FileShareRead or FileShareWrite' -or
        $installer -notmatch 'GenericRead or GenericWrite' -or
        $installer -notmatch 'FileFlagBackupSemantics or FileFlagOpenReparsePoint' -or
        $installer -notmatch 'DLLGetLastError' -or
        $installer -notmatch 'ErrorFileNotFound' -or
        $installer -notmatch 'ErrorPathNotFound' -or
        $installer -notmatch 'ErrorNoMoreFiles' -or
        $installer -notmatch 'ErrorHandleEof' -or
        $installer -notmatch 'FILE_ATTRIBUTE_REPARSE_POINT' -or
        $installer -notmatch 'case-insensitive path collision' -or
        $installer -notmatch 'alternate NTFS data stream' -or
        $installer -notmatch 'SizeOf\(FindDataLayoutProbe\) <> 592' -or
        $installer -notmatch 'SizeOf\(StreamDataLayoutProbe\) <> 600' -or
        $installer -notmatch 'CreateUninstallRegKey=IsNormalInstall' -or
        $installer -notmatch 'Check: IsNormalInstall' -or
        $installer -notmatch 'UsePreviousAppDir=no' -or
        $installer -notmatch 'UsePreviousLanguage=no' -or
        $installer -notmatch 'Setup did not delete or change any existing content' -or
        $installer -notmatch 'VerificationInstallDir\(Token\)' -or
        $installer -notmatch 'WizardDirValue' -or
        $installer -match '(?m)^\[(?:InstallDelete|UninstallDelete|Registry|INI)\]') {
        throw "Windows installer must preflight and recursively install only the validated portable payload."
    }
    if ([regex]::Matches($installer, 'GetFileAttributesW\(').Count -ne 2) {
        throw "Every installer attribute query must use the fail-closed error-classifying helper."
    }
    $inspectStart = $installer.IndexOf('function InspectExistingTree')
    $inspectEnd = $installer.IndexOf('function ValidateAndBindInstallTree', $inspectStart)
    $inspectSource = $installer.Substring($inspectStart, $inspectEnd - $inspectStart)
    $firstInspectionProbe = $inspectSource.IndexOf('BindDirectory(')
    $enumerationStart = $inspectSource.IndexOf('FindFirstFileW(')
    $enumerationEnd = $inspectSource.LastIndexOf('FindNextFileW(')
    if ($inspectStart -lt 0 -or
        $inspectEnd -le $inspectStart -or
        $firstInspectionProbe -lt 0 -or
        $enumerationStart -le $firstInspectionProbe -or
        $enumerationEnd -le $enumerationStart -or
        $inspectSource -notmatch 'ErrorCode := DLLGetLastError;\s+if ErrorCode = ErrorFileNotFound' -or
        $inspectSource -notmatch 'ErrorCode := DLLGetLastError;\s+if ErrorCode <> ErrorNoMoreFiles') {
        throw "Installer enumeration must bind directory identity and classify native enumeration errors immediately."
    }
    $prepareStart = $installer.IndexOf('function PrepareToInstall')
    $prepareEnd = $installer.IndexOf('procedure CurStepChanged', $prepareStart)
    $lifecycleSource = $installer.Substring($prepareStart)
    if ($prepareStart -lt 0 -or
        $prepareEnd -le $prepareStart -or
        $installer -notmatch 'procedure ReleaseBoundHandles' -or
        $installer -notmatch 'BoundHandles: array\[0\.\.31\] of THandle' -or
        $installer -notmatch 'RetainBoundHandle\(IdentityHandle' -or
        $installer -notmatch 'RetainBoundHandle\(DirectoryHandle' -or
        $lifecycleSource -notmatch 'if CurStep = ssPostInstall then\s+ReleaseBoundHandles\(\)' -or
        $lifecycleSource -notmatch 'procedure DeinitializeSetup\(\);\s+begin\s+ReleaseBoundHandles\(\)') {
        throw "Installer identity handles must remain bound through file installation and close on every completion path."
    }
    $uninstallerSharingStart = $installer.IndexOf('function IsInnoUninstallerArtifact')
    $uninstallerSharingEnd = $installer.IndexOf('function ValidateNoReparseAncestors', $uninstallerSharingStart)
    if ($uninstallerSharingStart -lt 0 -or $uninstallerSharingEnd -le $uninstallerSharingStart) {
        throw 'Could not isolate the Inno uninstaller sharing contract.'
    }
    $uninstallerSharingSource = $installer.Substring($uninstallerSharingStart, $uninstallerSharingEnd - $uninstallerSharingStart)
    if ($installer -notmatch 'FileShareDelete = \$00000004' -or
        $uninstallerSharingSource -notmatch "function IsInnoUninstallerArtifact[\s\S]*SameStr\(RelativePath, 'unins000\.exe'\)[\s\S]*SameStr\(RelativePath, 'unins000\.dat'\)" -or
        $uninstallerSharingSource -notmatch 'function BindFileForUpdate\([\s\S]*AllowDeleteSharing: Boolean' -or
        $uninstallerSharingSource -notmatch 'ShareMode := FileShareRead or FileShareWrite;[\s\S]*if AllowDeleteSharing then[\s\S]*ShareMode := ShareMode or FileShareDelete' -or
        $uninstallerSharingSource -notmatch 'Path, 0, ShareMode, 0, OpenExisting' -or
        $uninstallerSharingSource -notmatch 'Path, GenericRead or GenericWrite, ShareMode,' -or
        $installer -notmatch 'BindFileForUpdate\(\s*ChildPath, IsInnoUninstallerArtifact\(RelativePath\), ErrorText\)') {
        throw 'Installer must permit delete sharing only for the validated Inno uninstaller pair while retaining payload identity handles.'
    }
    $payloadSharingSource = $installer.Substring($installer.IndexOf('function IsAllowedExistingFile'), $uninstallerSharingStart - $installer.IndexOf('function IsAllowedExistingFile'))
    if ($payloadSharingSource -notmatch "SameStr\(RelativePath, 'local-transcriber\.exe'\)" -or
        $uninstallerSharingSource -match 'IsInnoUninstallerArtifact\(Path\)') {
        throw 'Installer uninstaller delete sharing must be selected only from the validated relative path, never an untrusted full path.'
    }
    foreach ($existingAllowedPath in @($expectedPortablePayloadPaths) + @('unins000.exe', 'unins000.dat')) {
        $innoPath = $existingAllowedPath.Replace('/', '\')
        if (-not $installer.Contains("'$innoPath'")) {
            throw "Windows installer existing-tree preflight is missing canonical path $existingAllowedPath."
        }
    }
    foreach ($requiredVerifierContract in @(
        '[switch]$ExerciseStableUpgrade',
        '[string]$EvidenceDirectory',
        'scribe-release-verification-$verificationToken',
        'accepted an override outside its derived temporary destination',
        'setCaseSensitiveInfo',
        'Stable case-insensitive path collision',
        'Stable unexpected legacy runtime tree',
        'Stable payload file with alternate data stream',
        'Stable payload directory with alternate data stream',
        'Stable incompatible file sharing',
        'Stable access-denied enumeration',
        'Stable access-denied update',
        'Stable root rename race',
        'Stable child-directory rename race',
        'Stable file rename race',
        'Invoke-ReparseRefusalFixture',
        'mutated controlled paths, bytes, metadata, entries, or streams'
    )) {
        if (-not $verifierSource.Contains($requiredVerifierContract)) {
            throw "Windows package verifier is missing compiled-installer contract: $requiredVerifierContract"
        }
    }
    $nativeProcessStart = $verifierSource.IndexOf('function Invoke-NativeProcess')
    $nativeProcessEnd = $verifierSource.IndexOf('function Assert-NoReparseAncestors', $nativeProcessStart)
    $nativeProcessSource = $verifierSource.Substring($nativeProcessStart, $nativeProcessEnd - $nativeProcessStart)
    Assert-OrderedWorkflowTokens $nativeProcessSource @(
        '$process.Start()',
        '$process.StandardOutput.ReadToEndAsync()',
        '$process.StandardError.ReadToEndAsync()',
        '$process.WaitForExit()',
        '$stdout.GetAwaiter().GetResult()',
        '$stderr.GetAwaiter().GetResult()',
        'finally {',
        '$process.Dispose()'
    ) 'Native verifier process lifetime'

    $temporaryCleanupClassifierStart = $verifierSource.IndexOf('function Test-TemporaryCleanupSharingViolation')
    $temporaryCleanupClassifierEnd = $verifierSource.IndexOf('function Remove-ValidatedTemporaryRoot', $temporaryCleanupClassifierStart)
    $temporaryCleanupClassifierSource = $verifierSource.Substring($temporaryCleanupClassifierStart, $temporaryCleanupClassifierEnd - $temporaryCleanupClassifierStart)
    $cleanupStart = $verifierSource.IndexOf('function Remove-ValidatedTemporaryRoot')
    $cleanupEnd = $verifierSource.IndexOf('function New-TestShellFixture', $cleanupStart)
    $cleanupSource = $verifierSource.Substring($cleanupStart, $cleanupEnd - $cleanupStart)
    Assert-OrderedWorkflowTokens $cleanupSource @(
        '[ValidateRange(1, 20)]',
        '$MaximumAttempts = 6',
        '[ValidateRange(1, 5000)]',
        '$MaximumRetryMilliseconds = 1000',
        '$cleanupStopwatch = [System.Diagnostics.Stopwatch]::StartNew()',
        'while ($true) {',
        'Split-Path -Parent $resolved',
        '^scribe-release-(?:verification|stable-test|shell-test|package-verifier)-[0-9a-f]{32}$',
        'Assert-NoReparseAncestors $resolved',
        'Assert-TreeHasNoReparsePoints $resolved',
        'Remove-Item -LiteralPath $resolved -Recurse -Force',
        'Test-TemporaryCleanupSharingViolation $_.Exception',
        '$attempt -ge $MaximumAttempts',
        '$cleanupStopwatch.ElapsedMilliseconds -ge $MaximumRetryMilliseconds',
        'Start-Sleep -Milliseconds'
    ) 'Temporary cleanup retry safety'
    if ($temporaryCleanupClassifierSource -notmatch '\$nativeErrorCode -in @\(32, 33\)' -or
        $cleanupSource -notmatch 'if \(-not \(Test-TemporaryCleanupSharingViolation \$_.Exception\)' -or
        $cleanupSource -match 'ErrorAction\s+SilentlyContinue') {
        throw 'Temporary cleanup retries must be limited to sharing or lock violations and fail closed otherwise.'
    }
    $stableUpgradeStart = $verifierSource.IndexOf('$stableUpgrade = Invoke-IsolatedInstallerProcess')
    $stableUpgradeEnd = $verifierSource.IndexOf('$legacyDirectory =', $stableUpgradeStart)
    $stableUpgradeSource = $verifierSource.Substring($stableUpgradeStart, $stableUpgradeEnd - $stableUpgradeStart)
    Assert-OrderedWorkflowTokens $stableUpgradeSource @(
        '$stableUpgrade = Invoke-IsolatedInstallerProcess',
        'Assert-Bundle -Root $stableRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts',
        'Assert-PayloadParity $bundle $stableRoot "Stable upgrade"',
        '$caseCollisionToken = [guid]::NewGuid().ToString(''N'')',
        '$caseCollisionContainer = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-release-stable-test-$caseCollisionToken"',
        'New-Item -ItemType Directory -Path $caseCollisionRoot',
        '"file", "setCaseSensitiveInfo", $caseCollisionRoot, "enable"',
        'Get-ChildItem -LiteralPath $stableRoot -Force',
        'Assert-Bundle -Root $caseCollisionRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts',
        'Assert-PayloadParity $bundle $caseCollisionRoot "Case-collision fixture"',
        'Copy-Item -LiteralPath $canonicalReadme -Destination $caseCollision',
        '"/SCRIBESTABLETEST=$caseCollisionToken"',
        '$installer $caseCollisionInstallArguments'
    ) 'Case-sensitive collision fixture isolation'
    if ($verifierSource.Contains('"file", "setCaseSensitiveInfo", $stableRoot, "enable"') -or
        -not $verifierSource.Contains('Remove-ValidatedTemporaryRoot $caseCollisionContainer')) {
        throw 'Canonical stable upgrades must remain case-insensitive and the isolated case-collision fixture must be token-root cleaned.'
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
