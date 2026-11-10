; E7-5: the hooks of LitePack's per-user NSIS installer (docs/notes/e7-design.md section 3), included by
; tauri-bundler 2.10.4's installer.nsi through tauri.nsis.conf.json's installerHooks. Part 1: route C, no sparse
; package (R304). The template already includes x64.nsh, LogicLib (through MUI2), FileFunc, utils.nsh (its
; CheckIfAppIsRunning) and the Restart Manager macros before this file.
;
; This file is included before the template defines its own symbols (PRODUCTNAME, MAINBINARYNAME, ARCH, ...), its
; pages and its sections. Code outside the four NSIS_HOOK_* macros therefore names LitePack's values directly; a test
; (crates/lpk-gui/src/register_tests.rs) keeps them equal to tauri.conf.json, and PREINSTALL fails the build when the
; template's ${ARCH} is not LPK_INSTALLER_ARCH.
;
; The template's installer.nsi sets no ManifestSupportedOS, so NSIS 3.13's default ("all": Vista to Windows 10/11)
; is in the manifest, and ${AtLeastBuild} sees the real build number instead of Windows 8's (E7-5's finding;
; the refusal itself is checked by hand on a Windows 10 VM in E7-10, CI has no Windows 10 runner).

!include WinVer.nsh

!define LPK_INSTALLER_ARCH "x64"
!define LPK_PRODUCT "LitePack"
!define LPK_MAIN_EXE "lpk-gui.exe"
!define LPK_RUNONCE "Software\Microsoft\Windows\CurrentVersion\RunOnce"

; HWND_BROADCAST, WM_SETTINGCHANGE, SMTO_ABORTIFHUNG: tell running programs that HKCU\Environment changed
; (R306), with NSIS's own SendMessage and a 5-second timeout.
!macro LPK_BROADCAST_ENVIRONMENT
  SendMessage 0xFFFF 0x001A 0 "STR:Environment" /TIMEOUT=5000
!macroend

; The refusals: below build 19045, and the x64 installer on an ARM64 PC (an x64 lpk_shell.dll cannot load into ARM64
; Explorer). Each exits with error level 1; `remove` names a folder to remove when empty (never recursively).
!macro LPK_REFUSE remove
  ${IfNot} ${AtLeastBuild} 19045
    MessageBox MB_ICONSTOP|MB_OK "${LPK_PRODUCT} needs Windows 10 version 22H2 (build 19045) or Windows 11." /SD IDOK
    SetErrorLevel 1
    !if "${remove}" != ""
      RMDir "${remove}"
    !endif
    Quit
  ${EndIf}
  !if "${LPK_INSTALLER_ARCH}" == "x64"
    ${If} ${IsNativeARM64}
      MessageBox MB_ICONSTOP|MB_OK "This is the x64 build of ${LPK_PRODUCT}. On this ARM64 PC, install the ARM64 build: its Explorer menu needs an ARM64 shell extension." /SD IDOK
      SetErrorLevel 1
      !if "${remove}" != ""
        RMDir "${remove}"
      !endif
      Quit
    ${EndIf}
  !endif
!macroend

; Interactive installs: the refusals before the first page, so before the reinstall page can run an old uninstaller
; (PageLeaveReinstall). The template defines no GUI-init function of its own.
!define MUI_CUSTOMFUNCTION_GUIINIT LpkGuiInit
Function LpkGuiInit
  !insertmacro LPK_REFUSE ""
FunctionEnd

; Silent installs (no GUI init): the first section. NSIS runs sections in the order they are defined, and this file
; comes before the template's sections, so this runs before the WebView2 section (which may download Microsoft's
; bootstrapper) and before Section Install's SetOutPath creates $INSTDIR.
Section "-LitePack preflight"
  !insertmacro LPK_REFUSE ""
SectionEnd

; A LitePack RunOnce command left by an earlier uninstall or upgrade (a shell DLL Explorer held): a reinstall
; before the next logon cancels them, so none can remove the new installation's files.
!macro LPK_CANCEL_PENDING_RUNONCE
  DeleteRegValue HKCU "${LPK_RUNONCE}" "LitePack uninstall"
  StrCpy $R5 0
  ${Do}
    EnumRegValue $R6 HKCU "${LPK_RUNONCE}" $R5
    ${If} $R6 == ""
      ${Break}
    ${EndIf}
    StrCpy $R7 $R6 15
    ${If} $R7 == "LitePack shell "
      DeleteRegValue HKCU "${LPK_RUNONCE}" $R6
    ${Else}
      IntOp $R5 $R5 + 1
    ${EndIf}
  ${Loop}
!macroend

; The next-logon removal of one shell\<version> folder Explorer still holds, under the RunOnce value `name`: the DLL,
; then the empty folders up to $INSTDIR, each non-recursively (no `rmdir /s`: nothing else is ever deleted).
!macro LPK_RUNONCE_REMOVE_SHELL name version
  WriteRegStr HKCU "${LPK_RUNONCE}" "${name}" \
    '"$SYSDIR\cmd.exe" /d /c del /f /q "$INSTDIR\shell\${version}\lpk_shell.dll" & rmdir "$INSTDIR\shell\${version}" & rmdir "$INSTDIR\shell" & rmdir "$INSTDIR"'
!macroend

; The shell\<version> folders other than this version's: removed when Explorer holds none of their files; one still
; held is removed at the next logon.
!macro LPK_REMOVE_OLD_SHELL_FOLDERS
  FindFirst $0 $1 "$INSTDIR\shell\*"
  ${DoWhile} $1 != ""
    ${If} $1 != "."
    ${AndIf} $1 != ".."
    ${AndIf} $1 != "${VERSION}"
      RMDir /r "$INSTDIR\shell\$1"
      ${If} ${FileExists} "$INSTDIR\shell\$1\*"
        !insertmacro LPK_RUNONCE_REMOVE_SHELL "LitePack shell $1" "$1"
      ${EndIf}
    ${EndIf}
    FindNext $0 $1
  ${Loop}
  FindClose $0
!macroend

; The uninstaller's "close LitePack" check, before PREUNINSTALL changes anything: a cancelled prompt aborts with
; LitePack still installed and registered. The template runs its own check (utils.nsh's CheckIfAppIsRunning) again
; after this hook. That macro cannot be used here: it reads $PassiveMode, which installer.nsi declares only after this
; file (R330), so this is the same Restart Manager check with the passive flag passed in $R9 by PREUNINSTALL, where
; $PassiveMode is declared. Silent and passive uninstalls close LitePack without asking, as the template's does.
Function un.LpkCheckRunning
  !insertmacro RestartManager_StartSession $R0
  ${If} $R0 != ""
    !insertmacro RestartManager_RegisterFile $R0 "$INSTDIR\${LPK_MAIN_EXE}"
    ${If} $0 = 0
      System::Call 'RSTRTMGR::RmGetList(p R0, *i .r1, *i .r2, p 0, *i .r3) i .r0'
      ${If} $0 = ${ERROR_MORE_DATA}
        ${IfNot} ${Silent}
        ${AndIf} $R9 <> 1
          MessageBox MB_OKCANCEL|MB_ICONEXCLAMATION "${LPK_PRODUCT} is running. Select OK to close it and uninstall." IDOK lpk_close
          !insertmacro RestartManager_EndSession $R0
          Abort "${LPK_PRODUCT} is still running; nothing was removed."
        ${EndIf}
        lpk_close:
        System::Call 'RSTRTMGR::RmShutdown(p R0, i ${RmForceShutdown}, p 0) i .r0'
        ${If} $0 <> 0
          !insertmacro RestartManager_EndSession $R0
          Abort "${LPK_PRODUCT} could not be closed; nothing was removed."
        ${EndIf}
      ${EndIf}
    ${EndIf}
    !insertmacro RestartManager_EndSession $R0
  ${EndIf}
FunctionEnd

!macro NSIS_HOOK_PREINSTALL
  !if "${ARCH}" != "${LPK_INSTALLER_ARCH}"
    !error "installer.nsh refuses by LPK_INSTALLER_ARCH (${LPK_INSTALLER_ARCH}), but this build is ${ARCH}"
  !endif
  !if "${MAINBINARYNAME}.exe" != "${LPK_MAIN_EXE}"
    !error "installer.nsh names ${LPK_MAIN_EXE}, the build's main binary is ${MAINBINARYNAME}.exe"
  !endif
  ; Section Install has run SetOutPath $INSTDIR: a refusal removes that folder again when it is empty.
  !insertmacro LPK_REFUSE "$INSTDIR"
  !insertmacro LPK_CANCEL_PENDING_RUNONCE
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Route C, the .lpk association, GuiPath and bin\ on Path (R306), all by the Rust code.
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --register-shell' $0
  ${If} $0 != 0
    DetailPrint "LitePack: --register-shell exited with $0"
    SetErrorLevel 3
  ${EndIf}
  !insertmacro LPK_BROADCAST_ENVIRONMENT
  !insertmacro LPK_REMOVE_OLD_SHELL_FOLDERS
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    StrCpy $R9 $PassiveMode
    Call un.LpkCheckRunning
    ; LitePack's own switch: /DELETEAPPDATA ticks the "delete the application data" box for a silent uninstall.
    ${GetOptions} $CMDLINE "/DELETEAPPDATA" $0
    ${IfNot} ${Errors}
      StrCpy $DeleteAppDataCheckboxState 1
    ${EndIf}
    ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --unregister-shell' $0
    DetailPrint "LitePack: --unregister-shell exited with $0"
    !insertmacro LPK_BROADCAST_ENVIRONMENT
    ExecWait '"$INSTDIR\bin\lpk-worker.exe" --remove-profile' $0
    DetailPrint "LitePack: lpk-worker --remove-profile exited with $0"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    SetShellVarContext current
    ; The local state (journal, WebView2 data, opened entries) always; the settings only with the box.
    RMDir /r "$LOCALAPPDATA\${BUNDLEID}"
    ${If} $DeleteAppDataCheckboxState = 1
      RMDir /r "$APPDATA\${PRODUCTNAME}"
    ${EndIf}
    ; Tauri's bookkeeping, which it keeps without the box: MANUPRODUCTKEY is Software\litepack\LitePack (the publisher
    ; from the identifier), which the registry, comparing names without case, makes Software\LitePack\LitePack, under
    ; LitePack's own Software\LitePack. --unregister-shell left that parent while this key was in it; it goes here,
    ; only when empty.
    DeleteRegValue HKCU "${MANUPRODUCTKEY}" "Installer Language"
    DeleteRegKey HKCU "${MANUPRODUCTKEY}"
    DeleteRegKey /ifempty HKCU "${MANUKEY}"
    ; The jump list Windows keeps for LitePack's AppUserModelID (the identifier): its file is named by the CRC-64
    ; of the upper-cased UTF-16 ID (a233dcd0c3d36a7d for ORG.LITEPACK.GUI; a test in register_tests.rs checks this
    ; against tauri.conf.json), and Windows Search's record of it (CI run 42486016300's diff).
    Delete "$APPDATA\Microsoft\Windows\Recent\AutomaticDestinations\a233dcd0c3d36a7d.automaticDestinations-ms"
    Delete "$APPDATA\Microsoft\Windows\Recent\CustomDestinations\a233dcd0c3d36a7d.customDestinations-ms"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Search\JumplistData" "${BUNDLEID}"
    ; A shell DLL Explorer still holds (this version's, after the template removed the rest): removed at the next
    ; logon, with the then-empty folders, never recursively.
    ${If} ${FileExists} "$INSTDIR\shell\${VERSION}\lpk_shell.dll"
      !insertmacro LPK_RUNONCE_REMOVE_SHELL "LitePack uninstall" "${VERSION}"
    ${EndIf}
  ${EndIf}
!macroend
