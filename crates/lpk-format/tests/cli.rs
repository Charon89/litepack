#![allow(clippy::unwrap_used)]

use lpk_format::cli::{check_extraction_path, run};
use lpk_format::{EntryFlags, FormatError, Writer, WriterOptions};
use std::path::Path;

fn options() -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 16 * 1024,
        archive_id: [0xAB; 16],
        ..WriterOptions::default()
    }
}

fn data(seed: u8, n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(seed).wrapping_add(i as u8 >> 3))
        .collect()
}

/// Write an archive of `files` (sorted by the caller), plus extras.
fn make(
    path: &Path,
    dirs: &[&str],
    files: &[(&str, Vec<u8>)],
    links: &[(&str, &str)],
) -> std::io::Result<()> {
    let mut all: Vec<(&str, u8)> = dirs.iter().map(|d| (*d, 0)).collect();
    all.extend(files.iter().map(|f| (f.0, 1)));
    all.extend(links.iter().map(|l| (l.0, 2)));
    all.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut w = Writer::new(std::fs::File::create(path)?, options()).unwrap();
    for (p, kind) in all {
        match kind {
            0 => w.add_directory(p, EntryFlags::EMPTY, 0).unwrap(),
            1 => {
                let d = &files.iter().find(|f| f.0 == p).unwrap().1;
                w.add_file(p, EntryFlags::EMPTY, 0, &mut d.as_slice())
                    .unwrap();
            }
            _ => {
                let t = links.iter().find(|l| l.0 == p).unwrap().1;
                w.add_symlink(p, EntryFlags::EMPTY, 0, t.as_bytes())
                    .unwrap();
            }
        }
    }
    w.finish().unwrap();
    Ok(())
}

fn cli(args: &[&str]) -> (i32, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut argv = vec!["lpk-decode"];
    argv.extend_from_slice(args);
    let code = run(argv, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn sample(dir: &Path) -> (std::path::PathBuf, Vec<(&'static str, Vec<u8>)>) {
    let files = vec![
        ("docs/a.txt", data(3, 10_000)),
        ("docs/sub/b.bin", data(7, 40_000)),
        ("empty", Vec::new()),
        ("z.dat", data(11, 4096)),
    ];
    let p = dir.join("s.lpk");
    make(&p, &["docs", "docs/sub"], &files, &[]).unwrap();
    (p, files)
}

#[test]
fn list_verify_info() {
    let t = tempfile::tempdir().unwrap();
    let (p, _) = sample(t.path());
    let p = p.to_str().unwrap();

    let (code, out, _) = cli(&["list", p]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "Directory\t0\tdocs\nFile\t10000\tdocs/a.txt\nDirectory\t0\tdocs/sub\n\
         File\t40000\tdocs/sub/b.bin\nFile\t0\tempty\nFile\t4096\tz.dat\n"
    );

    let (code, out, _) = cli(&["verify", p]);
    assert_eq!(code, 0);
    // 10000 + 40000 + 4096 bytes in 4096-byte chunks: 3 + 10 + 1.
    assert_eq!(out, "ok: 6 entries, 14 chunks, 4 blocks\n");

    let (code, out, _) = cli(&["info", p]);
    assert_eq!(code, 0);
    for want in [
        "format: 1.0",
        "archive id: abababababababababababababababab",
        "generation: 0",
        "entries: 6",
        "chunks: 14",
        "blocks: 4",
        "envelope max_block_plain: 16384",
        "envelope decode_memory: 16384",
    ] {
        assert!(out.contains(want), "info lacks {want:?}:\n{out}");
    }
    let len = std::fs::metadata(p).unwrap().len();
    assert!(out.contains(&format!("length: {len} bytes")));
}

#[test]
fn verify_reports_the_first_error_with_exit_1() {
    let t = tempfile::tempdir().unwrap();
    let (p, _) = sample(t.path());
    let mut bytes = std::fs::read(&p).unwrap();
    bytes[100] ^= 0xFF; // inside the first block
    std::fs::write(&p, &bytes).unwrap();
    let (code, out, err) = cli(&["verify", p.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(out.is_empty());
    assert!(err.starts_with("error: chunk 0 does not match"), "{err}");

    let (code, _, err) = cli(&["verify", t.path().join("missing.lpk").to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(err.starts_with("error: i/o error"), "{err}");
}

#[test]
fn extract_writes_files_and_directories() {
    let t = tempfile::tempdir().unwrap();
    let (p, files) = sample(t.path());
    let out_dir = t.path().join("out");
    let (code, out, err) = cli(&["extract", p.to_str().unwrap(), out_dir.to_str().unwrap()]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(out, "extracted 4 files, 2 directories\n");
    for (name, d) in &files {
        assert_eq!(&std::fs::read(out_dir.join(name)).unwrap(), d, "{name}");
    }
    assert!(out_dir.join("docs").join("sub").is_dir());
    // A second extraction does not overwrite.
    let (code, _, err) = cli(&["extract", p.to_str().unwrap(), out_dir.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(err.starts_with("error: i/o error"), "{err}");
}

#[test]
fn extract_refuses_unsafe_paths_and_symlinks_before_writing() {
    for bad in [
        "CON",
        "a:b",
        "dir/nul.txt",
        "x/COM1",
        "lpt9.log",
        "dot.",
        "sp ",
    ] {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("bad.lpk");
        make(
            &p,
            &[],
            &[("ok.txt", data(1, 100)), (bad, data(2, 100))],
            &[],
        )
        .unwrap();
        let out_dir = t.path().join("out");
        let (code, _, err) = cli(&["extract", p.to_str().unwrap(), out_dir.to_str().unwrap()]);
        assert_eq!(code, 1, "{bad}");
        assert!(err.contains("unsafe path"), "{bad}: {err}");
        assert!(!out_dir.exists(), "{bad}: nothing is written");
    }
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("link.lpk");
    make(&p, &[], &[("f", data(1, 10))], &[("l", "f")]).unwrap();
    let out_dir = t.path().join("out");
    let (code, _, err) = cli(&["extract", p.to_str().unwrap(), out_dir.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(err.contains("symlink entry \"l\" refused"), "{err}");
    assert!(!out_dir.exists());
}

#[test]
fn extraction_path_rules() {
    for ok in [
        "a/b", "console", "com10", "com0", "aux_x", "nul2", "a.b/c.d", ".hidden",
    ] {
        check_extraction_path(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
    for bad in [
        "CON",
        "con",
        "Prn.txt",
        "AUX.tar.gz",
        "NUL",
        "COM1",
        "com9.x",
        "LPT1",
        "lpt9",
        "a/CON/b",
        "a:b",
        "C:",
        "x/y:z",
        "a.",
        "a /b",
        "b ",
    ] {
        assert!(
            matches!(
                check_extraction_path(bad),
                Err(FormatError::UnsafePath { .. })
            ),
            "{bad}"
        );
    }
}

#[test]
fn usage_errors() {
    let (code, _, err) = cli(&["frobnicate"]);
    assert_eq!(code, 2);
    assert!(err.contains("Usage"), "{err}");
    let (code, out, _) = cli(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("verify"));
}
