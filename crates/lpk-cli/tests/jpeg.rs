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

/// `--jpeg-peel off` (E2-25, route (a)): the Fast tier stores the JPEG as it is — no
/// reconstruction record, no `jpeg-reconstruct` step, so the format's 1.0 tool reads it — and
/// `lpk x` extracts it by copy; `--jpeg-peel on` keeps the peel; the flag is refused with
/// `--store`.
#[test]
fn jpeg_peel_off_stores_the_jpeg_and_extracts_it_by_copy() {
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
    let s = src.path().to_str().unwrap();
    for (setting, peeled) in [("off", false), ("on", true)] {
        let arch = work.path().join(format!("{setting}.lpk"));
        let out = work.path().join(format!("out-{setting}"));
        let a = arch.to_str().unwrap();
        let (code, _, err) = run(&["lpk", "a", "-v", "--fast", "--jpeg-peel", setting, a, s]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            err.contains("peel: 1 files peeled"),
            peeled,
            "{setting}: {err}"
        );
        let (code, info, err) = run(&["lpk", "info", a]);
        assert_eq!(code, 0, "{err}");
        assert!(
            info.contains(&format!("records: {peeled}")),
            "{setting}: {info}"
        );
        let (code, _, err) = run(&["lpk", "x", a, out.to_str().unwrap()]);
        assert_eq!(code, 0, "{err}");
        assert!(
            std::fs::read(out.join("p.jpg")).unwrap() == jpeg,
            "{setting}"
        );
        // The 1.0 reference tool, which has no `jpeg-reconstruct`, verifies only the stored one.
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let code = lpk_format::cli::run(["lpk-decode", "verify", a], &mut o, &mut e);
        assert_eq!(
            code == 0,
            !peeled,
            "{setting}: {}",
            String::from_utf8_lossy(&e)
        );
    }
    let arch = work.path().join("store.lpk");
    let (code, _, err) = run(&[
        "lpk",
        "a",
        "--store",
        "--jpeg-peel",
        "off",
        arch.to_str().unwrap(),
        s,
    ]);
    assert_eq!(code, 1, "{err}");
    assert!(!arch.exists());
}
