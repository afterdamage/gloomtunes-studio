; Inno Setup script for the Windows installer (Inno Setup 6).
;
;   cargo build --profile dist --locked -p gt-app
;   iscc /DAppVersion=1.0.0 packaging\windows\gloomtunes.iss     ; writes dist\
;
; Installs per user by default (no administrator rights needed); the first page offers an
; all-users install instead. Paths are relative to this file.

#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif
#ifndef Profile
  #define Profile "dist"
#endif

[Setup]
AppId={{6E0B5A43-3C1F-4C7E-9B2E-1A7D5C9F0B21}
AppName=GloomTunes Studio
AppVersion={#AppVersion}
AppVerName=GloomTunes Studio {#AppVersion}
AppPublisher=Vasil Vasilev
AppPublisherURL=https://github.com/afterdamage/gloomtunes-studio
AppSupportURL=https://github.com/afterdamage/gloomtunes-studio/issues
DefaultDirName={autopf}\GloomTunes Studio
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
OutputDir=..\..\dist
OutputBaseFilename=GloomTunes-Studio-{#AppVersion}-x64-setup
SetupIconFile=gloomtunes.ico
UninstallDisplayIcon={app}\gloomtunes.ico
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ChangesAssociations=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "associate"; Description: "Open .gloom projects with GloomTunes Studio"; GroupDescription: "File types:"

[Files]
Source: "..\..\target\{#Profile}\gloomtunes.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "gloomtunes.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\GloomTunes Studio"; Filename: "{app}\gloomtunes.exe"; IconFilename: "{app}\gloomtunes.ico"
Name: "{autodesktop}\GloomTunes Studio"; Filename: "{app}\gloomtunes.exe"; IconFilename: "{app}\gloomtunes.ico"; Tasks: desktopicon

[Registry]
Root: HKA; Subkey: "Software\Classes\.gloom\OpenWithProgids"; ValueType: string; ValueName: "GloomTunes.Project"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\GloomTunes.Project"; ValueType: string; ValueName: ""; ValueData: "GloomTunes Studio project"; Flags: uninsdeletekey; Tasks: associate
Root: HKA; Subkey: "Software\Classes\GloomTunes.Project\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\gloomtunes.ico"; Tasks: associate
Root: HKA; Subkey: "Software\Classes\GloomTunes.Project\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\gloomtunes.exe"" ""%1"""; Tasks: associate

[Run]
Filename: "{app}\gloomtunes.exe"; Description: "{cm:LaunchProgram,GloomTunes Studio}"; Flags: nowait postinstall skipifsilent
