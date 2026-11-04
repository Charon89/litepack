//! E6-2: the archive extensions Explorer's menu recognises (`crates/lpk-shell/src/
//! archive_extensions.txt`, one list kept in `lpk-shell`, which depends on no `lpk-*` crate) stay
//! equal to `.lpk` plus the foreign formats the engine opens today, plus their usual aliases.
//! There is no `docs/FORMATS.md` yet: E4-4 adds it with RAR, ISO and CAB and updates the list
//! and this test then.

use std::collections::BTreeSet;
use std::path::Path;

use lpk_core::foreign::Format;

/// Whether the engine opens the format today; a new `Format` variant fails to compile here until
/// it is placed.
fn opened_today(f: Format) -> bool {
    match f {
        Format::TestFixture | Format::Rar => false,
        Format::Zip
        | Format::SevenZ
        | Format::Tar
        | Format::Gzip
        | Format::Bzip2
        | Format::Xz
        | Format::Zstd
        | Format::Lzip
        | Format::Lzma => true,
    }
}

const ALL: [Format; 11] = [
    Format::TestFixture,
    Format::Zip,
    Format::SevenZ,
    Format::Tar,
    Format::Gzip,
    Format::Bzip2,
    Format::Xz,
    Format::Zstd,
    Format::Lzip,
    Format::Lzma,
    Format::Rar,
];

/// Short names of a format that mean the same archive: `(alias, format's extension)`.
const ALIASES: [(&str, &str); 5] = [
    ("tgz", "gz"),
    ("tbz", "bz2"),
    ("tbz2", "bz2"),
    ("txz", "xz"),
    ("tzst", "zst"),
];

#[test]
fn the_menus_archive_extensions_are_lpk_plus_what_the_engine_opens() {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-shell/src/archive_extensions.txt"),
    )
    .expect("lpk-shell's list");
    let listed: BTreeSet<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    let mut want: BTreeSet<String> = BTreeSet::from(["lpk".to_string()]);
    for f in ALL.into_iter().filter(|f| opened_today(*f)) {
        assert!(
            f.read_by_worker(),
            "{}: opened but not read by the worker",
            f.name()
        );
        want.insert(f.name().to_string());
    }
    // `lz` and `tlz` for lzip, `lzma` for LZMA-alone: their names are the extensions.
    for (alias, target) in ALIASES {
        assert!(want.contains(target), "{alias}: {target} is not opened");
        want.insert(alias.to_string());
    }
    want.insert("tlz".to_string());
    let listed: BTreeSet<String> = listed.into_iter().map(String::from).collect();
    assert_eq!(listed, want);
    for e in &listed {
        assert_eq!(*e, e.to_lowercase(), "lower case, no dot");
        assert!(!e.contains('.'));
    }
}
