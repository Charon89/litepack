//! Tool discovery: local override path, then PATH, then the catalogue's install-location hints;
//! then the version. Paths are returned to the caller for execution and printing only; they are
//! never part of a result file (see `result::ToolsFile`).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::catalogue::{Catalogue, Local, Tool};

pub const SKIP_NOT_INSTALLED: &str = "not installed";
pub const SKIP_MANUAL: &str = "manual (see docs/BASELINES.md)";

const VERSION_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Found { version: String, path: PathBuf },
    Skipped { reason: String },
}

#[derive(Debug, Clone)]
pub struct Discovered {
    pub tool: Tool,
    pub status: Status,
    /// `bench/tools.local.toml` has an entry for this tool.
    pub local_override: bool,
}

/// The parts of the environment discovery depends on, injectable for tests.
pub struct Env {
    pub os: &'static str,
    pub path_dirs: Vec<PathBuf>,
    pub vars: VarLookup,
}

/// Environment-variable lookup.
pub type VarLookup = Box<dyn Fn(&str) -> Option<String>>;

impl Env {
    pub fn current() -> Env {
        let path_dirs = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        Env {
            os: super::catalogue::current_os(),
            path_dirs,
            vars: Box::new(|name| std::env::var(name).ok()),
        }
    }
}

/// Expand `%VAR%` references; `None` when a variable is unset (the hint does not apply).
pub fn expand_hint(hint: &str, vars: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::new();
    let mut rest = hint;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after.find('%')?;
        out.push_str(&vars(&after[..end])?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Candidate file names for an executable name: on Windows a bare name also tries the usual
/// executable extensions.
fn candidates(name: &str, os: &str) -> Vec<String> {
    let mut v = vec![name.to_string()];
    if os == "windows" && Path::new(name).extension().is_none() {
        for ext in ["exe", "cmd", "bat", "com"] {
            v.push(format!("{name}.{ext}"));
        }
    }
    v
}

/// Find a tool's executable: PATH first, then the install-location hints.
pub fn find_executable(tool: &Tool, env: &Env) -> Option<PathBuf> {
    if tool.hints_first {
        find_in_hints(tool, env).or_else(|| find_on_path(tool, env))
    } else {
        find_on_path(tool, env).or_else(|| find_in_hints(tool, env))
    }
}

fn find_on_path(tool: &Tool, env: &Env) -> Option<PathBuf> {
    for name in tool.exe_names(env.os) {
        for dir in &env.path_dirs {
            for cand in candidates(name, env.os) {
                let p = dir.join(cand);
                if is_executable(&p) {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn find_in_hints(tool: &Tool, env: &Env) -> Option<PathBuf> {
    for hint in tool.hint_list(env.os) {
        if let Some(expanded) = expand_hint(hint, &*env.vars) {
            if let Some(p) = expand_glob(&expanded)
                .into_iter()
                .find(|p| is_executable(p))
            {
                // A relative hint is relative to the current directory; the runner changes
                // directory, so the executable is recorded by its absolute path.
                return Some(std::path::absolute(&p).unwrap_or(p));
            }
        }
    }
    None
}

/// Order names so that digit runs compare as numbers (`v1.5.10` after `v1.5.9`).
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn chunks(s: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let bytes = s.as_bytes();
        for i in 1..=bytes.len() {
            if i == bytes.len() || bytes[i].is_ascii_digit() != bytes[start].is_ascii_digit() {
                out.push((bytes[start].is_ascii_digit(), &s[start..i]));
                start = i;
            }
        }
        out
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(&cb) {
        let ord = if x.0 && y.0 {
            let (nx, ny) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
            nx.len().cmp(&ny.len()).then_with(|| nx.cmp(ny))
        } else {
            x.1.cmp(y.1)
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    ca.len().cmp(&cb.len())
}

/// Does `name` match `pattern`, where `*` matches any run of characters (ASCII case-insensitive)?
fn wild_match(pattern: &str, name: &str) -> bool {
    let (p, n) = (pattern.to_ascii_lowercase(), name.to_ascii_lowercase());
    let parts: Vec<&str> = p.split('*').collect();
    let Some((first, rest)) = parts.split_first() else {
        return false;
    };
    let Some(mut tail) = n.strip_prefix(first) else {
        return false;
    };
    let Some((last, middle)) = rest.split_last() else {
        return tail.is_empty();
    };
    for part in middle {
        match tail.find(part) {
            Some(i) => tail = &tail[i + part.len()..],
            None => return false,
        }
    }
    tail.ends_with(last)
}

/// Expand `*` wildcards in directory or file name segments (one segment each, never across
/// separators), for hints such as winget's version-named portable folders. Candidates come back
/// in reverse name order so that the highest version tends to come first. Without `*` the path
/// is returned as is.
pub fn expand_glob(path: &str) -> Vec<PathBuf> {
    let Some(star) = path.find('*') else {
        return vec![PathBuf::from(path)];
    };
    let split = path[..star].rfind(['/', '\\']).map_or(0, |i| i + 1);
    let mut current = vec![PathBuf::from(&path[..split])];
    for seg in path[split..].split(['/', '\\']) {
        let mut next = Vec::new();
        for base in &current {
            if seg.contains('*') {
                let Ok(rd) = std::fs::read_dir(base) else {
                    continue;
                };
                let mut hits: Vec<String> = rd
                    .filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| wild_match(seg, n))
                    .collect();
                hits.sort_by(|a, b| natural_cmp(b, a));
                next.extend(hits.into_iter().map(|h| base.join(h)));
            } else {
                next.push(base.join(seg));
            }
        }
        current = next;
    }
    current
}

/// First capture group of `pattern` in `text`.
pub fn extract_version(text: &str, pattern: &str) -> Option<String> {
    let re = regex::Regex::new(pattern).ok()?;
    let m = re.captures(text)?.get(1)?;
    Some(m.as_str().replace(')', ""))
}

/// Run the executable with the version arguments and read the version from its output
/// (stdout and stderr; the exit status is ignored, several tools print help and exit non-zero).
pub fn read_version(exe: &Path, tool: &Tool) -> Result<String, String> {
    let mut child = Command::new(exe)
        .args(&tool.version.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run: {e}"))?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > VERSION_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("version probe timed out".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("could not wait: {e}")),
        }
    }
    let mut text = String::new();
    for h in [out, err] {
        if let Ok(bytes) = h.join() {
            text.push_str(&String::from_utf8_lossy(&bytes));
            text.push('\n');
        }
    }
    Ok(extract_version(&text, &tool.version.pattern).unwrap_or_else(|| "unknown".to_string()))
}

/// Discover one tool.
pub fn discover_tool(tool: &Tool, local: &Local, env: &Env) -> Status {
    let exe = if let Some(p) = local.paths.get(&tool.id) {
        let p = PathBuf::from(p);
        if !is_executable(&p) {
            return Status::Skipped {
                reason: format!("{SKIP_NOT_INSTALLED} (path in bench/tools.local.toml not found)"),
            };
        }
        Some(p)
    } else if tool.manual && !local.overridden.contains(&tool.id) {
        return Status::Skipped {
            reason: SKIP_MANUAL.to_string(),
        };
    } else {
        find_executable(tool, env)
    };
    let Some(path) = exe else {
        return Status::Skipped {
            reason: SKIP_NOT_INSTALLED.to_string(),
        };
    };
    match read_version(&path, tool) {
        Ok(version) => Status::Found { version, path },
        Err(e) => Status::Skipped {
            reason: format!("found but unusable: {e}"),
        },
    }
}

#[allow(dead_code)] // used by the tools.json writer, added with the result format
pub fn discover_all(cat: &Catalogue, local: &Local, env: &Env) -> Vec<Discovered> {
    cat.tools
        .iter()
        .map(|t| Discovered {
            tool: t.clone(),
            status: discover_tool(t, local, env),
            local_override: local.overridden.contains(&t.id),
        })
        .collect()
}
// `discover_all` feeds `tools.json` (see `result::ToolsFile::from_discovered`).

/// One line per tool for `--list-tools`.
pub fn list_line(d: &Discovered) -> String {
    match &d.status {
        Status::Found { version, path } => {
            format!(
                "{:<10} found    {:<14} {}",
                d.tool.id,
                version,
                path.display()
            )
        }
        Status::Skipped { reason } => format!("{:<10} skipped: {reason}", d.tool.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::catalogue::Catalogue;

    fn catalogue() -> Catalogue {
        Catalogue::parse(include_str!("../../../../bench/tools.toml")).expect("catalogue")
    }

    fn fake_env(os: &'static str, dirs: Vec<PathBuf>, vars: Vec<(&'static str, String)>) -> Env {
        Env {
            os,
            path_dirs: dirs,
            vars: Box::new(move |n| vars.iter().find(|(k, _)| *k == n).map(|(_, v)| v.clone())),
        }
    }

    fn touch_exe(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"x").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        p
    }

    #[test]
    fn hints_expand_environment_variables() {
        let vars = |n: &str| (n == "ProgramFiles").then(|| "PF".to_string());
        assert_eq!(
            expand_hint("%ProgramFiles%/7-Zip/7z.exe", &vars).as_deref(),
            Some("PF/7-Zip/7z.exe")
        );
        assert_eq!(expand_hint("%Nope%/x", &vars), None);
        assert_eq!(expand_hint("plain", &vars).as_deref(), Some("plain"));
        assert_eq!(expand_hint("%broken", &vars), None);
    }

    #[test]
    fn wildcard_hints_pick_the_highest_matching_folder() {
        assert!(wild_match("zstd-v*-win64", "zstd-v1.5.7-win64"));
        assert!(!wild_match("zstd-v*-win64", "zstd-v1.5.7-win32"));
        assert!(wild_match("a*b*c", "aXbYc") && !wild_match("a*b*c", "aXbY"));
        let tmp = tempfile::tempdir().expect("tmp");
        for v in ["1.5.6", "1.5.7"] {
            let d = tmp.path().join(format!("Pkg_x/zstd-v{v}-win64"));
            std::fs::create_dir_all(&d).expect("dir");
            touch_exe(&d, "zstd.exe");
        }
        let root = tmp.path().to_string_lossy().into_owned();
        let hits = expand_glob(&format!("{root}/Pkg_*/zstd-v*/zstd.exe"));
        assert!(hits[0].to_string_lossy().contains("1.5.7"), "{hits:?}");
        assert!(expand_glob(&format!("{root}/none_*/x")).is_empty());
        let mut tool = catalogue().get("zstd").expect("zstd").clone();
        tool.hints
            .insert("windows".into(), vec!["%WG%/Pkg_*/zstd-v*/zstd.exe".into()]);
        let env = fake_env("windows", vec![], vec![("WG", root)]);
        let found = find_executable(&tool, &env).expect("found");
        assert!(found.to_string_lossy().contains("1.5.7"));
    }

    #[test]
    fn version_numbers_compare_numerically_not_lexically() {
        use std::cmp::Ordering::{Greater, Less};
        assert_eq!(
            natural_cmp("zstd-v1.5.10-win64", "zstd-v1.5.9-win64"),
            Greater
        );
        assert_eq!(natural_cmp("v1.5.7", "v1.5.10"), Less);
        assert_eq!(natural_cmp("a", "a"), std::cmp::Ordering::Equal);
        let tmp = tempfile::tempdir().expect("tmp");
        for v in ["1.5.9", "1.5.10", "1.5.2"] {
            let d = tmp.path().join(format!("zstd-v{v}-win64"));
            std::fs::create_dir_all(&d).expect("dir");
            touch_exe(&d, "zstd.exe");
        }
        let root = tmp.path().to_string_lossy().into_owned();
        let hits = expand_glob(&format!("{root}/zstd-v*/zstd.exe"));
        assert!(hits[0].to_string_lossy().contains("1.5.10"), "{hits:?}");
    }

    #[test]
    fn hints_first_beats_path_and_otherwise_path_wins() {
        let tmp = tempfile::tempdir().expect("tmp");
        let path_dir = tmp.path().join("onpath");
        let hint_dir = tmp.path().join("hint");
        std::fs::create_dir_all(&path_dir).expect("dir");
        std::fs::create_dir_all(&hint_dir).expect("dir");
        let on_path = touch_exe(&path_dir, "tar.exe");
        let hinted = touch_exe(&hint_dir, "tar.exe");
        let mut tool = catalogue().get("store").expect("store").clone();
        tool.hints
            .insert("windows".into(), vec!["%H%/tar.exe".into()]);
        let env = fake_env(
            "windows",
            vec![path_dir],
            vec![("H", hint_dir.to_string_lossy().into_owned())],
        );
        assert!(tool.hints_first);
        assert_eq!(find_executable(&tool, &env), Some(hinted.clone()));
        tool.hints_first = false;
        assert_eq!(find_executable(&tool, &env), Some(on_path));
        // hints_first falls back to PATH when no hint exists.
        tool.hints_first = true;
        std::fs::remove_file(&hinted).expect("rm");
        assert!(find_executable(&tool, &env).is_some());
    }

    /// The workspace's release binary named by the catalogue's relative hint, resolved from the
    /// workspace root (the test runs in the crate directory).
    #[test]
    fn the_lpk_relative_hint_resolves_to_the_release_binary() {
        let real = include_str!("../../../../bench/tools.toml");
        let cat = Catalogue::parse(&real.replace("\"target/release/", "\"../../target/release/"))
            .expect("catalogue");
        let tool = cat.get("lpk").expect("lpk row");
        let env = fake_env(crate::run::catalogue::current_os(), Vec::new(), Vec::new());
        let Some(path) = find_executable(tool, &env) else {
            eprintln!(
                "SKIPPED: target/release/lpk is not built (cargo build --release -p lpk-cli)"
            );
            return;
        };
        assert!(path.is_absolute() && path.is_file(), "{}", path.display());
        match discover_tool(tool, &Local::default(), &env) {
            Status::Found { version, .. } => {
                assert!(version.starts_with(env!("CARGO_PKG_VERSION")), "{version}");
                assert!(version.contains('+'), "{version}");
            }
            Status::Skipped { reason } => panic!("found but skipped: {reason}"),
        }
    }

    #[test]
    fn list_tools_names_lpk_found_or_skipped() {
        let cat = catalogue();
        let tool = cat.get("lpk").expect("lpk row").clone();
        let status = discover_tool(&tool, &Local::default(), &Env::current());
        let line = list_line(&Discovered {
            tool,
            status,
            local_override: false,
        });
        assert!(line.starts_with("lpk"), "{line}");
        assert!(line.contains("found") || line.contains("skipped"), "{line}");
    }

    #[test]
    fn versions_are_read_with_the_catalogue_patterns() {
        let cat = catalogue();
        let pat = |id: &str| cat.get(id).expect("tool").version.pattern.clone();
        let cases = [
            (
                "7z",
                "\n7-Zip 26.03 (x64) : Copyright (c) 1999-2026 Igor Pavlov\n",
                "26.03",
            ),
            ("7z", "7-Zip (z) 26.03 (x64)", "26.03"),
            (
                "rar",
                "RAR 7.23   Copyright (c) 1993-2026 Alexander Roshal",
                "7.23",
            ),
            (
                "zstd",
                "*** Zstandard CLI (64-bit) v1.5.7, by Yann Collet ***",
                "1.5.7",
            ),
            ("xz", "xz (XZ Utils) 5.8.1\nliblzma 5.8.1", "5.8.1"),
            ("tsaur", "tsaur 1.0.0-rc.2", "1.0.0-rc.2"),
            (
                "store",
                "bsdtar 3.7.7 - libarchive 3.7.7 zlib/1.3.1",
                "bsdtar 3.7.7",
            ),
            ("store", "tar (GNU tar) 1.35\nCopyright", "GNU tar 1.35"),
            (
                "zpaqfranz",
                "zpaqfranz v65.4m-JIT67+M,HW SHA1/2,SSE2, SFX64 v55.1,(2026-09-01)",
                "65.4",
            ),
        ];
        for (id, text, want) in cases {
            assert_eq!(
                extract_version(text, &pat(id)).as_deref(),
                Some(want),
                "{id}"
            );
        }
        assert_eq!(extract_version("nothing", &pat("7z")), None);
    }

    #[test]
    fn path_search_precedes_hints_and_missing_is_not_installed() {
        let cat = catalogue();
        let tmp = tempfile::tempdir().expect("tmp");
        let tool = cat.get("7z").expect("7z");
        let none = Local::default();

        // Nothing on PATH, no variables: skipped.
        let env = fake_env("linux", vec![tmp.path().to_path_buf()], vec![]);
        assert_eq!(
            discover_tool(tool, &none, &env),
            Status::Skipped {
                reason: SKIP_NOT_INSTALLED.into()
            }
        );

        // `7zz` is found on PATH on Linux (second candidate name wins when the first is absent).
        let exe = touch_exe(tmp.path(), "7zz");
        assert_eq!(find_executable(tool, &env), Some(exe));

        // Windows: bare name gets `.exe`; the hint directory is used when PATH has nothing.
        let hint_dir = tmp.path().join("hint");
        std::fs::create_dir_all(hint_dir.join("7-Zip")).expect("dir");
        let hinted = touch_exe(&hint_dir.join("7-Zip"), "7z.exe");
        let env = fake_env(
            "windows",
            vec![],
            vec![("ProgramFiles", hint_dir.to_string_lossy().into_owned())],
        );
        assert_eq!(find_executable(tool, &env), Some(hinted));
        let on_path = touch_exe(tmp.path(), "7z.exe");
        let env = fake_env("windows", vec![tmp.path().to_path_buf()], vec![]);
        assert_eq!(find_executable(tool, &env), Some(on_path));
    }

    #[test]
    fn manual_tools_are_skipped_without_an_override() {
        let cat = catalogue();
        let tool = cat.get("wzzip").expect("wzzip");
        let env = fake_env("windows", vec![], vec![]);
        assert_eq!(
            discover_tool(tool, &Local::default(), &env),
            Status::Skipped {
                reason: SKIP_MANUAL.into()
            }
        );
        // With an override entry but nothing installed it is simply not installed.
        let mut local = Local::default();
        local.overridden.insert("wzzip".into());
        assert_eq!(
            discover_tool(tool, &local, &env),
            Status::Skipped {
                reason: SKIP_NOT_INSTALLED.into()
            }
        );
        // A bad explicit path says so.
        local.paths.insert("wzzip".into(), "no/such/file".into());
        match discover_tool(tool, &local, &env) {
            Status::Skipped { reason } => assert!(reason.starts_with(SKIP_NOT_INSTALLED)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_local_path_is_used_and_its_version_read() {
        // The test binary itself stands in for a tool: libtest `--help` prints "Usage".
        let exe = std::env::current_exe().expect("exe");
        let cat = catalogue();
        let mut tool = cat.get("wzzip").expect("wzzip").clone();
        tool.version.args = vec!["--help".into()];
        tool.version.pattern = "(Usage)".into();
        let mut local = Local::default();
        local.overridden.insert("wzzip".into());
        local
            .paths
            .insert("wzzip".into(), exe.to_string_lossy().into_owned());
        let env = fake_env("windows", vec![], vec![]);
        match discover_tool(&tool, &local, &env) {
            Status::Found { version, path } => {
                assert_eq!(version, "Usage");
                assert_eq!(path, exe);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn list_lines_name_id_status_version_and_path() {
        let cat = catalogue();
        let tool = cat.get("xz").expect("xz").clone();
        let found = Discovered {
            tool: tool.clone(),
            status: Status::Found {
                version: "5.8.1".into(),
                path: PathBuf::from("p/xz"),
            },
            local_override: false,
        };
        let line = list_line(&found);
        assert!(line.starts_with("xz") && line.contains("found") && line.contains("5.8.1"));
        assert!(line.ends_with("p/xz"));
        let skipped = Discovered {
            tool,
            status: Status::Skipped {
                reason: SKIP_NOT_INSTALLED.into(),
            },
            local_override: false,
        };
        assert_eq!(
            list_line(&skipped),
            format!("{:<10} skipped: not installed", "xz")
        );
    }
}
