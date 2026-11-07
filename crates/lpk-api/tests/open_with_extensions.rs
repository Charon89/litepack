//! E6-4: the foreign extensions `lpk-gui.exe --register-shell --open-with` lists in "Open with"
//! (`crates/lpk-gui/src/open_with_extensions.txt`; `lpk-gui` depends on no `lpk-*` crate but this
//! one and `lpk-gui-sys`) are what the engine opens today: ZIP, 7z, tar and tgz, plus RAR exactly
//! when `Format::Rar.read_by_worker()`. E4-4 making RAR readable fails this test until `rar` is
//! added to the file (decision (c) of the E6 header: RAR is offered only once it can be opened).

use std::collections::BTreeSet;
use std::path::Path;

use lpk_core::foreign::Format;

#[test]
fn open_with_extensions_are_what_the_engine_opens() {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-gui/src/open_with_extensions.txt"),
    )
    .expect("lpk-gui's list");
    let listed: BTreeSet<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    let mut want: BTreeSet<String> = ["zip", "7z", "tar", "tgz"].map(String::from).into();
    if Format::Rar.read_by_worker() {
        want.insert("rar".to_string());
    }
    assert_eq!(listed, want);
    // Every other listed name is a format the worker reads.
    assert!(Format::Zip.read_by_worker() && Format::SevenZ.read_by_worker());
    assert!(Format::Tar.read_by_worker() && Format::Gzip.read_by_worker());
}
