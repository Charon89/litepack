; E7-5: the hooks of LitePack's per-user NSIS installer (docs/notes/e7-design.md section 3), included by
; tauri-bundler 2.10.4's installer.nsi through tauri.nsis.conf.json's installerHooks. Part 1: route C, no sparse
; package (R304). The template already includes x64.nsh, LogicLib (through MUI2) and FileFunc.
;
; The template's installer.nsi sets no ManifestSupportedOS, so NSIS 3.13's default ("all": Vista to Windows 10/11)
; is in the manifest, and ${AtLeastBuild} sees the real build number instead of Windows 8's (E7-5's finding;
; the refusal itself is checked by hand on a Windows 10 VM in E7-10, CI has no Windows 10 runner).

!include WinVer.nsh

; HWND_BROADCAST, WM_SETTINGCHANGE, SMTO_ABORTIFHUNG: tell running programs that HKCU\Environment changed
; (R306), with NSIS's own SendMessage and a 5-second timeout.
!macro LPK_BROADCAST_ENVIRONMENT
  SendMessage 0xFFFF 0x001A 0 "STR:Environment" /TIMEOUT=5000
!macroend

; The shell\<version> folders other than this version's: removed when Explorer holds none of their files;
; one still held is removed at the next logon by a per-user RunOnce command.
!macro LPK_REMOVE_OLD_SHELL_FOLDERS
  FindFirst $0 $1 "$INSTDIR\shell\*"
  ${DoWhile} $1 != ""
    ${If} $1 != "."
    ${AndIf} $1 != ".."
    ${AndIf} $1 != "${VERSION}"
      RMDir /r "$INSTDIR\shell\$1"
      ${If} ${FileExists} "$INSTDIR\shell\$1\*"
        WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\RunOnce" "LitePack shell $1" \
          '"$SYSDIR\cmd.exe" /d /c rmdir /s /q "$INSTDIR\shell\$1"'
      ${EndIf}
    ${EndIf}
    FindNext $0 $1
  ${Loop}
  FindClose $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  ${IfNot} ${AtLeastBuild} 19045
    MessageBox MB_ICONSTOP|MB_OK "LitePack needs Windows 10 version 22H2 (build 19045) or Windows 11." /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
  !if "${ARCH}" == "x64"
    ${If} ${IsNativeARM64}
      MessageBox MB_ICONSTOP|MB_OK "This is the x64 build of LitePack. On this ARM64 PC, install the ARM64 build: its Explorer menu needs an ARM64 shell extension." /SD IDOK
      SetErrorLevel 1
      Quit
    ${EndIf}
  !endif
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
    ; A shell DLL Explorer still holds: removed at the next logon.
    ${If} ${FileExists} "$INSTDIR\shell\*"
      WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\RunOnce" "LitePack uninstall" \
        '"$SYSDIR\cmd.exe" /d /c rmdir /s /q "$INSTDIR"'
    ${EndIf}
  ${EndIf}
!macroend
