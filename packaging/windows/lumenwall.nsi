; Lumenwall installer for Windows: NSIS 3 (zlib licence), per user, no administrator rights.
;
; Built from the release zip by packaging/windows/build-installer.sh (release workflow):
;   makensis -DVERSION=0.2.0 -DVERSION_NUMERIC=0.2.0.0 -DSRCDIR=<extracted zip folder> \
;            -DOUTFILE=<setup.exe> packaging/windows/lumenwall.nsi
;
; Installs the whole zip into %LOCALAPPDATA%\Programs\Lumenwall (the same folder and
; files the app's installation dialog uses, writable by the user so the built-in
; updater keeps working: lumenwall.bundle marks it), a Start menu shortcut, optionally
; the start with Windows (HKCU Run value "Lumenwall", as the app's Settings toggle),
; and an entry in Settings > Apps. Installing over an earlier version stops its
; engine, moves programs still running aside (<name>.lumenwall-old, removed by the
; app at its next start, as after an in-app update) and starts the engine again.
; The uninstaller removes the folder, the shortcut, the Run value and the Apps entry;
; settings (%APPDATA%\Lumenwall) are kept.

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
  !define OUTFILE "lumenwall-${VERSION}-x86_64-windows-setup.exe"
!endif

!define APP "Lumenwall"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Lumenwall"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define HOMEPAGE "https://github.com/simnJS/lumenwall"

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "Sections.nsh"
!include "x64.nsh"
!include "FileFunc.nsh"

Name "${APP}"
OutFile "${OUTFILE}"
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\Lumenwall"
InstallDirRegKey HKCU "${UNINST_KEY}" "InstallLocation"
BrandingText "${APP} ${VERSION}"
ShowInstDetails show
ShowUninstDetails show

Var EngineWasRunning

; ------------------------------------------------------------------ pages

!define MUI_ICON "${__FILEDIR__}/../../assets/brand/icon/lumenwall.ico"
!define MUI_UNICON "${__FILEDIR__}/../../assets/brand/icon/lumenwall.ico"
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_NODESC

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\lumenwall-ui.exe"
!define MUI_FINISHPAGE_RUN_TEXT "$(RunText)"
!insertmacro MUI_PAGE_FINISH

!define MUI_UNCONFIRMPAGE_TEXT_TOP "$(UnConfirmText)"
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "French"

LangString SecMainName ${LANG_ENGLISH} "Lumenwall (engine and app)"
LangString SecMainName ${LANG_FRENCH} "Lumenwall (moteur et application)"
LangString SecShortcutName ${LANG_ENGLISH} "Start menu shortcut"
LangString SecShortcutName ${LANG_FRENCH} "Raccourci dans le menu Démarrer"
LangString SecAutostartName ${LANG_ENGLISH} "Start with Windows"
LangString SecAutostartName ${LANG_FRENCH} "Démarrer avec Windows"
LangString RunText ${LANG_ENGLISH} "Start Lumenwall"
LangString RunText ${LANG_FRENCH} "Lancer Lumenwall"
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

; ------------------------------------------------------------------ install

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

Section "$(SecMainName)" SecMain
  SectionIn RO
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  StrCpy $EngineWasRunning 0
  ${If} ${FileExists} "$INSTDIR\lumenwall-ui.exe"
    ; Exit 0: an engine was running and has stopped (3: none was running).
    ExecWait '"$INSTDIR\lumenwall-ui.exe" --stop-engine' $0
    ${If} $0 == 0
      StrCpy $EngineWasRunning 1
    ${EndIf}
  ${EndIf}
  !insertmacro MoveAside "lumenwall.exe"
  !insertmacro MoveAside "lumenwall-ui.exe"
  !insertmacro MoveAside "lumenwall-web-host.exe"
  ; Compile-time paths use "/": makensis accepts it on Linux and Windows.
  File /r "${SRCDIR}/*"
  WriteUninstaller "$INSTDIR\uninstall.exe"

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
SectionEnd

Section "$(SecShortcutName)" SecShortcut
  SetShellVarContext current
  CreateShortcut "$SMPROGRAMS\Lumenwall.lnk" "$INSTDIR\lumenwall-ui.exe" "" "$INSTDIR\lumenwall-ui.exe" 0
SectionEnd

Section "$(SecAutostartName)" SecAutostart
  WriteRegStr HKCU "${RUN_KEY}" "Lumenwall" '"$INSTDIR\lumenwall-ui.exe" --background'
SectionEnd

; Unticked over an earlier installation: removed. Then the engine is back if it ran.
Section "-finish"
  SetShellVarContext current
  ${IfNot} ${SectionIsSelected} ${SecShortcut}
    Delete "$SMPROGRAMS\Lumenwall.lnk"
  ${EndIf}
  ${IfNot} ${SectionIsSelected} ${SecAutostart}
    DeleteRegValue HKCU "${RUN_KEY}" "Lumenwall"
  ${EndIf}
  ${If} $EngineWasRunning == 1
    Exec '"$INSTDIR\lumenwall.exe"'
  ${EndIf}
SectionEnd

Function .onInit
  SetShellVarContext current
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "$(Need64)" /SD IDOK
    Abort
  ${EndIf}
  ; Over an earlier installation, keep the user's start-up choice.
  ReadRegStr $0 HKCU "${UNINST_KEY}" "InstallLocation"
  ${If} $0 != ""
    ReadRegStr $1 HKCU "${RUN_KEY}" "Lumenwall"
    ${If} $1 == ""
      !insertmacro UnselectSection ${SecAutostart}
    ${EndIf}
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
