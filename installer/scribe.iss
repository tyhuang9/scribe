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
  DriveFixed = 3;

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

function GetDriveType(const RootPathName: String): Cardinal;
  external 'GetDriveTypeW@kernel32.dll stdcall';

function IsAsciiDriveLetter(const Character: Char): Boolean;
begin
  Result :=
    ((Character >= 'A') and (Character <= 'Z')) or
    ((Character >= 'a') and (Character <= 'z'));
end;

function HasOnlyCanonicalDirectorySegments(const Path: String): Boolean;
var
  SegmentStart: Integer;
  SeparatorOffset: Integer;
  Segment: String;
begin
  Result := False;
  SegmentStart := 4;
  while SegmentStart <= Length(Path) do begin
    SeparatorOffset := Pos('\', Copy(Path, SegmentStart, Length(Path)));
    if SeparatorOffset = 0 then begin
      Segment := Copy(Path, SegmentStart, Length(Path));
      SegmentStart := Length(Path) + 1;
    end else begin
      Segment := Copy(Path, SegmentStart, SeparatorOffset - 1);
      SegmentStart := SegmentStart + SeparatorOffset;
    end;

    if Segment = '' then begin
      exit;
    end;
    if (Segment = '.') or (Segment = '..') or
       (Segment[Length(Segment)] = '.') or (Segment[Length(Segment)] = ' ') then begin
      exit;
    end;
  end;
  Result := True;
end;

function TryGetCanonicalFixedDirectory(const Candidate: String; var CanonicalPath: String): Boolean;
begin
  CanonicalPath := '';
  Result := False;
  if (Candidate = '') or (Candidate <> Trim(Candidate)) or
     (Length(Candidate) <= 3) then begin
    exit;
  end;
  if (Candidate[Length(Candidate)] = '\') or
     not IsAsciiDriveLetter(Candidate[1]) or
     (Candidate[2] <> ':') or
     (Candidate[3] <> '\') or
     (Pos('/', Candidate) <> 0) or
     PathHasInvalidCharacters(Candidate, True) or
     not HasOnlyCanonicalDirectorySegments(Candidate) then begin
    exit;
  end;

  CanonicalPath := ExpandFileName(Candidate);
  Result :=
    (CompareText(CanonicalPath, Candidate) = 0) and
    (GetDriveType(Copy(CanonicalPath, 1, 3)) = DriveFixed);
  if not Result then begin
    CanonicalPath := '';
  end;
end;

function ExtractUninstallerExecutable(const UninstallString: String; var Executable: String): Boolean;
var
  TrimmedCommand: String;
begin
  Result := False;
  Executable := '';
  TrimmedCommand := Trim(UninstallString);

  if (TrimmedCommand = '') or (Length(TrimmedCommand) < 3) then begin
    exit;
  end;
  if (TrimmedCommand[1] <> '"') or
     (TrimmedCommand[Length(TrimmedCommand)] <> '"') then begin
    exit;
  end;

  { Permit only outer whitespace. The command itself must be one quoted
    executable path with no arguments, switches, or suffixes. }
  if Pos('"', Copy(TrimmedCommand, 2, Length(TrimmedCommand) - 2)) <> 0 then begin
    exit;
  end;
  Executable := Copy(TrimmedCommand, 2, Length(TrimmedCommand) - 2);
  Result := Executable <> '';
end;

function IsInnoUninstallerFilename(const Filename: String): Boolean;
var
  NormalizedFilename: String;
  Index: Integer;
begin
  NormalizedFilename := Lowercase(Filename);
  Result :=
    (Length(NormalizedFilename) = 12) and
    (Copy(NormalizedFilename, 1, 5) = 'unins') and
    (Copy(NormalizedFilename, 9, 4) = '.exe');
  if not Result then begin
    exit;
  end;
  for Index := 6 to 8 do begin
    if (NormalizedFilename[Index] < '0') or (NormalizedFilename[Index] > '9') then begin
      Result := False;
      exit;
    end;
  end;
end;

function TryGetCanonicalUninstallerPath(const Candidate: String; var CanonicalPath: String): Boolean;
var
  CanonicalDirectory: String;
  Filename: String;
begin
  CanonicalPath := '';
  Filename := ExtractFileName(Candidate);
  Result :=
    IsInnoUninstallerFilename(Filename) and
    TryGetCanonicalFixedDirectory(ExtractFileDir(Candidate), CanonicalDirectory);
  if not Result then begin
    exit;
  end;

  CanonicalPath := ExpandFileName(Candidate);
  Result := CompareText(CanonicalPath, AddBackslash(CanonicalDirectory) + Filename) = 0;
  if not Result then begin
    CanonicalPath := '';
  end;
end;

function IsTrustedUninstaller(const Candidate, CanonicalInstallPath: String;
  var CanonicalUninstallerPath: String): Boolean;
var
  CanonicalUninstallerDirectory: String;
begin
  Result := TryGetCanonicalUninstallerPath(Candidate, CanonicalUninstallerPath);
  if not Result then begin
    exit;
  end;
  CanonicalUninstallerDirectory := ExtractFileDir(CanonicalUninstallerPath);
  Result :=
    FileExists(CanonicalUninstallerPath) and
    (CompareText(CanonicalUninstallerDirectory, CanonicalInstallPath) = 0);
  if not Result then begin
    CanonicalUninstallerPath := '';
  end;
end;

procedure LoadExistingInstallation;
var
  UninstallString: String;
  RegisteredInstallPath: String;
  UninstallerCandidate: String;
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
     not RegQueryStringValue(HKCU, ScribeUninstallRegKey, 'InstallLocation', RegisteredInstallPath) or
     not TryGetCanonicalFixedDirectory(RegisteredInstallPath, ExistingInstallPath) then begin
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
     ExtractUninstallerExecutable(UninstallString, UninstallerCandidate) then begin
    ExistingUninstallerTrusted :=
      IsTrustedUninstaller(UninstallerCandidate, ExistingInstallPath, ExistingUninstallerPath);
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

function RevalidateExistingUninstaller: Boolean;
var
  RegisteredInstallPath: String;
  CanonicalInstallPath: String;
  UninstallString: String;
  UninstallerCandidate: String;
  CanonicalUninstallerPath: String;
begin
  Result := False;
  if not RegKeyExists(HKCU, ScribeUninstallRegKey) or
     not RegQueryStringValue(HKCU, ScribeUninstallRegKey, 'InstallLocation', RegisteredInstallPath) or
     not TryGetCanonicalFixedDirectory(RegisteredInstallPath, CanonicalInstallPath) or
     (CompareText(CanonicalInstallPath, ExistingInstallPath) <> 0) or
     not RegQueryStringValue(HKCU, ScribeUninstallRegKey, 'UninstallString', UninstallString) or
     not ExtractUninstallerExecutable(UninstallString, UninstallerCandidate) or
     not IsTrustedUninstaller(UninstallerCandidate, CanonicalInstallPath, CanonicalUninstallerPath) then begin
    exit;
  end;

  Result := CompareText(CanonicalUninstallerPath, ExistingUninstallerPath) = 0;
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
  if not RevalidateExistingUninstaller then begin
    MsgBox('Remove is unavailable because Scribe installer registration changed or can no longer be trusted. ' +
      'Choose Update or Repair to recreate installer registration.', mbError, MB_OK);
    exit;
  end;
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
        the file-copy phase after the existing uninstaller has finished. }
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
