//! `lpk-gui.exe`: the Tauri host on Windows; elsewhere a stub (the command layer in the library
//! builds and is tested on every OS; the GUI ships for Windows alone).
#![forbid(unsafe_code)]
// No console window behind the GUI in a release build.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod host;

#[cfg(windows)]
fn main() {
    std::process::exit(host::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("lpk-gui: the LitePack GUI runs on Windows; use `lpk` here");
    std::process::exit(2);
}
