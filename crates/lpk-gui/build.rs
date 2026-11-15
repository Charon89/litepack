//! Tauri's build step on Windows (the host is Windows-only): the context, the icons, and one
//! permission per command (`AppManifest::commands`), so that a capability lists the commands a
//! window may call and nothing else is allowed.
#![forbid(unsafe_code)]

include!("src/command_list.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/command_list.rs");
    println!("cargo:rerun-if-changed=capabilities");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    // E7-5: the version of tauri.conf.json with any `--config` overlay merged (tauri-cli passes
    // the overlay to the build in `TAURI_CONFIG`): the installer's `shell\<version>` folder.
    println!("cargo:rerun-if-env-changed=TAURI_CONFIG");
    println!("cargo:rustc-env=LPK_APP_VERSION={}", app_version());
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

/// `version` of the `TAURI_CONFIG` overlay when it sets one (JSON text, or a path to a JSON
/// file), otherwise `version` of `tauri.conf.json`.
fn app_version() -> String {
    let version_of = |text: &str| -> Option<String> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        Some(value.get("version")?.as_str()?.to_string())
    };
    if let Ok(overlay) = std::env::var("TAURI_CONFIG") {
        let text = if overlay.trim_start().starts_with('{') {
            overlay
        } else {
            std::fs::read_to_string(&overlay).unwrap_or_default()
        };
        if let Some(v) = version_of(&text) {
            return v;
        }
    }
    let conf = std::fs::read_to_string("tauri.conf.json").expect("tauri.conf.json");
    version_of(&conf).expect("tauri.conf.json has a version")
}
