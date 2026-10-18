//! E3-7's generated reference: `docs/cli/lpk.md` (clap-markdown) and the man page
//! `docs/cli/lpk.1` (clap_mangen) are generated from the command line as clap defines it
//! (`lpk_cli::command()`, the crate version without the build hash) and must equal the
//! committed files. Line endings are normalised to LF on both sides (the repository's
//! `.gitattributes` checks everything out as LF anyway). To rewrite the files after a change
//! to the command line: `LPK_CLI_DOCS_WRITE=1 cargo test -p lpk-cli --test docs`.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

const WRITE_VAR: &str = "LPK_CLI_DOCS_WRITE";

fn docs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/cli")
}

fn lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn markdown() -> String {
    lf(&clap_markdown::help_markdown_command(&lpk_cli::command()))
}

fn man_page() -> String {
    let mut buf = Vec::new();
    clap_mangen::Man::new(lpk_cli::command())
        .render(&mut buf)
        .unwrap();
    lf(&String::from_utf8(buf).unwrap())
}

/// Compare one generated text with its committed file, or rewrite the file when asked.
fn check(name: &str, generated: &str) {
    let path = docs_dir().join(name);
    if std::env::var_os(WRITE_VAR).is_some() {
        std::fs::create_dir_all(docs_dir()).unwrap();
        std::fs::write(&path, generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; generate it with {WRITE_VAR}=1 cargo test -p lpk-cli --test docs",
            path.display()
        )
    });
    assert_eq!(
        lf(&committed),
        generated,
        "{} is out of date; regenerate it with {WRITE_VAR}=1 cargo test -p lpk-cli --test docs",
        path.display()
    );
}

#[test]
fn the_markdown_reference_equals_the_generated_one() {
    let md = markdown();
    assert!(md.contains("# Command-Line Help for `lpk`"), "{md}");
    for sub in ["`lpk a`", "`lpk l`", "`lpk x`", "`lpk t`", "`lpk append`"] {
        assert!(md.contains(sub), "{sub} missing from the reference");
    }
    // The build hash never reaches the reference, so one version has one text.
    assert!(!md.contains(env!("LPK_GIT_COMMIT")), "{md}");
    check("lpk.md", &md);
}

#[test]
fn the_man_page_equals_the_generated_one() {
    let man = man_page();
    assert!(man.contains(".TH lpk 1"), "{man}");
    assert!(!man.contains(env!("LPK_GIT_COMMIT")), "{man}");
    check("lpk.1", &man);
}
