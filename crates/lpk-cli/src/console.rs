//! The console: a progress line on stderr, rewritten in place, and the cancel token.
//!
//! The line is written only when stderr is a terminal (`std::io::IsTerminal`), never through
//! a pipe (the benchmark runner captures stderr through one, D-48: no progress byte may reach
//! it), and not under `--quiet`, `--no-progress` or `--json`. It is plain text, rewritten with
//! a carriage return and padded over the previous line's width: no VT escape sequence and no
//! colour in Phase 1, so it needs nothing of the console's mode. The engine limits the rate
//! ([`lpk_core::progress::MIN_INTERVAL`]); the console prints every event it receives and
//! clears the line before any other output.
//!
//! Ctrl-C: `main` installs a handler (the `ctrlc` crate) for the commands that poll the
//! token (`a`, `append`, `x`, `export`); it cancels the token, the engine stops at its next
//! poll and cleans up, and the command fails with exit 2 and `cancelled` (D-77's contract:
//! one line, or the JSON error line with `kind: "Cancelled"`). A second Ctrl-C exits at
//! once. The other commands keep the process default.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lpk_core::{CancelToken, Event, Monitor, Progress};

/// What the command line passes to the engine: the token and, on a terminal, the line.
#[derive(Debug, Clone)]
pub struct Console {
    cancel: CancelToken,
    line: Option<Arc<Line>>,
}

impl Console {
    /// A console over `cancel`: the progress line is shown when `show` and stderr is a
    /// terminal.
    pub fn new(cancel: CancelToken, show: bool) -> Self {
        let line = (show && std::io::stderr().is_terminal()).then(|| Arc::new(Line::default()));
        Console { cancel, line }
    }

    /// A console that shows nothing and is never cancelled (tests, library use).
    pub fn silent() -> Self {
        Console {
            cancel: CancelToken::new(),
            line: None,
        }
    }

    /// The engine's monitor: the token, and the line as the observer when it is shown.
    pub fn monitor(&self) -> Monitor {
        Monitor {
            progress: self
                .line
                .as_ref()
                .map(|l| Arc::clone(l) as Arc<dyn Progress>),
            cancel: self.cancel.clone(),
        }
    }

    /// Whether the progress line is shown.
    pub fn shows_progress(&self) -> bool {
        self.line.is_some()
    }

    /// Clear the progress line (before any other output on the terminal).
    pub fn clear(&self) {
        if let Some(l) = &self.line {
            l.clear();
        }
    }
}

/// The line on stderr: its last width, to pad the rewrite over it.
#[derive(Debug, Default)]
struct Line {
    width: AtomicUsize,
    lock: Mutex<()>,
}

impl Line {
    fn write(&self, text: &str) {
        let _guard = self.lock.lock();
        let previous = self.width.swap(text.chars().count(), Ordering::SeqCst);
        let pad = previous.saturating_sub(text.chars().count());
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "\r{text}{:pad$}", "", pad = pad);
        if pad > 0 {
            let _ = write!(err, "\r{text}");
        }
        let _ = err.flush();
    }

    fn clear(&self) {
        let _guard = self.lock.lock();
        let previous = self.width.swap(0, Ordering::SeqCst);
        if previous > 0 {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r{:previous$}\r", "", previous = previous);
            let _ = err.flush();
        }
    }
}

impl Progress for Line {
    fn event(&self, e: &Event) {
        self.write(&format_event(e));
    }
}

/// `1.5 MiB`, `312 KiB`, `17 B`.
fn size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// The text of one event: the stage, files done of the total, bytes in and out, blocks.
pub fn format_event(e: &Event) -> String {
    let files = match e.files_total {
        Some(t) => format!("{}/{t} files", e.files_done),
        None => format!("{} files", e.files_done),
    };
    let blocks = match e.blocks_total {
        Some(t) => format!("{}/{t} blocks", e.blocks_done),
        None => format!("{} blocks", e.blocks_done),
    };
    format!(
        "{}: {files}, {} in, {} out, {blocks}",
        e.stage.name(),
        size(e.bytes_in),
        size(e.bytes_out)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lpk_core::Stage;

    #[test]
    fn an_event_formats_with_its_totals_when_known() {
        let e = Event {
            stage: Stage::Encode,
            files_done: 3,
            files_total: Some(10),
            bytes_in: 1536,
            bytes_out: 17,
            blocks_done: 1,
            blocks_total: None,
            done: false,
        };
        assert_eq!(
            format_event(&e),
            "encode: 3/10 files, 1.5 KiB in, 17 B out, 1 blocks"
        );
        let e = Event {
            stage: Stage::Extract,
            files_total: None,
            blocks_total: Some(4),
            bytes_in: 3 << 20,
            ..e
        };
        assert_eq!(
            format_event(&e),
            "extract: 3 files, 3.0 MiB in, 17 B out, 1/4 blocks"
        );
    }

    #[test]
    fn a_silent_console_has_no_line_and_a_fresh_token() {
        let c = Console::silent();
        assert!(!c.shows_progress());
        let m = c.monitor();
        assert!(m.progress.is_none());
        assert!(!m.cancel.is_cancelled());
        c.clear();
    }
}
