//! E3-4 end to end: `lpk a` with several inputs, a list file and excludes; the patterns of
//! `lpk x`, `lpk l` and `lpk export`.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Run `lpk` from `cwd` (the naming rule and the list file resolve against it).
fn lpk(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lpk"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn prose(seed: u64, len: usize) -> Vec<u8> {
    const WORDS: [&str; 8] = ["the", "king", "of", "France", "and", "army", "sea", "road"];
    let mut s = 0x9E37_79B9_7F4A_7C15u64 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1;
    let mut out = Vec::new();
    while out.len() < len {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        out.extend_from_slice(WORDS[(s % 8) as usize].as_bytes());
        out.push(b' ');
    }
    out.truncate(len);
    out
}

/// `src/a.txt`, `src/docs/b.md`, `src/docs/deep/r.bin`, `src/empty/`, `src/zero`.
fn make_tree(src: &Path) {
    fs::create_dir_all(src.join("docs/deep")).unwrap();
    fs::create_dir_all(src.join("empty")).unwrap();
    fs::write(src.join("a.txt"), prose(1, 20_000)).unwrap();
    fs::write(src.join("docs/b.md"), prose(2, 7_000)).unwrap();
    fs::write(src.join("docs/deep/r.bin"), prose(3, 15_000)).unwrap();
    fs::write(src.join("zero"), b"").unwrap();
}

/// A work directory with the tree under `src`.
fn work() -> (tempfile::TempDir, PathBuf) {
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    make_tree(&src);
    (work, src)
}

/// The entry paths `lpk l` prints (the fourth column), in table order.
fn list(cwd: &Path, args: &[&str]) -> Vec<String> {
    let mut all = vec!["l"];
    all.extend_from_slice(args);
    let r = lpk(cwd, &all);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    text(&r.stdout)
        .lines()
        .map(|l| l.split('\t').nth(3).unwrap().to_string())
        .collect()
}

fn ok(r: &Output) {
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn several_inputs_enter_under_their_own_names_and_one_directory_keeps_its_contents() {
    let (w, src) = work();
    let wd = w.path();
    // One directory: its contents, as before (no `src` prefix).
    ok(&lpk(wd, &["a", "one.lpk", "src"]));
    assert_eq!(
        list(wd, &["one.lpk"]),
        [
            "a.txt",
            "docs",
            "docs/b.md",
            "docs/deep",
            "docs/deep/r.bin",
            "empty",
            "zero"
        ]
    );
    // Several relative inputs: entered as given, the directory itself an entry.
    ok(&lpk(wd, &["a", "two.lpk", "src/docs", "src/a.txt"]));
    assert_eq!(
        list(wd, &["two.lpk"]),
        [
            "src/a.txt",
            "src/docs",
            "src/docs/b.md",
            "src/docs/deep",
            "src/docs/deep/r.bin"
        ]
    );
    // From inside `src`: the names are the arguments.
    ok(&lpk(&src, &["a", "../three.lpk", "docs", "a.txt"]));
    assert_eq!(
        list(wd, &["three.lpk"]),
        ["a.txt", "docs", "docs/b.md", "docs/deep", "docs/deep/r.bin"]
    );
    // An absolute path: its last component; backslashes and a trailing separator are fine.
    let abs = src.join("docs");
    ok(&lpk(wd, &["a", "four.lpk", s(&abs), "src/zero"]));
    assert_eq!(
        list(wd, &["four.lpk"]),
        [
            "docs",
            "docs/b.md",
            "docs/deep",
            "docs/deep/r.bin",
            "src/zero"
        ]
    );
    // One file alone is entered under its name (only a directory alone means contents).
    ok(&lpk(wd, &["a", "five.lpk", "src/a.txt"]));
    assert_eq!(list(wd, &["five.lpk"]), ["src/a.txt"]);
    // The archive of several inputs round-trips.
    ok(&lpk(wd, &["x", "two.lpk", "out"]));
    assert_eq!(
        fs::read(wd.join("out/src/docs/deep/r.bin")).unwrap(),
        fs::read(src.join("docs/deep/r.bin")).unwrap()
    );
    assert_eq!(
        fs::read(wd.join("out/src/a.txt")).unwrap(),
        fs::read(src.join("a.txt")).unwrap()
    );
    assert!(!wd.join("out/src/zero").exists());
}

#[test]
fn conflicting_inputs_are_refused_naming_both_before_any_output() {
    let (w, src) = work();
    let wd = w.path();
    // The same name twice (two spellings of one directory, and a relative path beside the
    // absolute one that gives its last component).
    let abs = src.join("docs");
    for (args, what) in [
        (vec!["docs", "./docs"], "would both be entered"),
        (vec!["docs", s(&abs)], "would both be entered"),
        (vec!["docs/b.md", "docs"], "lies under"),
        (vec!["docs", "docs/deep/r.bin"], "lies under"),
    ] {
        let mut all = vec!["a", "../c.lpk"];
        all.extend_from_slice(&args);
        let r = lpk(&src, &all);
        assert_eq!(r.status.code(), Some(1), "{args:?}: {}", text(&r.stderr));
        let err = text(&r.stderr);
        assert!(err.contains(what), "{args:?}: {err}");
        for a in &args {
            assert!(
                err.contains(a) || err.contains(&a.replace('/', "\\")),
                "{args:?}: {err} does not name {a}"
            );
        }
        assert!(!wd.join("c.lpk").exists(), "{args:?}");
    }
    // With --json: one object, exit 1, kind InvalidOption.
    let r = lpk(&src, &["--json", "a", "../c.lpk", "docs", "docs"]);
    assert_eq!(r.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_str(text(&r.stderr).trim()).unwrap();
    assert_eq!(v["error"]["kind"], "InvalidOption");
    assert_eq!(v["error"]["code"], 1);
    // A path with no name to enter it under.
    let r = lpk(&src, &["a", "../c.lpk", ".", "a.txt"]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("no name"), "{}", text(&r.stderr));
    // A missing input is an I/O refusal (2) and leaves no archive.
    let r = lpk(&src, &["a", "../c.lpk", "docs", "missing.txt"]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("missing.txt"));
    assert!(!wd.join("c.lpk").exists());
}

#[test]
fn a_list_file_names_inputs_relative_to_the_current_directory() {
    let (w, src) = work();
    let wd = w.path();
    // CRLF, a comment, an empty line, a path with a `./` prefix.
    fs::write(
        wd.join("list.txt"),
        "# the inputs\r\ndocs/b.md\r\n\r\n./a.txt\nzero\r\n",
    )
    .unwrap();
    ok(&lpk(&src, &["a", "../l.lpk", "--list", "../list.txt"]));
    assert_eq!(list(wd, &["l.lpk"]), ["a.txt", "docs/b.md", "zero"]);
    // Positional inputs and the list together; the list's directory is entered by name.
    fs::write(wd.join("dirs.txt"), "docs\n").unwrap();
    ok(&lpk(
        &src,
        &["a", "../l2.lpk", "a.txt", "--list", "../dirs.txt"],
    ));
    assert_eq!(
        list(wd, &["l2.lpk"]),
        ["a.txt", "docs", "docs/b.md", "docs/deep", "docs/deep/r.bin"]
    );
    // A conflict between the list and a positional input is refused like any other.
    let r = lpk(
        &src,
        &["a", "../l3.lpk", "docs/b.md", "--list", "../dirs.txt"],
    );
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
    assert!(!wd.join("l3.lpk").exists());
    // A list file that cannot be read: refused (2) before any output.
    let r = lpk(&src, &["a", "../l4.lpk", "--list", "../none.txt"]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("none.txt"));
    assert!(!wd.join("l4.lpk").exists());
    // Without a positional input the list alone is accepted; without either it is a usage
    // error.
    let r = lpk(&src, &["a", "../l5.lpk"]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
    // The list's round trip through `x` (the runner's private-corpus shape: files by their
    // relative paths, no directory entries, the parents created on extraction).
    ok(&lpk(wd, &["x", "l.lpk", "out"]));
    assert_eq!(
        fs::read(wd.join("out/docs/b.md")).unwrap(),
        fs::read(src.join("docs/b.md")).unwrap()
    );
    assert!(wd.join("out/zero").exists());
}

#[test]
fn exclude_prunes_subtrees_and_matching_files() {
    let (w, src) = work();
    let wd = w.path();
    // On the contents form: a directory match prunes, `**` reaches every depth.
    ok(&lpk(
        wd,
        &[
            "a",
            "e.lpk",
            "src",
            "--exclude",
            "docs/deep",
            "--exclude",
            "**/*.txt",
        ],
    ));
    assert_eq!(list(wd, &["e.lpk"]), ["docs", "docs/b.md", "empty", "zero"]);
    // On several inputs: the patterns see the entry paths (with the inputs' prefixes), and an
    // input's own name is subject to them.
    ok(&lpk(
        wd,
        &[
            "a",
            "e2.lpk",
            "src/docs",
            "src/a.txt",
            "--exclude",
            "src/a.txt",
            "--exclude",
            "**/deep",
        ],
    ));
    assert_eq!(list(wd, &["e2.lpk"]), ["src/docs", "src/docs/b.md"]);
    // A pattern that does not compile is a usage error, before any output.
    let r = lpk(wd, &["a", "e3.lpk", "src", "--exclude", "[unclosed"]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
    assert!(!wd.join("e3.lpk").exists());
    // A `--exclude` that excludes nothing changes nothing (the same entries as without).
    ok(&lpk(
        wd,
        &["a", "e4.lpk", "src", "--exclude", "nothing-here"],
    ));
    ok(&lpk(wd, &["a", "e5.lpk", "src"]));
    assert_eq!(list(wd, &["e4.lpk"]), list(wd, &["e5.lpk"]));
    let _ = src;
}

#[test]
fn extract_patterns_select_a_subset_and_refuse_a_pattern_without_a_match() {
    let (w, src) = work();
    let wd = w.path();
    ok(&lpk(wd, &["a", "one.lpk", "src"]));
    // A directory pattern: its subtree, the parent created, nothing else.
    let r = lpk(wd, &["x", "-v", "one.lpk", "out", "docs/deep"]);
    ok(&r);
    assert_eq!(text(&r.stdout), "extracted 1 files, 1 directories\n");
    let err = text(&r.stderr);
    assert!(
        err.contains("1 files, 1 directories") && err.contains("blocks decoded"),
        "{err}"
    );
    assert_eq!(
        fs::read(wd.join("out/docs/deep/r.bin")).unwrap(),
        fs::read(src.join("docs/deep/r.bin")).unwrap()
    );
    assert!(!wd.join("out/a.txt").exists());
    assert!(!wd.join("out/docs/b.md").exists());
    assert!(!wd.join("out/empty").exists());
    // `**` and a plain file pattern; backslashes are separators.
    ok(&lpk(
        wd,
        &[
            "x",
            "one.lpk",
            "out2",
            "**/*.md",
            "zero",
            "docs\\deep\\r.bin",
        ],
    ));
    let mut got: Vec<String> = Vec::new();
    for e in walkdir(&wd.join("out2")) {
        got.push(e);
    }
    got.sort();
    assert_eq!(
        got,
        ["docs", "docs/b.md", "docs/deep", "docs/deep/r.bin", "zero"]
    );
    // No match: refused before any output, naming the pattern; with --json kind NoMatch.
    let r = lpk(wd, &["x", "one.lpk", "out3", "docs", "nope/*"]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("nope/*"), "{}", text(&r.stderr));
    assert!(!wd.join("out3").exists());
    let r = lpk(wd, &["--json", "x", "one.lpk", "out3", "nope/*"]);
    assert_eq!(r.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_str(text(&r.stderr).trim()).unwrap();
    assert_eq!(v["error"]["kind"], "NoMatch");
    assert!(v["error"]["message"].as_str().unwrap().contains("nope/*"));
    assert_eq!(v["error"]["path"], "one.lpk");
    // A bad pattern is a usage error (1).
    let r = lpk(wd, &["x", "one.lpk", "out3", "[x"]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
}

/// Every path under `root`, relative, `/`-separated.
fn walkdir(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            out.push(
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
    out
}

#[test]
fn list_patterns_filter_the_lines_and_refuse_a_pattern_without_a_match() {
    let (w, _src) = work();
    let wd = w.path();
    ok(&lpk(wd, &["a", "one.lpk", "src"]));
    assert_eq!(
        list(wd, &["one.lpk", "docs"]),
        ["docs", "docs/b.md", "docs/deep", "docs/deep/r.bin"]
    );
    assert_eq!(list(wd, &["one.lpk", "*.txt", "zero"]), ["a.txt", "zero"]);
    // --long keeps its header line; --json its objects.
    let r = lpk(wd, &["l", "--long", "one.lpk", "zero"]);
    ok(&r);
    let out = text(&r.stdout);
    assert!(out.starts_with("generation\t"), "{out}");
    assert_eq!(out.lines().count(), 2, "{out}");
    let r = lpk(wd, &["--json", "l", "one.lpk", "docs/*"]);
    ok(&r);
    let paths: Vec<String> = text(&r.stdout)
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["path"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(paths, ["docs/b.md", "docs/deep", "docs/deep/r.bin"]);
    // No match: exit 2, the pattern named, not a line written.
    let r = lpk(wd, &["l", "one.lpk", "docs", "nope"]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r.stderr));
    assert!(r.stdout.is_empty(), "{}", text(&r.stdout));
    assert!(text(&r.stderr).contains("nope"));
    let r = lpk(wd, &["--json", "l", "one.lpk", "nope"]);
    let v: serde_json::Value = serde_json::from_str(text(&r.stderr).trim()).unwrap();
    assert_eq!(v["error"]["kind"], "NoMatch");
}

#[test]
fn export_takes_the_same_patterns() {
    let (w, src) = work();
    let wd = w.path();
    ok(&lpk(wd, &["a", "one.lpk", "src"]));
    ok(&lpk(
        wd,
        &["export", "one.lpk", "sub.zip", "docs/deep", "zero"],
    ));
    let mut z = zip::ZipArchive::new(fs::File::open(wd.join("sub.zip")).unwrap()).unwrap();
    let mut names: Vec<String> = (0..z.len())
        .map(|i| z.by_index(i).unwrap().name().unwrap().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["docs/deep/", "docs/deep/r.bin", "zero"]);
    let mut content = Vec::new();
    std::io::copy(&mut z.by_name("docs/deep/r.bin").unwrap(), &mut content).unwrap();
    assert_eq!(content, fs::read(src.join("docs/deep/r.bin")).unwrap());
    // No match: refused, the output not created.
    let r = lpk(wd, &["export", "one.lpk", "none.zip", "nope"]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("nope"));
    assert!(!wd.join("none.zip").exists());
}
