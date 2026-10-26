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

    /// Every package reached from `lpk-shell` in the workspace's `Cargo.lock`, following each
    /// package's `dependencies` transitively. The lock records every target's dependencies,
    /// renames under their package name and `[dependencies.x]` tables alike, so nothing a
    /// manifest can declare escapes this walk.
    fn reachable_packages() -> Vec<String> {
        let lock =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"))
                .expect("Cargo.lock");
        let lock: toml::Table = toml::from_str(&lock).expect("Cargo.lock parses");
        let packages = lock
            .get("package")
            .and_then(toml::Value::as_array)
            .expect("[[package]] entries");
        let field = |p: &toml::Value, k: &str| -> String {
            p.get(k)
                .and_then(toml::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        // A dependency entry is "name", "name version" or "name version (source)".
        let find = |entry: &str| -> Vec<&toml::Value> {
            let mut parts = entry.split_whitespace();
            let name = parts.next().unwrap_or_default();
            let version = parts.next();
            packages
                .iter()
                .filter(|p| field(p, "name") == name)
                .filter(|p| version.is_none_or(|v| field(p, "version") == v))
                .collect()
        };
        let mut seen = std::collections::BTreeSet::new();
        let mut queue = vec!["lpk-shell".to_string()];
        let mut reached = Vec::new();
        while let Some(entry) = queue.pop() {
            let found = find(&entry);
            assert!(!found.is_empty(), "{entry}: not in Cargo.lock");
            for p in found {
                let key = (field(p, "name"), field(p, "version"));
                if !seen.insert(key.clone()) {
                    continue;
                }
                reached.push(key.0);
                if let Some(deps) = p.get("dependencies").and_then(toml::Value::as_array) {
                    queue.extend(
                        deps.iter()
                            .filter_map(toml::Value::as_str)
                            .map(String::from),
                    );
                }
            }
        }
        reached
    }

    #[test]
    fn the_handler_never_depends_on_the_engine_or_the_webview() {
        let reached = reachable_packages();
        assert!(
            reached.iter().any(|d| d == "windows"),
            "the Cargo.lock walk found no dependency at all: {reached:?}"
        );
        for d in reached.iter().filter(|d| d.as_str() != "lpk-shell") {
            let lower = d.to_ascii_lowercase();
            assert!(
                !lower.starts_with("lpk-"),
                "{d}: the handler must not load the engine or any LitePack crate"
            );
            assert!(
                !(lower.starts_with("tauri") || lower.starts_with("webview2") || lower == "wry"),
                "{d}: the handler must not load the webview"
            );
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
