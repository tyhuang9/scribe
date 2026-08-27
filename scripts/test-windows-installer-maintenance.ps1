$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$installerPath = Join-Path $repositoryRoot 'installer\scribe.iss'
$installer = Get-Content -LiteralPath $installerPath -Raw

function Assert-Contains {
    param(
        [Parameter(Mandatory)] [string] $Text,
        [Parameter(Mandatory)] [string] $Description
    )

    if (-not $installer.Contains($Text, [System.StringComparison]::Ordinal)) {
        throw "Installer maintenance contract missing ${Description}: $Text"
    }
}

function Assert-Matches {
    param(
        [Parameter(Mandatory)] [string] $Pattern,
        [Parameter(Mandatory)] [string] $Description
    )

    if ($installer -notmatch $Pattern) {
        throw "Installer maintenance contract missing $Description."
    }
}

$appGuid = '8E0F1935-8E3D-4B1D-9A42-7C7D7C3D5E7A'
$uninstallKey = "Software\Microsoft\Windows\CurrentVersion\Uninstall\{$appGuid}_is1"

# Identity and current installer payload contracts from main must remain stable.
Assert-Contains "AppId={{$appGuid}" 'stable AppId'
Assert-Contains 'DefaultDirName={localappdata}\Programs\Scribe' 'current-user install path'
Assert-Contains 'Source: "..\dist\portable\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs' 'recursive portable payload'
Assert-Contains "'$uninstallKey'" 'stable Inno current-user uninstall identity'
Assert-Contains 'RegKeyExists(HKCU, ScribeUninstallRegKey)' 'registry identity detection'
foreach ($valueName in @('DisplayVersion', 'InstallLocation', 'UninstallString')) {
    Assert-Contains "ScribeUninstallRegKey, '$valueName'" "registry $valueName lookup"
}
if ($installer -match 'DirExists\(' -or $installer -match 'FileExists\(.*\{app\}') {
    throw 'Installer maintenance detection must not infer installed state from an application directory.'
}

# Maintenance routing must be explicit and version-aware.
Assert-Contains "MaintenancePage.Add('Install Scribe')" 'fresh-install action'
Assert-Contains "MaintenancePage.Add('Update Scribe from '" 'update action'
Assert-Contains "MaintenancePage.Add('Repair Scribe {#AppVersion}')" 'repair action'
Assert-Contains "MaintenancePage.Add('Remove Scribe')" 'remove action'
Assert-Contains "MaintenancePage.Add('Cancel (do not downgrade)')" 'downgrade cancel action'
Assert-Matches 'ExistingVersionComparison < 0[\s\S]*maUpdate' 'update routing for older installed versions'
Assert-Matches 'ExistingVersionComparison = 0[\s\S]*maRepair' 'repair routing for matching versions'
Assert-Matches 'ExistingVersionComparison > 0[\s\S]*will not downgrade' 'downgrade guard'
Assert-Contains 'StrToVersion(ExistingVersion, ExistingPackedVersion)' 'validated installed version parsing'
Assert-Contains "StrToVersion('{#AppVersion}', SetupPackedVersion)" 'validated setup version parsing'
Assert-Contains 'ComparePackedVersion(ExistingPackedVersion, SetupPackedVersion)' 'supported packed-version comparison'
Assert-Contains 'Release CI permits only exact numeric x.y.z versions. StrToVersion' 'three-component release-version invariant'
if ($installer.Contains('CompareVersion(', [System.StringComparison]::Ordinal)) {
    throw 'Installer must use the supported packed-version comparison APIs.'
}
Assert-Contains 'UsePreviousAppDir=yes' 'preserved install path for update and repair'
Assert-Contains 'UsePreviousTasks=yes' 'preserved task choices for update and repair'
Assert-Contains 'CloseApplications=yes' 'supported running-process handling'
foreach ($actionLabel in @('Install Scribe', 'Update Scribe from ', 'Repair Scribe {#AppVersion}')) {
    $actionPosition = $installer.IndexOf("MaintenancePage.Add('$actionLabel", [System.StringComparison]::Ordinal)
    $selectionPosition = $installer.IndexOf('MaintenancePage.SelectedValueIndex := 0;', $actionPosition, [System.StringComparison]::Ordinal)
    if ($actionPosition -lt 0 -or $selectionPosition -le $actionPosition) {
        throw "Installer must explicitly select the safe default action for $actionLabel."
    }
}

# A repairable product registration is independent from the old uninstaller.
$loadExistingInstallStart = $installer.IndexOf('procedure LoadExistingInstallation;', [System.StringComparison]::Ordinal)
$loadExistingInstallEnd = $installer.IndexOf('procedure AddRemoveAction;', $loadExistingInstallStart, [System.StringComparison]::Ordinal)
if ($loadExistingInstallStart -lt 0 -or $loadExistingInstallEnd -le $loadExistingInstallStart) {
    throw 'Could not isolate existing-install detection for maintenance regression checks.'
}
$loadExistingInstall = $installer.Substring($loadExistingInstallStart, $loadExistingInstallEnd - $loadExistingInstallStart)
if ($loadExistingInstall -notmatch 'IsSafeInstallPath\(ExistingInstallPath\)' -or
    $loadExistingInstall -notmatch 'ExistingInstallUsable :=[\s\S]*StrToVersion\(ExistingVersion, ExistingPackedVersion\)[\s\S]*StrToVersion\(''\{#AppVersion\}'', SetupPackedVersion\)') {
    throw 'Update and Repair must require a valid stable registration, version, and install path.'
}
$safePathValidationPosition = $loadExistingInstall.IndexOf('IsSafeInstallPath(ExistingInstallPath)', [System.StringComparison]::Ordinal)
$productValidationPosition = $loadExistingInstall.IndexOf(
    'ExistingInstallUsable :=',
    $safePathValidationPosition,
    [System.StringComparison]::Ordinal
)
$uninstallerLookupPosition = $loadExistingInstall.IndexOf("RegQueryStringValue(HKCU, ScribeUninstallRegKey, 'UninstallString'", [System.StringComparison]::Ordinal)
if ($productValidationPosition -lt 0 -or $uninstallerLookupPosition -le $productValidationPosition) {
    throw 'Missing or corrupt uninstaller data must not block Update or Repair eligibility.'
}
$productValidation = $loadExistingInstall.Substring(
    $productValidationPosition,
    $uninstallerLookupPosition - $productValidationPosition
)
if ($productValidation.Contains('ExistingUninstallerTrusted', [System.StringComparison]::Ordinal)) {
    throw 'Existing uninstaller trust must not be a precondition for Update or Repair.'
}
Assert-Contains 'A missing or corrupt old uninstaller must not prevent an in-place update or' 'broken-uninstaller repair contract'
Assert-Contains 'MaintenancePage.Add(''Remove Scribe (unavailable: uninstaller is missing or invalid)'')' 'clear unavailable-remove action'

# Removal is terminal and runs only a validated executable path, never a registry command line.
Assert-Contains 'function IsTrustedUninstaller' 'uninstaller validation'
Assert-Contains "(Copy(CandidateName, 1, 5) = 'unins')" 'Inno uninstaller filename validation'
Assert-Contains "Exec(ExistingUninstallerPath, '/NORESTART'" 'validated uninstaller execution'
if ($installer -match 'Exec\(UninstallString') {
    throw 'Installer must never execute UninstallString directly.'
}
Assert-Matches 'SelectedMaintenanceAction = maRemove[\s\S]*RemoveExistingInstallation[\s\S]*WizardForm\.Close[\s\S]*Result := False' 'remove-only termination before installation'
Assert-Contains 'Scribe removal was cancelled or did not finish' 'removal cancellation check'
Assert-Contains 'RemovalCompleted := True;' 'successful removal state'
Assert-Matches 'procedure CancelButtonClick[\s\S]*RemovalCompleted[\s\S]*Confirm := False[\s\S]*Cancel := True' 'clean setup exit after successful removal'
Assert-Contains 'Remove is unavailable because this Scribe installation has no trusted uninstaller.' 'missing or corrupt uninstaller remove rejection'

# An Inno uninstaller may hand work to a self-copy after its original process exits.
Assert-Contains 'UninstallKeyPollIntervalMs = 250;' 'bounded uninstaller polling interval'
Assert-Contains 'UninstallKeyPollAttempts = 20;' 'bounded uninstaller polling attempt count'
Assert-Matches 'function WaitForUninstallRegistrationRemoval[\s\S]*for Attempt := 1 to UninstallKeyPollAttempts do[\s\S]*Sleep\(UninstallKeyPollIntervalMs\)[\s\S]*not RegKeyExists\(HKCU, ScribeUninstallRegKey\)' 'bounded post-uninstall registration wait'
Assert-Matches 'ResultCode <> 0[\s\S]*if not WaitForUninstallRegistrationRemoval then begin[\s\S]*Scribe removal was cancelled or did not finish' 'remove failure only after bounded completion wait'
if ($installer -match 'ExitProcess@|TerminateProcess@|\bExitProcess\(') {
    throw 'Terminal remove must not abruptly terminate Setup and bypass its cleanup lifecycle.'
}

# No installer deletion directive may target Scribe data outside {app}.
if ($installer -match '(?mi)^\[UninstallDelete\]' -or $installer -match '(?mi)^\[InstallDelete\]') {
    throw 'Installer must not add delete directives that could remove user data.'
}
Assert-Contains 'your Scribe settings, history, models, and runtimes stored outside the application folder are kept.' 'user-data preservation notice'

Write-Output 'Windows installer maintenance contracts passed.'
