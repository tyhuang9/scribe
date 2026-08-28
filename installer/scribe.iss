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
  InvalidFileAttributes = $FFFFFFFF;

function GetFileAttributesW(FileName: String): LongWord;
  external 'GetFileAttributesW@kernel32.dll stdcall';

function VerificationToken: String;
var
  I: Integer;
  C: Char;
begin
  Result := ExpandConstant('{param:SCRIBEVERIFY|}');
  if Result = '' then
    Exit;

  if Length(Result) <> 32 then
    RaiseException('Invalid Scribe installer verification token.');
  for I := 1 to Length(Result) do
  begin
    C := Result[I];
    if not (((C >= '0') and (C <= '9')) or ((C >= 'a') and (C <= 'f'))) then
      RaiseException('Invalid Scribe installer verification token.');
  end;
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

function VerificationInstallDir(Token: String): String;
begin
  Result := AddBackslash(VerificationRoot(Token)) + 'installed';
end;

function ResolveDefaultDir(Param: String): String;
var
  Token: String;
begin
  Token := VerificationToken();
  if Token = '' then
    Result := StableInstallDir()
  else
    Result := VerificationInstallDir(Token);
end;

function ResolveAppId(Param: String): String;
var
  Token: String;
begin
  Token := VerificationToken();
  Result := '{' + '{#StableAppIdGuid}' + '}';
  if Token <> '' then
    Result := Result + '.verification.' + Token;
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

function ValidateNoReparseAncestors(Path: String; var ErrorText: String): Boolean;
var
  CurrentPath: String;
  ParentPath: String;
  Attributes: LongWord;
begin
  Result := False;
  CurrentPath := RemoveBackslashUnlessRoot(ExpandFileName(Path));
  while CurrentPath <> '' do
  begin
    Attributes := GetFileAttributesW(CurrentPath);
    if (Attributes <> InvalidFileAttributes) and
       ((Attributes and FILE_ATTRIBUTE_REPARSE_POINT) <> 0) then
    begin
      ErrorText := 'Scribe Setup refused the destination because it crosses a symbolic link or reparse point: ' +
        CurrentPath;
      Exit;
    end;
    ParentPath := ExtractFileDir(CurrentPath);
    if (ParentPath = '') or SameText(ParentPath, CurrentPath) then
      Break;
    CurrentPath := ParentPath;
  end;
  Result := True;
end;

function InspectExistingTree(
  Root: String;
  RelativeDirectory: String;
  SeenPaths: TStringList;
  var HasUninstallerExe: Boolean;
  var HasUninstallerData: Boolean;
  var ErrorText: String
): Boolean;
var
  DirectoryPath: String;
  RelativePath: String;
  ChildPath: String;
  FindRec: TFindRec;
begin
  Result := False;
  DirectoryPath := Root;
  if RelativeDirectory <> '' then
    DirectoryPath := AddBackslash(Root) + RelativeDirectory;

  if FindFirst(AddBackslash(DirectoryPath) + '*', FindRec) then
  begin
    try
      repeat
        if not SameStr(FindRec.Name, '.') and not SameStr(FindRec.Name, '..') then
        begin
          RelativePath := FindRec.Name;
          if RelativeDirectory <> '' then
            RelativePath := AddBackslash(RelativeDirectory) + FindRec.Name;
          ChildPath := AddBackslash(Root) + RelativePath;

          if SeenPaths.IndexOf(RelativePath) >= 0 then
          begin
            ErrorText := 'Scribe Setup refused the existing program directory because it contains a case-insensitive path collision: ' +
              RelativePath;
            Exit;
          end;
          SeenPaths.Add(RelativePath);

          if (FindRec.Attributes and FILE_ATTRIBUTE_REPARSE_POINT) <> 0 then
          begin
            ErrorText := 'Scribe Setup refused the existing program directory because it contains a symbolic link or reparse point: ' +
              RelativePath;
            Exit;
          end;
          if (FindRec.Attributes and FILE_ATTRIBUTE_DEVICE) <> 0 then
          begin
            ErrorText := 'Scribe Setup refused the existing program directory because it contains a non-regular device entry: ' +
              RelativePath;
            Exit;
          end;

          if (FindRec.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0 then
          begin
            if not IsAllowedExistingDirectory(RelativePath) then
            begin
              ErrorText := 'Scribe Setup refused the existing program directory because it contains an unexpected or legacy directory: ' +
                RelativePath;
              Exit;
            end;
            if not InspectExistingTree(
              Root, RelativePath, SeenPaths, HasUninstallerExe,
              HasUninstallerData, ErrorText) then
              Exit;
          end
          else
          begin
            if not IsAllowedExistingFile(RelativePath) then
            begin
              ErrorText := 'Scribe Setup refused the existing program directory because it contains an unexpected or legacy file: ' +
                RelativePath;
              Exit;
            end;
            if SameStr(RelativePath, 'unins000.exe') then
              HasUninstallerExe := True;
            if SameStr(RelativePath, 'unins000.dat') then
              HasUninstallerData := True;
          end;
        end;
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;
  Result := True;
end;

function ValidateStableInstallTree(InstallRoot: String; var ErrorText: String): Boolean;
var
  Attributes: LongWord;
  SeenPaths: TStringList;
  HasUninstallerExe: Boolean;
  HasUninstallerData: Boolean;
begin
  Result := False;
  if not ValidateNoReparseAncestors(InstallRoot, ErrorText) then
    Exit;

  Attributes := GetFileAttributesW(InstallRoot);
  if Attributes = InvalidFileAttributes then
  begin
    Result := True;
    Exit;
  end;
  if (Attributes and FILE_ATTRIBUTE_DIRECTORY) = 0 then
  begin
    ErrorText := 'Scribe Setup refused the destination because the existing program path is not a directory: ' +
      InstallRoot;
    Exit;
  end;

  HasUninstallerExe := False;
  HasUninstallerData := False;
  SeenPaths := TStringList.Create;
  try
    SeenPaths.CaseSensitive := False;
    if not InspectExistingTree(
      InstallRoot, '', SeenPaths, HasUninstallerExe,
      HasUninstallerData, ErrorText) then
      Exit;
  finally
    SeenPaths.Free;
  end;
  if HasUninstallerExe <> HasUninstallerData then
  begin
    ErrorText := 'Scribe Setup refused the existing program directory because the Inno uninstaller pair is incomplete.';
    Exit;
  end;
  if not ValidateNoReparseAncestors(InstallRoot, ErrorText) then
    Exit;
  Result := True;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Token: String;
  InstallRoot: String;
  ExpectedRoot: String;
  Attributes: LongWord;
begin
  Result := '';
  Token := VerificationToken();
  InstallRoot := RemoveBackslashUnlessRoot(ExpandFileName(WizardDirValue));

  if Token <> '' then
  begin
    ExpectedRoot := RemoveBackslashUnlessRoot(VerificationInstallDir(Token));
    if not SameText(InstallRoot, ExpectedRoot) then
    begin
      Result := 'Scribe installer verification refused a destination outside its token-bound temporary directory.';
      Exit;
    end;
    if not ValidateNoReparseAncestors(InstallRoot, Result) then
      Exit;
    Attributes := GetFileAttributesW(InstallRoot);
    if Attributes <> InvalidFileAttributes then
    begin
      Result := 'Scribe installer verification refused an existing token-bound install directory.';
      Exit;
    end;
    Exit;
  end;

  if not ValidateStableInstallTree(InstallRoot, Result) then
  begin
    Result := Result + #13#10 + #13#10 +
      'Setup did not delete or change any existing content. Close Scribe, then choose whether to back up the program directory, uninstall the previous version, or remove the unexpected content yourself before retrying. Do not delete Scribe app-data settings, history, downloaded models, imported GGUF files, or external sentinels.';
  end;
end;
