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

; $R7 = "%" or "!" when $INSTDIR holds one (cmd would expand it), else "".
!macro LPK_FIND_CMD_CHAR
  StrCpy $R5 0
  StrCpy $R7 ""
  ${Do}
    StrCpy $R6 $INSTDIR 1 $R5
    ${If} $R6 == ""
      ${Break}
    ${EndIf}
    ${If} $R6 == "%"
    ${OrIf} $R6 == "!"
      StrCpy $R7 $R6
      ${Break}
    ${EndIf}
    IntOp $R5 $R5 + 1
  ${Loop}
!macroend

; The refusals: below build 19045, and the x64 installer on an ARM64 PC (an x64 lpk_shell.dll cannot load into ARM64
; Explorer), and (R336) an install folder holding `%` or `!`, which the next-logon cleanup's cmd line would expand.
; Each exits with error level 1; `remove` names a folder to remove when empty (never recursively).
!macro LPK_REFUSE remove
  !insertmacro LPK_FIND_CMD_CHAR
  ${If} $R7 != ""
    MessageBox MB_ICONSTOP|MB_OK "${LPK_PRODUCT} cannot be installed in a folder whose path holds $R7: $INSTDIR" /SD IDOK
    SetErrorLevel 1
    !if "${remove}" != ""
      RMDir "${remove}"
    !endif
    Quit
  ${EndIf}
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

; A LitePack RunOnce command left by an earlier uninstall or upgrade (a shell DLL Explorer held, a set-aside
; executable still running): a reinstall before the next logon cancels them, so none can remove the new
; installation's files.
!macro LPK_CANCEL_PENDING_RUNONCE
  ; Every value named `LitePack *` (`LitePack uninstall`, `LitePack shell <version>`, and any older name).
  StrCpy $R5 0
  ${Do}
    EnumRegValue $R6 HKCU "${LPK_RUNONCE}" $R5
    ${If} $R6 == ""
      ${Break}
    ${EndIf}
    StrCpy $R7 $R6 9
    ${If} $R7 == "LitePack "
      DeleteRegValue HKCU "${LPK_RUNONCE}" $R6
    ${Else}
      IntOp $R5 $R5 + 1
    ${EndIf}
  ${Loop}
!macroend

; R335, R336: Windows runs a Run/RunOnce command line of at most 260 characters, and documents neither the order of
; the values nor that one finishes before the next starts. So each next-logon cleanup is one value whose single cmd
; line runs its steps in sequence: it changes to $INSTDIR once (`&&`: nothing runs when that folder is gone), then
; uses relative paths; deletes first, then non-recursive rmdirs. The design does not depend on the order of values.
; The whole stored string is measured: over 260 characters (a very long install path), no value is written and what
; is left stays until the next install's cleanup. `/v:off`: no delayed expansion of `!`; a path holding `%` or `!` is
; refused before anything is written (LPK_REFUSE), and no value is ever written for one.
!define LPK_CMD '"$SYSDIR\cmd.exe" /d /v:off /c'

; Writes the RunOnce value `name` = `command` when it is at most 260 characters and $INSTDIR holds no `%` or `!`.
!macro LPK_RUNONCE_WRITE name command
  !insertmacro LPK_FIND_CMD_CHAR
  StrCpy $R8 '${command}'
  StrLen $R6 $R8
  ${If} $R6 <= 260
  ${AndIf} $R7 == ""
    WriteRegStr HKCU "${LPK_RUNONCE}" "${name}" $R8
  ${Else}
    DetailPrint "LitePack: no RunOnce command for this install path; what is left stays until the next install"
  ${EndIf}
!macroend

; After an upgrade: an old shell\<version> folder Explorer still holds (the install stays, so only that folder goes).
!macro LPK_RUNONCE_REMOVE_OLD_SHELL version
  !insertmacro LPK_RUNONCE_WRITE "LitePack shell ${version}" \
    '${LPK_CMD} cd /d "$INSTDIR" && (del /f /q shell\${version}\lpk_shell.dll & rmdir shell\${version})'
!macroend

; After an uninstall: this version's shell DLL Explorer still holds and bin\'s set-aside executables still running,
; then the empty folders up to $INSTDIR (named by its last component from its parent), each non-recursively.
!macro LPK_RUNONCE_REMOVE_INSTALL
  ${GetFileName} "$INSTDIR" $R4
  !insertmacro LPK_RUNONCE_WRITE "LitePack uninstall" \
    '${LPK_CMD} cd /d "$INSTDIR" && (del /f /q shell\${VERSION}\lpk_shell.dll bin\lpk*.exe.old-* & rmdir shell\${VERSION} & rmdir shell & rmdir bin & cd .. & rmdir "$R4")'
!macroend

; R334, R336: an upgrade while a bin\ executable runs (an lpk.exe compressing, the workers it started). Windows refuses
; to overwrite a running executable's file, and NSIS's File would then skip it in a silent install (its error box's
; silent default is Ignore), leaving a half-updated bin\. Windows does let a running executable's file be renamed, so
; before any file is written each existing bin\<name> is renamed to <name>.old-<n> (the first free n), recorded in
; its LpkOld* variable. A rename that fails renames the others back and the installer stops with exit code 2 and the
; installed version untouched. The set-aside copies are deleted only in POSTINSTALL, once every new file exists
; (LPK_FINISH_SET_ASIDE); held ones stay until the next install or the uninstall removes them.
Var LpkOld1
Var LpkOld2
Var LpkOld3
Var LpkOld4

!macro LPK_SET_ASIDE name var
  StrCpy ${var} ""
  ${If} $R1 == ""
  ${AndIf} ${FileExists} "$INSTDIR\bin\${name}"
    StrCpy $R2 1
    ${DoWhile} ${FileExists} "$INSTDIR\bin\${name}.old-$R2"
      IntOp $R2 $R2 + 1
    ${Loop}
    ClearErrors
    Rename "$INSTDIR\bin\${name}" "$INSTDIR\bin\${name}.old-$R2"
    ${If} ${Errors}
      StrCpy $R1 "${name}"
    ${Else}
      StrCpy ${var} "${name}.old-$R2"
    ${EndIf}
  ${EndIf}
!macroend

; Puts a set-aside copy back as bin\<name>; a rename that fails is named in $R3.
!macro LPK_PUT_BACK name var
  ${If} ${var} != ""
    ClearErrors
    Rename "$INSTDIR\bin\${var}" "$INSTDIR\bin\${name}"
    ${If} ${Errors}
      StrCpy $R3 "$R3 bin\${name} (still bin\${var})"
    ${Else}
      StrCpy ${var} ""
    ${EndIf}
  ${EndIf}
!macroend

!macro LPK_SET_ASIDE_BIN
  ; Leftovers of an earlier install whose process has exited since.
  Delete "$INSTDIR\bin\lpk*.exe.old-*"
  StrCpy $R1 ""
  StrCpy $R3 ""
  !insertmacro LPK_SET_ASIDE "lpk.exe" $LpkOld1
  !insertmacro LPK_SET_ASIDE "lpk-check.exe" $LpkOld2
  !insertmacro LPK_SET_ASIDE "lpk-worker.exe" $LpkOld3
  !insertmacro LPK_SET_ASIDE "lpk-worker-libarchive.exe" $LpkOld4
  ${If} $R1 != ""
    !insertmacro LPK_PUT_BACK "lpk.exe" $LpkOld1
    !insertmacro LPK_PUT_BACK "lpk-check.exe" $LpkOld2
    !insertmacro LPK_PUT_BACK "lpk-worker.exe" $LpkOld3
    !insertmacro LPK_PUT_BACK "lpk-worker-libarchive.exe" $LpkOld4
    ${If} $R3 == ""
      MessageBox MB_ICONSTOP|MB_OK "${LPK_PRODUCT} could not replace bin\$R1. Close the programs using it, then run the installer again. Nothing was changed." /SD IDOK
    ${Else}
      MessageBox MB_ICONSTOP|MB_OK "${LPK_PRODUCT} could not replace bin\$R1, and could not put back:$R3. Rename those files back by hand, or run the installer again." /SD IDOK
    ${EndIf}
    SetErrorLevel 2
    Quit
  ${EndIf}
!macroend

; POSTINSTALL: when every new bin\ executable exists, the set-aside copies are deleted (a held one stays, reported);
; when one is missing (its File failed), its copy is put back, the others are kept, and the install exits 2 naming it.
!macro LPK_DROP_SET_ASIDE var
  ${If} ${var} != ""
    Delete "$INSTDIR\bin\${var}"
    ${If} ${FileExists} "$INSTDIR\bin\${var}"
      DetailPrint "LitePack: bin\${var} is in use; it is removed by the next install or the uninstall"
    ${EndIf}
  ${EndIf}
!macroend

!macro LPK_MISSING name
  ${IfNot} ${FileExists} "$INSTDIR\bin\${name}"
    StrCpy $R1 "$R1 bin\${name}"
  ${EndIf}
!macroend

!macro LPK_FINISH_SET_ASIDE
  StrCpy $R1 ""
  StrCpy $R3 ""
  !insertmacro LPK_MISSING "lpk.exe"
  !insertmacro LPK_MISSING "lpk-check.exe"
  !insertmacro LPK_MISSING "lpk-worker.exe"
  !insertmacro LPK_MISSING "lpk-worker-libarchive.exe"
  ${If} $R1 == ""
    !insertmacro LPK_DROP_SET_ASIDE $LpkOld1
    !insertmacro LPK_DROP_SET_ASIDE $LpkOld2
    !insertmacro LPK_DROP_SET_ASIDE $LpkOld3
    !insertmacro LPK_DROP_SET_ASIDE $LpkOld4
  ${Else}
    ${IfNot} ${FileExists} "$INSTDIR\bin\lpk.exe"
      !insertmacro LPK_PUT_BACK "lpk.exe" $LpkOld1
    ${EndIf}
    ${IfNot} ${FileExists} "$INSTDIR\bin\lpk-check.exe"
      !insertmacro LPK_PUT_BACK "lpk-check.exe" $LpkOld2
    ${EndIf}
    ${IfNot} ${FileExists} "$INSTDIR\bin\lpk-worker.exe"
      !insertmacro LPK_PUT_BACK "lpk-worker.exe" $LpkOld3
    ${EndIf}
    ${IfNot} ${FileExists} "$INSTDIR\bin\lpk-worker-libarchive.exe"
      !insertmacro LPK_PUT_BACK "lpk-worker-libarchive.exe" $LpkOld4
    ${EndIf}
    ${If} $R3 == ""
      MessageBox MB_ICONSTOP|MB_OK "${LPK_PRODUCT} could not write$R1; the previous version's copies were put back. Run the installer again." /SD IDOK
    ${Else}
      MessageBox MB_ICONSTOP|MB_OK "${LPK_PRODUCT} could not write$R1, and could not put back:$R3. Run the installer again." /SD IDOK
    ${EndIf}
    SetErrorLevel 2
  ${EndIf}
!macroend

; R336: an uninstall while a bin\ executable runs. Each one that cannot be deleted is renamed to <name>.old-<n>, so the
; template's removal finds nothing to skip and the set-aside copy is removed by the `LitePack uninstall` command.
!macro LPK_UNINSTALL_SET_ASIDE name
  Delete "$INSTDIR\bin\${name}"
  ${If} ${FileExists} "$INSTDIR\bin\${name}"
    StrCpy $R2 1
    ${DoWhile} ${FileExists} "$INSTDIR\bin\${name}.old-$R2"
      IntOp $R2 $R2 + 1
    ${Loop}
    Rename "$INSTDIR\bin\${name}" "$INSTDIR\bin\${name}.old-$R2"
  ${EndIf}
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
        !insertmacro LPK_RUNONCE_REMOVE_OLD_SHELL "$1"
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
; The installer runs the same check (LpkCheckRunning) in PREINSTALL, before bin\'s executables are set aside (R334):
; an interactive cancel then stops the install before anything changed, not between the set-aside and the copy.
!macro LPK_CHECK_RUNNING action changed
  !insertmacro RestartManager_StartSession $R0
  ${If} $R0 != ""
    !insertmacro RestartManager_RegisterFile $R0 "$INSTDIR\${LPK_MAIN_EXE}"
    ${If} $0 = 0
      System::Call 'RSTRTMGR::RmGetList(p R0, *i .r1, *i .r2, p 0, *i .r3) i .r0'
      ${If} $0 = ${ERROR_MORE_DATA}
        ${IfNot} ${Silent}
        ${AndIf} $R9 <> 1
          MessageBox MB_OKCANCEL|MB_ICONEXCLAMATION "${LPK_PRODUCT} is running. Select OK to close it and ${action}." IDOK lpk_close
          !insertmacro RestartManager_EndSession $R0
          Abort "${LPK_PRODUCT} is still running; nothing was ${changed}."
        ${EndIf}
        lpk_close:
        System::Call 'RSTRTMGR::RmShutdown(p R0, i ${RmForceShutdown}, p 0) i .r0'
        ${If} $0 <> 0
          !insertmacro RestartManager_EndSession $R0
          Abort "${LPK_PRODUCT} could not be closed; nothing was ${changed}."
        ${EndIf}
      ${EndIf}
    ${EndIf}
    !insertmacro RestartManager_EndSession $R0
  ${EndIf}
!macroend

Function LpkCheckRunning
  !insertmacro LPK_CHECK_RUNNING "install" "changed"
FunctionEnd

Function un.LpkCheckRunning
  !insertmacro LPK_CHECK_RUNNING "uninstall" "removed"
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
  ; Before the RunOnce values are cancelled: an installer stopped here leaves the old installation as it was. The GUI
  ; is closed (or the install cancelled) first, so the template's own check after this hook finds nothing to close.
  StrCpy $R9 $PassiveMode
  Call LpkCheckRunning
  !insertmacro LPK_SET_ASIDE_BIN
  !insertmacro LPK_CANCEL_PENDING_RUNONCE
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; R336: the set-aside copies go only now that the new files exist.
  !insertmacro LPK_FINISH_SET_ASIDE
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
  ; bin\'s set-aside executables (R334) whose process has exited, before the template removes bin\ when empty.
  Delete "$INSTDIR\bin\lpk*.exe.old-*"
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
    ; R336: an executable still running is renamed aside, for the `LitePack uninstall` command.
    !insertmacro LPK_UNINSTALL_SET_ASIDE "lpk.exe"
    !insertmacro LPK_UNINSTALL_SET_ASIDE "lpk-check.exe"
    !insertmacro LPK_UNINSTALL_SET_ASIDE "lpk-worker.exe"
    !insertmacro LPK_UNINSTALL_SET_ASIDE "lpk-worker-libarchive.exe"
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
    ; A shell DLL Explorer still holds (this version's, after the template removed the rest) and set-aside executables
    ; still running (R336): removed at the next logon by one command, with the then-empty folders, never recursively.
    Delete "$INSTDIR\bin\lpk*.exe.old-*"
    ${If} ${FileExists} "$INSTDIR\shell\${VERSION}\lpk_shell.dll"
    ${OrIf} ${FileExists} "$INSTDIR\bin\lpk*.exe.old-*"
      !insertmacro LPK_RUNONCE_REMOVE_INSTALL
    ${EndIf}
  ${EndIf}
!macroend
