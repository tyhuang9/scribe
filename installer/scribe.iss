#define AppName "Scribe"
#define AppPublisher "Scribe"
#define AppExeName "local-transcriber.exe"

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={{8E0F1935-8E3D-4B1D-9A42-7C7D7C3D5E7A}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={localappdata}\Programs\Scribe
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
UsePreviousAppDir=yes
UsePreviousTasks=yes
CloseApplications=yes
OutputDir=..\dist
OutputBaseFilename=Scribe-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64
PrivilegesRequired=lowest
UninstallDisplayIcon={app}\{#AppExeName}
VersionInfoCompany={#AppPublisher}
VersionInfoProductName={#AppName}
VersionInfoProductVersion={#AppVersion}
VersionInfoVersion={#AppVersion}
WizardStyle=modern

[Files]
Source: "..\dist\portable\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional icons:"

[Run]
Filename: "{app}\{#AppExeName}"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent

[Code]
const
  { Keep this tied to the stable AppId above. Inno Setup writes current-user
    installations under this identity, independent of the chosen app folder. }
  ScribeUninstallRegKey =
    'Software\Microsoft\Windows\CurrentVersion\Uninstall\{8E0F1935-8E3D-4B1D-9A42-7C7D7C3D5E7A}_is1';
  UninstallKeyPollIntervalMs = 250;
  UninstallKeyPollAttempts = 20;

type
  TMaintenanceAction = (maInstall, maUpdate, maRepair, maRemove, maBlocked);

var
  ExistingInstallDetected: Boolean;
  ExistingInstallUsable: Boolean;
  ExistingUninstallerTrusted: Boolean;
  ExistingVersion: String;
  ExistingInstallPath: String;
  ExistingUninstallerPath: String;
  ExistingVersionComparison: Integer;
  ExistingPackedVersion: Int64;
  SetupPackedVersion: Int64;
  MaintenancePage: TInputOptionWizardPage;
  SelectedMaintenanceAction: TMaintenanceAction;
  RemovalCompleted: Boolean;

function NormalizeDirectory(const Directory: String): String;
begin
  Result := RemoveBackslashUnlessRoot(Directory);
end;

function IsSafeInstallPath(const InstallPath: String): Boolean;
begin
  Result :=
    (InstallPath <> '') and
    PathIsRooted(InstallPath) and
    not PathHasInvalidCharacters(InstallPath, True);
end;

function ExtractUninstallerExecutable(const UninstallString: String; var Executable: String): Boolean;
var
  ClosingQuote: Integer;
  FirstSpace: Integer;
begin
  Result := False;
  Executable := '';

  if UninstallString = '' then begin
    exit;
  end;

  if UninstallString[1] = '"' then begin
    ClosingQuote := Pos('"', Copy(UninstallString, 2, Length(UninstallString)));
    if ClosingQuote = 0 then begin
      exit;
    end;
    Executable := Copy(UninstallString, 2, ClosingQuote - 1);
  end else begin
    { An unquoted command with spaces is ambiguous. Refuse it rather than
      guessing at a registry-provided command line. }
    FirstSpace := Pos(' ', UninstallString);
    if FirstSpace <> 0 then begin
      exit;
    end;
    Executable := UninstallString;
  end;

  Result := (Executable <> '') and PathIsRooted(Executable);
end;

function IsTrustedUninstaller(const Candidate, InstallPath: String): Boolean;
var
  CandidateName: String;
begin
  CandidateName := Lowercase(ExtractFileName(Candidate));
  Result :=
    FileExists(Candidate) and
    (CompareText(NormalizeDirectory(ExtractFileDir(Candidate)), NormalizeDirectory(InstallPath)) = 0) and
    (Copy(CandidateName, 1, 5) = 'unins') and
    (ExtractFileExt(CandidateName) = '.exe');
end;

procedure LoadExistingInstallation;
var
  UninstallString: String;
begin
  ExistingInstallDetected := RegKeyExists(HKCU, ScribeUninstallRegKey);
  ExistingInstallUsable := False;
  ExistingUninstallerTrusted := False;
  ExistingVersion := '';
  ExistingInstallPath := '';
  ExistingUninstallerPath := '';
  ExistingVersionComparison := 0;

  if not ExistingInstallDetected then begin
    exit;
  end;

  if not RegQueryStringValue(HKCU, ScribeUninstallRegKey, 'DisplayVersion', ExistingVersion) or
     not RegQueryStringValue(HKCU, ScribeUninstallRegKey, 'InstallLocation', ExistingInstallPath) or
     not IsSafeInstallPath(ExistingInstallPath) then begin
    exit;
  end;

  ExistingInstallUsable :=
    (ExistingVersion <> '') and
    { Release CI permits only exact numeric x.y.z versions. StrToVersion
      compares that form as x.y.z.0; prerelease and build metadata are out of scope. }
    StrToVersion(ExistingVersion, ExistingPackedVersion) and
    StrToVersion('{#AppVersion}', SetupPackedVersion);
  if ExistingInstallUsable then
    ExistingVersionComparison := ComparePackedVersion(ExistingPackedVersion, SetupPackedVersion);

  { A missing or corrupt old uninstaller must not prevent an in-place update or
    repair from recreating it. It is required only for the Remove action. }
  if RegQueryStringValue(HKCU, ScribeUninstallRegKey, 'UninstallString', UninstallString) and
     ExtractUninstallerExecutable(UninstallString, ExistingUninstallerPath) then begin
    ExistingUninstallerTrusted :=
      IsTrustedUninstaller(ExistingUninstallerPath, ExistingInstallPath);
  end;
end;

procedure AddRemoveAction;
begin
  if ExistingUninstallerTrusted then
    MaintenancePage.Add('Remove Scribe')
  else
    MaintenancePage.Add('Remove Scribe (unavailable: uninstaller is missing or invalid)');
end;

procedure AddMaintenancePage;
begin
  MaintenancePage := CreateInputOptionPage(
    wpWelcome,
    'Scribe maintenance',
    'Choose what to do with this Scribe installation.',
    'Updates and repairs keep the existing install location and shortcut choices. ' +
      'Removing Scribe deletes only installed application files and registration; ' +
      'your Scribe settings, history, models, and runtimes stored outside the application folder are kept.',
    True,
    True);

  if not ExistingInstallDetected then begin
    MaintenancePage.Add('Install Scribe');
    MaintenancePage.SelectedValueIndex := 0;
    SelectedMaintenanceAction := maInstall;
  end else if not ExistingInstallUsable then begin
    AddRemoveAction;
    MaintenancePage.Add('Cancel');
    MaintenancePage.SelectedValueIndex := 1;
    SelectedMaintenanceAction := maBlocked;
  end else if ExistingVersionComparison < 0 then begin
    MaintenancePage.Add('Update Scribe from ' + ExistingVersion + ' to {#AppVersion}');
    AddRemoveAction;
    MaintenancePage.SelectedValueIndex := 0;
    SelectedMaintenanceAction := maUpdate;
  end else if ExistingVersionComparison = 0 then begin
    MaintenancePage.Add('Repair Scribe {#AppVersion}');
    AddRemoveAction;
    MaintenancePage.SelectedValueIndex := 0;
    SelectedMaintenanceAction := maRepair;
  end else begin
    AddRemoveAction;
    MaintenancePage.Add('Cancel (do not downgrade)');
    MaintenancePage.SelectedValueIndex := 1;
    SelectedMaintenanceAction := maBlocked;
  end;
end;

function RequestedMaintenanceAction: TMaintenanceAction;
begin
  if not ExistingInstallDetected then begin
    Result := maInstall;
  end else if not ExistingInstallUsable then begin
    if MaintenancePage.SelectedValueIndex = 0 then Result := maRemove else Result := maBlocked;
  end else if ExistingVersionComparison < 0 then begin
    if MaintenancePage.SelectedValueIndex = 0 then Result := maUpdate else Result := maRemove;
  end else if ExistingVersionComparison = 0 then begin
    if MaintenancePage.SelectedValueIndex = 0 then Result := maRepair else Result := maRemove;
  end else begin
    if MaintenancePage.SelectedValueIndex = 0 then Result := maRemove else Result := maBlocked;
  end;
end;

function WaitForUninstallRegistrationRemoval: Boolean;
var
  Attempt: Integer;
begin
  Result := not RegKeyExists(HKCU, ScribeUninstallRegKey);
  for Attempt := 1 to UninstallKeyPollAttempts do begin
    if Result then begin
      exit;
    end;
    Sleep(UninstallKeyPollIntervalMs);
    Result := not RegKeyExists(HKCU, ScribeUninstallRegKey);
  end;
end;

function RemoveExistingInstallation: Boolean;
var
  ResultCode: Integer;
begin
  Result := False;
  if not ExistingUninstallerTrusted then begin
    MsgBox('Remove is unavailable because this Scribe installation has no trusted uninstaller. ' +
      'Choose Update or Repair to recreate installer registration, or remove it through Windows after fixing the installation.',
      mbError, MB_OK);
    exit;
  end;

  { Run only the validated uninstaller executable and supply our own arguments;
    never replay the registry command line. No /SILENT flag lets its normal UI
    report cancellation or failure to the user. }
  if not Exec(ExistingUninstallerPath, '/NORESTART', '', SW_SHOWNORMAL,
      ewWaitUntilTerminated, ResultCode) then begin
    MsgBox('Scribe could not start its uninstaller. Nothing was installed or changed.', mbError, MB_OK);
    exit;
  end;
  if ResultCode <> 0 then begin
    MsgBox('Scribe removal did not complete (uninstaller exit code ' + IntToStr(ResultCode) + '). ' +
      'Nothing will be installed by this setup.', mbError, MB_OK);
    exit;
  end;

  if not WaitForUninstallRegistrationRemoval then begin
    MsgBox('Scribe removal was cancelled or did not finish. Nothing will be installed by this setup.',
      mbInformation, MB_OK);
    exit;
  end;

  Result := True;
end;

procedure InitializeWizard;
begin
  LoadExistingInstallation;
  AddMaintenancePage;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := (PageID = MaintenancePage.ID) and WizardSilent;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if CurPageID <> MaintenancePage.ID then begin
    exit;
  end;

  SelectedMaintenanceAction := RequestedMaintenanceAction;
  if SelectedMaintenanceAction = maRemove then begin
    if RemoveExistingInstallation then begin
      { Removal is a terminal action: do not allow this setup to continue into
        [Files] after the existing uninstaller has finished. }
      RemovalCompleted := True;
      WizardForm.Close;
    end;
    Result := False;
  end else if SelectedMaintenanceAction = maBlocked then begin
    MsgBox('This setup will not overwrite a newer or invalid existing Scribe installation. ' +
      'Choose Remove to uninstall it, or cancel this setup.', mbInformation, MB_OK);
    Result := False;
  end;
end;

procedure CancelButtonClick(CurPageID: Integer; var Cancel, Confirm: Boolean);
begin
  if RemovalCompleted then begin
    { Closing after a successful Remove is intentional, so do not show the
      generic setup-cancellation confirmation over the completed action. }
    Confirm := False;
    Cancel := True;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if ExistingInstallDetected and ExistingInstallUsable and
     (ExistingVersionComparison > 0) then begin
    Result := 'A newer Scribe ' + ExistingVersion + ' installation is already present. ' +
      'This installer will not downgrade it.';
  end else if ExistingInstallDetected and not ExistingInstallUsable then begin
    Result := 'Scribe is registered as installed, but its installation details cannot be safely validated. ' +
      'This installer will not overwrite it.';
  end;
end;
