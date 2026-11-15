; E7-2: the hooks file of the ARM64 NSIS installer (the release workflow's overlay names it as installerHooks for
; --target aarch64-pc-windows-msvc; scripts/e7-nsis-overlay.py). tauri-bundler 2.10.4's template includes the hooks
; file before it defines ${ARCH}, and the bundler passes makensis no /D, so installer.nsh cannot see the architecture
; it is built for: this file names it, then includes the shared hooks, which default to x64 only when nothing is
; defined. installer.nsh's PREINSTALL still stops the build when the template's ${ARCH} differs.
!define LPK_INSTALLER_ARCH "arm64"
!include "${__FILEDIR__}\installer.nsh"
