; ShareKeep for Windows: the app, and (optionally) the household hub as a Windows service.
;
;   makensis /DVERSION=0.1.0 /DBIN=..\..\target\release installer\windows\sharekeep.nsi
;
; The hub part runs `sharekeep-hub setup` (first time only: the certificate authority and the
; admin token, in %ProgramData%\ShareKeep Hub) and `sharekeep-hub install`, which registers
; the service under its own virtual account, locks the data folder down, lets the installing
; user read the admin token (for the app's Hub tab) and opens the firewall for the home network.
; No PowerShell anywhere (see sk-hub's service.rs).

Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "Sections.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef BIN
  !define BIN "..\..\target\release"
!endif
; The file name keeps the full version (0.1.0-dev.5); VERSION is the numeric part.
!ifndef FULLVERSION
  !define FULLVERSION "${VERSION}"
!endif

Name "ShareKeep"
; MoneyKeep Launcher finds it by this name (sharekeep-<version>-windows-x64-setup.exe)
OutFile "sharekeep-${FULLVERSION}-windows-x64-setup.exe"
InstallDir "$PROGRAMFILES64\ShareKeep"
InstallDirRegKey HKLM "Software\ShareKeep" "InstallDir"
RequestExecutionLevel admin
SetCompressor /SOLID lzma
BrandingText "ShareKeep ${FULLVERSION} — MoneyKeep family"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "ShareKeep"
VIAddVersionKey "CompanyName" "MoneyKeep Project"
VIAddVersionKey "FileDescription" "ShareKeep setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "MIT OR Apache-2.0"

!define MUI_ICON "..\..\assets\icon.ico"
!define MUI_UNICON "..\..\assets\icon.ico"
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_SMALLDESC
; also run silently (/S) by MoneyKeep Launcher: no questions then, and the hub part only on an
; upgrade of a hub computer
!define MUI_FINISHPAGE_RUN "$INSTDIR\sharekeep.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Open ShareKeep"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Var HubData

Section "ShareKeep" SecApp
  SectionIn RO
  SetOutPath "$INSTDIR"
  ; a running hub holds its program open: stop it before replacing it (upgrade)
  IfFileExists "$INSTDIR\sharekeep-hub.exe" 0 +2
    nsExec::ExecToLog 'sc.exe stop ShareKeepHub'
  Sleep 1500
  File "${BIN}\sharekeep.exe"
  File "${BIN}\sharekeep-hub.exe"
  File "..\..\assets\icon.ico"
  File "..\..\LICENSE-MIT"
  File "..\..\LICENSE-APACHE"
  File "..\..\NOTICE"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  SetShellVarContext all
  CreateShortcut "$SMPROGRAMS\ShareKeep.lnk" "$INSTDIR\sharekeep.exe" "" "$INSTDIR\icon.ico"
  CreateShortcut "$DESKTOP\ShareKeep.lnk" "$INSTDIR\sharekeep.exe" "" "$INSTDIR\icon.ico"
  WriteRegStr HKLM "Software\ShareKeep" "InstallDir" "$INSTDIR"
  !define UNINST "Software\Microsoft\Windows\CurrentVersion\Uninstall\ShareKeep"
  WriteRegStr HKLM "${UNINST}" "DisplayName" "ShareKeep"
  WriteRegStr HKLM "${UNINST}" "DisplayVersion" "${FULLVERSION}"
  WriteRegStr HKLM "${UNINST}" "Publisher" "MoneyKeep Project"
  WriteRegStr HKLM "${UNINST}" "DisplayIcon" "$INSTDIR\icon.ico"
  WriteRegStr HKLM "${UNINST}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKLM "${UNINST}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST}" "NoRepair" 1
SectionEnd

Section /o "Household hub on this computer" SecHub
  ReadEnvStr $0 "ProgramData"
  StrCpy $HubData "$0\ShareKeep Hub"
  ; first time only: an existing hub keeps its certificate (devices stay paired) and books
  ${IfNot} ${FileExists} "$HubData\hub.json"
    DetailPrint "Setting up the hub…"
    nsExec::ExecToLog '"$INSTDIR\sharekeep-hub.exe" setup --data "$HubData" --address auto'
    Pop $1
    ${If} $1 != 0
      MessageBox MB_ICONEXCLAMATION "The hub couldn't be set up: this computer doesn't seem to be on the home network. Connect it and run this setup again." /SD IDOK
      Goto hubdone
    ${EndIf}
  ${EndIf}
  ReadEnvStr $2 "USERDOMAIN"
  ReadEnvStr $3 "USERNAME"
  DetailPrint "Starting the hub service…"
  nsExec::ExecToLog '"$INSTDIR\sharekeep-hub.exe" install --data "$HubData" --user "$2\$3"'
  Pop $1
  ${If} $1 != 0
    MessageBox MB_ICONEXCLAMATION "The hub service couldn't be started. The details are in the list above; $HubData\hub.log may say more." /SD IDOK
  ${EndIf}
  WriteRegStr HKLM "Software\ShareKeep" "HubData" "$HubData"
  hubdone:
SectionEnd

LangString DESC_App ${LANG_ENGLISH} "The ShareKeep app: your household's shared money."
LangString DESC_Hub ${LANG_ENGLISH} "Choose this on the one computer that keeps the household's books. It runs in the background, and the household's phones and computers connect to it over the home Wi-Fi. Give this computer a fixed address on your router first."
!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecApp} $(DESC_App)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecHub} $(DESC_Hub)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Function .onInit
  ; an upgrade of a hub computer keeps the hub ticked
  ReadRegStr $0 HKLM "Software\ShareKeep" "HubData"
  ${If} $0 != ""
    !insertmacro SelectSection ${SecHub}
  ${EndIf}
FunctionEnd

Section "Uninstall"
  IfFileExists "$INSTDIR\sharekeep-hub.exe" 0 +2
    nsExec::ExecToLog '"$INSTDIR\sharekeep-hub.exe" uninstall'
  Delete "$INSTDIR\sharekeep.exe"
  Delete "$INSTDIR\sharekeep-hub.exe"
  Delete "$INSTDIR\icon.ico"
  Delete "$INSTDIR\LICENSE-MIT"
  Delete "$INSTDIR\LICENSE-APACHE"
  Delete "$INSTDIR\NOTICE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  SetShellVarContext all
  Delete "$SMPROGRAMS\ShareKeep.lnk"
  Delete "$DESKTOP\ShareKeep.lnk"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\ShareKeep"
  DeleteRegKey HKLM "Software\ShareKeep"
  ; the household's books are never removed by uninstalling
  ReadEnvStr $0 "ProgramData"
  IfFileExists "$0\ShareKeep Hub\hub.json" 0 +2
    MessageBox MB_ICONINFORMATION "The household's books stay in $0\ShareKeep Hub. Delete that folder yourself if you no longer need them." /SD IDOK
SectionEnd
