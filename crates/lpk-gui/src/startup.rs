//! Startup hardening (E7-7, R342): retrying the main webview, and the local crash note.
//!
//! After "Install and restart", the installer starts the new GUI about a second after the old one
//! called `process::exit`, while the old WebView2 browser may still hold the user data folder;
//! creating the webview can then fail for a moment. [`retry`] gives it a bounded number of
//! further tries. [`append_crash_note`] keeps a short local record of a panic, a failed
//! start or a failed hand-off to the installer: never sent
//! anywhere, at most [`CRASH_LOG_MAX`] bytes, in the local state folder the uninstaller removes.

use std::path::Path;
use std::time::Duration;

/// How often the main webview is tried again, and for how long in all (R342).
pub const WEBVIEW_RETRY_INTERVAL: Duration = Duration::from_millis(500);
/// The whole window of retries.
pub const WEBVIEW_RETRY_FOR: Duration = Duration::from_secs(10);

/// The crash note's file name in the local state folder.
pub const CRASH_LOG: &str = "crash.log";
/// The crash note's bound: older entries go first.
pub const CRASH_LOG_MAX: usize = 64 * 1024;
/// At most this many entries are kept.
pub const CRASH_LOG_ENTRIES: usize = 8;
/// The line that starts each entry.
const ENTRY_MARK: &str = "=== ";

/// Run `attempt` until it succeeds, or until `total / interval` further tries have failed.
/// `on_error(n, e)` sees each failure (n from 1); `sleep` waits between tries. Returns the last
/// error when every try failed.
pub fn retry<T, E>(
    interval: Duration,
    total: Duration,
    mut attempt: impl FnMut() -> Result<T, E>,
    mut on_error: impl FnMut(u32, &E),
    mut sleep: impl FnMut(Duration),
) -> Result<T, E> {
    let retries = if interval.is_zero() {
        0
    } else {
        (total.as_millis() / interval.as_millis()) as u32
    };
    let mut n = 0;
    loop {
        match attempt() {
            Ok(v) => return Ok(v),
            Err(e) => {
                n += 1;
                on_error(n, &e);
                if n > retries {
                    return Err(e);
                }
                sleep(interval);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

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
            Duration::from_secs(10),
            || {
                tries.set(tries.get() + 1);
                if tries.get() <= 3 {
                    Err(format!("fail {}", tries.get()))
                } else {
                    Ok(tries.get())
                }
            },
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
            Duration::from_secs(10),
            || {
                tries.set(tries.get() + 1);
                Err(tries.get())
            },
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
            WEBVIEW_RETRY_FOR,
            || Ok(7),
            |_, _| panic!("no error"),
            |_| slept = true,
        );
        assert_eq!(out, Ok(7));
        assert!(!slept);
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
