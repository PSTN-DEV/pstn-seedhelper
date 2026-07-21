#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef AppArch
  #define AppArch "x64"
#endif

#define AppName    "Seed Helper"
#define AppExe     "Seed Helper.exe"
#define AppBinary  "pstn-seedhelper.exe"
#define AppPublisher "PSTN Squad"
#define AppURL     "https://github.com/PSTN-DEV/pstn-seedhelper"

#if AppArch == "x64"
  #define TargetTriple "x86_64-pc-windows-msvc"
#else
  #define TargetTriple "i686-pc-windows-msvc"
#endif

[Setup]
AppId={{F2A1B3C4-D5E6-4F7A-8B9C-0D1E2F3A4B5C}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}
AppUpdatesURL={#AppURL}/releases
DefaultDirName={autopf}\Seed Helper
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
OutputDir=Output
OutputBaseFilename=pstn-seedhelper-{#AppVersion}-setup-{#AppArch}
SetupIconFile=icon.ico
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
#if AppArch == "x64"
ArchitecturesInstallIn64BitMode=x64os
ArchitecturesAllowed=x64os
#endif
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
MinVersion=10.0

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional icons:"; Flags: unchecked

[Files]
Source: "target\{#TargetTriple}\release\{#AppBinary}"; DestDir: "{app}"; DestName: "{#AppExe}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}";    Filename: "{app}\{#AppExe}"
Name: "{group}\Uninstall";     Filename: "{uninstallexe}"
Name: "{commondesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent

[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ConfigDir: String;
begin
  if CurUninstallStep = usPostUninstall then begin
    ConfigDir := ExpandConstant('{userappdata}\.SeedHelper');
    if DirExists(ConfigDir) then
      if MsgBox('Delete configuration and logs?' + #13#10 + ConfigDir, mbConfirmation, MB_YESNO) = IDYES then
        DelTree(ConfigDir, True, True, True);
  end;
end;
