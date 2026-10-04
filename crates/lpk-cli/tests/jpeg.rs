//! `lpk x` and `lpk t` open archives through `lpk-core`'s full reader: a peeled JPEG extracts and
//! verifies, while the format's own tool (`lpk-decode`, the 1.0 reader) reports
//! `UnimplementedPrimitive`.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

fn run(args: &[&str]) -> (i32, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = lpk_cli::run(args.iter().copied(), &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn a_peeled_jpeg_extracts_and_tests_through_lpk() {
    let jpeg = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("lpk-core")
            .join("tests")
            .join("fixtures")
            .join("baseline.jpg"),
    )
    .unwrap();
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("p.jpg"), &jpeg).unwrap();
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("a.lpk");
    let out = work.path().join("out");
    let (a, s) = (arch.to_str().unwrap(), src.path().to_str().unwrap());
    let (code, _, err) = run(&["lpk", "a", "-v", a, s]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("peel: 1 files peeled"), "{err}");
    let (code, out_t, err) = run(&["lpk", "t", a]);
    assert_eq!((code, err.as_str()), (0, ""), "{out_t}");
    let (code, _, err) = run(&["lpk", "x", a, out.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert!(std::fs::read(out.join("p.jpg")).unwrap() == jpeg);
    // The 1.0 reference tool cannot rebuild it.
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let code = lpk_format::cli::run(["lpk-decode", "verify", a], &mut o, &mut e);
    assert_eq!(code, 1);
    assert!(String::from_utf8(e).unwrap().contains("7"));
}
