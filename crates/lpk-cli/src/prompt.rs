//! Reading a password from the terminal without echo (E3-5).
//!
//! The rules: `--password-prompt` reads from the terminal, with a confirmation when a new
//! archive is sealed; without the flag a prompt happens only when stdin and stderr are both
//! terminals and the archive needs a credential that was not given; through a pipe (the
//! benchmark runner, a script) there is never a prompt, so the command fails with
//! `PasswordRequired` as D-48 requires. The prompt and the typed characters go through
//! `rpassword` (Apache-2.0) to the process's console (`/dev/tty`, `CONIN$`), not to the
//! streams this crate captures, and the password lives in [`Zeroizing`] memory from the moment
//! it is read.

use std::io::{IsTerminal, Read, Write};
use std::sync::{Arc, Mutex};

use zeroize::Zeroizing;

use crate::Failure;

/// The reason `--password-prompt` is refused through a pipe (a usage error, exit 1).
pub const NOT_A_TERMINAL: &str =
    "--password-prompt needs a terminal: stdin and stderr are not both terminals";

/// Where a password prompt reads from.
#[derive(Clone)]
enum Source {
    /// The process's console, found interactive (stdin and stderr are terminals).
    Console,
    /// A pipe or a file somewhere: no prompt is ever shown.
    NotATerminal,
    /// An injected reader and writer: the unit tests of the prompt path.
    Injected(Injected),
}

/// The injected terminal of a test: the lines typed and what the prompt wrote.
#[derive(Clone, Default)]
struct Injected {
    input: Arc<Mutex<std::io::Cursor<Vec<u8>>>>,
    output: Arc<Mutex<Vec<u8>>>,
}

/// `rpassword` takes its reader by value: a handle on the shared input.
struct SharedInput(Arc<Mutex<std::io::Cursor<Vec<u8>>>>);

impl Read for SharedInput {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.lock().map_or(Ok(0), |mut c| c.read(buf))
    }
}

/// `rpassword` takes its writer by value: a handle on the shared output.
struct SharedOutput(Arc<Mutex<Vec<u8>>>);

impl Write for SharedOutput {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut v) = self.0.lock() {
            v.extend_from_slice(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The terminal a command may prompt on.
#[derive(Clone)]
pub struct Terminal {
    source: Source,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self.source {
            Source::Console => "Console",
            Source::NotATerminal => "NotATerminal",
            Source::Injected(_) => "Injected",
        };
        f.debug_struct("Terminal").field("source", &s).finish()
    }
}

impl Terminal {
    /// The process's own: interactive when stdin and stderr are both terminals.
    pub fn detect() -> Self {
        let source = if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
            Source::Console
        } else {
            Source::NotATerminal
        };
        Terminal { source }
    }

    /// Never a terminal: what a program that must not prompt passes.
    pub fn none() -> Self {
        Terminal {
            source: Source::NotATerminal,
        }
    }

    /// A terminal that reads `typed` (newline-terminated lines) and keeps what the prompt
    /// wrote, for tests of the prompt path; [`Terminal::written`] returns the latter.
    pub fn injected(typed: &[u8]) -> Self {
        let injected = Injected {
            input: Arc::new(Mutex::new(std::io::Cursor::new(typed.to_vec()))),
            output: Arc::new(Mutex::new(Vec::new())),
        };
        Terminal {
            source: Source::Injected(injected),
        }
    }

    /// What the prompts of an injected terminal wrote (empty for the others).
    pub fn written(&self) -> Vec<u8> {
        match &self.source {
            Source::Injected(i) => i.output.lock().map(|v| v.clone()).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// True when a prompt can be shown.
    pub fn interactive(&self) -> bool {
        !matches!(self.source, Source::NotATerminal)
    }

    /// One prompt and one line typed without echo.
    fn read_line(&self, prompt: &str) -> Result<Zeroizing<Vec<u8>>, Failure> {
        let config = match &self.source {
            Source::Console => rpassword::ConfigBuilder::new().build(),
            Source::NotATerminal => return Err(Failure::usage(NOT_A_TERMINAL)),
            Source::Injected(i) => rpassword::ConfigBuilder::new()
                .input_reader(SharedInput(Arc::clone(&i.input)))
                .output_writer(SharedOutput(Arc::clone(&i.output)))
                .build(),
        };
        let typed = rpassword::prompt_password_with_config(prompt, config)
            .map_err(|e| Failure::failed("Io", format!("reading the password: {e}"), None))?;
        Ok(Zeroizing::new(typed.into_bytes()))
    }

    /// The password for `archive` (its file name in the prompt), typed without echo. With
    /// `confirm` (a new archive) it is typed twice, the two must match and it may not be
    /// empty (a keyfile alone makes a keyfile-only archive; a slip on Enter must not). On a
    /// terminal that is not interactive the prompt is refused with [`NOT_A_TERMINAL`]
    /// (exit 1); a mismatch and an empty password are usage errors too.
    pub fn read_password(
        &self,
        archive: &std::path::Path,
        confirm: bool,
    ) -> Result<Zeroizing<Vec<u8>>, Failure> {
        let name = archive
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| archive.display().to_string());
        let first = self.read_line(&format!("Password for {name}: "))?;
        if !confirm {
            return Ok(first);
        }
        if first.is_empty() {
            return Err(Failure::usage(
                "the password is empty (a keyfile alone seals a keyfile-only archive)",
            ));
        }
        let second = self.read_line("Confirm password: ")?;
        if *first != *second {
            return Err(Failure::usage("the passwords do not match"));
        }
        Ok(first)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_pipe_is_refused_as_a_usage_error_before_any_prompt() {
        let t = Terminal::none();
        assert!(!t.interactive());
        let e = t.read_password(Path::new("a.lpk"), false).unwrap_err();
        assert_eq!((e.code, e.kind.as_str()), (crate::EXIT_USAGE, "Usage"));
        assert_eq!(e.message, NOT_A_TERMINAL);
    }

    #[test]
    fn an_injected_terminal_reads_one_line_and_the_prompt_names_the_archive() {
        let t = Terminal::injected(b"open sesame\nnot read\n");
        assert!(t.interactive());
        let p = t.read_password(Path::new("dir/e.lpk"), false).unwrap();
        assert_eq!(&**p, b"open sesame");
        let written = String::from_utf8(t.written()).unwrap();
        assert!(written.starts_with("Password for e.lpk: "), "{written}");
        assert!(!written.contains("sesame"), "echoed: {written}");
        assert!(!written.contains("not read"), "{written}");
    }

    #[test]
    fn a_new_archive_confirms_and_refuses_a_mismatch_or_an_empty_password() {
        let t = Terminal::injected(b"pw\npw\n");
        let p = t.read_password(Path::new("n.lpk"), true).unwrap();
        assert_eq!(&**p, b"pw");
        let written = String::from_utf8(t.written()).unwrap();
        assert!(written.contains("Confirm password: "), "{written}");
        assert!(!written.contains("pw"), "echoed: {written}");

        let t = Terminal::injected(b"pw\npv\n");
        let e = t.read_password(Path::new("n.lpk"), true).unwrap_err();
        assert_eq!(e.code, crate::EXIT_USAGE);
        assert_eq!(e.message, "the passwords do not match");
        assert!(!String::from_utf8(t.written()).unwrap().contains("pv"));

        let t = Terminal::injected(b"\n");
        let e = t.read_password(Path::new("n.lpk"), true).unwrap_err();
        assert_eq!(e.code, crate::EXIT_USAGE);
        assert!(
            e.message.starts_with("the password is empty"),
            "{}",
            e.message
        );
        // Only one prompt was shown: nothing is confirmed for an empty password.
        assert!(!String::from_utf8(t.written()).unwrap().contains("Confirm"));
    }

    #[test]
    fn an_opened_archive_takes_an_empty_line_as_an_empty_password() {
        // Opening tries what was typed; the key slot says whether it is right.
        let t = Terminal::injected(b"\n");
        let p = t.read_password(Path::new("e.lpk"), false).unwrap();
        assert!(p.is_empty());
    }
}
