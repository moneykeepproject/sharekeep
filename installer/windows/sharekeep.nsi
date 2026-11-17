; ShareKeep's Windows setup: the app, and (optionally) the household hub as a Windows service.
;
; Built by `cargo xtask installer` (docs/specs/installers.md), which builds the programs, puts
; them with the licence files in dist\ShareKeep-<version>, and calls makensis with:
;   -DVERSION=0.1.0-alpha.1     the version (semver)
;   -DVERSION_QUAD=0.1.0.1      four numbers for Windows file versions
;   -DVERSION_ORDER=0.1.0.1.1   dotted numbers that compare in semver order
;                               (major.minor.patch.stage.number; stage 1 alpha, 2 beta, 3 rc,
;                               9 a release), for the upgrade check
;   -DSOURCE_DIR=<folder>       the programs, licence.txt, THIRD-PARTY-LICENSES.txt, icon.ico
;   -DOUT_FILE=<file>           dist\sharekeep-<version>-windows-x64-setup.exe (the name
;                               MoneyKeep Launcher installs)
;
; Modelled on MoneyKeep's setup (installer/moneykeep.nsi), with ShareKeep's hub in place of the
; capture server. Written from the NSIS documentation (MUI2, LogicLib, x64.nsh, FileFunc.nsh,
; WordFunc.nsh). No PowerShell anywhere: sc.exe, net.exe, tasklist.exe and taskkill.exe, and the
; hub program's own `setup`, `install` and `uninstall` (which use sc.exe, icacls.exe, netsh.exe).
;
; What it does:
; - per-machine install in C:\Program Files\ShareKeep, with administrator permission (the hub
;   runs as a service);
; - pages: welcome (says which version is installed or updated), licence, options, folder
;   (skipped when updating), progress, finish (no "Open ShareKeep": Setup runs as administrator,
;   and ShareKeep should not);
; - options: Start menu shortcut (on), Desktop shortcut (off), and "Household hub on this
;   computer" (off; on when this computer already is the hub, or with /HUB);
; - ShareKeep open: asks to close it (Retry / Cancel); silently: asks it to close, waits up to
;   30 s, then stops with exit code 2;
; - updating: stops the hub service with net stop (which waits) before replacing its program;
;   the hub part then registers and starts it again; an update keeps the hub's certificate, so
;   paired phones and computers keep working;
; - the hub option turned off on a hub computer: the service and its firewall rule are removed;
;   the books stay;
; - registers in Windows "Installed apps" (name, version, publisher, icon, size, location, the
;   uninstaller and its silent form);
; - the uninstaller removes the programs, shortcuts, registration, the service and its firewall
;   rule; the household's books and the hub's data are never removed.
;
; Silent mode (/S, as MoneyKeep Launcher runs it): no pages and no questions; the hub part only on
; a computer that already is the hub, or with /HUB; exit code 0 when done, 2 when ShareKeep stayed
; open, 3 when anything else stopped Setup.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma

!macro REQUIRE name
  !ifndef ${name}
    !error "${name} is not set: run cargo xtask installer (it passes VERSION, VERSION_QUAD, VERSION_ORDER, SOURCE_DIR and OUT_FILE)."
  !endif
!macroend
!insertmacro REQUIRE VERSION
!insertmacro REQUIRE VERSION_QUAD
!insertmacro REQUIRE VERSION_ORDER
!insertmacro REQUIRE SOURCE_DIR
!insertmacro REQUIRE OUT_FILE

; ----- names shared with ShareKeep's code -----------------------------------------------------

!define PRODUCT "ShareKeep"
!define PUBLISHER "MoneyKeep Project"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\ShareKeep"
!define SETTINGS_KEY "Software\ShareKeep"
; sk-hub's service.rs: SERVICE_NAME, and the data folder the app looks for (config.rs)
!define SERVICE "ShareKeepHub"
!define HUB_DATA_DIR "ShareKeep Hub"
!define APP_EXE "sharekeep.exe"
!define HUB_EXE "sharekeep-hub.exe"
!define ICON_FILE "icon.ico"
; silent mode's exit codes besides 0 (MoneyKeep Launcher reads them)
!define EXIT_APP_OPEN 2
!define EXIT_FAILED 3

; ----- the setup program ----------------------------------------------------------------------

Name "${PRODUCT}"
OutFile "${OUT_FILE}"
InstallDir "$PROGRAMFILES64\${PRODUCT}"
RequestExecutionLevel admin
BrandingText "${PRODUCT} ${VERSION} — MoneyKeep family"

VIProductVersion "${VERSION_QUAD}"
VIFileVersion "${VERSION_QUAD}"
VIAddVersionKey "ProductName" "${PRODUCT}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "FileDescription" "${PRODUCT} Setup"
VIAddVersionKey "LegalCopyright" "Copyright (c) 2026 MoneyKeep Project contributors (MIT OR Apache-2.0)"

!include MUI2.nsh
!include LogicLib.nsh
!include Sections.nsh
!include x64.nsh
!include FileFunc.nsh
!include WordFunc.nsh

Var OldVersion      ; the installed version, "" when none
Var OldOrder        ; its VERSION_ORDER, "" when it has none (installed before this setup)
Var WelcomeText
Var HubData         ; %ProgramData%\ShareKeep Hub
Var HubWasInstalled ; 1 when the hub service is registered
Var Haystack        ; Contains: is $Needle in $Haystack? -> $Found (1 or 0)
Var Needle
Var Found

!define MUI_ICON "${SOURCE_DIR}\${ICON_FILE}"
!define MUI_UNICON "${SOURCE_DIR}\${ICON_FILE}"
!define MUI_ABORTWARNING

; ----- pages ----------------------------------------------------------------------------------

!define MUI_WELCOMEPAGE_TITLE "${PRODUCT} ${VERSION}"
!define MUI_WELCOMEPAGE_TEXT "$WelcomeText"
!insertmacro MUI_PAGE_WELCOME

; the licence (NOTICE: MIT OR Apache-2.0); nothing to agree to, so the button is Next
!define MUI_LICENSEPAGE_TEXT_TOP "ShareKeep is free software: use it under the MIT or the Apache 2.0 licence, at your option."
!define MUI_LICENSEPAGE_TEXT_BOTTOM "The full licences are installed with ShareKeep (LICENSE-MIT, LICENSE-APACHE, THIRD-PARTY-LICENSES.txt)."
!define MUI_LICENSEPAGE_BUTTON "$(^NextBtn)"
!insertmacro MUI_PAGE_LICENSE "${SOURCE_DIR}\licence.txt"

; leaving the options page checks that ShareKeep is closed
!define MUI_COMPONENTSPAGE_SMALLDESC
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE CheckAppClosed
!insertmacro MUI_PAGE_COMPONENTS

; the folder; skipped when updating (the update goes where ShareKeep is)
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipFolderOnUpdate
!insertmacro MUI_PAGE_DIRECTORY

!insertmacro MUI_PAGE_INSTFILES

; no "Open ShareKeep" box: Setup runs as administrator, and ShareKeep should not
!define MUI_FINISHPAGE_TEXT "${PRODUCT} ${VERSION} is installed. Open it from the Start menu."
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; ----- helpers (each for Setup and, with the un. prefix, for the uninstaller) -----------------

; $Found = 1 when $Needle occurs in $Haystack, else 0.
!macro CONTAINS_FN un
Function ${un}Contains
  StrCpy $Found 0
  StrLen $R1 $Needle
  StrCpy $R2 0
  ${Do}
    StrCpy $R3 $Haystack $R1 $R2
    ${If} $R3 == ""
      ${Break}
    ${EndIf}
    ${If} $R3 == $Needle
      StrCpy $Found 1
      ${Break}
    ${EndIf}
    IntOp $R2 $R2 + 1
  ${Loop}
FunctionEnd
!macroend
!insertmacro CONTAINS_FN ""
!insertmacro CONTAINS_FN "un."

; ShareKeep open? Ask to close it: Retry checks again, Cancel stops (on a page: stays on it).
!macro APP_CLOSED_FN un
Function ${un}CheckAppClosed
  ${Do}
    nsExec::ExecToStack `"$SYSDIR\tasklist.exe" /FI "IMAGENAME eq ${APP_EXE}" /FO CSV /NH`
    Pop $0
    Pop $Haystack
    StrCpy $Needle "${APP_EXE}"
    Call ${un}Contains
    ${If} $Found != 1
      ${Break}
    ${EndIf}
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "ShareKeep is open. Close it (everything is saved as you work), then press Retry." /SD IDCANCEL IDRETRY retry
    ${If} ${Silent}
      SetErrorLevel ${EXIT_APP_OPEN}
    ${EndIf}
    Abort
    retry:
  ${Loop}
FunctionEnd
!macroend
!insertmacro APP_CLOSED_FN ""
!insertmacro APP_CLOSED_FN "un."

; Silent: ShareKeep open? Ask it to close (taskkill without /F sends its window the close
; request its close button sends) and wait up to 30 s; still open: stop with exit code 2.
!macro SILENT_CLOSE_FN un
Function ${un}SilentCloseApp
  StrCpy $R5 0
  ${Do}
    nsExec::ExecToStack `"$SYSDIR\tasklist.exe" /FI "IMAGENAME eq ${APP_EXE}" /FO CSV /NH`
    Pop $0
    Pop $Haystack
    StrCpy $Needle "${APP_EXE}"
    Call ${un}Contains
    ${If} $Found != 1
      ${Break}
    ${EndIf}
    ${If} $R5 == 0
      nsExec::ExecToStack `"$SYSDIR\taskkill.exe" /IM ${APP_EXE}`
      Pop $0
      Pop $0
    ${EndIf}
    ${If} $R5 >= 30
      SetErrorLevel ${EXIT_APP_OPEN}
      Quit
    ${EndIf}
    Sleep 1000
    IntOp $R5 $R5 + 1
  ${Loop}
FunctionEnd
!macroend
!insertmacro SILENT_CLOSE_FN ""
!insertmacro SILENT_CLOSE_FN "un."

; $HubWasInstalled = 1 when the hub service is registered.
!macro HUB_INSTALLED_FN un
Function ${un}HubInstalled
  StrCpy $HubWasInstalled 0
  ReadRegStr $0 HKLM "SYSTEM\CurrentControlSet\Services\${SERVICE}" "ImagePath"
  ${If} $0 != ""
    StrCpy $HubWasInstalled 1
  ${EndIf}
FunctionEnd
!macroend
!insertmacro HUB_INSTALLED_FN ""
!insertmacro HUB_INSTALLED_FN "un."

; Stop the hub service when it runs (net stop waits until it has stopped), so its program can
; be replaced or removed.
!macro STOP_HUB_FN un
Function ${un}StopHub
  nsExec::ExecToStack `"$SYSDIR\sc.exe" query ${SERVICE}`
  Pop $0
  Pop $Haystack
  StrCpy $Needle "RUNNING"
  Call ${un}Contains
  ${If} $Found == 1
    DetailPrint "Stopping the household hub"
    nsExec::ExecToLog `"$SYSDIR\net.exe" stop ${SERVICE}`
    Pop $0
    Sleep 1000 ; the process lets go of its program file
  ${EndIf}
FunctionEnd
!macroend
!insertmacro STOP_HUB_FN ""
!insertmacro STOP_HUB_FN "un."

; ----- Setup ----------------------------------------------------------------------------------

Section "ShareKeep (required)" SecMain
  SectionIn RO
  Call CheckAppClosed
  Call StopHub

  SetOutPath "$INSTDIR"
  File "${SOURCE_DIR}\${APP_EXE}"
  File "${SOURCE_DIR}\${HUB_EXE}"
  File "${SOURCE_DIR}\${ICON_FILE}"
  File "${SOURCE_DIR}\LICENSE-MIT"
  File "${SOURCE_DIR}\LICENSE-APACHE"
  File "${SOURCE_DIR}\NOTICE"
  ; every crate compiled into the programs with its licence (xtask/src/licences.rs)
  File "${SOURCE_DIR}\THIRD-PARTY-LICENSES.txt"

  ; Windows "Installed apps"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "${SETTINGS_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "VersionOrder" "${VERSION_ORDER}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${ICON_FILE}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "EstimatedSize" "$0"
SectionEnd

Section "Start menu shortcut" SecStartMenu
  SetShellVarContext all
  CreateShortcut "$SMPROGRAMS\${PRODUCT}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${ICON_FILE}" 0
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  SetShellVarContext all
  CreateShortcut "$DESKTOP\${PRODUCT}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${ICON_FILE}" 0
SectionEnd

Section /o "Household hub on this computer" SecHub
  ; a data folder that isn't the hub's own (anything in it owned by someone other than the
  ; administrators, SYSTEM or a service, or a link) is moved aside, never trusted (review H2)
  DetailPrint "Checking the household hub's folder"
  nsExec::ExecToLog `"$INSTDIR\${HUB_EXE}" prepare --data "$HubData"`
  Pop $1
  ${If} $1 != 0
    MessageBox MB_OK|MB_ICONEXCLAMATION "The household hub's folder ($HubData) wasn't made by ShareKeep's setup and can't be moved aside. The details are in the list above. Move or delete that folder, then run this setup again." /SD IDOK
    ${If} ${Silent}
      SetErrorLevel ${EXIT_FAILED}
      Quit
    ${EndIf}
    Goto hubdone
  ${EndIf}
  ; first time only: an existing hub keeps its certificate (devices stay paired) and its books
  ${IfNot} ${FileExists} "$HubData\hub.json"
    DetailPrint "Setting up the household hub"
    nsExec::ExecToLog `"$INSTDIR\${HUB_EXE}" setup --data "$HubData" --address auto`
    Pop $1
    ${If} $1 != 0
      MessageBox MB_OK|MB_ICONEXCLAMATION "The household hub couldn't be set up: this computer doesn't seem to be on the home network. Connect it to the home Wi-Fi or network, then run this setup again." /SD IDOK
      ${If} ${Silent}
        SetErrorLevel ${EXIT_FAILED}
        Quit
      ${EndIf}
      Goto hubdone
    ${EndIf}
  ${EndIf}
  ReadEnvStr $2 "USERDOMAIN"
  ReadEnvStr $3 "USERNAME"
  DetailPrint "Starting the household hub service"
  nsExec::ExecToLog `"$INSTDIR\${HUB_EXE}" install --data "$HubData" --user "$2\$3"`
  Pop $1
  ${If} $1 != 0
    MessageBox MB_OK|MB_ICONEXCLAMATION "The household hub service couldn't be started. The details are in the list above; $HubData\hub.log may say more." /SD IDOK
    ${If} ${Silent}
      SetErrorLevel ${EXIT_FAILED}
      Quit
    ${EndIf}
  ${EndIf}
  WriteRegStr HKLM "${SETTINGS_KEY}" "HubData" "$HubData"
  hubdone:
SectionEnd

; The hub option turned off on a computer that was the hub: its service and firewall rule go;
; the books and the hub's data stay.
Section "-Hub turned off"
  ${IfNot} ${SectionIsSelected} ${SecHub}
  ${AndIf} $HubWasInstalled == 1
    DetailPrint "Removing the household hub service (the books stay in $HubData)"
    nsExec::ExecToLog `"$INSTDIR\${HUB_EXE}" uninstall --data "$HubData"`
    Pop $0
    DeleteRegValue HKLM "${SETTINGS_KEY}" "HubData"
  ${EndIf}
SectionEnd

; An update with a shortcut turned off removes the one an earlier version made. A silent update
; chose nothing, so it leaves the shortcuts that are there alone.
Section "-Shortcuts turned off"
  ${IfNot} ${Silent}
    SetShellVarContext all
    ${IfNot} ${SectionIsSelected} ${SecStartMenu}
      Delete "$SMPROGRAMS\${PRODUCT}.lnk"
    ${EndIf}
    ${IfNot} ${SectionIsSelected} ${SecDesktop}
      Delete "$DESKTOP\${PRODUCT}.lnk"
    ${EndIf}
  ${EndIf}
SectionEnd

; the last section: Setup got this far, so it succeeded (silent mode's exit code 0)
Section "-Done"
  SetErrorLevel 0
SectionEnd

LangString DESC_Main ${LANG_ENGLISH} "The ShareKeep app: your household's shared money."
LangString DESC_StartMenu ${LANG_ENGLISH} "ShareKeep in the Start menu."
LangString DESC_Desktop ${LANG_ENGLISH} "A ShareKeep icon on the desktop."
LangString DESC_Hub ${LANG_ENGLISH} "Choose this on the one computer that keeps the household's books. It runs in the background, and the household's phones and computers connect to it over the home Wi-Fi. Give this computer a fixed address on your router first."
!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecMain} $(DESC_Main)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecStartMenu} $(DESC_StartMenu)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} $(DESC_Desktop)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecHub} $(DESC_Hub)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Function .onInit
  ; silent: anything that stops Setup before it is done exits 3 (the last section sets 0)
  ${If} ${Silent}
    SetErrorLevel ${EXIT_FAILED}
  ${EndIf}
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "ShareKeep needs 64-bit Windows." /SD IDOK
    Abort
  ${EndIf}
  ; the 64-bit registry and System32 (sc.exe, net.exe, tasklist.exe)
  SetRegView 64
  ${DisableX64FSRedirection}
  ReadEnvStr $0 "ProgramData"
  StrCpy $HubData "$0\${HUB_DATA_DIR}"
  ReadRegStr $OldVersion HKLM "${UNINSTALL_KEY}" "DisplayVersion"
  ReadRegStr $OldOrder HKLM "${UNINSTALL_KEY}" "VersionOrder"
  ReadRegStr $0 HKLM "${UNINSTALL_KEY}" "InstallLocation"
  ${If} $0 != ""
    StrCpy $INSTDIR $0
  ${EndIf}
  ; a computer that is the hub keeps the hub ticked; /HUB ticks it too (silent installs)
  Call HubInstalled
  ${If} $HubWasInstalled == 1
    !insertmacro SelectSection ${SecHub}
  ${EndIf}
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/HUB" $R1
  ${IfNot} ${Errors}
    !insertmacro SelectSection ${SecHub}
  ${EndIf}
  StrCpy $1 "Setup needs administrator permission: ShareKeep is installed for all users, and the household hub runs as a service."
  ${If} $OldVersion == ""
    StrCpy $WelcomeText "Setup will install ${PRODUCT} ${VERSION} on this computer.$\r$\n$\r$\n$1"
  ${Else}
    ; semver order: 0 the same, 1 the installed one is newer, 2 this one is newer; a version
    ; from before VersionOrder is older
    StrCpy $2 2
    ${If} $OldVersion == "${VERSION}"
      StrCpy $2 0
    ${ElseIf} $OldOrder != ""
      ${VersionCompare} "$OldOrder" "${VERSION_ORDER}" $2
    ${EndIf}
    ${If} $2 == 0
      StrCpy $WelcomeText "${PRODUCT} ${VERSION} is already installed; Setup will install it again. The household's books and paired devices are kept.$\r$\n$\r$\n$1"
    ${ElseIf} $2 == 1
      StrCpy $WelcomeText "${PRODUCT} $OldVersion is installed, which is newer than ${VERSION}. Setup will replace it with ${VERSION}. The household's books and paired devices are kept.$\r$\n$\r$\n$1"
    ${Else}
      StrCpy $WelcomeText "${PRODUCT} $OldVersion will be updated to ${VERSION}. The household's books and paired devices are kept.$\r$\n$\r$\n$1"
    ${EndIf}
  ${EndIf}
  ${If} ${Silent}
    Call SilentCloseApp
  ${EndIf}
FunctionEnd

Function SkipFolderOnUpdate
  ${If} $OldVersion != ""
    Abort ; skips the page
  ${EndIf}
FunctionEnd

; ----- the uninstaller ------------------------------------------------------------------------

Function un.onInit
  ${If} ${Silent}
    SetErrorLevel ${EXIT_FAILED}
  ${EndIf}
  SetRegView 64
  ${DisableX64FSRedirection}
  ReadEnvStr $0 "ProgramData"
  StrCpy $HubData "$0\${HUB_DATA_DIR}"
  ${If} ${Silent}
    Call un.SilentCloseApp
  ${EndIf}
FunctionEnd

Section "Uninstall"
  Call un.CheckAppClosed

  ; the hub service and its firewall rule, when this computer is the hub
  Call un.HubInstalled
  ${If} $HubWasInstalled == 1
    Call un.StopHub
    DetailPrint "Removing the household hub service"
    nsExec::ExecToLog `"$INSTDIR\${HUB_EXE}" uninstall --data "$HubData"`
    Pop $0
  ${EndIf}

  ; only the files Setup installed: the household's books are never in this list
  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\${HUB_EXE}"
  Delete "$INSTDIR\${ICON_FILE}"
  Delete "$INSTDIR\LICENSE-MIT"
  Delete "$INSTDIR\LICENSE-APACHE"
  Delete "$INSTDIR\NOTICE"
  Delete "$INSTDIR\THIRD-PARTY-LICENSES.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  SetShellVarContext all
  Delete "$SMPROGRAMS\${PRODUCT}.lnk"
  Delete "$DESKTOP\${PRODUCT}.lnk"
  DeleteRegKey HKLM "${UNINSTALL_KEY}"
  DeleteRegKey HKLM "${SETTINGS_KEY}"

  ; the household's books are never removed by uninstalling
  ${If} ${FileExists} "$HubData\hub.json"
    MessageBox MB_OK|MB_ICONINFORMATION "The household's books and the hub's data stay in $HubData. Installing ShareKeep again with the household hub carries on with them, and paired devices keep working. Delete that folder yourself only if you no longer need the books." /SD IDOK
  ${EndIf}
  SetErrorLevel 0
SectionEnd
