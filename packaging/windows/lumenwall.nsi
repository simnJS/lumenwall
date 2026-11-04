; Lumenwall installer for Windows: NSIS 3 (zlib licence), per user, no administrator rights.
;
; Built from the release zip by packaging/windows/build-installer.sh (release workflow):
;   makensis -INPUTCHARSET UTF8 -DVERSION=0.2.0 -DVERSION_NUMERIC=0.2.0.0 \
;            -DSRCDIR=<extracted zip folder> [-DOUTFILE=<setup.exe>] packaging/windows/lumenwall.nsi
; The output is Lumenwall-Setup.exe unless -DOUTFILE says otherwise.
;
; Installs the whole zip into %LOCALAPPDATA%\Programs\Lumenwall (the same folder and
; files the app's installation dialog uses, writable by the user so the built-in
; updater keeps working: lumenwall.bundle marks it), a Start menu shortcut, optionally
; the start with Windows (HKCU Run value "Lumenwall", as the app's Settings toggle) and
; a desktop shortcut, and an entry in Settings > Apps. Installing over an earlier
; version stops its engine, moves programs still running aside (<name>.lumenwall-old,
; removed by the app at its next start, as after an in-app update) and starts the
; engine again. The uninstaller removes the folder, both shortcuts, the Run value and
; the Apps entry; settings (%APPDATA%\Lumenwall) are kept.
;
; One window: the app icon, the version, "Start with Windows" (ticked) and "Desktop
; shortcut" (unticked), and an Install button. Over an installation the boxes show
; its current state and the button says Update. The progress bar follows in the same
; window, which then closes and opens Lumenwall. The folder is shown, not chosen: the
; app's Settings > Installation and its updater expect this one. /S installs with the
; same defaults (over an installation: its current state) and opens nothing.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma

!ifndef VERSION
  !error "-DVERSION=<x.y.z> is required"
!endif
!ifndef VERSION_NUMERIC
  !error "-DVERSION_NUMERIC=<x.y.z.0> is required"
!endif
!ifndef SRCDIR
  !error "-DSRCDIR=<folder of the extracted release zip> is required"
!endif
!ifndef OUTFILE
  !define OUTFILE "Lumenwall-Setup.exe"
!endif

!define APP "Lumenwall"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Lumenwall"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define HOMEPAGE "https://github.com/simnJS/lumenwall"
; Compile-time paths use "\": makensis on Windows finds no file with "/" in File and
; ReserveFile, and makensis on Linux turns "\" into "/". Relative ones start from this
; folder (makensis works in the script's folder).
!define BRAND_ICON "..\..\assets\brand\icon\lumenwall.ico"
; docs/BRAND.md, light theme: background, Ink, muted text.
!define COLOR_BG "FFFFFF"
!define COLOR_TEXT "16171C"
!define COLOR_MUTED "4D5060"

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"
!include "FileFunc.nsh"
!include "nsDialogs.nsh"

Name "${APP}"
OutFile "${OUTFILE}"
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\Lumenwall"
InstallDirRegKey HKCU "${UNINST_KEY}" "InstallLocation"
BrandingText "${APP} ${VERSION}"
SetFont "Segoe UI" 9
ShowInstDetails nevershow
ShowUninstDetails show
; The window closes when the installation succeeds; Lumenwall opens instead.
AutoCloseWindow true

Var Upgrade ; 1: Lumenwall is already in $INSTDIR
Var InstalledVersion ; its version in Settings > Apps, when known
Var Autostart ; 1: the Run value
Var DesktopLink ; 1: the desktop shortcut
Var EngineWasRunning
Var Options.Icon
Var Options.Autostart
Var Options.Desktop
Var Options.TitleFont

; Read before the programs, from the start of the solid block.
ReserveFile "${BRAND_ICON}"
ReserveFile /plugin nsDialogs.dll
ReserveFile /plugin System.dll

; ------------------------------------------------------------------ pages

!define MUI_ICON "${BRAND_ICON}"
!define MUI_UNICON "${BRAND_ICON}"

Page custom OptionsPage OptionsPageLeave
!define MUI_PAGE_CUSTOMFUNCTION_SHOW InstFilesShow
!insertmacro MUI_PAGE_INSTFILES

!define MUI_UNCONFIRMPAGE_TEXT_TOP "$(UnConfirmText)"
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; The first one is used when Windows is in neither language.
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "French"

LangString InstallButton ${LANG_ENGLISH} "Install"
LangString InstallButton ${LANG_FRENCH} "Installer"
LangString UpdateButton ${LANG_ENGLISH} "Update"
LangString UpdateButton ${LANG_FRENCH} "Mettre à jour"
LangString VersionText ${LANG_ENGLISH} "Version ${VERSION}"
LangString VersionText ${LANG_FRENCH} "Version ${VERSION}"
LangString VersionOverText ${LANG_ENGLISH} "Version ${VERSION} (installed: $InstalledVersion)"
LangString VersionOverText ${LANG_FRENCH} "Version ${VERSION} (installée : $InstalledVersion)"
LangString AutostartText ${LANG_ENGLISH} "Start with Windows"
LangString AutostartText ${LANG_FRENCH} "Démarrer avec Windows"
LangString DesktopText ${LANG_ENGLISH} "Desktop shortcut"
LangString DesktopText ${LANG_FRENCH} "Raccourci sur le bureau"
LangString FolderText ${LANG_ENGLISH} "Folder: $INSTDIR"
LangString FolderText ${LANG_FRENCH} "Dossier : $INSTDIR"
LangString InstallingTitle ${LANG_ENGLISH} "Installing Lumenwall"
LangString InstallingTitle ${LANG_FRENCH} "Installation de Lumenwall"
LangString UpdatingTitle ${LANG_ENGLISH} "Updating Lumenwall"
LangString UpdatingTitle ${LANG_FRENCH} "Mise à jour de Lumenwall"
LangString OpensAfterText ${LANG_ENGLISH} "Lumenwall opens as soon as it is done."
LangString OpensAfterText ${LANG_FRENCH} "Lumenwall s'ouvre dès que c'est terminé."
LangString StoppingEngine ${LANG_ENGLISH} "Stopping the engine..."
LangString StoppingEngine ${LANG_FRENCH} "Arrêt du moteur..."
LangString StartingEngine ${LANG_ENGLISH} "Starting the engine again..."
LangString StartingEngine ${LANG_FRENCH} "Redémarrage du moteur..."
LangString UnConfirmText ${LANG_ENGLISH} "Lumenwall will be removed from your account. Your settings and wallpapers are kept."
LangString UnConfirmText ${LANG_FRENCH} "Lumenwall va être retiré de votre compte. Vos réglages et fonds d'écran sont conservés."
LangString Need64 ${LANG_ENGLISH} "Lumenwall needs a 64-bit (x64) version of Windows."
LangString Need64 ${LANG_FRENCH} "Lumenwall nécessite une version 64 bits (x64) de Windows."

VIProductVersion "${VERSION_NUMERIC}"
VIFileVersion "${VERSION_NUMERIC}"
!macro VersionKeys LANG
  VIAddVersionKey /LANG=${LANG} "ProductName" "${APP}"
  VIAddVersionKey /LANG=${LANG} "ProductVersion" "${VERSION}"
  VIAddVersionKey /LANG=${LANG} "FileVersion" "${VERSION}"
  VIAddVersionKey /LANG=${LANG} "FileDescription" "${APP} ${VERSION} setup"
  VIAddVersionKey /LANG=${LANG} "CompanyName" "${APP} project"
  VIAddVersionKey /LANG=${LANG} "LegalCopyright" "${APP} contributors, MIT or Apache-2.0"
!macroend
!insertmacro VersionKeys ${LANG_ENGLISH}
!insertmacro VersionKeys ${LANG_FRENCH}

; ------------------------------------------------------------------ the window

; Like MUI's welcome page: the page covers the header, on a white background.
Function FullWindowOn
  LockWindow on
  ShowWindow $mui.Branding.Background ${SW_HIDE}
  ShowWindow $mui.Branding.Text ${SW_HIDE}
  ShowWindow $mui.Header.Text ${SW_HIDE}
  ShowWindow $mui.Header.SubText ${SW_HIDE}
  ShowWindow $mui.Header.Image ${SW_HIDE}
  ShowWindow $mui.Line.Standard ${SW_HIDE}
  ShowWindow $mui.Line.FullWindow ${SW_NORMAL}
  LockWindow off
FunctionEnd

Function FullWindowOff
  LockWindow on
  ShowWindow $mui.Branding.Background ${SW_NORMAL}
  ShowWindow $mui.Branding.Text ${SW_NORMAL}
  ShowWindow $mui.Header.Text ${SW_NORMAL}
  ShowWindow $mui.Header.SubText ${SW_NORMAL}
  ShowWindow $mui.Header.Image ${SW_NORMAL}
  ShowWindow $mui.Line.Standard ${SW_NORMAL}
  ShowWindow $mui.Line.FullWindow ${SW_HIDE}
  LockWindow off
FunctionEnd

Function OptionsPage
  InitPluginsDir
  File "/oname=$PLUGINSDIR\lumenwall.ico" "${BRAND_ICON}"
  nsDialogs::Create 1044
  Pop $0
  ${If} $0 == error
    Abort
  ${EndIf}
  SetCtlColors $0 "" "${COLOR_BG}"

  ; The app icon, 64 px at 100 %: loaded at the screen's scale so it stays sharp.
  System::Call 'user32::GetDC(p 0)p.r1'
  System::Call 'gdi32::GetDeviceCaps(p r1, i 90)i.r2' ; LOGPIXELSY
  System::Call 'user32::ReleaseDC(p 0, p r1)'
  IntOp $3 $2 * 64
  IntOp $3 $3 / 96
  ${NSD_CreateIcon} 28u 40u $3 $3 ""
  Pop $1
  SetCtlColors $1 "" "${COLOR_BG}"
  System::Call 'user32::LoadImage(p 0, t "$PLUGINSDIR\lumenwall.ico", i ${IMAGE_ICON}, i r3, i r3, i ${LR_LOADFROMFILE})p.s'
  Pop $Options.Icon
  SendMessage $1 ${STM_SETICON} $Options.Icon 0

  ${NSD_CreateLabel} 76u 41u 230u 18u "${APP}"
  Pop $1
  CreateFont $Options.TitleFont "$(^Font)" 16 600
  SendMessage $1 ${WM_SETFONT} $Options.TitleFont 0
  SetCtlColors $1 "${COLOR_TEXT}" "${COLOR_BG}"

  ${If} $InstalledVersion != ""
    ${NSD_CreateLabel} 76u 61u 230u 10u "$(VersionOverText)"
  ${Else}
    ${NSD_CreateLabel} 76u 61u 230u 10u "$(VersionText)"
  ${EndIf}
  Pop $1
  SetCtlColors $1 "${COLOR_MUTED}" "${COLOR_BG}"

  ${NSD_CreateCheckbox} 76u 92u 230u 12u "$(AutostartText)"
  Pop $Options.Autostart
  SetCtlColors $Options.Autostart "" "${COLOR_BG}"
  ${If} $Autostart == 1
    ${NSD_Check} $Options.Autostart
  ${EndIf}
  ${NSD_CreateCheckbox} 76u 108u 230u 12u "$(DesktopText)"
  Pop $Options.Desktop
  SetCtlColors $Options.Desktop "" "${COLOR_BG}"
  ${If} $DesktopLink == 1
    ${NSD_Check} $Options.Desktop
  ${EndIf}

  ; A long path loses its middle, not its end.
  nsDialogs::CreateControl STATIC ${DEFAULT_STYLES}|${SS_NOPREFIX}|${SS_PATHELLIPSIS} 0 76u 134u 230u 10u "$(FolderText)"
  Pop $1
  SetCtlColors $1 "${COLOR_MUTED}" "${COLOR_BG}"

  Call FullWindowOn
  ShowWindow $mui.Button.Back ${SW_HIDE}
  ${If} $Upgrade == 1
    SendMessage $mui.Button.Next ${WM_SETTEXT} 0 "STR:$(UpdateButton)"
  ${Else}
    SendMessage $mui.Button.Next ${WM_SETTEXT} 0 "STR:$(InstallButton)"
  ${EndIf}
  nsDialogs::Show
  Call FullWindowOff
  ${NSD_FreeIcon} $Options.Icon
FunctionEnd

Function OptionsPageLeave
  ${NSD_GetState} $Options.Autostart $0
  ${If} $0 == ${BST_CHECKED}
    StrCpy $Autostart 1
  ${Else}
    StrCpy $Autostart 0
  ${EndIf}
  ${NSD_GetState} $Options.Desktop $0
  ${If} $0 == ${BST_CHECKED}
    StrCpy $DesktopLink 1
  ${Else}
    StrCpy $DesktopLink 0
  ${EndIf}
FunctionEnd

Function InstFilesShow
  ShowWindow $mui.Button.Back ${SW_HIDE}
  ${If} $Upgrade == 1
    !insertmacro MUI_HEADER_TEXT "$(UpdatingTitle)" "$(OpensAfterText)"
  ${Else}
    !insertmacro MUI_HEADER_TEXT "$(InstallingTitle)" "$(OpensAfterText)"
  ${EndIf}
FunctionEnd

; ------------------------------------------------------------------ install

; Starts a program (on the stack) with the user's normal rights. This installer asks
; for none, but if it was run as administrator anyway, Exec would pass that on: then
; Explorer starts it, as a double-click would.
Function StartAsUser
  Exch $0
  Push $1
  System::Call 'shell32::IsUserAnAdmin()i.r1'
  ${If} $1 <> 0
    Exec '"$WINDIR\explorer.exe" "$0"'
  ${Else}
    Exec '"$0"'
  ${EndIf}
  Pop $1
  Pop $0
FunctionEnd

; A program still running (the tray, the window) cannot be overwritten but can be
; renamed: moved aside, it is deleted by the app at its next start.
!macro MoveAside NAME
  Delete "$INSTDIR\${NAME}.lumenwall-old"
  ${If} ${FileExists} "$INSTDIR\${NAME}"
    ClearErrors
    Delete "$INSTDIR\${NAME}"
    ${If} ${Errors}
      Rename "$INSTDIR\${NAME}" "$INSTDIR\${NAME}.lumenwall-old"
    ${EndIf}
  ${EndIf}
!macroend

Section "-Lumenwall"
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  StrCpy $EngineWasRunning 0
  ${If} ${FileExists} "$INSTDIR\lumenwall-ui.exe"
    DetailPrint "$(StoppingEngine)"
    ; Exit 0: an engine was running and has stopped (3: none was running).
    StrCpy $0 ""
    ExecWait '"$INSTDIR\lumenwall-ui.exe" --stop-engine' $0
    ${If} $0 == 0
      StrCpy $EngineWasRunning 1
    ${EndIf}
  ${EndIf}
  !insertmacro MoveAside "lumenwall.exe"
  !insertmacro MoveAside "lumenwall-ui.exe"
  !insertmacro MoveAside "lumenwall-web-host.exe"
  File /r "${SRCDIR}\*"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  ; Back first, so that it answers by the time the window opens.
  ${If} $EngineWasRunning == 1
    DetailPrint "$(StartingEngine)"
    Push "$INSTDIR\lumenwall.exe"
    Call StartAsUser
  ${EndIf}

  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "${APP} project"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\lumenwall-ui.exe,0"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegStr HKCU "${UNINST_KEY}" "URLInfoAbout" "${HOMEPAGE}"
  WriteRegStr HKCU "${UNINST_KEY}" "HelpLink" "${HOMEPAGE}/issues"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" $0

  CreateShortcut "$SMPROGRAMS\Lumenwall.lnk" "$INSTDIR\lumenwall-ui.exe" "" "$INSTDIR\lumenwall-ui.exe" 0
  ; Unticked over an earlier installation: removed.
  ${If} $DesktopLink == 1
    CreateShortcut "$DESKTOP\Lumenwall.lnk" "$INSTDIR\lumenwall-ui.exe" "" "$INSTDIR\lumenwall-ui.exe" 0
  ${Else}
    Delete "$DESKTOP\Lumenwall.lnk"
  ${EndIf}
  ${If} $Autostart == 1
    WriteRegStr HKCU "${RUN_KEY}" "Lumenwall" '"$INSTDIR\lumenwall-ui.exe" --background'
  ${Else}
    DeleteRegValue HKCU "${RUN_KEY}" "Lumenwall"
  ${EndIf}
SectionEnd

Function .onInit
  SetShellVarContext current
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "$(Need64)" /SD IDOK
    Abort
  ${EndIf}
  ; A new installation starts with Windows and has no desktop shortcut; over an
  ; earlier one, both keep the user's choice.
  StrCpy $Upgrade 0
  StrCpy $InstalledVersion ""
  StrCpy $Autostart 1
  StrCpy $DesktopLink 0
  ${If} ${FileExists} "$INSTDIR\lumenwall-ui.exe"
    StrCpy $Upgrade 1
    ReadRegStr $InstalledVersion HKCU "${UNINST_KEY}" "DisplayVersion"
    ReadRegStr $0 HKCU "${RUN_KEY}" "Lumenwall"
    ${If} $0 == ""
      StrCpy $Autostart 0
    ${EndIf}
    ${If} ${FileExists} "$DESKTOP\Lumenwall.lnk"
      StrCpy $DesktopLink 1
    ${EndIf}
  ${EndIf}
FunctionEnd

Function .onInstSuccess
  ${IfNot} ${Silent}
    Push "$INSTDIR\lumenwall-ui.exe"
    Call StartAsUser
  ${EndIf}
FunctionEnd

; ------------------------------------------------------------------ uninstall

Section "Uninstall"
  SetShellVarContext current
  ${If} ${FileExists} "$INSTDIR\lumenwall-ui.exe"
    ExecWait '"$INSTDIR\lumenwall-ui.exe" --stop-engine'
  ${EndIf}
  ; The tray icon and the window use the files being removed.
  nsExec::Exec 'taskkill /F /IM lumenwall-ui.exe'
  Pop $0
  Delete "$SMPROGRAMS\Lumenwall.lnk"
  Delete "$DESKTOP\Lumenwall.lnk"
  DeleteRegValue HKCU "${RUN_KEY}" "Lumenwall"
  DeleteRegKey HKCU "${UNINST_KEY}"
  ; The whole folder: in-app updates may have added files since. Only a Lumenwall
  ; folder (marked by lumenwall.bundle) is removed recursively.
  ${If} ${FileExists} "$INSTDIR\lumenwall.bundle"
    RMDir /r "$INSTDIR"
  ${Else}
    Delete "$INSTDIR\lumenwall.exe"
    Delete "$INSTDIR\lumenwall-ui.exe"
    Delete "$INSTDIR\uninstall.exe"
    RMDir "$INSTDIR"
  ${EndIf}
SectionEnd
