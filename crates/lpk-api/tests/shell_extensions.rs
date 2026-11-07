//! E6-2: the archive extensions Explorer's menu recognises (`crates/lpk-shell/src/
//! archive_extensions.txt`, one list kept in `lpk-shell`, which depends on no `lpk-*` crate) stay
//! equal to `.lpk` plus the foreign formats the engine opens today, plus their usual aliases.
//! "Opens today" is `Format::read_by_worker`, so E4-4 making RAR readable fails this test until
//! `rar` is in the list. E4-4 made every format of `docs/FORMATS.md`'s libarchive rows readable and
//! added them (and `lha`, `deb`) here.

use std::collections::BTreeSet;
use std::path::Path;

use lpk_core::foreign::Format;

/// The variant's place in [`ALL`]; the exhaustive match makes a new `Format` variant fail to
/// compile here until it is added, and the test checks `ALL` against it.
fn index(f: Format) -> usize {
    match f {
        Format::TestFixture => 0,
        Format::Zip => 1,
        Format::SevenZ => 2,
        Format::Tar => 3,
        Format::Gzip => 4,
        Format::Bzip2 => 5,
        Format::Xz => 6,
        Format::Zstd => 7,
        Format::Lzip => 8,
        Format::Lzma => 9,
        Format::Rar => 10,
        Format::Iso => 11,
        Format::Cab => 12,
        Format::Lha => 13,
        Format::Ar => 14,
        Format::Cpio => 15,
        Format::Rpm => 16,
        Format::Compress => 17,
        Format::Uue => 18,
    }
}

const ALL: [Format; 19] = [
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
    Format::Iso,
    Format::Cab,
    Format::Lha,
    Format::Ar,
    Format::Cpio,
    Format::Rpm,
    Format::Compress,
    Format::Uue,
];

/// Short names of a format that mean the same archive: `(alias, format's extension)`.
const ALIASES: [(&str, &str); 7] = [
    ("tgz", "gz"),
    ("tbz", "bz2"),
    ("tbz2", "bz2"),
    ("txz", "xz"),
    ("tzst", "zst"),
    ("lha", "lzh"),
    ("deb", "ar"),
];

#[test]
fn the_menus_archive_extensions_are_lpk_plus_what_the_engine_opens() {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-shell/src/archive_extensions.txt"),
    )
    .expect("lpk-shell's list");
    let listed: BTreeSet<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();

    for (i, f) in ALL.into_iter().enumerate() {
        assert_eq!(index(f), i, "ALL lists every Format variant, in order");
    }
    // What the engine opens today: whatever its worker reads, the test fixture apart.
    let mut want: BTreeSet<String> = BTreeSet::from(["lpk".to_string()]);
    for f in ALL
        .into_iter()
        .filter(|f| *f != Format::TestFixture && f.read_by_worker())
    {
        want.insert(f.name().to_string());
    }
    for (alias, target) in ALIASES {
        assert!(want.contains(target), "{alias}: {target} is not opened");
        want.insert(alias.to_string());
    }
    // `lzip`'s short form.
    want.insert("tlz".to_string());
    assert_eq!(listed, want);
    for e in &listed {
        assert_eq!(*e, e.to_lowercase(), "lower case, no dot");
        assert!(!e.contains('.'));
    }
}
