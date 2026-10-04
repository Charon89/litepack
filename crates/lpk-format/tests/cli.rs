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
    assert!(
        err.starts_with("error: payload hash mismatch in frame kind 2"),
        "{err}"
    );

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
        "a/b",
        "console",
        "com10",
        "com0",
        "aux_x",
        "nul2",
        "a.b/c.d",
        ".hidden",
        "con x.txt",
        "com\u{b9}0",
        "lpt\u{b9}\u{b2}",
        "conin",
        "conout$x",
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
        "con .txt",
        "NUL .log",
        "nul  .tar.gz",
        "COM\u{b9}",
        "com\u{b2}.txt",
        "LPT\u{b3}",
        "lpt\u{b9}.x",
        "CONIN$",
        "conout$",
        "CONOUT$.txt",
        "d/conin$ .x",
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

#[test]
fn a_damaged_later_chunk_leaves_no_partial_file() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("d.lpk");
    make(
        &p,
        &[],
        &[("a.txt", data(1, 100)), ("big", data(9, 60_000))],
        &[],
    )
    .unwrap();
    // Damage the last block, which holds the end of `big`.
    let (off, len) = {
        let a = lpk_format::Archive::open(
            std::fs::File::open(&p).unwrap(),
            &lpk_format::Resources::default(),
        )
        .unwrap();
        let b = *a.index().blocks.last().unwrap();
        (b.frame_offset, b.frame_len)
    };
    let mut bytes = std::fs::read(&p).unwrap();
    bytes[(off + len / 2) as usize] ^= 0xFF;
    std::fs::write(&p, &bytes).unwrap();
    let out_dir = t.path().join("out");
    let (code, _, err) = cli(&["extract", p.to_str().unwrap(), out_dir.to_str().unwrap()]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("does not match"), "{err}");
    let names: Vec<_> = std::fs::read_dir(&out_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["a.txt"], "no big and no big.lpk-partial");
}

#[test]
fn list_escapes_control_characters() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("c.lpk");
    make(&p, &[], &[("a\nb\u{1b}c", data(1, 5))], &[]).unwrap();
    let (code, out, _) = cli(&["list", p.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(out, "File\t5\ta\\nb\\u{1b}c\n");
}

#[test]
fn an_existing_target_is_left_untouched() {
    let t = tempfile::tempdir().unwrap();
    let (p, _) = sample(t.path());
    let out_dir = t.path().join("out");
    std::fs::create_dir_all(out_dir.join("docs")).unwrap();
    std::fs::write(out_dir.join("docs").join("a.txt"), b"mine").unwrap();
    let (code, _, err) = cli(&["extract", p.to_str().unwrap(), out_dir.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(err.starts_with("error: i/o error"), "{err}");
    assert_eq!(
        std::fs::read(out_dir.join("docs").join("a.txt")).unwrap(),
        b"mine"
    );
}

/// An archive of `sample`'s files with 10 percent recovery in 4 KiB shards.
fn recovering(dir: &Path) -> std::path::PathBuf {
    let p = dir.join("r.lpk");
    let mut o = options();
    o.recovery = lpk_format::RecoveryOptions {
        percent: 10,
        shard_len: 4096,
        group_shards: 16,
    };
    let mut w = Writer::new(std::fs::File::create(&p).unwrap(), o).unwrap();
    for (name, d) in [("a", data(3, 50_000)), ("b", data(5, 30_000))] {
        w.add_file(name, EntryFlags::EMPTY, 0, &mut d.as_slice())
            .unwrap();
    }
    w.finish().unwrap();
    p
}

#[test]
fn check_and_repair_on_a_damaged_archive() {
    let t = tempfile::tempdir().unwrap();
    let p = recovering(t.path());
    let good = std::fs::read(&p).unwrap();
    let (code, out, err) = cli(&["check", p.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "recovery frames: 2, unusable: 0, damaged shards: 0, repaired shards: 0\n"
    );
    // Damage two shards: bytes in the first and in the third shard.
    let mut bad = good.clone();
    bad[40] ^= 0xFF;
    bad[32 + 2 * 4096 + 7] ^= 0x55;
    std::fs::write(&p, &bad).unwrap();
    let (code, out, err) = cli(&["check", p.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(out.contains("damaged shards: 2"), "{out}");
    assert!(err.contains("damage found: 2 shards damaged"), "{err}");
    let fixed = t.path().join("fixed.lpk");
    let (code, out, err) = cli(&["repair", p.to_str().unwrap(), fixed.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "recovery frames: 2, unusable: 0, damaged shards: 2, repaired shards: 2\n"
    );
    assert_eq!(std::fs::read(&fixed).unwrap(), good);
    let (code, out, _) = cli(&["verify", fixed.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(out.starts_with("ok:"));
    // The target is never replaced.
    let (code, _, err) = cli(&["repair", p.to_str().unwrap(), fixed.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(err.starts_with("error: i/o error"), "{err}");
}

#[test]
fn repair_exits_1_beyond_capacity_and_keeps_the_copy() {
    let t = tempfile::tempdir().unwrap();
    let p = recovering(t.path());
    let mut bad = std::fs::read(&p).unwrap();
    // 80 KB of data is about 20 shards, 10 percent is 2: damage 3.
    for s in 0..3 {
        bad[32 + s * 4096 + 1] ^= 0xFF;
    }
    std::fs::write(&p, &bad).unwrap();
    let fixed = t.path().join("fixed.lpk");
    let (code, _, err) = cli(&["repair", p.to_str().unwrap(), fixed.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(err.contains("3 shards damaged, it can rebuild"), "{err}");
    assert_eq!(std::fs::read(&fixed).unwrap(), bad);
}

#[test]
fn repair_of_a_damaged_index_writes_nothing() {
    let t = tempfile::tempdir().unwrap();
    let p = recovering(t.path());
    let mut bad = std::fs::read(&p).unwrap();
    let n = bad.len();
    bad[n - 100] ^= 0xFF;
    std::fs::write(&p, &bad).unwrap();
    let fixed = t.path().join("fixed.lpk");
    let (code, _, err) = cli(&["repair", p.to_str().unwrap(), fixed.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(err.starts_with("error: "), "{err}");
    assert!(!fixed.exists());
}

#[test]
fn check_on_an_archive_without_recovery_reports_zero_frames() {
    let t = tempfile::tempdir().unwrap();
    let (p, _) = sample(t.path());
    let (code, out, _) = cli(&["check", p.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "recovery frames: 0, unusable: 0, damaged shards: 0, repaired shards: 0\n"
    );
}
