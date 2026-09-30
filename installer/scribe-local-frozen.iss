; LOCAL-ONLY frozen-worker installer. This file intentionally has no shared
; AppId, maintenance path, shortcuts, auto-launch, or registration with the
; production Scribe installer.
#define AppName "Scribe LOCAL Frozen Test"
#define AppPublisher "Scribe local test"
#define AppExeName "local-transcriber.exe"
#define LocalFrozenAppIdGuid "0A4FB857-7CB8-43F5-98D8-B92D7E1EFD4A"

#ifndef LocalFrozenBundleRoot
  #error LocalFrozenBundleRoot must name a validated staging payload.
#endif
#ifndef LocalFrozenInstallerOutputRoot
  #error LocalFrozenInstallerOutputRoot must name a validated staging output directory.
#endif
#ifndef LocalFrozenTestToken
  #error LocalFrozenTestToken must be a generated lower-case hexadecimal token.
#endif
#ifndef AppVersion
  #error AppVersion must be provided by the local frozen installer builder.
#endif

[Setup]
AppId={code:ResolveLocalFrozenAppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={code:ResolveLocalFrozenDefaultDir}
DisableDirPage=yes
DisableProgramGroupPage=yes
UsePreviousAppDir=no
UsePreviousGroup=no
UsePreviousLanguage=no
UsePreviousSetupType=no
UsePreviousUserInfo=no
CloseApplications=no
RestartApplications=no
CreateUninstallRegKey=no
Uninstallable=yes
OutputDir={#LocalFrozenInstallerOutputRoot}
OutputBaseFilename=Scribe-LOCAL-Frozen-Test-{#LocalFrozenTestToken}
Compression=lzma2
SolidCompression=yes
SetupArchitecture=x86
ArchitecturesInstallIn64BitMode=x64compatible
ArchitecturesAllowed=x64compatible
PrivilegesRequired=lowest
WizardStyle=modern
DisableReadyPage=yes

[Files]
Source: "{#LocalFrozenBundleRoot}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Code]
const
  InvalidFileAttributes = $FFFFFFFF;
  FileAttributeReparsePoint = $00000400;
  ErrorFileNotFound = 2;
  ErrorPathNotFound = 3;

function GetFileAttributesW(FileName: String): LongWord;
  external 'GetFileAttributesW@kernel32.dll stdcall';

function IsLowerHexToken(Value: String): Boolean;
var
  Index: Integer;
  Character: Char;
begin
  Result := Length(Value) = 32;
  if not Result then
    Exit;
  for Index := 1 to Length(Value) do begin
    Character := Value[Index];
    if not (((Character >= '0') and (Character <= '9')) or
            ((Character >= 'a') and (Character <= 'f'))) then begin
      Result := False;
      Exit;
    end;
  end;
end;

function LocalFrozenInstallDir(): String;
begin
  Result := ExpandConstant('{localappdata}\Scribe\LOCAL-Frozen-Test\{#LocalFrozenTestToken}');
end;

function ResolveLocalFrozenDefaultDir(Param: String): String;
begin
  Result := LocalFrozenInstallDir();
end;

function ResolveLocalFrozenAppId(Param: String): String;
begin
  Result := '{' + '{#LocalFrozenAppIdGuid}' + '}.local-frozen.{#LocalFrozenTestToken}';
end;

function HasNoReparseAncestors(Path: String; var ErrorText: String): Boolean;
var
  CurrentPath: String;
  ParentPath: String;
  Attributes: LongWord;
  LastError: Integer;
begin
  Result := False;
  CurrentPath := RemoveBackslashUnlessRoot(ExpandFileName(Path));
  while CurrentPath <> '' do begin
    Attributes := GetFileAttributesW(CurrentPath);
    if Attributes = InvalidFileAttributes then begin
      LastError := DLLGetLastError;
      if (LastError <> ErrorFileNotFound) and (LastError <> ErrorPathNotFound) then begin
        ErrorText := 'Scribe LOCAL Frozen Test could not inspect a destination ancestor.';
        Exit;
      end;
    end
    else begin
      if (Attributes and FileAttributeReparsePoint) <> 0 then begin
        ErrorText := 'Scribe LOCAL Frozen Test refused a destination through a symbolic link or reparse point.';
        Exit;
      end;
    end;
    ParentPath := ExtractFileDir(CurrentPath);
    if (ParentPath = '') or (CompareText(ParentPath, CurrentPath) = 0) then begin
      Result := True;
      Exit;
    end;
    CurrentPath := ParentPath;
  end;
end;

function RejectUnsafeLocalFrozenDestination(): String;
var
  ExpectedPath: String;
begin
  Result := '';
  if ExpandConstant('{param:DIR|}') <> '' then begin
    Result := 'Scribe LOCAL Frozen Test does not accept a /DIR override.';
    Exit;
  end;
  ExpectedPath := RemoveBackslashUnlessRoot(ExpandFileName(LocalFrozenInstallDir()));
  if not HasNoReparseAncestors(ExpectedPath, Result) then
    Exit;
  if DirExists(ExpectedPath) or FileExists(ExpectedPath) then
    Result := 'Scribe LOCAL Frozen Test refused an existing local test installation. It will not update, repair, or overwrite it.';
end;

function RejectLocalFrozenWizardDestination(): String;
var
  ExpectedPath: String;
  ActualPath: String;
begin
  Result := RejectUnsafeLocalFrozenDestination();
  if Result <> '' then
    Exit;
  ExpectedPath := RemoveBackslashUnlessRoot(ExpandFileName(LocalFrozenInstallDir()));
  ActualPath := RemoveBackslashUnlessRoot(ExpandFileName(WizardDirValue));
  if CompareText(ExpectedPath, ActualPath) <> 0 then
    Result := 'Scribe LOCAL Frozen Test refused an unexpected install location.';
end;

function InitializeSetup(): Boolean;
var
  ErrorText: String;
begin
  Result := False;
  if not IsLowerHexToken('{#LocalFrozenTestToken}') then begin
    SuppressibleMsgBox('Scribe LOCAL Frozen Test has an invalid compiled test token.', mbError, MB_OK, IDOK);
    Exit;
  end;
  ErrorText := RejectUnsafeLocalFrozenDestination();
  if ErrorText <> '' then begin
    SuppressibleMsgBox(ErrorText, mbError, MB_OK, IDOK);
    Exit;
  end;
  Result := True;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := RejectLocalFrozenWizardDestination();
end;
