//! The revision 1.1 vectors (in `crates/lpk-format/tests/vectors/`), built by the JPEG peel
//! through the Fast pipeline, and their full-reader expectations (`expected-1.1.toml` beside
//! them): `jpeg-peel.lpk`, a peeled JPEG in a new archive, and `journal-record.lpk`, a plain
//! generation 0 and an appended generation that adds a peeled JPEG and its record. The 1.0
//! expectations of `jpeg-peel.lpk` (listing, `info`, the `UnimplementedPrimitive` of `verify`)
//! are in `expected.toml`, written by lpk-format's conformance test. Regenerate with
//! `cargo test -p lpk-core --test jpeg_vector -- --ignored`, then lpk-format's
//! `cargo test -p lpk-format --test conformance -- --ignored`.
#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use lpk_core::{register_full_reader, FastOptions, Pipeline};
use lpk_format::{Archive, EntryFlags, EntryKind, Resources, Writer, WriterOptions};

const VECTOR: &str = "jpeg-peel.lpk";
const RECORD_VECTOR: &str = "journal-record.lpk";
const EXPECTED: &str = "expected-1.1.toml";
/// The revision 1.1 vectors, in the order of `expected-1.1.toml`.
const VECTORS: [&str; 2] = [VECTOR, RECORD_VECTOR];

fn vectors() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("lpk-format")
        .join("tests")
        .join("vectors")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

/// `photo.jpg`: the baseline fixture with an APP2 `MPF\0` segment, then the secondary fixture
/// after its EOI with a few bytes before and after it.
fn photo() -> Vec<u8> {
    let base = fixture("baseline.jpg");
    let mut v = base[..2].to_vec();
    v.extend_from_slice(&[0xFF, 0xE2, 0x00, 0x0A]);
    v.extend_from_slice(b"MPF\0\0\0\0\0");
    v.extend_from_slice(&base[2..]);
    v.extend_from_slice(b"gap");
    v.extend_from_slice(&fixture("secondary.jpg"));
    v.extend_from_slice(b"tail bytes");
    v
}

/// `readme.txt` of `journal-record.lpk`'s generation 0.
fn readme() -> Vec<u8> {
    b"journal-record.lpk: generation 0 holds this text file, store-encoded.\n\
      Generation 1 is an append that adds photo.jpg, peeled, with its jpeg record:\n\
      the Records frame of the append restates the records before it (none) and adds one.\n"
        .to_vec()
}

fn set_mtime(p: &Path) {
    let f = std::fs::File::options().write(true).open(p).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_nanos(1000))
        .unwrap();
}

/// A directory holding `photo.jpg` with the vectors' mtime.
fn photo_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("photo.jpg");
    std::fs::write(&p, photo()).unwrap();
    set_mtime(&p);
    dir
}

/// The Fast pipeline the vectors are written with: chunk size 4096 (the fixed cut, no fold
/// stage), archive id 0x5A repeated.
fn pipeline() -> Pipeline {
    let mut pipeline = Pipeline::fast(FastOptions::default());
    pipeline.fold = None;
    pipeline.seal.writer.chunk_size = 4096;
    pipeline.seal.writer.archive_id = [0x5A; 16];
    pipeline
}

/// `jpeg-peel.lpk`'s bytes: `photo.jpg` through the pipeline, mtime 1000 ns.
fn build() -> Vec<u8> {
    let dir = photo_dir();
    let mut out = Vec::new();
    let s = pipeline().run(dir.path(), &mut out).unwrap();
    assert_eq!(s.peel.peeled.files, 1);
    out
}

/// `journal-record.lpk`'s bytes: generation 0 is `readme.txt`, store-encoded by the format's
/// writer under revision 1.1 (header minor 1, so the append may use `jpeg-reconstruct`), the
/// same chunk size and archive id; generation 1 appends `photo.jpg` through the pipeline.
fn build_record() -> Vec<u8> {
    let opts = WriterOptions {
        chunk_size: 4096,
        archive_id: [0x5A; 16],
        ..WriterOptions::default()
    };
    let mut g0 = Vec::new();
    let mut w = Writer::new_revision(&mut g0, opts, 1).unwrap();
    w.add_file("readme.txt", EntryFlags::EMPTY, 1000, &mut &readme()[..])
        .unwrap();
    w.finish().unwrap();
    let dir = photo_dir();
    let existing = Archive::open(Cursor::new(g0.clone()), &Resources::default()).unwrap();
    let mut tail = Vec::new();
    let s = pipeline()
        .append(dir.path(), existing, &mut tail, None)
        .unwrap();
    assert_eq!(s.peel.peeled.files, 1);
    assert_eq!(s.writer.generation, 1);
    g0.extend(tail);
    g0
}

fn build_named(name: &str) -> Vec<u8> {
    match name {
        VECTOR => build(),
        RECORD_VECTOR => build_record(),
        _ => unreachable!(),
    }
}

fn tool(args: &[&str]) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = lpk_format::cli::run_with(
        args.iter().copied(),
        &mut out,
        &mut err,
        register_full_reader::<std::fs::File>,
    );
    assert_eq!(String::from_utf8(err).unwrap(), "");
    (code, String::from_utf8(out).unwrap())
}

fn open(b: Vec<u8>) -> Archive<Cursor<Vec<u8>>> {
    let mut a = Archive::open(Cursor::new(b), &Resources::default()).unwrap();
    register_full_reader(&mut a);
    a
}

fn render() -> String {
    let mut s = String::from(
        "# Expected results of a revision 1.1 reader (docs/spec/CONFORMANCE.md).\n\
         # Generated by lpk-core's full reader; do not edit. `files`: path size BLAKE3-256 of the\n\
         # extracted bytes, in entry-table order; `verify` is the stdout of `verify` (exit code 0).\n",
    );
    for name in VECTORS {
        let path = vectors().join(name);
        let mut a = open(std::fs::read(&path).unwrap());
        let t = a.entry_table().unwrap();
        let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
        writeln!(s, "\n[\"{name}\"]").unwrap();
        writeln!(s, "files = [").unwrap();
        for e in &entries {
            let mut got = Vec::new();
            if e.kind == EntryKind::File {
                a.extract(e, &mut got).unwrap();
            }
            let line = format!("{} {} {}", e.path, got.len(), blake3::hash(&got).to_hex());
            writeln!(s, "  {line:?},").unwrap();
        }
        writeln!(s, "]").unwrap();
        let (code, out) = tool(&["lpk", "verify", path.to_str().unwrap()]);
        assert_eq!(code, 0);
        writeln!(s, "verify = {out:?}").unwrap();
    }
    s
}

/// The vectors are committed as the workspace build writes them (`cargo nextest run
/// --workspace`, what CI runs). `lepton_jpeg` deflates its stream header with `flate2`, whose
/// backend Cargo unifies across the build: in the workspace `lpk-bench` enables the zlib backend,
/// while `lpk-core` built alone gets `miniz_oxide`, and the two write different (equally valid)
/// zlib bytes. The committed vectors were written by the workspace build, that is with flate2's
/// zlib backend. A build with the other backend gets a structural check instead of byte identity:
/// the same Merkle root (the chunk table hashes the original bytes), the same records, the same
/// blocks but the Lepton one (the last), and the same extracted files. When the environment
/// variable `CI` is set (CI builds the workspace, the backend of the committed vectors) a
/// difference fails instead.
#[test]
fn the_vectors_regenerate_identically() {
    for name in VECTORS {
        let committed = std::fs::read(vectors().join(name)).unwrap();
        let built = build_named(name);
        if committed == built {
            continue;
        }
        assert!(
            std::env::var_os("CI").is_none(),
            "{name} differs from this build's under CI, where byte identity is required"
        );
        eprintln!("{name} differs from this build's (flate2 backend?): structural check");
        let (mut a, mut b) = (open(committed.clone()), open(built.clone()));
        assert_eq!(a.index().merkle_root, b.index().merkle_root);
        let recs = |x: &mut Archive<Cursor<Vec<u8>>>| {
            let t = x.records().unwrap().unwrap();
            let t = t.table().unwrap();
            t.iter().map(|r| r.unwrap()).collect::<Vec<_>>()
        };
        assert_eq!(recs(&mut a), recs(&mut b));
        let blocks = |x: &Archive<Cursor<Vec<u8>>>, bytes: &[u8]| {
            x.index()
                .blocks
                .iter()
                .map(|l| {
                    bytes[l.frame_offset as usize..(l.frame_offset + l.frame_len) as usize].to_vec()
                })
                .collect::<Vec<_>>()
        };
        let (ba, bb) = (blocks(&a, &committed), blocks(&b, &built));
        assert_eq!(ba.len(), bb.len());
        assert!(
            ba[..ba.len() - 1] == bb[..bb.len() - 1],
            "{name}: a block before the Lepton one differs"
        );
        for x in [&mut a, &mut b] {
            x.verify().unwrap();
            let t = x.entry_table().unwrap();
            let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
            for e in &entries {
                let mut got = Vec::new();
                x.extract(e, &mut got).unwrap();
                let want = if e.path == "photo.jpg" {
                    photo()
                } else {
                    readme()
                };
                assert!(got == want, "{name}: {}", e.path);
            }
        }
    }
}

#[test]
fn the_vectors_extract_to_the_originals_and_match_their_expectations() {
    let want = std::fs::read_to_string(vectors().join(EXPECTED)).unwrap();
    assert_eq!(want.replace("\r\n", "\n"), render());
    let line = format!(
        "photo.jpg {} {}",
        photo().len(),
        blake3::hash(&photo()).to_hex()
    );
    assert_eq!(want.matches(&line).count(), 2, "{want}");
    let line = format!(
        "readme.txt {} {}",
        readme().len(),
        blake3::hash(&readme()).to_hex()
    );
    assert!(want.contains(&line), "{want}");
    for name in VECTORS {
        let a = open(std::fs::read(vectors().join(name)).unwrap());
        assert_eq!(a.header().version.minor, 1, "{name}");
    }
    // The journal vector: two generations; the appended one wrote the records frame and the
    // peeled file's blocks after generation 0's, and holds the one record.
    let mut a = open(std::fs::read(vectors().join(RECORD_VECTOR)).unwrap());
    assert_eq!(a.generation(), 1);
    assert_eq!(a.history().unwrap().len(), 2);
    let idx = a.index();
    assert_eq!(idx.generations.len(), 2);
    let gen1_start = idx.generations[1].start_offset;
    assert_eq!(
        idx.blocks.len(),
        3,
        "readme, the nested parts, the Lepton stream"
    );
    assert!(idx.blocks[0].frame_offset < gen1_start);
    assert!(idx.blocks[1].frame_offset >= gen1_start);
    assert!(idx.records.unwrap().offset >= gen1_start);
    assert_eq!(a.records().unwrap().unwrap().len(), 1);
    let t = a.entry_table().unwrap();
    let paths: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap().path).collect();
    assert_eq!(paths, ["photo.jpg", "readme.txt"]);
}

/// The independent decoder (`lpk-check`) in its revision 1.0 mode (`--revision-1-0`) reports
/// `UnimplementedPrimitive` for primitive 7 on the vectors; without the flag it decodes revision
/// 1.1 and `verify` prints its `ok:` line.
#[test]
fn lpk_check_reports_unimplemented_primitive_7() {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let bin = dir.join(format!("lpk-check{}", std::env::consts::EXE_SUFFIX));
    if !bin.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let st = std::process::Command::new(cargo)
            .args(["build", "-p", "lpk-check"])
            .status()
            .unwrap();
        assert!(st.success());
    }
    for name in VECTORS {
        let run = |args: &[&str]| {
            let out = std::process::Command::new(&bin)
                .args(args)
                .arg(vectors().join(name))
                .output()
                .unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                text,
            )
        };
        let (ok, _, text) = run(&["--revision-1-0", "verify"]);
        assert!(!ok, "{name}: {text}");
        assert!(text.contains("0x0007"), "{name}: {text}");
        // The tool prints the message only; the class comes from the same decoder as a library.
        let data = std::fs::read(vectors().join(name)).unwrap();
        let opts = lpk_check::archive::Options {
            revision_1_0: true,
            ..lpk_check::archive::Options::default()
        };
        let e = lpk_check::archive::Archive::open(data, opts)
            .unwrap()
            .verify()
            .unwrap_err();
        assert_eq!(e.class, "UnimplementedPrimitive");
        assert!(e.to_string().contains("0x0007"), "{name}: {e}");
        // Without the flag the independent decoder reads revision 1.1 and verifies the vector.
        let (ok, stdout, text) = run(&["verify"]);
        assert!(ok, "{name}: {text}");
        assert!(stdout.starts_with("ok:"), "{name}: {text}");
    }
}

#[test]
#[ignore = "rewrites jpeg-peel.lpk, journal-record.lpk and expected-1.1.toml in crates/lpk-format/tests/vectors"]
fn regenerate() {
    for name in VECTORS {
        std::fs::write(vectors().join(name), build_named(name)).unwrap();
    }
    std::fs::write(vectors().join(EXPECTED), render()).unwrap();
}
