//! LitePack's Explorer context-menu handler (E6-1, `docs/notes/e6-design.md` section 1).
//!
//! `lpk_shell.dll` is a COM in-process server that Explorer (and any program showing a shell
//! menu) loads. It never loads the engine or the webview: its one action, filled in by E6-2,
//! starts `lpk-gui.exe` with one of E5-7's verbs (`docs/API.md`, "Activation verbs"). This crate
//! is the E6-1 skeleton: the COM class, its class factory, `DllGetClassObject` and
//! `DllCanUnloadNow`, and an `IExplorerCommand` that gives a fixed title and answers every other
//! method with `E_NOTIMPL`. Nothing here registers the handler or writes to the registry.
//!
//! The crate denies `unsafe_code`; only [`com`] (Windows only) allows it, and every `unsafe`
//! block there carries a `SAFETY:` comment. On other targets the crate is empty.

#[cfg(windows)]
pub mod com;
pub mod model;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::Path;

    /// The dependency names declared anywhere in this crate's manifest (every
    /// `[*dependencies]` table, target-specific ones included).
    fn declared_dependencies() -> Vec<String> {
        let manifest =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .expect("manifest");
        let mut names = Vec::new();
        let mut in_deps = false;
        for line in manifest.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_deps = line.trim_end_matches(']').ends_with("dependencies");
                continue;
            }
            if !in_deps || line.is_empty() || line.starts_with('#') || line.starts_with(']') {
                continue;
            }
            if let Some((key, _)) = line.split_once('=') {
                let key = key.trim();
                if !key.starts_with('"') {
                    names.push(key.to_string());
                }
            }
        }
        names
    }

    #[test]
    fn the_handler_never_depends_on_the_engine_or_the_webview() {
        let deps = declared_dependencies();
        assert!(
            deps.iter().any(|d| d == "windows"),
            "the manifest scan found no dependency at all: {deps:?}"
        );
        for d in &deps {
            let lower = d.to_ascii_lowercase();
            assert!(
                !lower.starts_with("lpk"),
                "{d}: the handler must not load the engine or any LitePack crate"
            );
            for banned in ["tauri", "wry", "webview", "tao"] {
                assert!(
                    !lower.contains(banned),
                    "{d}: the handler must not load the webview"
                );
            }
        }
    }

    #[test]
    fn unsafe_is_allowed_only_in_the_com_boundary_module() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut allowing = Vec::new();
        for entry in std::fs::read_dir(&src).expect("src") {
            let path = entry.expect("entry").path();
            let text = std::fs::read_to_string(&path).expect("source");
            let marker = ["allow(", "unsafe_code)"].concat();
            if text.contains(&marker) {
                allowing.push(
                    path.file_name()
                        .expect("name")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        assert_eq!(allowing, vec!["com.rs".to_string()]);
        let manifest =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .expect("manifest");
        assert!(manifest.contains("unsafe_code = \"deny\""));
        assert!(manifest.contains("undocumented_unsafe_blocks = \"deny\""));
    }
}
