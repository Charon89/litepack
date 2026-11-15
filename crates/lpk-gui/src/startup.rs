//! Startup hardening (E7-7, R342): retrying the main webview, and the local crash note.
//!
//! After "Install and restart", the installer starts the new GUI about a second after the old one
//! called `process::exit`, while the old WebView2 browser may still hold the user data folder;
//! creating the webview can then fail for a moment. [`retry`] gives it bounded further tries
//! ([`webview_retry`] decides how long). [`append_crash_note`] keeps a short local record of a panic, a failed
//! start or a failed hand-off to the installer: never sent
//! anywhere, at most [`CRASH_LOG_MAX`] bytes, in the local state folder the uninstaller removes.

use std::path::Path;
use std::time::Duration;

/// How often the main webview is tried again, and for how long in all (R342).
pub const WEBVIEW_RETRY_INTERVAL: Duration = Duration::from_millis(500);
/// The whole window of retries for an error other than "in use".
pub const WEBVIEW_RETRY_FOR: Duration = Duration::from_secs(10);
/// The whole window of retries while the user data folder is in use (R358): the old instance's
/// WebView2 browser process can hold it for more than 10 s after that instance has gone.
pub const WEBVIEW_IN_USE_RETRY_FOR: Duration = Duration::from_secs(30);
/// The HRESULTs that mean the user data folder is still in use: another process (the old
/// instance's WebView2 browser) holds the folder or its files. The codes are the same in every
/// language, the text is not.
/// - `0x800700AA`, `ERROR_BUSY` ("The requested resource is in use.", R358);
/// - `0x80070020`, `ERROR_SHARING_VIOLATION` (a file of the folder is open in another process);
/// - `0x80070021`, `ERROR_LOCK_VIOLATION` (a part of a file of the folder is locked by another
///   process).
pub const IN_USE_HRESULTS: [&str; 3] = ["0x800700AA", "0x80070020", "0x80070021"];

/// Whether `error` (a failed try's text) is the user data folder still in use: it names one of
/// [`IN_USE_HRESULTS`], in either case.
pub fn in_use(error: &str) -> bool {
    let error = error.to_ascii_uppercase();
    IN_USE_HRESULTS
        .iter()
        .any(|code| error.contains(&code.to_ascii_uppercase()))
}

/// Whether the main webview is tried again after a try failed with `error`, `spent` after the
/// first try (R358): for up to [`WEBVIEW_IN_USE_RETRY_FOR`] while the folder is in use, for up to
/// [`WEBVIEW_RETRY_FOR`] for any other error.
pub fn webview_retry(error: &str, spent: Duration) -> bool {
    let bound = if in_use(error) {
        WEBVIEW_IN_USE_RETRY_FOR
    } else {
        WEBVIEW_RETRY_FOR
    };
    spent < bound
}

/// How long one WebView2 folder stays in use before the next try uses the other (R362).
pub const WEBVIEW_SWITCH_AFTER: Duration = Duration::from_secs(2);

/// Which of the two WebView2 user data folders a try uses (R362, `paths::webview_dirs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebviewFolder {
    /// `<state>\WebView2`, which every start tries first.
    Primary,
    /// `<state>\WebView2-2`, used while the primary is held by a LitePack that ended abruptly.
    Alternate,
}

impl WebviewFolder {
    /// The other folder.
    pub fn other(self) -> Self {
        match self {
            Self::Primary => Self::Alternate,
            Self::Alternate => Self::Primary,
        }
    }

    /// Its index in `paths::webview_dirs`.
    pub fn index(self) -> usize {
        match self {
            Self::Primary => 0,
            Self::Alternate => 1,
        }
    }
}

/// One failed try of the main webview (R362): the folder it used, whether its error was "in
/// use" ([`in_use`]), and when it ran (the time waited between tries before it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderTry {
    pub folder: WebviewFolder,
    pub in_use: bool,
    pub at: Duration,
}

/// The folder of the next try after the failed `tries` (oldest first), `spent` after the first
/// try; `None` when no try is left ([`webview_retry`]'s bound on the last error). With no try yet,
/// the primary. While the last folder has been in use for [`WEBVIEW_SWITCH_AFTER`] (its run of
/// consecutive "in use" tries, measured from the first), the other one, so both are used in
/// turn while both are in use; any other error keeps the last folder.
pub fn next_webview_folder(tries: &[FolderTry], spent: Duration) -> Option<WebviewFolder> {
    let Some(last) = tries.last() else {
        return Some(WebviewFolder::Primary);
    };
    let bound = if last.in_use {
        WEBVIEW_IN_USE_RETRY_FOR
    } else {
        WEBVIEW_RETRY_FOR
    };
    if spent >= bound {
        return None;
    }
    if !last.in_use {
        return Some(last.folder);
    }
    let since = tries
        .iter()
        .rev()
        .take_while(|t| t.folder == last.folder && t.in_use)
        .last()
        .map_or(last.at, |t| t.at);
    if spent.saturating_sub(since) >= WEBVIEW_SWITCH_AFTER {
        Some(last.folder.other())
    } else {
        Some(last.folder)
    }
}

/// The crash note's file name in the local state folder.
pub const CRASH_LOG: &str = "crash.log";
/// The crash note's bound: older entries go first.
pub const CRASH_LOG_MAX: usize = 64 * 1024;
/// At most this many entries are kept.
pub const CRASH_LOG_ENTRIES: usize = 8;
/// The line that starts each entry.
const ENTRY_MARK: &str = "=== ";

/// Run `attempt` until it succeeds, or until `again(e, spent)` says no more (`spent`: the time
/// waited between tries so far). `on_error(n, e)` sees each failure (n from 1); `sleep` waits
/// `interval` between tries. Returns the last error when every try failed.
pub fn retry<T, E>(
    interval: Duration,
    mut attempt: impl FnMut() -> Result<T, E>,
    mut again: impl FnMut(&E, Duration) -> bool,
    mut on_error: impl FnMut(u32, &E),
    mut sleep: impl FnMut(Duration),
) -> Result<T, E> {
    let mut n = 0;
    let mut spent = Duration::ZERO;
    loop {
        match attempt() {
            Ok(v) => return Ok(v),
            Err(e) => {
                n += 1;
                on_error(n, &e);
                if interval.is_zero() || !again(&e, spent) {
                    return Err(e);
                }
                sleep(interval);
                spent += interval;
            }
        }
    }
}

/// `existing` with `entry` appended, keeping at most [`CRASH_LOG_ENTRIES`] entries and
/// [`CRASH_LOG_MAX`] bytes (the oldest dropped first; an entry alone longer than the bound is cut).
pub fn bounded_crash_log(existing: &str, entry: &str) -> String {
    let mut entry = format!("{ENTRY_MARK}{}\n", entry.trim_end());
    if entry.len() > CRASH_LOG_MAX {
        let mut cut = CRASH_LOG_MAX - 1;
        while !entry.is_char_boundary(cut) {
            cut -= 1;
        }
        entry.truncate(cut);
        entry.push('\n');
    }
    let mut entries: Vec<&str> = Vec::new();
    let mut rest = existing;
    while let Some(start) = rest.find(ENTRY_MARK) {
        let body = &rest[start..];
        let next = body[ENTRY_MARK.len()..]
            .find(&format!("\n{ENTRY_MARK}"))
            .map(|i| i + ENTRY_MARK.len() + 1)
            .unwrap_or(body.len());
        entries.push(&body[..next]);
        rest = &body[next..];
    }
    entries.push(&entry);
    let mut start = entries.len().saturating_sub(CRASH_LOG_ENTRIES);
    while start + 1 < entries.len()
        && entries[start..].iter().map(|e| e.len()).sum::<usize>() > CRASH_LOG_MAX
    {
        start += 1;
    }
    entries[start..].concat()
}

/// Append `entry` to the crash note in `dir` (created if missing), bounded. Errors are ignored:
/// the note must never make a crash worse.
pub fn append_crash_note(dir: &Path, entry: &str) {
    let _ = std::fs::create_dir_all(dir);
    let file = dir.join(CRASH_LOG);
    let existing = std::fs::read(&file)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let _ = std::fs::write(&file, bounded_crash_log(&existing, entry));
}

/// The one crash note a start writes about the main window's tries (R348): the number of tries
/// and the first error, only when a retry happened (`tries` above one); `None` otherwise.
pub fn retry_note(tries: u32, first: Option<&str>) -> Option<String> {
    match first {
        Some(e) if tries > 1 => Some(format!(
            "the main window took {tries} tries, the first failed: {e}"
        )),
        _ => None,
    }
}

/// [`retry_note`] for a start whose main window opened (R362), naming the folder it opened with
/// (`dir`) when that is the alternate.
pub fn opened_note(
    tries: u32,
    first: Option<&str>,
    folder: WebviewFolder,
    dir: Option<&Path>,
) -> Option<String> {
    let note = retry_note(tries, first)?;
    Some(match (folder, dir) {
        (WebviewFolder::Alternate, Some(d)) => format!(
            "{note}; it opened with the second WebView2 folder {}, the first still in use",
            d.display()
        ),
        (WebviewFolder::Alternate, None) => {
            format!("{note}; it opened with the second WebView2 folder, the first still in use")
        }
        (WebviewFolder::Primary, _) => note,
    })
}

/// What [`remove_alternate`] found at `<state>\WebView2-2` (R364).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlternateSweep {
    /// The start's webview runs on the alternate folder itself: nothing is touched.
    InUse,
    /// No alternate folder.
    Absent,
    /// The alternate folder was removed.
    Removed,
    /// Something other than a plain folder (a file, a symbolic link, a junction or another reparse
    /// point) is at that path: left as it is, never followed.
    NotAFolder,
}

/// R364: once a start has created its webview on the primary folder (`used`), remove the
/// alternate WebView2 folder (`<state>\WebView2-2`, `paths::WEBVIEW_ALTERNATE`, made only while the
/// primary was held by a LitePack that had just ended) if it is there; the caller runs this in the
/// background and best effort (an error is logged, never shown; the next start tries again).
///
/// Only that exact path under `state` is removed, and only when it is a plain folder: a file or a
/// reparse point there is left alone, so the removal never follows a link out of the state folder.
/// Inside the folder, `std::fs::remove_dir_all` does not follow links either: it removes a symbolic
/// link or a junction it meets as the link itself, never its target (Rust's documented behaviour
/// since 1.58.1, CVE-2022-21658; `the_alternate_folder_goes_and_a_link_inside_it_is_not_followed`
/// checks it).
pub fn remove_alternate(state: &Path, used: WebviewFolder) -> std::io::Result<AlternateSweep> {
    if used != WebviewFolder::Primary {
        return Ok(AlternateSweep::InUse);
    }
    let dir = state.join(crate::paths::WEBVIEW_ALTERNATE);
    match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AlternateSweep::Absent),
        Err(e) => Err(e),
        Ok(m) if !m.is_dir() || !crate::update::plain(&m) => Ok(AlternateSweep::NotAFolder),
        Ok(_) => std::fs::remove_dir_all(&dir).map(|()| AlternateSweep::Removed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A directory link at `link` to `target`: a symbolic link on Unix, a junction on Windows
    /// (`mklink /J` needs no privilege, as lpk-bench's scan tests use it).
    fn dir_link(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        {
            let made = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .expect("spawn cmd for mklink /J");
            assert!(made.status.success(), "mklink /J failed");
        }
    }

    fn webview_state() -> (tempfile::TempDir, std::path::PathBuf) {
        let state = tempfile::tempdir().unwrap();
        let alternate = state.path().join(crate::paths::WEBVIEW_ALTERNATE);
        (state, alternate)
    }

    #[test]
    fn the_alternate_folder_is_removed_after_a_start_on_the_primary() {
        let (state, alternate) = webview_state();
        std::fs::create_dir_all(alternate.join("EBWebView").join("Default")).unwrap();
        std::fs::write(alternate.join("EBWebView").join("Local State"), b"{}").unwrap();
        std::fs::create_dir(state.path().join("WebView2")).unwrap();
        std::fs::write(state.path().join("settings.json"), b"{}").unwrap();
        assert_eq!(
            remove_alternate(state.path(), WebviewFolder::Primary).unwrap(),
            AlternateSweep::Removed
        );
        assert!(!alternate.exists());
        // Nothing else in the state folder is touched; a second start finds nothing to do.
        assert!(state.path().join("WebView2").is_dir());
        assert!(state.path().join("settings.json").is_file());
        assert_eq!(
            remove_alternate(state.path(), WebviewFolder::Primary).unwrap(),
            AlternateSweep::Absent
        );
    }

    #[test]
    fn nothing_is_removed_when_the_start_used_the_alternate() {
        let (state, alternate) = webview_state();
        std::fs::create_dir_all(alternate.join("EBWebView")).unwrap();
        assert_eq!(
            remove_alternate(state.path(), WebviewFolder::Alternate).unwrap(),
            AlternateSweep::InUse
        );
        assert!(alternate.join("EBWebView").is_dir());
    }

    #[test]
    fn a_file_at_the_alternate_path_stays() {
        let (state, alternate) = webview_state();
        std::fs::write(&alternate, b"not a folder").unwrap();
        assert_eq!(
            remove_alternate(state.path(), WebviewFolder::Primary).unwrap(),
            AlternateSweep::NotAFolder
        );
        assert_eq!(std::fs::read(&alternate).unwrap(), b"not a folder");
    }

    #[test]
    fn a_link_at_the_alternate_path_is_not_followed() {
        let (state, alternate) = webview_state();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep.txt"), b"the user's").unwrap();
        dir_link(outside.path(), &alternate);
        assert_eq!(
            remove_alternate(state.path(), WebviewFolder::Primary).unwrap(),
            AlternateSweep::NotAFolder
        );
        assert!(std::fs::symlink_metadata(&alternate).is_ok());
        assert_eq!(
            std::fs::read(outside.path().join("keep.txt")).unwrap(),
            b"the user's"
        );
    }

    #[test]
    fn the_alternate_folder_goes_and_a_link_inside_it_is_not_followed() {
        let (state, alternate) = webview_state();
        std::fs::create_dir(&alternate).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep.txt"), b"the user's").unwrap();
        dir_link(outside.path(), &alternate.join("link"));
        assert_eq!(
            remove_alternate(state.path(), WebviewFolder::Primary).unwrap(),
            AlternateSweep::Removed
        );
        assert!(!alternate.exists());
        assert_eq!(
            std::fs::read(outside.path().join("keep.txt")).unwrap(),
            b"the user's"
        );
    }

    #[test]
    fn one_retry_note_per_start_only_after_a_retry() {
        assert_eq!(retry_note(1, None), None);
        assert_eq!(retry_note(1, Some("busy")), None);
        assert_eq!(
            retry_note(4, Some("busy")).as_deref(),
            Some("the main window took 4 tries, the first failed: busy")
        );
    }

    #[test]
    fn retry_succeeds_after_failures() {
        let tries = Cell::new(0);
        let mut errors = Vec::new();
        let mut slept = Duration::ZERO;
        let out: Result<u32, String> = retry(
            Duration::from_millis(500),
            || {
                tries.set(tries.get() + 1);
                if tries.get() <= 3 {
                    Err(format!("fail {}", tries.get()))
                } else {
                    Ok(tries.get())
                }
            },
            |_, spent| spent < Duration::from_secs(10),
            |n, e| errors.push(format!("{n}: {e}")),
            |d| slept += d,
        );
        assert_eq!(out, Ok(4));
        assert_eq!(errors, ["1: fail 1", "2: fail 2", "3: fail 3"]);
        assert_eq!(slept, Duration::from_millis(1500));
    }

    #[test]
    fn retry_gives_up_after_the_window() {
        let tries = Cell::new(0u32);
        let mut slept = Duration::ZERO;
        let out: Result<(), u32> = retry(
            Duration::from_millis(500),
            || {
                tries.set(tries.get() + 1);
                Err(tries.get())
            },
            |_, spent| spent < Duration::from_secs(10),
            |_, _| {},
            |d| slept += d,
        );
        // The first try and 20 retries, 500 ms apart: 10 s of waiting in all.
        assert_eq!(out, Err(21));
        assert_eq!(slept, Duration::from_secs(10));
    }

    #[test]
    fn retry_first_try_needs_no_wait() {
        let mut slept = false;
        let out: Result<u8, ()> = retry(
            WEBVIEW_RETRY_INTERVAL,
            || Ok(7),
            |_: &(), _| panic!("no decision"),
            |_, _| panic!("no error"),
            |_| slept = true,
        );
        assert_eq!(out, Ok(7));
        assert!(!slept);
    }

    const BUSY: &str = r#"HRESULT(0x800700AA) "The requested resource is in use.""#;

    #[test]
    fn an_in_use_folder_is_waited_out_longer_than_other_errors() {
        assert!(in_use(BUSY));
        assert!(in_use("hresult(0x800700aa)"));
        assert!(!in_use("HRESULT(0x80070005) Access is denied."));
        assert!(webview_retry(BUSY, Duration::from_secs(12)));
        assert!(webview_retry(BUSY, Duration::from_millis(29_500)));
        assert!(!webview_retry(BUSY, Duration::from_secs(30)));
        assert!(!webview_retry(BUSY, Duration::from_secs(31)));
        assert!(webview_retry("another error", Duration::from_millis(9_500)));
        assert!(!webview_retry("another error", Duration::from_secs(10)));
        assert!(!webview_retry("another error", Duration::from_secs(11)));
    }

    const SHARING: &str = "HRESULT(0x80070020) The process cannot access the file because it is being used by another process.";
    const LOCK: &str = "HRESULT(0x80070021) The process cannot access the file because another process has locked a portion of the file.";

    #[test]
    fn sharing_and_lock_violations_are_in_use_too() {
        for (code, text) in [
            ("0x800700AA", BUSY),
            ("0x80070020", SHARING),
            ("0x80070021", LOCK),
        ] {
            assert!(IN_USE_HRESULTS.contains(&code));
            assert!(in_use(text), "{code}");
            assert!(
                in_use(&format!("hresult({})", code.to_ascii_lowercase())),
                "{code}"
            );
            assert!(
                in_use(&format!("HRESULT({})", code.to_ascii_uppercase())),
                "{code}"
            );
            // The 30 s bound, not the 10 s one.
            assert!(webview_retry(text, Duration::from_secs(12)), "{code}");
            assert!(webview_retry(text, Duration::from_millis(29_500)), "{code}");
            assert!(!webview_retry(text, Duration::from_secs(30)), "{code}");
        }
        // Neighbouring and other codes are not.
        for other in [
            "HRESULT(0x80070005) Access is denied.",
            "HRESULT(0x8007001F)",
            "HRESULT(0x80070022)",
            "HRESULT(0x800700AB)",
            "HRESULT(0x80004005) Unspecified error",
            "another error",
        ] {
            assert!(!in_use(other), "{other}");
            assert!(!webview_retry(other, Duration::from_secs(10)), "{other}");
        }
    }

    #[test]
    fn each_in_use_code_switches_after_2_s() {
        for text in [BUSY, SHARING, LOCK] {
            // The primary fails with `text`, the alternate opens.
            let (used, ended) = run(|f, _| (f == WebviewFolder::Primary).then(|| in_use(text)));
            assert!(!ended, "{text}");
            let mut want = vec![WebviewFolder::Primary; 5];
            want.push(WebviewFolder::Alternate);
            assert_eq!(used, want, "{text}");
            // Both fail with `text` for the whole 30 s.
            let (used, ended) = run(|_, _| Some(in_use(text)));
            assert!(ended, "{text}");
            assert_eq!(used.len(), 61, "{text}");
        }
    }

    #[test]
    fn retry_waits_out_an_in_use_folder_for_30_s() {
        let tries = Cell::new(0u32);
        let mut slept = Duration::ZERO;
        let out: Result<(), String> = retry(
            WEBVIEW_RETRY_INTERVAL,
            || {
                tries.set(tries.get() + 1);
                Err(BUSY.to_owned())
            },
            |e, spent| webview_retry(e, spent),
            |_, _| {},
            |d| slept += d,
        );
        // The first try and 60 retries, 500 ms apart.
        assert_eq!(out.unwrap_err(), BUSY);
        assert_eq!(tries.get(), 61);
        assert_eq!(slept, WEBVIEW_IN_USE_RETRY_FOR);
    }

    #[test]
    fn retry_stops_at_10_s_when_the_error_changes() {
        let tries = Cell::new(0u32);
        let out: Result<(), String> = retry(
            WEBVIEW_RETRY_INTERVAL,
            || {
                tries.set(tries.get() + 1);
                // In use for 12 s, then another error.
                if tries.get() <= 25 {
                    Err(BUSY.to_owned())
                } else {
                    Err("another error".to_owned())
                }
            },
            |e, spent| webview_retry(e, spent),
            |_, _| {},
            |_| {},
        );
        assert_eq!(out.unwrap_err(), "another error");
        assert_eq!(tries.get(), 26);
    }

    const MS: u64 = 500;

    /// Every try in `busy` (true: "in use") on the folders `next_webview_folder` picks, 500 ms
    /// apart; the folders used, and whether the bound ended it.
    fn run(errors: impl Fn(WebviewFolder, Duration) -> Option<bool>) -> (Vec<WebviewFolder>, bool) {
        let mut tries = Vec::new();
        let mut used = Vec::new();
        let mut spent = Duration::ZERO;
        let mut folder = next_webview_folder(&tries, spent).unwrap();
        loop {
            used.push(folder);
            let Some(busy) = errors(folder, spent) else {
                return (used, false);
            };
            tries.push(FolderTry {
                folder,
                in_use: busy,
                at: spent,
            });
            match next_webview_folder(&tries, spent) {
                Some(f) => folder = f,
                None => return (used, true),
            }
            spent += Duration::from_millis(MS);
        }
    }

    #[test]
    fn the_primary_folder_comes_first() {
        assert_eq!(
            next_webview_folder(&[], Duration::ZERO),
            Some(WebviewFolder::Primary)
        );
    }

    #[test]
    fn in_use_for_1_s_stays_on_the_primary() {
        // Busy at 0, 0.5 and 1 s, free at 1.5 s.
        let (used, ended) = run(|_, at| (at < Duration::from_millis(1_500)).then_some(true));
        assert!(!ended);
        assert_eq!(used, vec![WebviewFolder::Primary; 4]);
    }

    #[test]
    fn in_use_for_2_s_switches_to_the_alternate() {
        let (used, ended) = run(|f, _| (f == WebviewFolder::Primary).then_some(true));
        assert!(!ended);
        // Tries at 0, 0.5, 1, 1.5 and 2 s on the primary, then the alternate opens.
        let mut want = vec![WebviewFolder::Primary; 5];
        want.push(WebviewFolder::Alternate);
        assert_eq!(used, want);
    }

    #[test]
    fn both_in_use_alternates_within_the_bound() {
        let (used, ended) = run(|_, _| Some(true));
        assert!(ended);
        // 30 s at 500 ms: tries at 0..=30 s, 61 in all; five per folder in turn.
        assert_eq!(used.len(), 61);
        for (i, f) in used.iter().enumerate() {
            let want = if (i / 5) % 2 == 0 {
                WebviewFolder::Primary
            } else {
                WebviewFolder::Alternate
            };
            assert_eq!(*f, want, "try {i}");
        }
    }

    #[test]
    fn another_error_never_switches() {
        let (used, ended) = run(|_, _| Some(false));
        assert!(ended);
        assert_eq!(used.len(), 21);
        assert!(used.iter().all(|f| *f == WebviewFolder::Primary));
        // In use, then another error on the alternate: it stays there.
        let (used, ended) = run(|f, _| Some(f == WebviewFolder::Primary));
        assert!(ended);
        assert_eq!(used[..5], [WebviewFolder::Primary; 5]);
        assert!(used[5..].iter().all(|f| *f == WebviewFolder::Alternate));
    }

    #[test]
    fn the_bound_still_ends_it() {
        let busy = |folder, at| FolderTry {
            folder,
            in_use: true,
            at,
        };
        let tries = [busy(WebviewFolder::Primary, Duration::ZERO)];
        assert_eq!(next_webview_folder(&tries, Duration::from_secs(30)), None);
        assert!(next_webview_folder(&tries, Duration::from_millis(29_500)).is_some());
        let other = [FolderTry {
            folder: WebviewFolder::Alternate,
            in_use: false,
            at: Duration::from_secs(10),
        }];
        assert_eq!(next_webview_folder(&other, Duration::from_secs(10)), None);
        assert_eq!(
            next_webview_folder(&other, Duration::from_millis(9_500)),
            Some(WebviewFolder::Alternate)
        );
    }

    #[test]
    fn the_note_names_the_alternate_folder_only() {
        let dir = Path::new("state").join("WebView2-2");
        assert_eq!(
            opened_note(1, None, WebviewFolder::Alternate, Some(&dir)),
            None
        );
        assert_eq!(
            opened_note(3, Some("busy"), WebviewFolder::Primary, Some(&dir)),
            retry_note(3, Some("busy"))
        );
        let note = opened_note(6, Some("busy"), WebviewFolder::Alternate, Some(&dir)).unwrap();
        assert!(
            note.starts_with(&retry_note(6, Some("busy")).unwrap()),
            "{note}"
        );
        assert!(note.contains(&dir.display().to_string()), "{note}");
        assert!(note.contains("second WebView2 folder"), "{note}");
    }

    #[test]
    fn crash_log_keeps_the_last_entries() {
        let mut log = String::new();
        for i in 0..20 {
            log = bounded_crash_log(&log, &format!("panic {i}\nat src/x.rs:1"));
        }
        assert_eq!(log.matches(ENTRY_MARK).count(), CRASH_LOG_ENTRIES);
        assert!(log.starts_with("=== panic 12\n"), "{log}");
        assert!(log.ends_with("=== panic 19\nat src/x.rs:1\n"), "{log}");
    }

    #[test]
    fn crash_log_stays_under_the_byte_bound() {
        let big = "x".repeat(20 * 1024);
        let mut log = String::new();
        for _ in 0..6 {
            log = bounded_crash_log(&log, &big);
            assert!(log.len() <= CRASH_LOG_MAX, "{}", log.len());
        }
        assert_eq!(log.matches(ENTRY_MARK).count(), 3);
        let huge = "é".repeat(CRASH_LOG_MAX);
        let log = bounded_crash_log(&log, &huge);
        assert!(log.len() <= CRASH_LOG_MAX);
        assert_eq!(log.matches(ENTRY_MARK).count(), 1);
    }

    #[test]
    fn crash_note_appends_in_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("state");
        append_crash_note(&sub, "one");
        append_crash_note(&sub, "two");
        let text = std::fs::read_to_string(sub.join(CRASH_LOG)).unwrap();
        assert_eq!(text, "=== one\n=== two\n");
    }
}
