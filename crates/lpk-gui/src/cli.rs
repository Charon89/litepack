//! The command line of `lpk-gui.exe`, E5-2's part: `--open <archive>` or a bare path opens the
//! archive (double-click, association). The other verbs of `docs/API.md` (E5-7) are refused
//! by name until E5-7 implements them.

use std::ffi::OsString;
use std::path::PathBuf;

/// What the command line asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Startup {
    /// The archive to open.
    pub open: Option<PathBuf>,
}

/// The verbs of `docs/API.md` that E5-7 adds.
pub const LATER_VERBS: &[&str] = &[
    "--extract-here",
    "--extract-to",
    "--create",
    "--name",
    "--test",
    "--convert",
];

/// Parse the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Startup, String> {
    let mut s = Startup::default();
    let mut it = args.into_iter();
    let set = |p: OsString, s: &mut Startup| -> Result<(), String> {
        if s.open.is_some() {
            return Err("one archive at a time".into());
        }
        s.open = Some(PathBuf::from(p));
        Ok(())
    };
    while let Some(a) = it.next() {
        let text = a.to_string_lossy();
        // `--open=<archive>`: the form the Edge WebDriver can pass (it turns each argument into a
        // Chromium switch, lower-casing a bare path; E5-11's driver harness).
        if let Some(p) = a.to_str().and_then(|t| t.strip_prefix("--open=")) {
            if p.is_empty() {
                return Err("--open needs an archive".into());
            }
            set(OsString::from(p), &mut s)?;
        } else if text == "--open" {
            let p = it.next().ok_or("--open needs an archive")?;
            set(p, &mut s)?;
        } else if text == "--" {
            for p in it.by_ref() {
                set(p, &mut s)?;
            }
        } else if LATER_VERBS.contains(&text.as_ref()) {
            return Err(format!("{text}: not in this version yet"));
        } else if text.starts_with("--") {
            return Err(format!("{text}: unknown option"));
        } else {
            set(a, &mut s)?;
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Startup, String> {
        parse(a.iter().map(OsString::from))
    }

    #[test]
    fn open_forms() {
        assert_eq!(p(&[]).unwrap().open, None);
        assert_eq!(p(&["a.lpk"]).unwrap().open, Some(PathBuf::from("a.lpk")));
        assert_eq!(
            p(&["--open", "C:\\x y\\a.lpk"]).unwrap().open,
            Some(PathBuf::from("C:\\x y\\a.lpk"))
        );
        assert_eq!(
            p(&["--open=C:\\x y\\A.lpk"]).unwrap().open,
            Some(PathBuf::from("C:\\x y\\A.lpk"))
        );
        assert_eq!(
            p(&["--", "--odd.lpk"]).unwrap().open,
            Some(PathBuf::from("--odd.lpk"))
        );
    }

    #[test]
    fn refusals() {
        assert!(p(&["--open"]).is_err());
        assert!(p(&["--open="]).is_err());
        assert!(p(&["--open=a.lpk", "b.lpk"]).is_err());
        assert!(p(&["a.lpk", "b.lpk"]).is_err());
        assert!(p(&["--bogus"]).is_err());
        assert!(p(&["--extract-here", "a.lpk"])
            .unwrap_err()
            .contains("not in this version"));
    }
}
