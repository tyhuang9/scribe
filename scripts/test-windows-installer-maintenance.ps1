$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$installerPath = Join-Path $repositoryRoot 'installer\scribe.iss'
$installer = Get-Content -LiteralPath $installerPath -Raw
$windowsDocumentation = Get-Content -LiteralPath (Join-Path $repositoryRoot 'website\src\content\docs\platforms\windows.md') -Raw

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

function Get-InstallerSection {
    param(
        [Parameter(Mandatory)] [string] $StartMarker,
        [Parameter(Mandatory)] [string] $EndMarker,
        [Parameter(Mandatory)] [string] $Description
    )

    $start = $installer.IndexOf($StartMarker, [System.StringComparison]::Ordinal)
    $end = $installer.IndexOf($EndMarker, $start + $StartMarker.Length, [System.StringComparison]::Ordinal)
    if ($start -lt 0 -or $end -le $start) {
        throw "Could not isolate installer $Description."
    }
    return $installer.Substring($start, $end - $start)
}

function Test-CanonicalFixedDirectory {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [bool] $IsFixedDrive = $true
    )

    if ($Path -cne $Path.Trim() -or
        $Path -notmatch '^[A-Za-z]:\\' -or
        $Path.Length -le 3 -or
        $Path.EndsWith('\') -or
        $Path.Contains('/') -or
        -not $IsFixedDrive) {
        return $false
    }
    foreach ($segment in ($Path.Substring(3) -split '\\')) {
        if ($segment.Length -eq 0 -or $segment -in @('.', '..') -or
            $segment.EndsWith('.') -or $segment.EndsWith(' ')) {
            return $false
        }
    }
    return $true
}

function Test-TrustedUninstallerScenario {
    param(
        [Parameter(Mandatory)] [string] $InstallPath,
        [Parameter(Mandatory)] [string] $UninstallString,
        [bool] $IsFixedDrive = $true
    )

    if (-not (Test-CanonicalFixedDirectory $InstallPath $IsFixedDrive)) {
        return $false
    }
    $trimmedCommand = $UninstallString.Trim()
    if ($trimmedCommand -notmatch '^"(?<path>[^"]+)"$') {
        return $false
    }
    $uninstallerPath = $Matches.path
    if (-not (Test-CanonicalFixedDirectory ([System.IO.Path]::GetDirectoryName($uninstallerPath)) $IsFixedDrive) -or
        [System.IO.Path]::GetFileName($uninstallerPath) -notmatch '(?i)^unins\d{3}\.exe$') {
        return $false
    }
    return [string]::Equals(
        [System.IO.Path]::GetDirectoryName($uninstallerPath),
        $InstallPath,
        [System.StringComparison]::OrdinalIgnoreCase
    )
}

$appGuid = '8E0F1935-8E3D-4B1D-9A42-7C7D7C3D5E7A'
$uninstallKey = "Software\Microsoft\Windows\CurrentVersion\Uninstall\{$appGuid}_is1"

# Identity and current installer payload contracts from main must remain stable.
Assert-Contains "#define StableAppIdGuid `"$appGuid`"" 'stable AppId constant'
Assert-Contains 'AppId={code:ResolveAppId}' 'test-aware AppId resolver'
Assert-Contains "Result := '{' + '{#StableAppIdGuid}' + '}'" 'normal stable AppId result'
Assert-Contains 'DefaultDirName={code:ResolveDefaultDir}' 'test-aware install-path resolver'
Assert-Contains "Result := ExpandConstant('{localappdata}\Programs\Scribe')" 'current-user install path'
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
if ([regex]::Matches($installer, '(?m)^UsePreviousAppDir=').Count -ne 1 -or
    [regex]::Matches($installer, '(?m)^UsePreviousTasks=').Count -ne 1) {
    throw 'Installer must declare each previous-install preservation directive exactly once.'
}
$initializeWizard = Get-InstallerSection 'procedure InitializeWizard;' 'function ShouldSkipPage' 'InitializeWizard callback'
$shouldSkipPage = Get-InstallerSection 'function ShouldSkipPage' 'function NextButtonClick' 'ShouldSkipPage callback'
$nextButtonClick = Get-InstallerSection 'function NextButtonClick' 'procedure CancelButtonClick' 'NextButtonClick callback'
$installRootRouting = Get-InstallerSection 'function IsAllowedNormalInstallRoot' 'function InitializeSetup' 'install-root routing function'
if ($initializeWizard -notmatch 'if not IsNormalInstall\(\) then\s+Exit;\s+LoadExistingInstallation' -or
    $shouldSkipPage -notmatch 'if not IsNormalInstall\(\) then\s+Exit;\s+Result := \(PageID = MaintenancePage\.ID\)' -or
    $nextButtonClick -notmatch 'if not IsNormalInstall\(\) then\s+Exit;\s+if CurPageID <> MaintenancePage\.ID then') {
    throw 'Every maintenance callback must bypass verification and stable-test AppIds before touching MaintenancePage or the stable registry.'
}
if ($installRootRouting -notmatch 'if not ExistingInstallDetected then begin\s+Result := SameStr\(InstallRoot, RemoveBackslashUnlessRoot\(StableInstallDir\(\)\)\);\s+Exit;\s+end;' -or
    $installRootRouting -notmatch 'ExistingInstallUsable and[\s\S]*\(CompareText\(InstallRoot, ExistingInstallPath\) = 0\);') {
    throw 'Fresh installs must use the canonical stable root, while Update and Repair must use the exact validated registered root.'
}
foreach ($actionLabel in @('Install Scribe', 'Update Scribe from ', 'Repair Scribe {#AppVersion}')) {
    $actionPosition = $installer.IndexOf("MaintenancePage.Add('$actionLabel", [System.StringComparison]::Ordinal)
    $selectionPosition = $installer.IndexOf('MaintenancePage.SelectedValueIndex := 0;', $actionPosition, [System.StringComparison]::Ordinal)
    if ($actionPosition -lt 0 -or $selectionPosition -le $actionPosition) {
        throw "Installer must explicitly select the safe default action for $actionLabel."
    }
}
$maintenancePageRouting = Get-InstallerSection 'procedure AddMaintenancePage;' 'function RequestedMaintenanceAction' 'maintenance-page routing'
if ($maintenancePageRouting -notmatch 'else if not ExistingInstallUsable then begin[\s\S]*?MaintenancePage\.Add\(''Cancel''\);\s+MaintenancePage\.SelectedValueIndex := 1;\s+SelectedMaintenanceAction := maBlocked;' -or
    $maintenancePageRouting -notmatch 'else begin\s+AddRemoveAction;\s+MaintenancePage\.Add\(''Cancel \(do not downgrade\)''\);\s+MaintenancePage\.SelectedValueIndex := 1;\s+SelectedMaintenanceAction := maBlocked;') {
    throw 'Invalid registrations and downgrade attempts must default to Cancel and remain blocked.'
}

# A repairable product registration is independent from the old uninstaller.
$loadExistingInstallStart = $installer.IndexOf('procedure LoadExistingInstallation;', [System.StringComparison]::Ordinal)
$loadExistingInstallEnd = $installer.IndexOf('procedure AddRemoveAction;', $loadExistingInstallStart, [System.StringComparison]::Ordinal)
if ($loadExistingInstallStart -lt 0 -or $loadExistingInstallEnd -le $loadExistingInstallStart) {
    throw 'Could not isolate existing-install detection for maintenance regression checks.'
}
$loadExistingInstall = $installer.Substring($loadExistingInstallStart, $loadExistingInstallEnd - $loadExistingInstallStart)
if ($loadExistingInstall -notmatch 'TryGetCanonicalFixedDirectory\(RegisteredInstallPath, ExistingInstallPath\)' -or
    $loadExistingInstall -notmatch 'ExistingInstallUsable :=[\s\S]*StrToVersion\(ExistingVersion, ExistingPackedVersion\)[\s\S]*StrToVersion\(''\{#AppVersion\}'', SetupPackedVersion\)') {
    throw 'Update and Repair must require a valid stable registration, version, and install path.'
}
$safePathValidationPosition = $loadExistingInstall.IndexOf('TryGetCanonicalFixedDirectory(RegisteredInstallPath, ExistingInstallPath)', [System.StringComparison]::Ordinal)
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
Assert-Contains 'function IsInnoUninstallerFilename' 'strict Inno uninstaller filename validation'
Assert-Contains "(Length(NormalizedFilename) = 12)" 'three-digit uninstaller filename length'
Assert-Contains "(Copy(NormalizedFilename, 1, 5) = 'unins')" 'uninstaller filename prefix'
Assert-Contains "(Copy(NormalizedFilename, 9, 4) = '.exe')" 'uninstaller executable extension'
Assert-Contains 'GetDriveType(Copy(CanonicalPath, 1, 3)) = DriveFixed' 'fixed-drive path requirement'
Assert-Contains 'function RevalidateExistingUninstaller' 'pre-execution uninstaller revalidation'
Assert-Matches 'function ExtractUninstallerExecutable[\s\S]*Trim\(UninstallString\)[\s\S]*TrimmedCommand\[1\][\s\S]*TrimmedCommand\[Length\(TrimmedCommand\)\][\s\S]*no arguments, switches, or suffixes' 'exact quoted uninstall command requirement'
Assert-Matches 'function RevalidateExistingUninstaller[\s\S]*TryGetCanonicalFixedDirectory[\s\S]*IsTrustedUninstaller[\s\S]*CompareText\(CanonicalUninstallerPath, ExistingUninstallerPath\)' 'pre-execution registry and path revalidation'
Assert-Matches 'function RemoveExistingInstallation[\s\S]*if not RevalidateExistingUninstaller then begin[\s\S]*Exec\(ExistingUninstallerPath' 'revalidation immediately before execution'
Assert-Contains "Exec(ExistingUninstallerPath, '/NORESTART'" 'validated uninstaller execution'
if ($installer -match 'Exec\(UninstallString') {
    throw 'Installer must never execute UninstallString directly.'
}
Assert-Matches 'SelectedMaintenanceAction = maRemove[\s\S]*RemoveExistingInstallation[\s\S]*WizardForm\.Close[\s\S]*Result := False' 'remove-only termination before installation'
Assert-Contains 'Scribe removal was cancelled or did not finish' 'removal cancellation check'
Assert-Contains 'RemovalCompleted := True;' 'successful removal state'
Assert-Matches 'procedure CancelButtonClick[\s\S]*RemovalCompleted[\s\S]*Confirm := False[\s\S]*Cancel := True' 'terminal no-install routing through Inno cancellation'
Assert-Contains 'Remove is unavailable because this Scribe installation has no trusted uninstaller.' 'missing or corrupt uninstaller remove rejection'
if (-not $windowsDocumentation.Contains('Setup reports exit code `2` even when the uninstaller succeeded.', [System.StringComparison]::Ordinal) -or
    -not $windowsDocumentation.Contains('Verify that Scribe''s uninstall registration is gone', [System.StringComparison]::Ordinal)) {
    throw 'Windows documentation must define the Remove automation exit contract and registry verification.'
}

# Exercise the strict registry-command and path trust model with representative cases.
foreach ($invalidPath in @(
    'C:\',
    '\\server\share\Scribe',
    '\\?\C:\Scribe',
    'C:\Scribe\..\Other',
    'C:\Scribe\\Nested',
    'C:\Scribe.\Nested',
    'C:\Scribe\'
)) {
    if (Test-CanonicalFixedDirectory $invalidPath) {
        throw "Unsafe registered install path was accepted: $invalidPath"
    }
}
if (Test-CanonicalFixedDirectory 'Z:\Scribe' $false) {
    throw 'A non-fixed/network drive install path was accepted.'
}
foreach ($validFilename in @('unins000.exe', 'unins001.exe')) {
    if (-not (Test-TrustedUninstallerScenario 'C:\Apps\Scribe' "`"C:\Apps\Scribe\$validFilename`"")) {
        throw "Compatible Inno uninstaller was rejected: $validFilename"
    }
}
foreach ($invalidCommand in @(
    '"C:\Apps\Scribe\uninsEvil.exe"',
    '"C:\Apps\Scribe\unins0000.exe"',
    '"C:\Apps\Scribe\unins000.exe" /SILENT',
    '"C:\Other\unins000.exe"'
)) {
    if (Test-TrustedUninstallerScenario 'C:\Apps\Scribe' $invalidCommand) {
        throw "Unsafe registered uninstaller was accepted: $invalidCommand"
    }
}

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
