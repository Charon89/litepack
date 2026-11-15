//! The update check's gate (E5-7; `docs/notes/e5-design.md` section 4, `docs/notes/e7-design.md`
//! section 6), plain Rust on every OS: the request itself is `tauri-plugin-updater`'s, made from
//! Rust by the host (`src/updates.rs`, Windows).
//!
//! The key and the endpoint live in one place, `tauri.conf.json`'s `plugins.updater`, which
//! E7-7 fills with the owner's public key and the release URL. Until then the key is
//! [`PLACEHOLDER_KEY`], and a key that is not a minisign public key fails closed: no request and
//! no install, never an unverified one. With the check off in the settings, nothing is
//! requested at all.

/// The key `tauri.conf.json` carries until E7-1 supplies the owner's (E7-7 runs on test keys).
pub const PLACEHOLDER_KEY: &str = "E7-7 replaces this with the release signing public key";

/// The one place the repository is named: every release URL below is built from it.
macro_rules! repository {
    () => {
        "Charon89/litepack"
    };
}

/// The GitHub repository the releases come from.
pub const REPOSITORY: &str = repository!();

/// The prefix every download URL must start with (E7-7, `docs/notes/e7-design.md` section 6):
/// the plugin refuses a non-HTTPS endpoint but only warns about a download URL, so the GUI
/// refuses a `url` outside the repository's release assets before anything is downloaded.
pub const RELEASE_DOWNLOAD_PREFIX: &str =
    concat!("https://github.com/", repository!(), "/releases/download/");

/// The release page a portable copy links to (it never downloads, section 6).
pub const RELEASES_PAGE: &str = concat!("https://github.com/", repository!(), "/releases/latest");

/// Whether `url` (as the plugin parsed it, so `..` segments are already resolved) is one of the
/// release assets under `prefix` ([`RELEASE_DOWNLOAD_PREFIX`] in the shipped GUI; a local
/// server's in the tests). Compared byte for byte: the path is case-sensitive, so no case
/// folding; and something must follow the prefix, with no backslash, query, fragment or control
/// character.
pub fn download_allowed(url: &str, prefix: &str) -> bool {
    let Some(rest) = url.strip_prefix(prefix) else {
        return false;
    };
    prefix.ends_with('/')
        && !rest.is_empty()
        && !rest.starts_with('/')
        && !rest
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | '?' | '#'))
}

/// Whether the installer may start LitePack again after an update (R323). Its `/R` restart
/// replays the launch's arguments (`/ARGS`), so a GUI started with a verb (`--extract-here
/// C:\x.lpk`) would run it again: the installer restarts LitePack only when it was started with
/// no argument (`args` is the whole command line, the executable first). Otherwise the update
/// installs and the user starts LitePack again.
pub fn restart_after_install<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> bool {
    args.len() <= 1
}

/// What a copy does with a newer version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The installed copy: notes with "Install and restart", "Later" and "Skip this version".
    Install,
    /// A portable copy: a notice with a link to the release page, never a download (the
    /// installer would turn the portable copy into an installed one).
    NotifyOnly,
}

/// The mode for a copy that is `portable` or not.
pub fn mode(portable: bool) -> Mode {
    if portable {
        Mode::NotifyOnly
    } else {
        Mode::Install
    }
}

/// How often the check runs at most.
pub const SECONDS_PER_DAY: u64 = 86_400;

/// The updater's local state (`update.json` in the state folder, never the page's to write): the
/// day of the last check and the version the user skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct State {
    /// The last check's day, in days since the Unix epoch (UTC).
    #[serde(default)]
    pub last_check_day: Option<u64>,
    /// The version "Skip this version" was pressed for.
    #[serde(default)]
    pub skipped: Option<String>,
}

impl State {
    /// Read `path`; a missing or unreadable file is the empty state (a check is then due).
    pub fn load(path: &std::path::Path) -> State {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Write to `path`, temp-then-rename.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(self).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(&tmp, path)
    }

    /// Whether a check is due on `today` (days since the epoch): at most once a day.
    pub fn due(&self, today: u64) -> bool {
        self.last_check_day != Some(today)
    }

    /// Whether `version` is offered: not the skipped one, nor older than it (a newer one is).
    pub fn offers(&self, version: &str) -> bool {
        match &self.skipped {
            None => true,
            Some(s) => match (numeric(version), numeric(s)) {
                (Some(v), Some(s)) => v > s,
                _ => version.trim_start_matches('v') != s.trim_start_matches('v'),
            },
        }
    }
}

/// Today in days since the Unix epoch (UTC).
pub fn today() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / SECONDS_PER_DAY)
        .unwrap_or(0)
}

/// `1.2.3` (an optional `v`, no pre-release) as numbers, for the skip comparison.
fn numeric(v: &str) -> Option<Vec<u64>> {
    v.trim_start_matches('v')
        .split('.')
        .map(|p| p.parse().ok())
        .collect()
}

/// R365: the version in the name of a folder the updater plugin downloads into,
/// `{app}-{version}-updater-{suffix}` (`tempfile`'s six random ASCII letters and digits as the
/// suffix; the version numeric, as [`numeric`] reads it). `None` for any other name: not ours.
pub fn download_folder_version<'a>(name: &'a str, app: &str) -> Option<&'a str> {
    let rest = name.strip_prefix(app)?.strip_prefix('-')?;
    let at = rest.rfind("-updater-")?;
    let (version, suffix) = (&rest[..at], &rest[at + "-updater-".len()..]);
    let suffix_ok = suffix.len() == 6 && suffix.bytes().all(|b| b.is_ascii_alphanumeric());
    (suffix_ok && numeric(version).is_some()).then_some(version)
}

/// Whether `meta` is a plain entry: no symbolic link, junction or other reparse point.
fn plain(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        /// `FILE_ATTRIBUTE_REPARSE_POINT`.
        const REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & REPARSE_POINT != 0 {
            return false;
        }
    }
    !meta.file_type().is_symlink()
}

/// R365: remove one download folder of the updater plugin (`dir`, named as
/// [`download_folder_version`] reads it): only a real folder, holding nothing but its one regular
/// file `{app}-{version}-installer.exe` (or nothing). That file goes, then the folder,
/// non-recursively. A folder holding anything else is left untouched, its installer too: it is
/// not plainly the plugin's.
pub fn remove_download(dir: &std::path::Path, app: &str) -> std::io::Result<()> {
    let refuse = |what: &str| Err(std::io::Error::other(format!("{}: {what}", dir.display())));
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let Some(version) = download_folder_version(name, app) else {
        return refuse("not an update download");
    };
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || !plain(&meta) {
        return refuse("not a plain folder");
    }
    let installer = format!("{app}-{version}-installer.exe");
    for entry in std::fs::read_dir(dir)? {
        if entry?.file_name() != installer.as_str() {
            return refuse("holds another entry");
        }
    }
    let file = dir.join(&installer);
    match std::fs::symlink_metadata(&file) {
        Ok(m) if m.is_file() && plain(&m) => std::fs::remove_file(&file)?,
        Ok(_) => return refuse("its installer is not a regular file"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::fs::remove_dir(dir)
}

/// R365: remove the updater plugin's download folders in `temp` whose version is at most
/// `running` (a newer one may be the download this process is about to install; a non-numeric
/// one is left). The number of such folders still there, for the caller's retries: 0 when none
/// is left (or `temp` cannot be read). Errors are not reported.
pub fn sweep_downloads(temp: &std::path::Path, app: &str, running: &str) -> usize {
    let Some(running) = numeric(running) else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(temp) else {
        return 0;
    };
    let mut left = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(version) = name.to_str().and_then(|n| download_folder_version(n, app)) else {
            continue;
        };
        if numeric(version).is_none_or(|v| v > running) {
            continue;
        }
        let path = entry.path();
        let folder = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir() && plain(&m));
        if folder && remove_download(&path, app).is_err() {
            left += 1;
        }
    }
    left
}

/// What the gate decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// The user turned the check off: no request.
    Off,
    /// No usable public key: no request (fail closed).
    NoKey,
    /// Ask the endpoint.
    Check,
}

/// The gate for a check with the setting `enabled` and the configured `pubkey`.
pub fn gate(enabled: bool, pubkey: &str) -> Gate {
    if !enabled {
        Gate::Off
    } else if !key_is_usable(pubkey) {
        Gate::NoKey
    } else {
        Gate::Check
    }
}

/// Whether `pubkey` is a Tauri updater public key: the base64 of a minisign public key file,
/// whose second line is the base64 of `Ed`, an 8-byte key id and a 32-byte Ed25519 key.
pub fn key_is_usable(pubkey: &str) -> bool {
    let Some(text) = base64(pubkey.trim()).and_then(|b| String::from_utf8(b).ok()) else {
        return false;
    };
    let mut lines = text.lines();
    let comment_ok = lines
        .next()
        .is_some_and(|l| l.starts_with("untrusted comment:"));
    let key = lines.next().and_then(|l| base64(l.trim()));
    comment_ok && key.is_some_and(|k| k.len() == 42 && k.starts_with(b"Ed"))
}

/// The updater's configuration in `tauri.conf.json`: its endpoints and public key.
pub fn config(conf: &serde_json::Value) -> (Vec<String>, String) {
    let u = &conf["plugins"]["updater"];
    let endpoints = u["endpoints"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    (
        endpoints,
        u["pubkey"].as_str().unwrap_or_default().to_owned(),
    )
}

/// Standard base64 with padding; `None` if `s` is not.
fn base64(s: &str) -> Option<Vec<u8>> {
    let s = s.as_bytes();
    if s.is_empty() || !s.len().is_multiple_of(4) {
        return None;
    }
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let chunks = s.len() / 4;
    for (i, q) in s.chunks(4).enumerate() {
        let pad = q.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && i + 1 != chunks) {
            return None;
        }
        let mut n = 0u32;
        for &c in &q[..4 - pad] {
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * pad as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minisign public key made for these tests alone: random bytes, no private half exists.
    pub const TEST_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDQ2MkQ5NjQ3QTlDRTk0NDUKUldSRmxNNnBSNVl0UnVQbkVQMTlDd25BSGY4V083dHVYY3U0ajEralhBZ2RaazZKcE9RaWJGNVkK";

    #[test]
    fn base64_decodes() {
        assert_eq!(base64("TWFu").unwrap(), b"Man");
        assert_eq!(base64("TWE=").unwrap(), b"Ma");
        assert_eq!(base64("TQ==").unwrap(), b"M");
        for bad in ["", "TWF", "TQ=a", "T===", "TQ==TWFu", "TW!u"] {
            assert!(base64(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_placeholder_and_malformed_keys_fail_closed() {
        assert!(key_is_usable(TEST_KEY));
        for bad in [
            PLACEHOLDER_KEY,
            "",
            "   ",
            "RWRFlM6pR5YtRuPnEP19CwnAHf8WO7tuXcu4j1+jXAgdZk6JpOQibF5Y",
            "dW50cnVzdGVk",
        ] {
            assert!(!key_is_usable(bad), "{bad}");
            assert_eq!(gate(true, bad), Gate::NoKey);
        }
        assert_eq!(gate(true, TEST_KEY), Gate::Check);
        assert_eq!(gate(false, TEST_KEY), Gate::Off);
        assert_eq!(gate(false, PLACEHOLDER_KEY), Gate::Off);
    }

    #[test]
    fn the_release_prefix_is_built_from_the_repository() {
        assert_eq!(
            RELEASE_DOWNLOAD_PREFIX,
            format!("https://github.com/{REPOSITORY}/releases/download/")
        );
        assert!(RELEASES_PAGE.starts_with(&format!("https://github.com/{REPOSITORY}/")));
    }

    #[test]
    fn only_release_assets_may_be_downloaded() {
        let p = RELEASE_DOWNLOAD_PREFIX;
        let ok = format!("{p}v0.2.0/LitePack_0.2.0_x64-setup.exe");
        assert!(download_allowed(&ok, p));
        for bad in [
            // Another repository, another host, plain HTTP, another case of the path.
            "https://github.com/Charon89/other/releases/download/v1/x.exe".to_owned(),
            "https://github.com/charon89/litepack/releases/download/v1/x.exe".to_owned(),
            "https://github.com/Charon89/LitePack/releases/download/v1/x.exe".to_owned(),
            "http://github.com/Charon89/litepack/releases/download/v1/x.exe".to_owned(),
            "https://github.com.evil.example/Charon89/litepack/releases/download/v1/x.exe"
                .to_owned(),
            "https://github.com@evil.example/Charon89/litepack/releases/download/v1/x.exe"
                .to_owned(),
            "https://evil.example/https://github.com/Charon89/litepack/releases/download/x"
                .to_owned(),
            // The prefix alone, or with something odd after it.
            p.to_owned(),
            format!("{p}/x.exe"),
            format!("{p}v1/x.exe?y=1"),
            format!("{p}v1/x.exe#y"),
            format!("{p}v1\\x.exe"),
            format!("{p}v1/x\nexe"),
            // Not under `releases/download/`.
            "https://github.com/Charon89/litepack/archive/v1.zip".to_owned(),
            String::new(),
        ] {
            assert!(!download_allowed(&bad, p), "{bad}");
        }
        // A prefix without its closing slash allows nothing.
        assert!(!download_allowed(&ok, p.trim_end_matches('/')));
    }

    #[test]
    fn the_restart_never_replays_a_launch_verb() {
        assert!(restart_after_install(&["lpk-gui.exe"]));
        assert!(restart_after_install::<&str>(&[]));
        for args in [
            &["lpk-gui.exe", "--extract-here", "C:\\x.lpk"][..],
            &["lpk-gui.exe", "--test", "C:\\x.lpk"][..],
            &["lpk-gui.exe", "C:\\x.lpk"][..],
            &["lpk-gui.exe", "--shell-file", "C:\\t\\a.json"][..],
        ] {
            assert!(!restart_after_install(args), "{args:?}");
        }
    }

    #[test]
    fn portable_copies_only_notify() {
        assert_eq!(mode(true), Mode::NotifyOnly);
        assert_eq!(mode(false), Mode::Install);
    }

    #[test]
    fn the_check_runs_at_most_once_a_day() {
        let mut s = State::default();
        assert!(s.due(20_000));
        s.last_check_day = Some(20_000);
        assert!(!s.due(20_000));
        assert!(s.due(20_001));
        let now = today();
        assert!(now > 20_000, "{now}");
    }

    #[test]
    fn a_skipped_version_is_not_offered_again_but_a_newer_one_is() {
        let mut s = State::default();
        assert!(s.offers("0.2.0"));
        s.skipped = Some("0.2.0".into());
        assert!(!s.offers("0.2.0"));
        assert!(!s.offers("v0.2.0"));
        assert!(!s.offers("0.1.9"));
        assert!(s.offers("0.2.1"));
        assert!(s.offers("0.10.0"));
        s.skipped = Some("0.2.0-beta".into());
        assert!(!s.offers("0.2.0-beta"));
        assert!(s.offers("0.2.0"));
    }

    #[test]
    fn the_state_survives_a_restart_and_a_bad_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state").join("update.json");
        assert_eq!(State::load(&p), State::default());
        let s = State {
            last_check_day: Some(20_400),
            skipped: Some("0.3.0".into()),
        };
        s.save(&p).unwrap();
        assert_eq!(State::load(&p), s);
        std::fs::write(&p, b"{not json").unwrap();
        assert_eq!(State::load(&p), State::default());
    }

    /// Every key anywhere under `v`, lower-cased.
    fn keys(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, v) in m {
                    out.push(k.to_lowercase());
                    keys(v, out);
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|v| keys(v, out)),
            _ => {}
        }
    }

    #[test]
    fn the_shipped_configuration_sets_no_dangerous_option_and_no_http_endpoint() {
        let conf: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let mut all = Vec::new();
        keys(&conf, &mut all);
        for k in &all {
            assert!(!k.contains("dangerous"), "{k}");
        }
        let u = &conf["plugins"]["updater"];
        assert_eq!(u["requireSignedVersion"], true);
        assert_ne!(u["allowDowngrades"], true);
        assert_eq!(u["windows"]["installMode"], "passive");
        let (endpoints, _) = config(&conf);
        assert!(!endpoints.is_empty());
        for e in &endpoints {
            assert!(
                e.starts_with(&format!("https://github.com/{REPOSITORY}/releases/")),
                "{e}"
            );
        }
    }

    #[test]
    fn the_shipped_configuration_fails_closed_until_e7_7() {
        let conf: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let (endpoints, key) = config(&conf);
        assert_eq!(endpoints.len(), 1);
        assert_eq!(key, PLACEHOLDER_KEY);
        assert_eq!(gate(true, &key), Gate::NoKey);
    }

    /// A download folder `name` in `temp`, with its installer (and `extra`, if given).
    fn download(temp: &std::path::Path, name: &str, extra: Option<&str>) -> std::path::PathBuf {
        let dir = temp.join(name);
        std::fs::create_dir(&dir).unwrap();
        let version = download_folder_version(name, "LitePack").unwrap_or("x");
        std::fs::write(dir.join(format!("LitePack-{version}-installer.exe")), b"MZ").unwrap();
        if let Some(extra) = extra {
            std::fs::write(dir.join(extra), b"other").unwrap();
        }
        dir
    }

    #[test]
    fn a_download_folder_name_is_parsed_strictly() {
        let v = |n: &'static str| download_folder_version(n, "LitePack");
        assert_eq!(v("LitePack-0.0.2-updater-5EsJTL"), Some("0.0.2"));
        assert_eq!(v("LitePack-1.10.0-updater-abc123"), Some("1.10.0"));
        assert_eq!(v("LitePack-0.0.2-updater-5EsJT"), None);
        assert_eq!(v("LitePack-0.0.2-updater-5EsJTLx"), None);
        assert_eq!(v("LitePack-0.0.2-updater-5Es-TL"), None);
        assert_eq!(v("Other-0.0.2-updater-5EsJTL"), None);
        assert_eq!(v("LitePackX-0.0.2-updater-5EsJTL"), None);
        assert_eq!(v("LitePack-0.0.2-beta-updater-5EsJTL"), None);
        assert_eq!(v("LitePack--updater-5EsJTL"), None);
    }

    #[test]
    fn the_sweep_removes_only_plain_downloads_up_to_the_running_version() {
        let temp = tempfile::tempdir().unwrap();
        let t = temp.path();
        let same = download(t, "LitePack-0.0.2-updater-5EsJTL", None);
        let older = download(t, "LitePack-0.0.1-updater-aaaaaa", None);
        let extra = download(t, "LitePack-0.0.1-updater-bbbbbb", Some("notes.txt"));
        let newer = download(t, "LitePack-0.0.3-updater-cccccc", None);
        let short = download(t, "LitePack-0.0.1-updater-ddddd", None);
        let other = download(t, "Other-0.0.1-updater-eeeeee", None);
        let beta = download(t, "LitePack-0.0.1-beta-updater-ffffff", None);
        let file = t.join("LitePack-0.0.1-updater-gggggg");
        std::fs::write(&file, b"a file").unwrap();
        // The folder with another file counts as left; the rest are not candidates.
        assert_eq!(sweep_downloads(t, "LitePack", "0.0.2"), 1);
        assert!(!same.exists() && !older.exists());
        // Left untouched, its installer too: the folder is not plainly the plugin's.
        assert!(extra.join("LitePack-0.0.1-installer.exe").is_file());
        assert!(extra.join("notes.txt").is_file());
        assert!(newer.join("LitePack-0.0.3-installer.exe").is_file());
        for d in [&short, &other, &beta] {
            assert!(d.is_dir(), "{}", d.display());
        }
        assert_eq!(std::fs::read(&file).unwrap(), b"a file");
        // Once the extra file is gone, the next attempt removes the folder.
        std::fs::remove_file(extra.join("notes.txt")).unwrap();
        assert_eq!(sweep_downloads(t, "LitePack", "0.0.2"), 0);
        assert!(!extra.exists() && newer.is_dir());
        // A non-numeric running version removes nothing.
        assert_eq!(sweep_downloads(t, "LitePack", "dev"), 0);
        assert!(newer.is_dir());
    }

    #[test]
    fn an_empty_download_folder_goes_and_a_file_is_never_removed() {
        let temp = tempfile::tempdir().unwrap();
        let empty = temp.path().join("LitePack-0.0.2-updater-5EsJTL");
        std::fs::create_dir(&empty).unwrap();
        remove_download(&empty, "LitePack").unwrap();
        assert!(!empty.exists());
        let file = temp.path().join("LitePack-0.0.2-updater-5EsJTL");
        std::fs::write(&file, b"x").unwrap();
        assert!(remove_download(&file, "LitePack").is_err());
        assert!(file.is_file());
        // An installer that is a folder, not a regular file, stays.
        let odd = temp.path().join("LitePack-0.0.2-updater-zzzzzz");
        std::fs::create_dir_all(odd.join("LitePack-0.0.2-installer.exe")).unwrap();
        assert!(remove_download(&odd, "LitePack").is_err());
        assert!(odd.join("LitePack-0.0.2-installer.exe").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_download_folder_stays() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("LitePack-0.0.2-installer.exe"), b"MZ").unwrap();
        let link = temp.path().join("LitePack-0.0.2-updater-5EsJTL");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(sweep_downloads(temp.path(), "LitePack", "0.0.2"), 0);
        assert!(remove_download(&link, "LitePack").is_err());
        assert!(link.exists() && real.join("LitePack-0.0.2-installer.exe").is_file());
    }
}
