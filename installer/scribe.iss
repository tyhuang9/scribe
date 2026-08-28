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
DefaultDirName={localappdata}\Programs\Scribe
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

function ResolveAppId(Param: String): String;
var
  Token: String;
begin
  Token := VerificationToken();
  Result := '{' + '{#StableAppIdGuid}' + '}';
  if Token <> '' then
    Result := Result + '.verification.' + Token;
end;
