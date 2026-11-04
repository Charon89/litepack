//! The command line of `lpk-gui.exe` (E5-7): the activation verbs of `docs/API.md` ("Activation
//! verbs", which `tests/verbs.rs` parses and compares with [`VERBS`]), `@file` response files,
//! and the relative paths of a second instance resolved against that instance's folder (the
//! single-instance plugin forwards its arguments and working folder to the first).
//!
//! Only Rust reads these paths; each becomes a grant ([`crate::activation`]) exactly as `--open`
//! always has (D-117), so nothing the page sends is ever taken as a path.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The verb flags of `docs/API.md`'s table, in its order (`--name` is `--create`'s modifier).
pub const VERBS: &[&str] = &[
    "--open",
    "--extract-here",
    "--extract-to",
    "--create",
    "--test",
    "--convert",
];

/// `--create`'s modifier: the archive's file name, for `Add to "<name>.lpk"`.
pub const NAME: &str = "--name";

/// The most bytes read across all the response files of one command line (a command line holds
/// at most 32,767 characters; response files carry more, but not without bound).
pub const MAX_RESPONSE_FILE: u64 = 16 << 20;

/// The most arguments a command line may hold once its response files are expanded.
pub const MAX_ARGS: usize = 1 << 20;

/// One activation: what the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    /// `--open <archive>`, `--open=<archive>` or a bare path: open it in the browser.
    Open(PathBuf),
    /// `--extract-here <archive>...`.
    ExtractHere(Vec<PathBuf>),
    /// `--extract-to <archive>...`.
    ExtractTo(Vec<PathBuf>),
    /// `--create <path>... [--name <file>.lpk]`.
    Create {
        /// The inputs.
        inputs: Vec<PathBuf>,
        /// The archive's file name (`<file>.lpk`, no folder), when given: create at once.
        name: Option<String>,
    },
    /// `--test <archive>...`.
    Test(Vec<PathBuf>),
    /// `--convert <archive>`.
    Convert(PathBuf),
}

impl Verb {
    /// The verb's flag, as in [`VERBS`] (`Open` from a bare path too).
    pub fn flag(&self) -> &'static str {
        match self {
            Verb::Open(_) => "--open",
            Verb::ExtractHere(_) => "--extract-here",
            Verb::ExtractTo(_) => "--extract-to",
            Verb::Create { .. } => "--create",
            Verb::Test(_) => "--test",
            Verb::Convert(_) => "--convert",
        }
    }

    /// The paths the verb names.
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            Verb::Open(p) | Verb::Convert(p) => vec![p.as_path()],
            Verb::ExtractHere(v) | Verb::ExtractTo(v) | Verb::Test(v) => {
                v.iter().map(PathBuf::as_path).collect()
            }
            Verb::Create { inputs, .. } => inputs.iter().map(PathBuf::as_path).collect(),
        }
    }
}

/// What the command line asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Startup {
    /// The verb, if any (none: the empty window).
    pub verb: Option<Verb>,
    /// The Explorer menu's response files the command line named and `parse` read
    /// (`crate::shellfile`): the instance that goes on to act on the verb deletes them with
    /// [`crate::shellfile::delete`]; a second instance only forwards the argument and leaves them.
    pub shell_files: Vec<PathBuf>,
}

impl Startup {
    /// The archive `--open` names (E5-2's form).
    pub fn open(&self) -> Option<&Path> {
        match &self.verb {
            Some(Verb::Open(p)) => Some(p),
            _ => None,
        }
    }
}

/// Parse the arguments after the program name. Relative paths (those of a response file
/// included) resolve against `cwd` when it is given: the second instance's folder.
pub fn parse(
    args: impl IntoIterator<Item = OsString>,
    cwd: Option<&Path>,
) -> Result<Startup, String> {
    parse_in(args, cwd, crate::shellfile::verbs_dir().as_deref())
}

/// [`parse`] with the folder of the Explorer menu's response files named (`None`: there is no
/// such folder, every `@file` is E5-7's text format).
pub fn parse_in(
    args: impl IntoIterator<Item = OsString>,
    cwd: Option<&Path>,
    verbs: Option<&Path>,
) -> Result<Startup, String> {
    parse_collect(args, cwd, verbs, &mut Vec::new())
}

/// [`parse`] that also returns the Explorer menu's response files it opened, refusal or not
/// (the instance that acts deletes them whatever the outcome).
pub fn parse_with_files(
    args: impl IntoIterator<Item = OsString>,
    cwd: Option<&Path>,
) -> (Result<Startup, String>, Vec<PathBuf>) {
    let mut files = Vec::new();
    let verbs = crate::shellfile::verbs_dir();
    let r = parse_collect(args, cwd, verbs.as_deref(), &mut files);
    (r, files)
}

/// [`parse_in`] filling `shell_files` with every response file opened, even when parsing fails.
pub fn parse_collect(
    args: impl IntoIterator<Item = OsString>,
    cwd: Option<&Path>,
    verbs: Option<&Path>,
    shell_files: &mut Vec<PathBuf>,
) -> Result<Startup, String> {
    let args = expand_within(args, cwd, MAX_RESPONSE_FILE, MAX_ARGS, verbs, shell_files)?;
    let mut it = args.into_iter().peekable();
    let resolve = |p: OsString| -> PathBuf {
        let p = PathBuf::from(p);
        match cwd {
            Some(c) if p.is_relative() => c.join(p),
            _ => p,
        }
    };
    let Some(first) = it.next() else {
        return Ok(Startup::default());
    };
    let text = first.to_string_lossy().into_owned();
    // `--open=<archive>`: the form the Edge WebDriver can pass (it turns each argument into a
    // Chromium switch, lower-casing a bare path; E5-11's driver harness).
    let (flag, inline) = match first.to_str().and_then(|t| t.strip_prefix("--open=")) {
        Some("") => return Err("--open needs an archive".into()),
        Some(p) => ("--open", Some(OsString::from(p))),
        // The same switch-safe form for one `--create` input (E5-5's driver test).
        // A non-UTF-8 argument is refused as `--open=`'s is (an unknown option).
        None if first
            .to_str()
            .and_then(|t| t.strip_prefix("--create="))
            .is_some_and(|p| !p.is_empty()) =>
        {
            let p = first.to_str().and_then(|t| t.strip_prefix("--create="));
            ("--create", p.map(OsString::from))
        }
        // And for one `--test` archive (E5-8's driver test).
        None if first
            .to_str()
            .and_then(|t| t.strip_prefix("--test="))
            .is_some_and(|p| !p.is_empty()) =>
        {
            let p = first.to_str().and_then(|t| t.strip_prefix("--test="));
            ("--test", p.map(OsString::from))
        }
        None if VERBS.contains(&text.as_str()) => (
            VERBS[VERBS.iter().position(|v| *v == text).unwrap_or(0)],
            None,
        ),
        None if text == NAME => return Err("--name belongs to --create".into()),
        None if text == "--" => ("", None),
        None if text.starts_with("--") => return Err(format!("{text}: unknown option")),
        // A bare path: Open.
        None => ("", Some(first)),
    };
    let mut paths: Vec<PathBuf> = inline.into_iter().map(resolve).collect();
    let mut name: Option<String> = None;
    let mut literal = text == "--";
    while let Some(a) = it.next() {
        let t = a.to_string_lossy();
        if !literal && t == "--" {
            literal = true;
        } else if !literal && t == NAME {
            if flag != "--create" {
                return Err("--name belongs to --create".into());
            }
            if name.is_some() {
                return Err("--name given twice".into());
            }
            let n = it.next().ok_or("--name needs a file name")?;
            name = Some(archive_name(&n)?);
        } else if !literal && t.starts_with("--") {
            if VERBS.contains(&t.as_ref()) || t.starts_with("--open=") {
                return Err("one verb at a time".into());
            }
            return Err(format!("{t}: unknown option"));
        } else {
            paths.push(resolve(a));
        }
    }
    let need = |what: &str| {
        format!(
            "{} needs {what}",
            if flag.is_empty() { "--open" } else { flag }
        )
    };
    let verb = match flag {
        "" | "--open" => match <[PathBuf; 1]>::try_from(paths) {
            Ok([p]) => Verb::Open(p),
            Err(v) if v.is_empty() => return Err(need("an archive")),
            Err(_) => return Err("one archive at a time".into()),
        },
        "--convert" => match <[PathBuf; 1]>::try_from(paths) {
            Ok([p]) => Verb::Convert(p),
            Err(v) if v.is_empty() => return Err(need("an archive")),
            Err(_) => return Err("--convert takes one archive".into()),
        },
        _ if paths.is_empty() => {
            return Err(need(if flag == "--create" {
                "a path"
            } else {
                "an archive"
            }))
        }
        "--extract-here" => Verb::ExtractHere(paths),
        "--extract-to" => Verb::ExtractTo(paths),
        "--test" => Verb::Test(paths),
        _ => Verb::Create {
            inputs: paths,
            name,
        },
    };
    Ok(Startup {
        verb: Some(verb),
        shell_files: shell_files.clone(),
    })
}

/// `--name`'s value: a file name ending in `.lpk`, with no folder in it.
fn archive_name(n: &OsString) -> Result<String, String> {
    let s = n.to_str().ok_or("--name: not valid Unicode")?;
    let stem_ok = s.len() > 4
        && s.to_ascii_lowercase().ends_with(".lpk")
        && !s.contains(['/', '\\', ':'])
        && s.trim() == s;
    if !stem_ok {
        return Err(format!(
            "--name {s:?}: a file name ending in .lpk, no folder"
        ));
    }
    Ok(s.to_owned())
}

/// Replace each `@file` argument before a `--` by the file's lines, one argument per line
/// (UTF-8, an optional byte-order mark, CRLF or LF, blank lines skipped). Not recursive: a line
/// that starts with `@` is a path. A file in the `verbs` folder named as the Explorer menu names
/// its files (`crate::shellfile`) is its binary format instead, and is listed in `shell_files`.
/// `max_bytes` read across every response file, `max_args`
/// arguments after the expansion.
fn expand_within(
    args: impl IntoIterator<Item = OsString>,
    cwd: Option<&Path>,
    max_bytes: u64,
    max_args: usize,
    verbs: Option<&Path>,
    shell_files: &mut Vec<PathBuf>,
) -> Result<Vec<OsString>, String> {
    let mut out = Vec::new();
    let mut literal = false;
    let mut budget = max_bytes;
    for a in args {
        let at = a.to_str().and_then(|t| t.strip_prefix('@'));
        match at {
            Some(file) if !literal => {
                if file.is_empty() {
                    return Err("@ needs a response file".into());
                }
                let mut path = PathBuf::from(file);
                if let (Some(c), true) = (cwd, path.is_relative()) {
                    path = c.join(path);
                }
                if verbs.is_some_and(|d| crate::shellfile::is_shell_file(&path, d)) {
                    // Listed before the read: a file that is refused is deleted too.
                    shell_files.push(path.clone());
                    let request = crate::shellfile::read(&path)?;
                    out.extend(crate::shellfile::arguments(&request));
                } else {
                    read_response_file(&path, &mut budget, max_bytes, &mut out, max_args)?;
                }
            }
            _ => {
                literal |= a == "--";
                out.push(a);
            }
        }
        if out.len() > max_args {
            return Err(format!("more than {max_args} arguments"));
        }
    }
    Ok(out)
}

/// Read one response file through a single handle: a regular file only (by the opened handle's
/// metadata, so nothing changes between the check and the read), at most `budget` bytes, which
/// the read uses up; its arguments go onto `out`, which may hold at most `max_args`.
fn read_response_file(
    path: &Path,
    budget: &mut u64,
    max_bytes: u64,
    out: &mut Vec<OsString>,
    max_args: usize,
) -> Result<(), String> {
    use std::io::Read;
    let err = |e: &dyn std::fmt::Display| format!("@{}: {e}", path.display());
    let file = std::fs::File::open(path).map_err(|e| err(&e))?;
    if !file.metadata().map_err(|e| err(&e))?.is_file() {
        return Err(err(&"not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(*budget + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| err(&e))?;
    if bytes.len() as u64 > *budget {
        return Err(err(&format!(
            "the response files hold more than {max_bytes} bytes"
        )));
    }
    *budget -= bytes.len() as u64;
    let text = String::from_utf8(bytes).map_err(|_| err(&"not UTF-8"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    // Counted while collected: a file of short lines never holds more than `max_args` strings.
    for line in text
        .lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter(|l| !l.is_empty())
    {
        if out.len() >= max_args {
            return Err(format!("more than {max_args} arguments"));
        }
        out.push(OsString::from(line));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Startup, String> {
        parse(a.iter().map(OsString::from), None)
    }

    fn verb(a: &[&str]) -> Verb {
        p(a).unwrap().verb.unwrap()
    }

    fn paths(v: &[&str]) -> Vec<PathBuf> {
        v.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn open_forms() {
        assert_eq!(p(&[]).unwrap().verb, None);
        assert_eq!(p(&["a.lpk"]).unwrap().open(), Some(Path::new("a.lpk")));
        assert_eq!(
            p(&["--open", "C:\\x y\\a.lpk"]).unwrap().open(),
            Some(Path::new("C:\\x y\\a.lpk"))
        );
        assert_eq!(
            p(&["--open=C:\\x y\\A.lpk"]).unwrap().open(),
            Some(Path::new("C:\\x y\\A.lpk"))
        );
        assert_eq!(
            p(&["--", "--odd.lpk"]).unwrap().open(),
            Some(Path::new("--odd.lpk"))
        );
        assert_eq!(
            p(&["--open", "--", "--odd.lpk"]).unwrap().open(),
            Some(Path::new("--odd.lpk"))
        );
    }

    #[test]
    fn every_verb() {
        assert_eq!(
            verb(&["--extract-here", "a.zip", "b.lpk"]),
            Verb::ExtractHere(paths(&["a.zip", "b.lpk"]))
        );
        assert_eq!(
            verb(&["--extract-to", "a.7z"]),
            Verb::ExtractTo(paths(&["a.7z"]))
        );
        assert_eq!(
            verb(&["--create", "x", "y"]),
            Verb::Create {
                inputs: paths(&["x", "y"]),
                name: None
            }
        );
        // A non-UTF-8 `--create=` argument is refused like `--open=`'s.
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let bad = OsString::from_wide(&[
                u16::from(b'-'),
                u16::from(b'-'),
                u16::from(b'c'),
                u16::from(b'r'),
                u16::from(b'e'),
                u16::from(b'a'),
                u16::from(b't'),
                u16::from(b'e'),
                u16::from(b'='),
                0xD800,
            ]);
            assert!(parse([bad], None).is_err());
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let mut v = b"--create=".to_vec();
            v.push(0xFF);
            assert!(parse([OsString::from_vec(v)], None).is_err());
        }
        // `--test=` is the same form for one archive; an empty or non-UTF-8 one is refused.
        assert_eq!(verb(&["--test=a.lpk"]), Verb::Test(paths(&["a.lpk"])));
        assert!(parse([OsString::from("--test=")], None).is_err());
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let mut w: Vec<u16> = "--test=".encode_utf16().collect();
            w.push(0xD800);
            assert!(parse([OsString::from_wide(&w)], None).is_err());
        }
        // The switch-safe inline form (the driver tests pass it).
        assert_eq!(
            verb(&["--create=x"]),
            Verb::Create {
                inputs: paths(&["x"]),
                name: None
            }
        );
        assert_eq!(
            verb(&["--create", "x", "y", "--name", "x.lpk"]),
            Verb::Create {
                inputs: paths(&["x", "y"]),
                name: Some("x.lpk".into())
            }
        );
        assert_eq!(
            verb(&["--create", "--name", "x.LPK", "x"]),
            Verb::Create {
                inputs: paths(&["x"]),
                name: Some("x.LPK".into())
            }
        );
        assert_eq!(verb(&["--test", "a.lpk"]), Verb::Test(paths(&["a.lpk"])));
        assert_eq!(verb(&["--convert", "a.zip"]), Verb::Convert("a.zip".into()));
        for (args, flag) in [
            (&["a"][..], "--open"),
            (&["--extract-here", "a"][..], "--extract-here"),
            (&["--extract-to", "a"][..], "--extract-to"),
            (&["--create", "a"][..], "--create"),
            (&["--test", "a"][..], "--test"),
            (&["--convert", "a"][..], "--convert"),
        ] {
            assert_eq!(verb(args).flag(), flag);
        }
    }

    #[test]
    fn refusals() {
        for bad in [
            &["--open"][..],
            &["--open="],
            &["--open=a.lpk", "b.lpk"],
            &["a.lpk", "b.lpk"],
            &["--bogus"],
            &["--extract-here"],
            &["--create"],
            &["--create", "--name", "x.lpk"],
            &["--convert", "a.zip", "b.zip"],
            &["--name", "x.lpk"],
            &["--test", "a.lpk", "--name", "x.lpk"],
            &["--create", "x", "--name"],
            &["--create", "x", "--name", "x.zip"],
            &["--create", "x", "--name", "..\\x.lpk"],
            &["--create", "x", "--name", "d/x.lpk"],
            &["--create", "x", "--name", ".lpk"],
            &["--create", "x", "--name", "a.lpk", "--name", "b.lpk"],
            &["--extract-here", "a", "--test", "b"],
            &["--test", "a", "--open=b"],
            &["a.lpk", "--whatever"],
            &["@"],
        ] {
            assert!(p(bad).is_err(), "{bad:?} accepted");
        }
        assert_eq!(
            p(&["--test", "a", "--open"]).unwrap_err(),
            "one verb at a time"
        );
    }

    #[test]
    fn relative_paths_resolve_against_the_second_instances_folder() {
        let cwd = Path::new(if cfg!(windows) { "C:\\work" } else { "/work" });
        let abs = if cfg!(windows) { "D:\\a.lpk" } else { "/a.lpk" };
        let s = parse(
            ["--extract-to", "sub/a.lpk", abs].map(OsString::from),
            Some(cwd),
        )
        .unwrap();
        assert_eq!(
            s.verb,
            Some(Verb::ExtractTo(vec![cwd.join("sub/a.lpk"), abs.into()]))
        );
        let s = parse(["--open=b.lpk"].map(OsString::from), Some(cwd)).unwrap();
        assert_eq!(s.open(), Some(cwd.join("b.lpk").as_path()));
    }

    #[test]
    fn response_files_carry_the_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let many: Vec<String> = (0..5000)
            .map(|i| format!("input {i:05} with spaces"))
            .collect();
        let body = format!(
            "\u{feff}--create\r\n{}\r\n\r\n--name\nbig.lpk\n",
            many.join("\r\n")
        );
        assert!(body.len() > 32_767, "more than a command line holds");
        std::fs::write(dir.path().join("args.txt"), body).unwrap();
        let s = parse(["@args.txt"].map(OsString::from), Some(dir.path())).unwrap();
        let Some(Verb::Create { inputs, name }) = s.verb else {
            panic!("not create")
        };
        assert_eq!(inputs.len(), 5000);
        assert_eq!(inputs[7], dir.path().join("input 00007 with spaces"));
        assert_eq!(name.as_deref(), Some("big.lpk"));
        // Mixed with ordinary arguments, and not recursive: an `@` line is a path.
        std::fs::write(dir.path().join("two.txt"), "b.lpk\n@c.lpk\n").unwrap();
        let s = parse(
            ["--test", "a.lpk", "@two.txt"].map(OsString::from),
            Some(dir.path()),
        )
        .unwrap();
        assert_eq!(
            s.verb,
            Some(Verb::Test(
                ["a.lpk", "b.lpk", "@c.lpk"]
                    .iter()
                    .map(|n| dir.path().join(n))
                    .collect()
            ))
        );
        // After `--`, `@x` is a path.
        let s = parse(["--", "@x.lpk"].map(OsString::from), Some(dir.path())).unwrap();
        assert_eq!(s.open(), Some(dir.path().join("@x.lpk").as_path()));
        // A missing file, a non-UTF-8 one.
        assert!(parse(["@none.txt"].map(OsString::from), Some(dir.path())).is_err());
        std::fs::write(dir.path().join("bad.txt"), [0xff, 0xfe, 0x00, 0x41]).unwrap();
        assert!(parse(["@bad.txt"].map(OsString::from), Some(dir.path()))
            .unwrap_err()
            .contains("UTF-8"));
    }

    #[test]
    fn a_folder_is_not_a_response_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert!(parse(["@sub"].map(OsString::from), Some(dir.path())).is_err());
    }

    #[test]
    fn the_whole_expansion_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        // Two files of 6 bytes each: 12 bytes in all.
        std::fs::write(dir.path().join("one.txt"), "a1\nb1\n").unwrap();
        std::fs::write(dir.path().join("two.txt"), "a2\nb2\n").unwrap();
        let args = || ["--test", "@one.txt", "@two.txt"].map(OsString::from);
        let ok = expand_within(args(), Some(dir.path()), 12, 5, None, &mut Vec::new()).unwrap();
        assert_eq!(ok.len(), 5);
        // The bytes across both files, not per file: 11 refuses the second.
        let e = expand_within(args(), Some(dir.path()), 11, 5, None, &mut Vec::new()).unwrap_err();
        assert!(e.contains("more than 11 bytes"), "{e}");
        // The arguments after the expansion: 4 refuses the fifth.
        let e = expand_within(args(), Some(dir.path()), 12, 4, None, &mut Vec::new()).unwrap_err();
        assert!(e.contains("more than 4 arguments"), "{e}");
        // One file with more lines than the allowance is refused while it is collected.
        std::fs::write(dir.path().join("many.txt"), "a\nb\nc\nd\ne\nf\n").unwrap();
        let e = expand_within(
            ["@many.txt"].map(OsString::from),
            Some(dir.path()),
            1000,
            5,
            None,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(e.contains("more than 5 arguments"), "{e}");
        // Without response files too.
        let many = (0..6).map(|i| OsString::from(format!("p{i}")));
        assert!(expand_within(many, None, 12, 5, None, &mut Vec::new()).is_err());
    }
}
