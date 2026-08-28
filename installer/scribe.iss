#define AppName "Scribe"
#define AppPublisher "Scribe"
#define AppExeName "local-transcriber.exe"
#define StableAppIdGuid "8E0F1935-8E3D-4B1D-9A42-7C7D7C3D5E7A"

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={code:ResolveAppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={code:ResolveDefaultDir}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
UsePreviousAppDir=yes
UsePreviousTasks=yes
CloseApplications=yes
OutputDir=..\dist
OutputBaseFilename=Scribe-Setup-{#AppVersion}
Compression=lzma2
CreateUninstallRegKey=IsNormalInstall
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64
PrivilegesRequired=lowest
UninstallDisplayIcon={app}\{#AppExeName}
UsePreviousGroup=no
UsePreviousLanguage=no
UsePreviousSetupType=no
UsePreviousUserInfo=no
VersionInfoCompany={#AppPublisher}
VersionInfoProductName={#AppName}
VersionInfoProductVersion={#AppVersion}
VersionInfoVersion={#AppVersion}
WizardStyle=modern

[Files]
Source: "..\dist\portable\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs; BeforeInstall: ReleasePayloadHandleForCurrentFile

[Icons]
Name: "{code:ResolveStartMenuDirectory}\{#AppName}"; Filename: "{code:ResolveLaunchTarget}"; Check: IsNormalInstall
Name: "{code:ResolveDesktopDirectory}\{#AppName}"; Filename: "{code:ResolveLaunchTarget}"; Tasks: desktopicon; Check: IsNormalInstall

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional icons:"; Check: IsNormalInstall

[Run]
Filename: "{code:ResolveLaunchTarget}"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent; Check: IsNormalInstall

[Code]
type
  TNativeFileTime = record
    LowDateTime: LongWord;
    HighDateTime: LongWord;
  end;
  TNativeFileName = array[0..259] of Char;
  TNativeAlternateFileName = array[0..13] of Char;
  TNativeStreamName = array[0..295] of Char;
  TWin32FindDataW = record
    FileAttributes: LongWord;
    CreationTime: TNativeFileTime;
    LastAccessTime: TNativeFileTime;
    LastWriteTime: TNativeFileTime;
    FileSizeHigh: LongWord;
    FileSizeLow: LongWord;
    Reserved0: LongWord;
    Reserved1: LongWord;
    FileName: TNativeFileName;
    AlternateFileName: TNativeAlternateFileName;
  end;
  TWin32FindStreamData = record
    StreamSize: Int64;
    StreamName: TNativeStreamName;
  end;
  TMaintenanceAction = (maInstall, maUpdate, maRepair, maRemove, maBlocked);

const
  InvalidFileAttributes = $FFFFFFFF;
  InvalidHandleValue = -1;
  ErrorFileNotFound = 2;
  ErrorPathNotFound = 3;
  ErrorNoMoreFiles = 18;
  ErrorHandleEof = 38;
  FileShareRead = $00000001;
  FileShareWrite = $00000002;
  GenericWrite = $40000000;
  GenericRead = $80000000;
  OpenExisting = 3;
  FileFlagOpenReparsePoint = $00200000;
  FileFlagBackupSemantics = $02000000;
  FindStreamInfoStandard = 0;
  MaxBoundHandles = 32;
  { Keep this tied to StableAppIdGuid. Inno Setup writes current-user
    installations under this identity, independent of the chosen app folder. }
  ScribeUninstallRegKey =
    'Software\Microsoft\Windows\CurrentVersion\Uninstall\{8E0F1935-8E3D-4B1D-9A42-7C7D7C3D5E7A}_is1';
  UninstallKeyPollIntervalMs = 250;
  UninstallKeyPollAttempts = 20;
  DriveFixed = 3;

var
  BoundHandles: array[0..31] of THandle;
  BoundHandlePaths: array[0..31] of String;
  BoundHandleReleaseBeforeInnoReplacement: array[0..31] of Boolean;
  BoundHandleCount: Integer;
  TestPauseRequested: Boolean;
  TestContainerRoot: String;
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

function GetFileAttributesW(FileName: String): LongWord;
  external 'GetFileAttributesW@kernel32.dll stdcall';

function GetDriveType(const RootPathName: String): Cardinal;
  external 'GetDriveTypeW@kernel32.dll stdcall';

function CreateFileW(
  FileName: String;
  DesiredAccess: LongWord;
  ShareMode: LongWord;
  SecurityAttributes: LongWord;
  CreationDisposition: LongWord;
  FlagsAndAttributes: LongWord;
  TemplateFile: THandle
): THandle;
  external 'CreateFileW@kernel32.dll stdcall';

function CreateDirectoryW(PathName: String; SecurityAttributes: LongWord): Boolean;
  external 'CreateDirectoryW@kernel32.dll stdcall';

function CloseHandle(Handle: THandle): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';

function FindFirstFileW(
  FileName: String;
  var FindFileData: TWin32FindDataW
): THandle;
  external 'FindFirstFileW@kernel32.dll stdcall';

function FindNextFileW(
  FindFile: THandle;
  var FindFileData: TWin32FindDataW
): Boolean;
  external 'FindNextFileW@kernel32.dll stdcall';

function NativeFindClose(FindFile: THandle): Boolean;
  external 'FindClose@kernel32.dll stdcall';

function FindFirstStreamW(
  FileName: String;
  InfoLevel: LongWord;
  var FindStreamData: TWin32FindStreamData;
  Flags: LongWord
): THandle;
  external 'FindFirstStreamW@kernel32.dll stdcall';

function FindNextStreamW(
  FindStream: THandle;
  var FindStreamData: TWin32FindStreamData
): Boolean;
  external 'FindNextStreamW@kernel32.dll stdcall';

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
      Exit;
    end;
    if (Segment = '.') or (Segment = '..') or
       (Segment[Length(Segment)] = '.') or (Segment[Length(Segment)] = ' ') then begin
      Exit;
    end;
  end;
  Result := True;
end;

function TryGetCanonicalFixedDirectory(
  const Candidate: String;
  var CanonicalPath: String
): Boolean;
begin
  CanonicalPath := '';
  Result := False;
  if (Candidate = '') or (Candidate <> Trim(Candidate)) or
     (Length(Candidate) <= 3) then begin
    Exit;
  end;
  if (Candidate[Length(Candidate)] = '\') or
     not IsAsciiDriveLetter(Candidate[1]) or
     (Candidate[2] <> ':') or
     (Candidate[3] <> '\') or
     (Pos('/', Candidate) <> 0) or
     PathHasInvalidCharacters(Candidate, True) or
     not HasOnlyCanonicalDirectorySegments(Candidate) then begin
    Exit;
  end;

  CanonicalPath := ExpandFileName(Candidate);
  Result :=
    (CompareText(CanonicalPath, Candidate) = 0) and
    (GetDriveType(Copy(CanonicalPath, 1, 3)) = DriveFixed);
  if not Result then
    CanonicalPath := '';
end;

function ExtractUninstallerExecutable(
  const UninstallString: String;
  var Executable: String
): Boolean;
var
  TrimmedCommand: String;
begin
  Result := False;
  Executable := '';
  TrimmedCommand := Trim(UninstallString);

  if (TrimmedCommand = '') or (Length(TrimmedCommand) < 3) then
    Exit;
  if (TrimmedCommand[1] <> '"') or
     (TrimmedCommand[Length(TrimmedCommand)] <> '"') then
    Exit;

  { Permit only outer whitespace. The command itself must be one quoted
    executable path with no arguments, switches, or suffixes. }
  if Pos('"', Copy(TrimmedCommand, 2, Length(TrimmedCommand) - 2)) <> 0 then
    Exit;
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
  if not Result then
    Exit;
  for Index := 6 to 8 do begin
    if (NormalizedFilename[Index] < '0') or (NormalizedFilename[Index] > '9') then begin
      Result := False;
      Exit;
    end;
  end;
end;

function TryGetCanonicalUninstallerPath(
  const Candidate: String;
  var CanonicalPath: String
): Boolean;
var
  CanonicalDirectory: String;
  Filename: String;
begin
  CanonicalPath := '';
  Filename := ExtractFileName(Candidate);
  Result :=
    IsInnoUninstallerFilename(Filename) and
    TryGetCanonicalFixedDirectory(ExtractFileDir(Candidate), CanonicalDirectory);
  if not Result then
    Exit;

  CanonicalPath := ExpandFileName(Candidate);
  Result := CompareText(CanonicalPath, AddBackslash(CanonicalDirectory) + Filename) = 0;
  if not Result then
    CanonicalPath := '';
end;

function IsTrustedUninstaller(
  const Candidate: String;
  const CanonicalInstallPath: String;
  var CanonicalUninstallerPath: String
): Boolean;
var
  CanonicalUninstallerDirectory: String;
begin
  Result := TryGetCanonicalUninstallerPath(Candidate, CanonicalUninstallerPath);
  if not Result then
    Exit;
  CanonicalUninstallerDirectory := ExtractFileDir(CanonicalUninstallerPath);
  Result :=
    FileExists(CanonicalUninstallerPath) and
    (CompareText(CanonicalUninstallerDirectory, CanonicalInstallPath) = 0);
  if not Result then
    CanonicalUninstallerPath := '';
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

  if not ExistingInstallDetected then
    Exit;

  if not RegQueryStringValue(HKCU, ScribeUninstallRegKey, 'DisplayVersion', ExistingVersion) or
     not RegQueryStringValue(HKCU, ScribeUninstallRegKey, 'InstallLocation', RegisteredInstallPath) or
     not TryGetCanonicalFixedDirectory(RegisteredInstallPath, ExistingInstallPath) then
    Exit;

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
  if not ExistingInstallDetected then
    Result := maInstall
  else if not ExistingInstallUsable then begin
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
    if Result then
      Exit;
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
     not IsTrustedUninstaller(UninstallerCandidate, CanonicalInstallPath, CanonicalUninstallerPath) then
    Exit;

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
    Exit;
  end;

  { Run only the validated uninstaller executable and supply our own arguments;
    never replay the registry command line. No /SILENT flag lets its normal UI
    report cancellation or failure to the user. }
  if not RevalidateExistingUninstaller then begin
    MsgBox('Remove is unavailable because Scribe installer registration changed or can no longer be trusted. ' +
      'Choose Update or Repair to recreate installer registration.', mbError, MB_OK);
    Exit;
  end;
  if not Exec(ExistingUninstallerPath, '/NORESTART', '', SW_SHOWNORMAL,
      ewWaitUntilTerminated, ResultCode) then begin
    MsgBox('Scribe could not start its uninstaller. Nothing was installed or changed.', mbError, MB_OK);
    Exit;
  end;
  if ResultCode <> 0 then begin
    MsgBox('Scribe removal did not complete (uninstaller exit code ' + IntToStr(ResultCode) + '). ' +
      'Nothing will be installed by this setup.', mbError, MB_OK);
    Exit;
  end;

  if not WaitForUninstallRegistrationRemoval then begin
    MsgBox('Scribe removal was cancelled or did not finish. Nothing will be installed by this setup.',
      mbInformation, MB_OK);
    Exit;
  end;

  Result := True;
end;

function IsLowerHexToken(Token: String): Boolean;
var
  I: Integer;
  C: Char;
begin
  Result := False;
  if Length(Token) <> 32 then
    Exit;
  for I := 1 to Length(Token) do
  begin
    C := Token[I];
    if not (((C >= '0') and (C <= '9')) or ((C >= 'a') and (C <= 'f'))) then
      Exit;
  end;
  Result := True;
end;

function ReadBoundedToken(ParameterName: String): String;
begin
  Result := ExpandConstant('{param:' + ParameterName + '|}');
  if (Result <> '') and not IsLowerHexToken(Result) then
    RaiseException('Invalid Scribe installer test token for /' + ParameterName + '.');
end;

function VerificationToken: String;
begin
  Result := ReadBoundedToken('SCRIBEVERIFY');
end;

function StableTestToken: String;
begin
  Result := ReadBoundedToken('SCRIBESTABLETEST');
end;

function StableInstallDir: String;
begin
  Result := ExpandConstant('{localappdata}\Programs\Scribe');
end;

function VerificationRoot(Token: String): String;
begin
  Result := AddBackslash(RemoveBackslashUnlessRoot(ExpandFileName(GetTempDir))) +
    'scribe-release-verification-' + Token;
end;

function StableTestRoot(Token: String): String;
begin
  Result := AddBackslash(RemoveBackslashUnlessRoot(ExpandFileName(GetTempDir))) +
    'scribe-release-stable-test-' + Token;
end;

function TestShellRoot(Token: String): String;
begin
  Result := AddBackslash(RemoveBackslashUnlessRoot(ExpandFileName(GetTempDir))) +
    'scribe-release-shell-test-' + Token;
end;

function VerificationInstallDir(Token: String): String;
begin
  Result := AddBackslash(VerificationRoot(Token)) + 'installed';
end;

function StableTestInstallDir(Token: String): String;
begin
  Result := AddBackslash(StableTestRoot(Token)) + 'installed';
end;

function ResolveDefaultDir(Param: String): String;
var
  VerifyToken: String;
  TestToken: String;
begin
  VerifyToken := VerificationToken();
  TestToken := StableTestToken();
  if (VerifyToken <> '') and (TestToken <> '') then
    RaiseException('/SCRIBEVERIFY and /SCRIBESTABLETEST are mutually exclusive.');
  if VerifyToken <> '' then
    Result := VerificationInstallDir(VerifyToken)
  else if TestToken <> '' then
    Result := StableTestInstallDir(TestToken)
  else
    Result := StableInstallDir();
end;

function ResolveAppId(Param: String): String;
var
  VerifyToken: String;
  TestToken: String;
begin
  VerifyToken := VerificationToken();
  TestToken := StableTestToken();
  Result := '{' + '{#StableAppIdGuid}' + '}';
  if VerifyToken <> '' then
    Result := Result + '.verification.' + VerifyToken
  else if TestToken <> '' then
    Result := Result + '.stable-test.' + TestToken;
end;

function IsNormalInstall(): Boolean;
begin
  Result := (VerificationToken() = '') and (StableTestToken() = '');
end;

function ActiveTestToken(): String;
begin
  Result := VerificationToken();
  if Result = '' then
    Result := StableTestToken();
end;

function ResolveStartMenuDirectory(Param: String): String;
var
  Token: String;
begin
  Token := ActiveTestToken();
  if Token = '' then
    Result := ExpandConstant('{autoprograms}')
  else
    Result := AddBackslash(TestShellRoot(Token)) + 'StartMenu';
end;

function ResolveDesktopDirectory(Param: String): String;
var
  Token: String;
begin
  Token := ActiveTestToken();
  if Token = '' then
    Result := ExpandConstant('{autodesktop}')
  else
    Result := AddBackslash(TestShellRoot(Token)) + 'Desktop';
end;

function ResolveLaunchTarget(Param: String): String;
var
  Token: String;
begin
  Token := ActiveTestToken();
  if Token = '' then
    Result := ExpandConstant('{app}\{#AppExeName}')
  else
    Result := AddBackslash(TestShellRoot(Token)) + 'run-sentinel.exe';
end;

procedure ReleaseBoundHandles();
var
  I: Integer;
begin
  for I := BoundHandleCount - 1 downto 0 do
  begin
    if BoundHandles[I] <> InvalidHandleValue then
      CloseHandle(BoundHandles[I]);
    BoundHandles[I] := InvalidHandleValue;
    BoundHandlePaths[I] := '';
    BoundHandleReleaseBeforeInnoReplacement[I] := False;
  end;
  BoundHandleCount := 0;
end;

procedure ReleaseInnoUninstallerHandles();
var
  I: Integer;
begin
  for I := BoundHandleCount - 1 downto 0 do
  begin
    if BoundHandleReleaseBeforeInnoReplacement[I] then
    begin
      if BoundHandles[I] <> InvalidHandleValue then
        CloseHandle(BoundHandles[I]);
      BoundHandles[I] := InvalidHandleValue;
      BoundHandlePaths[I] := '';
      BoundHandleReleaseBeforeInnoReplacement[I] := False;
    end;
  end;
end;

procedure ReleasePayloadHandleForCurrentFile();
var
  CurrentPath: String;
  MatchingHandleIndex: Integer;
  I: Integer;
  ErrorCode: LongInt;
begin
  CurrentPath := RemoveBackslashUnlessRoot(ExpandFileName(ExpandConstant(CurrentFilename)));
  MatchingHandleIndex := -1;
  for I := 0 to BoundHandleCount - 1 do
  begin
    if (BoundHandles[I] <> InvalidHandleValue) and
       SameStr(BoundHandlePaths[I], CurrentPath) then
    begin
      if BoundHandleReleaseBeforeInnoReplacement[I] then
        RaiseException('Scribe Setup refused to release an Inno uninstaller handle from a payload file callback.');
      if MatchingHandleIndex <> -1 then
        RaiseException('Scribe Setup refused to release an ambiguously matched payload file handle: ' + CurrentPath);
      MatchingHandleIndex := I;
    end;
  end;
  if FileExists(CurrentPath) then
  begin
    if MatchingHandleIndex = -1 then
      RaiseException('Scribe Setup refused to install over an existing payload file without its retained preflight handle: ' + CurrentPath);
    if not CloseHandle(BoundHandles[MatchingHandleIndex]) then
    begin
      ErrorCode := DLLGetLastError;
      RaiseException('Scribe Setup could not release the retained payload file handle immediately before replacement: ' +
        CurrentPath + ' (' + SysErrorMessage(ErrorCode) + ').');
    end;
    BoundHandles[MatchingHandleIndex] := InvalidHandleValue;
    BoundHandlePaths[MatchingHandleIndex] := '';
    BoundHandleReleaseBeforeInnoReplacement[MatchingHandleIndex] := False;
  end
  else if MatchingHandleIndex <> -1 then
    RaiseException('Scribe Setup refused a payload file that changed identity before replacement: ' + CurrentPath);
end;

function RetainBoundHandle(
  Handle: THandle;
  Path: String;
  ReleaseBeforeInnoReplacement: Boolean;
  var ErrorText: String
): Boolean;
begin
  Result := False;
  if BoundHandleCount >= MaxBoundHandles then
  begin
    ErrorText := 'Scribe Setup refused the destination because its safety handle limit was exceeded.';
    CloseHandle(Handle);
    Exit;
  end;
  BoundHandles[BoundHandleCount] := Handle;
  BoundHandlePaths[BoundHandleCount] := Path;
  BoundHandleReleaseBeforeInnoReplacement[BoundHandleCount] := ReleaseBeforeInnoReplacement;
  BoundHandleCount := BoundHandleCount + 1;
  Result := True;
end;

function IsAllowedExistingDirectory(RelativePath: String): Boolean;
begin
  Result := SameStr(RelativePath, 'licenses');
end;

function IsAllowedExistingFile(RelativePath: String): Boolean;
begin
  Result :=
    SameStr(RelativePath, 'bundle-inventory.json') or
    SameStr(RelativePath, 'bundled-model-manifest.json') or
    SameStr(RelativePath, 'local-transcriber.exe') or
    SameStr(RelativePath, 'README.txt') or
    SameStr(RelativePath, 'whisper-base.en-Q8_0.gguf') or
    SameStr(RelativePath, 'licenses\Apache-2.0.txt') or
    SameStr(RelativePath, 'licenses\OpenAI-Whisper-MIT.txt') or
    SameStr(RelativePath, 'licenses\sherpa-onnx-PROVENANCE.md') or
    SameStr(RelativePath, 'licenses\Silero-VAD-MIT.txt') or
    SameStr(RelativePath, 'licenses\Silero-VAD-PROVENANCE.md') or
    SameStr(RelativePath, 'licenses\THIRD-PARTY-NOTICES.txt') or
    SameStr(RelativePath, 'licenses\transcribe.cpp-MIT.txt') or
    SameStr(RelativePath, 'licenses\transcribe.cpp-PROVENANCE.md') or
    SameStr(RelativePath, 'licenses\Whisper-Base-En-NOTICE.txt') or
    SameStr(RelativePath, 'licenses\whisper.cpp-MIT.txt') or
    SameStr(RelativePath, 'licenses\whisper.cpp-PROVENANCE.md') or
    SameStr(RelativePath, 'unins000.exe') or
    SameStr(RelativePath, 'unins000.dat');
end;

function IsInnoUninstallerArtifact(RelativePath: String): Boolean;
begin
  Result := SameStr(RelativePath, 'unins000.exe') or
    SameStr(RelativePath, 'unins000.dat');
end;

function QueryExistingAttributes(
  Path: String;
  var Attributes: LongWord;
  var PathExists: Boolean;
  var ErrorText: String
): Boolean;
var
  ErrorCode: LongInt;
begin
  Result := False;
  Attributes := GetFileAttributesW(Path);
  if Attributes <> InvalidFileAttributes then
  begin
    PathExists := True;
    Result := True;
    Exit;
  end;
  ErrorCode := DLLGetLastError;
  if (ErrorCode = ErrorFileNotFound) or (ErrorCode = ErrorPathNotFound) then
  begin
    PathExists := False;
    Result := True;
    Exit;
  end;
  ErrorText := 'Scribe Setup could not safely inspect the destination: ' +
    Path + ' (' + SysErrorMessage(ErrorCode) + ').';
end;

function FileNameFromNative(Name: TNativeFileName; var Value: String): Boolean;
var
  I: Integer;
begin
  Value := '';
  for I := 0 to 259 do
  begin
    if Name[I] = #0 then
    begin
      Result := True;
      Exit;
    end;
    Value := Value + Name[I];
  end;
  Result := False;
end;

function StreamNameFromNative(Name: TNativeStreamName; var Value: String): Boolean;
var
  I: Integer;
begin
  Value := '';
  for I := 0 to 295 do
  begin
    if Name[I] = #0 then
    begin
      Result := True;
      Exit;
    end;
    Value := Value + Name[I];
  end;
  Result := False;
end;

function RejectAlternateStreams(
  Path: String;
  IsDirectory: Boolean;
  var ErrorText: String
): Boolean;
var
  StreamData: TWin32FindStreamData;
  StreamHandle: THandle;
  StreamName: String;
  ErrorCode: LongInt;
begin
  Result := False;
  StreamHandle := FindFirstStreamW(
    Path, FindStreamInfoStandard, StreamData, 0);
  if StreamHandle = InvalidHandleValue then
  begin
    ErrorCode := DLLGetLastError;
    if IsDirectory and (ErrorCode = ErrorHandleEof) then
    begin
      Result := True;
      Exit;
    end;
    ErrorText := 'Scribe Setup refused the destination because alternate-stream enumeration failed: ' +
      Path + ' (' + SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  try
    if not StreamNameFromNative(StreamData.StreamName, StreamName) then
    begin
      ErrorText := 'Scribe Setup refused the destination because a stream name was not terminated safely: ' + Path;
      Exit;
    end;
    if IsDirectory or not SameStr(StreamName, '::$DATA') then
    begin
      ErrorText := 'Scribe Setup refused the destination because it contains an alternate NTFS data stream: ' + Path;
      Exit;
    end;
    if FindNextStreamW(StreamHandle, StreamData) then
    begin
      ErrorText := 'Scribe Setup refused the destination because it contains an alternate NTFS data stream: ' + Path;
      Exit;
    end;
    ErrorCode := DLLGetLastError;
    if ErrorCode <> ErrorHandleEof then
    begin
      ErrorText := 'Scribe Setup refused the destination because alternate-stream enumeration did not complete safely: ' +
        Path + ' (' + SysErrorMessage(ErrorCode) + ').';
      Exit;
    end;
  finally
    NativeFindClose(StreamHandle);
  end;
  Result := True;
end;

function BindDirectory(Path: String; var ErrorText: String): Boolean;
var
  DirectoryHandle: THandle;
  Attributes: LongWord;
  PathExists: Boolean;
  ErrorCode: LongInt;
begin
  Result := False;
  DirectoryHandle := CreateFileW(
    Path, 0, FileShareRead or FileShareWrite, 0, OpenExisting,
    FileFlagBackupSemantics or FileFlagOpenReparsePoint, 0);
  if DirectoryHandle = InvalidHandleValue then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup refused the destination because a directory could not be identity-bound: ' +
      Path + ' (' + SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  if not RetainBoundHandle(DirectoryHandle, Path, False, ErrorText) then
    Exit;
  if not QueryExistingAttributes(Path, Attributes, PathExists, ErrorText) then
    Exit;
  if not PathExists or ((Attributes and FILE_ATTRIBUTE_DIRECTORY) = 0) or
     ((Attributes and FILE_ATTRIBUTE_REPARSE_POINT) <> 0) or
     ((Attributes and FILE_ATTRIBUTE_DEVICE) <> 0) then
  begin
    ErrorText := 'Scribe Setup refused the destination because an expected directory changed identity or type: ' + Path;
    Exit;
  end;
  if not RejectAlternateStreams(Path, True, ErrorText) then
    Exit;
  Result := True;
end;

function BindFileForUpdate(
  Path: String;
  ReleaseBeforeInnoReplacement: Boolean;
  var ErrorText: String
): Boolean;
var
  IdentityHandle: THandle;
  UpdateProbe: THandle;
  IdentityAccess: LongWord;
  Attributes: LongWord;
  PathExists: Boolean;
  ErrorCode: LongInt;
begin
  Result := False;
  IdentityAccess := 0;
  if not ReleaseBeforeInnoReplacement then
    IdentityAccess := GenericRead;
  IdentityHandle := CreateFileW(
    Path, IdentityAccess, FileShareRead or FileShareWrite, 0, OpenExisting,
    FileFlagOpenReparsePoint, 0);
  if IdentityHandle = InvalidHandleValue then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup refused the destination because an allowed file could not be identity-bound: ' +
      Path + ' (' + SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  if not RetainBoundHandle(
    IdentityHandle, Path, ReleaseBeforeInnoReplacement, ErrorText) then
    Exit;
  if not QueryExistingAttributes(Path, Attributes, PathExists, ErrorText) then
    Exit;
  if not PathExists or ((Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0) or
     ((Attributes and FILE_ATTRIBUTE_REPARSE_POINT) <> 0) or
     ((Attributes and FILE_ATTRIBUTE_DEVICE) <> 0) then
  begin
    ErrorText := 'Scribe Setup refused the destination because an allowed file changed identity or type: ' + Path;
    Exit;
  end;
  if (Attributes and FILE_ATTRIBUTE_READONLY) <> 0 then
  begin
    ErrorText := 'Scribe Setup refused the destination because an allowed file is read-only: ' + Path;
    Exit;
  end;
  if not RejectAlternateStreams(Path, False, ErrorText) then
    Exit;
  UpdateProbe := CreateFileW(
    Path, GenericRead or GenericWrite, FileShareRead or FileShareWrite,
    0, OpenExisting, FileFlagOpenReparsePoint, 0);
  if UpdateProbe = InvalidHandleValue then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup refused the destination because an allowed file cannot be updated safely: ' +
      Path + ' (' + SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  CloseHandle(UpdateProbe);
  Result := True;
end;

function ValidateNoReparseAncestors(Path: String; var ErrorText: String): Boolean;
var
  CurrentPath: String;
  ParentPath: String;
  Attributes: LongWord;
  PathExists: Boolean;
begin
  Result := False;
  CurrentPath := RemoveBackslashUnlessRoot(ExpandFileName(Path));
  while CurrentPath <> '' do
  begin
    if not QueryExistingAttributes(CurrentPath, Attributes, PathExists, ErrorText) then
      Exit;
    if PathExists and ((Attributes and FILE_ATTRIBUTE_REPARSE_POINT) <> 0) then
    begin
      ErrorText := 'Scribe Setup refused the destination because it crosses a symbolic link or reparse point: ' + CurrentPath;
      Exit;
    end;
    ParentPath := ExtractFileDir(CurrentPath);
    if (ParentPath = '') or SameText(ParentPath, CurrentPath) then
      Break;
    CurrentPath := ParentPath;
  end;
  Result := True;
end;

function EnsureParentDirectory(Path: String; var ErrorText: String): Boolean;
var
  ParentPath: String;
  Attributes: LongWord;
  PathExists: Boolean;
  ErrorCode: LongInt;
begin
  Result := False;
  ParentPath := ExtractFileDir(RemoveBackslashUnlessRoot(Path));
  if ParentPath = '' then
  begin
    ErrorText := 'Scribe Setup refused to create a destination without a bounded parent.';
    Exit;
  end;
  if not QueryExistingAttributes(ParentPath, Attributes, PathExists, ErrorText) then
    Exit;
  if not PathExists then
  begin
    if not EnsureParentDirectory(ParentPath, ErrorText) then
      Exit;
    if not CreateDirectoryW(ParentPath, 0) then
    begin
      ErrorCode := DLLGetLastError;
      ErrorText := 'Scribe Setup could not create the destination parent safely: ' +
        ParentPath + ' (' + SysErrorMessage(ErrorCode) + ').';
      Exit;
    end;
  end
  else if ((Attributes and FILE_ATTRIBUTE_DIRECTORY) = 0) or
          ((Attributes and FILE_ATTRIBUTE_REPARSE_POINT) <> 0) or
          ((Attributes and FILE_ATTRIBUTE_DEVICE) <> 0) then
  begin
    ErrorText := 'Scribe Setup refused an unsafe destination parent: ' + ParentPath;
    Exit;
  end;
  Result := True;
end;

function CreateAndBindDirectory(Path: String; var ErrorText: String): Boolean;
var
  ErrorCode: LongInt;
begin
  Result := False;
  if not EnsureParentDirectory(Path, ErrorText) then
    Exit;
  if not CreateDirectoryW(Path, 0) then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup could not create and bind its exact destination directory: ' +
      Path + ' (' + SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  Result := BindDirectory(Path, ErrorText);
end;

function InspectExistingTree(
  Root: String;
  RelativeDirectory: String;
  SeenPaths: TStringList;
  var HasLicensesDirectory: Boolean;
  var HasUninstallerExe: Boolean;
  var HasUninstallerData: Boolean;
  var ErrorText: String
): Boolean;
var
  DirectoryPath: String;
  RelativePath: String;
  ChildPath: String;
  EntryName: String;
  FindData: TWin32FindDataW;
  FindHandle: THandle;
  HasNext: Boolean;
  ErrorCode: LongInt;
begin
  Result := False;
  DirectoryPath := Root;
  if RelativeDirectory <> '' then
    DirectoryPath := AddBackslash(Root) + RelativeDirectory;
  if not BindDirectory(DirectoryPath, ErrorText) then
    Exit;

  FindHandle := FindFirstFileW(AddBackslash(DirectoryPath) + '*', FindData);
  if FindHandle = InvalidHandleValue then
  begin
    ErrorCode := DLLGetLastError;
    if ErrorCode = ErrorFileNotFound then
    begin
      Result := True;
      Exit;
    end;
    ErrorText := 'Scribe Setup refused the destination because enumeration could not start safely: ' +
      DirectoryPath + ' (' + SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  try
    repeat
      if not FileNameFromNative(FindData.FileName, EntryName) then
      begin
        ErrorText := 'Scribe Setup refused the destination because an entry name was not terminated safely: ' + DirectoryPath;
        Exit;
      end;
      if not SameStr(EntryName, '.') and not SameStr(EntryName, '..') then
      begin
        RelativePath := EntryName;
        if RelativeDirectory <> '' then
          RelativePath := AddBackslash(RelativeDirectory) + EntryName;
        ChildPath := AddBackslash(Root) + RelativePath;
        if SeenPaths.IndexOf(RelativePath) >= 0 then
        begin
          ErrorText := 'Scribe Setup refused the destination because it contains a case-insensitive path collision: ' + RelativePath;
          Exit;
        end;
        SeenPaths.Add(RelativePath);
        if ((FindData.FileAttributes and FILE_ATTRIBUTE_REPARSE_POINT) <> 0) or
           ((FindData.FileAttributes and FILE_ATTRIBUTE_DEVICE) <> 0) then
        begin
          ErrorText := 'Scribe Setup refused the destination because it contains a reparse point or device entry: ' + RelativePath;
          Exit;
        end;
        if (FindData.FileAttributes and FILE_ATTRIBUTE_DIRECTORY) <> 0 then
        begin
          if not IsAllowedExistingDirectory(RelativePath) then
          begin
            ErrorText := 'Scribe Setup refused the destination because it contains an unexpected or legacy directory: ' + RelativePath;
            Exit;
          end;
          if SameStr(RelativePath, 'licenses') then
            HasLicensesDirectory := True;
          if not InspectExistingTree(
            Root, RelativePath, SeenPaths, HasLicensesDirectory,
            HasUninstallerExe, HasUninstallerData, ErrorText) then
            Exit;
        end
        else
        begin
          if not IsAllowedExistingFile(RelativePath) then
          begin
            ErrorText := 'Scribe Setup refused the destination because it contains an unexpected or legacy file: ' + RelativePath;
            Exit;
          end;
          if not BindFileForUpdate(
            ChildPath, IsInnoUninstallerArtifact(RelativePath), ErrorText) then
            Exit;
          if SameStr(RelativePath, 'unins000.exe') then
            HasUninstallerExe := True;
          if SameStr(RelativePath, 'unins000.dat') then
            HasUninstallerData := True;
        end;
      end;
      HasNext := FindNextFileW(FindHandle, FindData);
      if not HasNext then
      begin
        ErrorCode := DLLGetLastError;
        if ErrorCode <> ErrorNoMoreFiles then
        begin
          ErrorText := 'Scribe Setup refused the destination because enumeration failed before completion: ' +
            DirectoryPath + ' (' + SysErrorMessage(ErrorCode) + ').';
          Exit;
        end;
      end;
    until not HasNext;
  finally
    NativeFindClose(FindHandle);
  end;
  Result := True;
end;

function ValidateAndBindInstallTree(InstallRoot: String; var ErrorText: String): Boolean;
var
  Attributes: LongWord;
  PathExists: Boolean;
  SeenPaths: TStringList;
  HasLicensesDirectory: Boolean;
  HasUninstallerExe: Boolean;
  HasUninstallerData: Boolean;
begin
  Result := False;
  if not ValidateNoReparseAncestors(InstallRoot, ErrorText) then
    Exit;
  if not QueryExistingAttributes(InstallRoot, Attributes, PathExists, ErrorText) then
    Exit;
  if not PathExists then
  begin
    if not CreateAndBindDirectory(InstallRoot, ErrorText) then
      Exit;
    if not CreateAndBindDirectory(AddBackslash(InstallRoot) + 'licenses', ErrorText) then
      Exit;
    Result := ValidateNoReparseAncestors(InstallRoot, ErrorText);
    Exit;
  end;
  if ((Attributes and FILE_ATTRIBUTE_DIRECTORY) = 0) or
     ((Attributes and FILE_ATTRIBUTE_REPARSE_POINT) <> 0) or
     ((Attributes and FILE_ATTRIBUTE_DEVICE) <> 0) then
  begin
    ErrorText := 'Scribe Setup refused the destination because the existing program path is not a regular directory: ' + InstallRoot;
    Exit;
  end;

  HasLicensesDirectory := False;
  HasUninstallerExe := False;
  HasUninstallerData := False;
  SeenPaths := TStringList.Create;
  try
    SeenPaths.CaseSensitive := False;
    if not InspectExistingTree(
      InstallRoot, '', SeenPaths, HasLicensesDirectory,
      HasUninstallerExe, HasUninstallerData, ErrorText) then
      Exit;
  finally
    SeenPaths.Free;
  end;
  if HasUninstallerExe <> HasUninstallerData then
  begin
    ErrorText := 'Scribe Setup refused the destination because the Inno uninstaller pair is incomplete.';
    Exit;
  end;
  if not HasLicensesDirectory then
    if not CreateAndBindDirectory(AddBackslash(InstallRoot) + 'licenses', ErrorText) then
      Exit;
  if not ValidateNoReparseAncestors(InstallRoot, ErrorText) then
    Exit;
  Result := True;
end;

function BindExistingOrCreateTestContainer(Path: String; var ErrorText: String): Boolean;
var
  Attributes: LongWord;
  PathExists: Boolean;
begin
  Result := False;
  if not ValidateNoReparseAncestors(Path, ErrorText) then
    Exit;
  if not QueryExistingAttributes(Path, Attributes, PathExists, ErrorText) then
    Exit;
  if PathExists then
    Result := BindDirectory(Path, ErrorText)
  else
    Result := CreateAndBindDirectory(Path, ErrorText);
end;

function PrepareVerificationRoot(
  Token: String;
  InstallRoot: String;
  var ErrorText: String
): Boolean;
var
  ExpectedRoot: String;
  Attributes: LongWord;
  PathExists: Boolean;
begin
  Result := False;
  ExpectedRoot := RemoveBackslashUnlessRoot(VerificationInstallDir(Token));
  TestContainerRoot := RemoveBackslashUnlessRoot(VerificationRoot(Token));
  if not SameStr(InstallRoot, ExpectedRoot) then
  begin
    ErrorText := 'Scribe installer verification refused a destination outside its exact token-bound temporary directory.';
    Exit;
  end;
  if not ValidateNoReparseAncestors(TestContainerRoot, ErrorText) then
    Exit;
  if not QueryExistingAttributes(TestContainerRoot, Attributes, PathExists, ErrorText) then
    Exit;
  if PathExists then
  begin
    ErrorText := 'Scribe installer verification refused an existing token-bound temporary directory.';
    Exit;
  end;
  if not CreateAndBindDirectory(TestContainerRoot, ErrorText) then
    Exit;
  if not CreateAndBindDirectory(InstallRoot, ErrorText) then
    Exit;
  if not CreateAndBindDirectory(AddBackslash(InstallRoot) + 'licenses', ErrorText) then
    Exit;
  Result := True;
end;

procedure WaitAtTestBoundary();
var
  ReadyPath: String;
  ContinuePath: String;
  WaitCount: Integer;
begin
  if not TestPauseRequested then
    Exit;
  ReadyPath := AddBackslash(TestContainerRoot) + 'preflight-ready';
  ContinuePath := AddBackslash(TestContainerRoot) + 'preflight-continue';
  if not SaveStringToFile(ReadyPath, 'ready', False) then
    RaiseException('Could not write the bounded installer race-test marker.');
  WaitCount := 0;
  while not FileExists(ContinuePath) do
  begin
    Sleep(50);
    WaitCount := WaitCount + 1;
    if WaitCount > 1200 then
      RaiseException('Timed out waiting for the bounded installer race-test continuation marker.');
  end;
end;

procedure InitializeWizard;
begin
  SelectedMaintenanceAction := maInstall;
  if not IsNormalInstall() then
    Exit;
  LoadExistingInstallation;
  AddMaintenancePage;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := False;
  if not IsNormalInstall() then
    Exit;
  Result := (PageID = MaintenancePage.ID) and WizardSilent;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if not IsNormalInstall() then
    Exit;
  if CurPageID <> MaintenancePage.ID then
    Exit;

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

function IsAllowedNormalInstallRoot(const InstallRoot: String): Boolean;
begin
  if not ExistingInstallDetected then begin
    Result := SameStr(InstallRoot, RemoveBackslashUnlessRoot(StableInstallDir()));
    Exit;
  end;
  Result :=
    ExistingInstallUsable and
    ((SelectedMaintenanceAction = maUpdate) or
     (SelectedMaintenanceAction = maRepair)) and
    (CompareText(InstallRoot, ExistingInstallPath) = 0);
end;

function InitializeSetup(): Boolean;
var
  VerifyToken: String;
  TestToken: String;
  FindDataLayoutProbe: TWin32FindDataW;
  StreamDataLayoutProbe: TWin32FindStreamData;
begin
  Result := False;
  BoundHandleCount := 0;
  if SizeOf(FindDataLayoutProbe) <> 592 then
    RaiseException('Unsupported WIN32_FIND_DATAW ABI layout.');
  if SizeOf(StreamDataLayoutProbe) <> 600 then
    RaiseException('Unsupported WIN32_FIND_STREAM_DATA ABI layout.');
  VerifyToken := VerificationToken();
  TestToken := StableTestToken();
  if (VerifyToken <> '') and (TestToken <> '') then
    RaiseException('/SCRIBEVERIFY and /SCRIBESTABLETEST are mutually exclusive.');
  TestPauseRequested := ExpandConstant('{param:SCRIBETESTPAUSE|}') = '1';
  if TestPauseRequested and (VerifyToken = '') and (TestToken = '') then
    RaiseException('/SCRIBETESTPAUSE is restricted to a bounded installer test token.');
  Result := True;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  VerifyToken: String;
  TestToken: String;
  InstallRoot: String;
begin
  Result := '';
  ReleaseBoundHandles();
  TestContainerRoot := '';
  VerifyToken := VerificationToken();
  TestToken := StableTestToken();
  InstallRoot := RemoveBackslashUnlessRoot(ExpandFileName(WizardDirValue));

  if VerifyToken <> '' then
  begin
    if not PrepareVerificationRoot(VerifyToken, InstallRoot, Result) then
      ReleaseBoundHandles();
    Exit;
  end;

  if IsNormalInstall() then begin
    if ExistingInstallDetected and ExistingInstallUsable and
       (ExistingVersionComparison > 0) then
      Result := 'A newer Scribe ' + ExistingVersion + ' installation is already present. ' +
        'This installer will not downgrade it.'
    else if ExistingInstallDetected and not ExistingInstallUsable then
      Result := 'Scribe is registered as installed, but its installation details cannot be safely validated. ' +
        'This installer will not overwrite it.';
    if Result <> '' then begin
      Result := Result + #13#10 + #13#10 +
        'Setup did not delete or change any existing content.';
      Exit;
    end;
  end;

  if TestToken <> '' then
  begin
    TestContainerRoot := RemoveBackslashUnlessRoot(StableTestRoot(TestToken));
    if not BindExistingOrCreateTestContainer(TestContainerRoot, Result) then
    begin
      ReleaseBoundHandles();
      Exit;
    end;
    if not SameStr(InstallRoot, RemoveBackslashUnlessRoot(StableTestInstallDir(TestToken))) then
      Result := 'Scribe stable-upgrade testing refused a destination outside its exact token-bound temporary directory.'
    else if not ValidateAndBindInstallTree(InstallRoot, Result) then
    begin
    end;
  end
  else if not IsAllowedNormalInstallRoot(InstallRoot) then
    Result := 'Scribe Setup refused the destination. Fresh installs must use the canonical per-user program directory; updates and repairs must use the exact registered installation directory.'
  else if not ValidateAndBindInstallTree(InstallRoot, Result) then
  begin
  end;

  if Result <> '' then
  begin
    ReleaseBoundHandles();
    Result := Result + #13#10 + #13#10 +
      'Setup did not delete or change any existing content. Close Scribe, then choose whether to back up the program directory, uninstall the previous version, or remove the unexpected content yourself before retrying. Do not delete Scribe app-data settings, history, downloaded models, imported GGUF files, or external sentinels.';
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then
  begin
    ReleaseInnoUninstallerHandles();
    WaitAtTestBoundary();
  end;
  if CurStep = ssPostInstall then
    ReleaseBoundHandles();
end;

procedure DeinitializeSetup();
begin
  ReleaseBoundHandles();
end;
