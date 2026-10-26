//! Tauri's build step on Windows (the host is Windows-only): the context, the icons, and one
//! permission per command (`AppManifest::commands`), so that a capability lists the commands a
//! window may call and nothing else is allowed.
#![forbid(unsafe_code)]

include!("src/command_list.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/command_list.rs");
    println!("cargo:rerun-if-changed=capabilities");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    #[cfg(windows)]
    {
        let attrs = tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));
        if let Err(e) = tauri_build::try_build(attrs) {
            panic!("tauri-build: {e:#}");
        }
    }
    #[cfg(not(windows))]
    {
        let _ = COMMANDS;
    }
}
