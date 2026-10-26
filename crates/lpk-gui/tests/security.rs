//! The page's security configuration (E5-2; `docs/notes/e5-design.md` section 4), checked on
//! every OS without a webview: the CSP, the capability files, the command list, that no
//! command takes a path, the HTML-sink grep over `apps/gui/`, no Node toolchain in the
//! repository, and the string table.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lpk_gui::security::{CSP, MAIN_WINDOW};
use lpk_gui::COMMANDS;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    crate_dir().join("../..")
}

fn json(p: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn files(dir: &Path, out: &mut Vec<PathBuf>, skip: &[&str]) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !skip.contains(&name.as_str()) {
                files(&p, out, skip);
            }
        } else {
            out.push(p);
        }
    }
}

fn gui_files() -> Vec<PathBuf> {
    let mut v = Vec::new();
    files(&repo_root().join("apps/gui"), &mut v, &[]);
    assert!(!v.is_empty());
    v
}

#[test]
fn the_csp_is_the_designs() {
    let conf = json(&crate_dir().join("tauri.conf.json"));
    assert_eq!(conf["app"]["security"]["csp"].as_str().unwrap(), CSP);
    for d in [
        "default-src 'none'",
        "script-src 'self'",
        "style-src 'self'",
        "object-src 'none'",
        "frame-src 'none'",
        "base-uri 'none'",
        "form-action 'none'",
        "connect-src ipc: http://ipc.localhost",
        "require-trusted-types-for 'script'",
        "trusted-types 'none'",
    ] {
        assert!(CSP.split("; ").any(|x| x == d), "missing {d}");
    }
    assert!(!CSP.contains("unsafe-"), "no unsafe-inline or unsafe-eval");
    // No remote origin: the only URL is Tauri's IPC endpoint.
    let urls: Vec<&str> = CSP
        .split([' ', ';'])
        .filter(|w| w.contains("://"))
        .collect();
    assert_eq!(urls, ["http://ipc.localhost"]);
    // No dev server, no remote frontend.
    assert!(conf["build"].get("devUrl").is_none());
    assert_eq!(conf["build"]["frontendDist"], "../../apps/gui");
    assert_ne!(
        conf["app"]["security"]["dangerousDisableAssetCspModification"],
        true
    );
}

#[test]
fn each_window_lists_its_commands_and_nothing_else() {
    let conf = json(&crate_dir().join("tauri.conf.json"));
    let mut ids = BTreeSet::new();
    let mut allowed = BTreeSet::new();
    let mut windows = BTreeSet::new();
    let mut caps = Vec::new();
    files(&crate_dir().join("capabilities"), &mut caps, &[]);
    assert!(!caps.is_empty());
    for c in caps {
        let v = json(&c);
        ids.insert(v["identifier"].as_str().unwrap().to_string());
        let w: Vec<&str> = v["windows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(w.len(), 1, "{}: one file per window", c.display());
        assert!(!w[0].contains('*'), "no window glob");
        windows.insert(w[0].to_string());
        assert!(v.get("remote").is_none(), "{}: no remote URL", c.display());
        for p in v["permissions"].as_array().unwrap() {
            let p = p
                .as_str()
                .unwrap_or_else(|| panic!("{p}: no scoped permission"));
            assert!(!p.contains(':'), "{p}: no plugin or core permission");
            let cmd = p
                .strip_prefix("allow-")
                .unwrap_or_else(|| panic!("{p}: only allow-<command>"))
                .replace('-', "_");
            assert!(COMMANDS.contains(&cmd.as_str()), "{p}: not a command");
            allowed.insert(cmd);
        }
    }
    assert!(windows.contains(MAIN_WINDOW));
    let all: BTreeSet<String> = COMMANDS.iter().map(|c| c.to_string()).collect();
    assert_eq!(allowed, all, "every command is allowed in some window");
    let listed: BTreeSet<String> = conf["app"]["security"]["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        listed, ids,
        "tauri.conf.json names exactly the capability files"
    );
}

fn host_source() -> String {
    std::fs::read_to_string(crate_dir().join("src/host.rs")).unwrap()
}

#[test]
fn the_handler_registers_exactly_the_command_list() {
    let src = host_source();
    let start = src.find("generate_handler![").unwrap() + "generate_handler![".len();
    let end = start + src[start..].find(']').unwrap();
    let names: Vec<&str> = src[start..end]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(names, COMMANDS);
    assert_eq!(src.matches("#[tauri::command]").count(), COMMANDS.len());
    // build.rs builds the permissions from the same list.
    let build = std::fs::read_to_string(crate_dir().join("build.rs")).unwrap();
    assert!(build.contains("include!(\"src/command_list.rs\")"));
    assert!(build.contains(".commands(COMMANDS)"));
}

#[test]
fn no_command_takes_a_path() {
    let src = host_source();
    let mut seen = 0;
    for (i, _) in src.match_indices("#[tauri::command]") {
        let rest = &src[i..];
        let sig = &rest[..rest.find('{').unwrap()];
        seen += 1;
        for barred in ["Path", "OsStr", "OsString"] {
            assert!(!sig.contains(barred), "a command takes a {barred}: {sig}");
        }
    }
    assert_eq!(seen, COMMANDS.len());
}

#[test]
fn the_page_has_no_html_sink_no_eval_no_inline_code_and_no_url() {
    for f in gui_files() {
        let text = std::fs::read_to_string(&f).unwrap();
        let lower = text.to_ascii_lowercase();
        for barred in [
            "innerhtml",
            "outerhtml",
            "insertadjacenthtml",
            "document.write",
            "eval(",
            "new function",
            "srcdoc",
            "javascript:",
            "http://",
            "https://",
            "createcontextualfragment",
            "settimeout(\"",
            "setinterval(\"",
        ] {
            assert!(!lower.contains(barred), "{}: {barred}", f.display());
        }
        if f.extension().is_some_and(|x| x == "html") {
            // No inline handler attribute (` onclick=` and the like).
            for (i, _) in lower.match_indices(" on") {
                let word: String = lower[i + 3..]
                    .chars()
                    .take_while(char::is_ascii_alphabetic)
                    .collect();
                let after = lower[i + 3 + word.len()..].trim_start();
                assert!(
                    word.is_empty() || !after.starts_with('='),
                    "{}: inline handler on{word}",
                    f.display()
                );
            }
            // Every script is a file: `<script ... src=...></script>`, nothing inline, no style
            // element.
            for (i, _) in lower.match_indices("<script") {
                let tag_end = i + lower[i..].find('>').unwrap();
                assert!(
                    lower[i..tag_end].contains(" src="),
                    "{}: inline script",
                    f.display()
                );
                assert!(lower[tag_end + 1..].starts_with("</script>"));
            }
            assert!(!lower.contains("<style"), "{}: inline style", f.display());
            assert!(!lower.contains(" style="), "{}: inline style", f.display());
        }
    }
}

#[test]
fn the_repository_holds_no_node_toolchain() {
    let mut all = Vec::new();
    files(
        &repo_root(),
        &mut all,
        &["target", ".git", "corpus", ".claude", "tmp"],
    );
    for f in all {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            ![
                "package.json",
                "package-lock.json",
                "yarn.lock",
                "pnpm-lock.yaml",
                ".npmrc",
            ]
            .contains(&name.as_str()),
            "{}",
            f.display()
        );
        assert!(
            !f.components().any(|c| c.as_os_str() == "node_modules"),
            "{}",
            f.display()
        );
    }
}

/// The quoted text after each occurrence of `prefix` up to `close`.
fn quoted_after(text: &str, prefix: &str, close: char) -> Vec<String> {
    text.match_indices(prefix)
        // A whole word: `t("` but not `createElement("`.
        .filter(|(i, _)| {
            !text[..*i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
        })
        .filter_map(|(i, _)| {
            let rest = &text[i + prefix.len()..];
            rest.find(close).map(|e| rest[..e].to_string())
        })
        .collect()
}

#[test]
fn every_string_is_in_the_table_and_every_key_is_used() {
    let table = lpk_gui::strings::table();
    let mut used = BTreeSet::new();
    for f in gui_files() {
        let text = std::fs::read_to_string(&f).unwrap();
        used.extend(quoted_after(&text, "data-string=\"", '"'));
        used.extend(quoted_after(&text, "data-label=\"", '"'));
        used.extend(quoted_after(&text, "t(\"", '"'));
    }
    let mut rust = Vec::new();
    files(&crate_dir().join("src"), &mut rust, &[]);
    for f in rust {
        let text = std::fs::read_to_string(&f).unwrap();
        used.extend(quoted_after(&text, "get(\"", '"'));
    }
    for k in &used {
        assert!(table.contains_key(k), "{k} is used but not in en.json");
    }
    for k in table.keys() {
        assert!(used.contains(k), "{k} is in en.json but unused");
    }
    // No literal text in the markup: every element's text comes from the table.
    let html = std::fs::read_to_string(repo_root().join("apps/gui/index.html")).unwrap();
    let mut outside = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => outside.push(c),
            _ => {}
        }
    }
    assert!(
        outside.trim().is_empty(),
        "literal text in index.html: {outside:?}"
    );
}
