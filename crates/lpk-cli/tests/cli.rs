//! The `lpk` binary end to end on temporary trees.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use lpk_core::{DictionaryKind, DictionaryPolicy, FastOptions, ProvidedDictionaries};
use lpk_format::{Archive, Resources};

fn lpk(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lpk"))
        .args(args)
        .output()
        .unwrap()
}

fn run(args: &[&str]) -> Output {
    let os: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
    lpk(&os)
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

fn random(len: usize) -> Vec<u8> {
    let mut s = 0x1234_5678_9ABC_DEF1u64;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

fn make_tree(root: &Path) {
    fs::create_dir_all(root.join("docs/deep")).unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::write(root.join("a.txt"), prose(1, 200_000)).unwrap();
    fs::write(root.join("docs/b.md"), prose(2, 70_000)).unwrap();
    fs::write(root.join("docs/deep/r.bin"), random(150_000)).unwrap();
    fs::write(root.join("zero"), b"").unwrap();
}

fn tree_hash(root: &Path) -> Vec<(String, Option<[u8; 32]>)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Option<[u8; 32]>)>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if p.is_dir() {
                out.push((rel, None));
                walk(&p, base, out);
            } else {
                out.push((rel, Some(*blake3::hash(&fs::read(&p).unwrap()).as_bytes())));
            }
        }
    }
    let mut v = Vec::new();
    walk(root, root, &mut v);
    v.sort();
    v
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn add_extract_test_round_trip_is_bit_exact() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("t.lpk");
    let out = work.path().join("out");
    let a = run(&["a", arch.to_str().unwrap(), src.path().to_str().unwrap()]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let x = run(&["x", arch.to_str().unwrap(), out.to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out), tree_hash(src.path()));
    let t = run(&["t", arch.to_str().unwrap()]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
    assert!(text(&t.stdout).starts_with("ok: "), "{}", text(&t.stdout));
}

#[test]
fn extract_threads_1_and_8_give_the_same_tree_and_v_prints_the_plan() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("t.lpk");
    let a = run(&["a", arch.to_str().unwrap(), src.path().to_str().unwrap()]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let one = work.path().join("one");
    let eight = work.path().join("eight");
    let x1 = run(&[
        "x",
        "--threads",
        "1",
        "-v",
        arch.to_str().unwrap(),
        one.to_str().unwrap(),
    ]);
    assert_eq!(x1.status.code(), Some(0), "{}", text(&x1.stderr));
    assert!(
        text(&x1.stderr).contains("pool: 1 workers"),
        "{}",
        text(&x1.stderr)
    );
    let x8 = run(&[
        "x",
        "--threads",
        "8",
        "-v",
        arch.to_str().unwrap(),
        eight.to_str().unwrap(),
    ]);
    assert_eq!(x8.status.code(), Some(0), "{}", text(&x8.stderr));
    assert!(
        text(&x8.stderr).contains("blocks decoded"),
        "{}",
        text(&x8.stderr)
    );
    assert_eq!(tree_hash(&one), tree_hash(&eight));
    assert_eq!(tree_hash(&one), tree_hash(src.path()));
    let bad = run(&[
        "x",
        "--threads",
        "0",
        arch.to_str().unwrap(),
        one.to_str().unwrap(),
    ]);
    assert_eq!(bad.status.code(), Some(1));
    let huge = run(&[
        "x",
        "--threads",
        "1000000",
        arch.to_str().unwrap(),
        one.to_str().unwrap(),
    ]);
    assert_eq!(huge.status.code(), Some(1), "{}", text(&huge.stderr));
    assert!(text(&huge.stderr).contains("--threads"));
}

#[test]
fn add_refuses_an_existing_archive_and_extract_refuses_to_overwrite() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("t.lpk");
    let out = work.path().join("out");
    let (arch_s, src_s, out_s) = (
        arch.to_str().unwrap(),
        src.path().to_str().unwrap(),
        out.to_str().unwrap(),
    );
    assert_eq!(run(&["a", arch_s, src_s]).status.code(), Some(0));
    let again = run(&["a", arch_s, src_s]);
    assert_eq!(again.status.code(), Some(2));
    assert!(
        text(&again.stderr).contains("t.lpk"),
        "{}",
        text(&again.stderr)
    );
    assert_eq!(run(&["x", arch_s, out_s]).status.code(), Some(0));
    let over = run(&["x", arch_s, out_s]);
    assert_eq!(over.status.code(), Some(2));
    assert!(
        text(&over.stderr).contains("error"),
        "{}",
        text(&over.stderr)
    );
    // The existing files are untouched.
    assert_eq!(tree_hash(&out), tree_hash(src.path()));
}

fn first_block_primitive(path: &Path) -> lpk_format::PrimitiveId {
    let mut a = Archive::open(fs::File::open(path).unwrap(), &Resources::default()).unwrap();
    lpk_core::register_full_reader(&mut a);
    let b = a.index().blocks[0];
    let at = lpk_format::FrameLocation {
        offset: b.frame_offset,
        len: b.frame_len,
        sequence: b.sequence,
    };
    let frame = a
        .read_frame_at(at, lpk_format::FrameKind::ChunkData)
        .unwrap();
    let (header, _) = lpk_format::BlockHeader::parse(&frame.payload, 0, 0).unwrap();
    header.graph.steps[0].primitive
}

#[test]
fn store_and_fast_graphs_differ_in_the_block_table() {
    let src = tempfile::tempdir().unwrap();
    fs::write(src.path().join("a.txt"), prose(1, 300_000)).unwrap();
    let work = tempfile::tempdir().unwrap();
    let (fast, store) = (work.path().join("f.lpk"), work.path().join("s.lpk"));
    let s = src.path().to_str().unwrap();
    assert_eq!(
        run(&["a", fast.to_str().unwrap(), s, "--fast"])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(&["a", store.to_str().unwrap(), s, "--store"])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(first_block_primitive(&fast), lpk_format::PrimitiveId::Zstd);
    assert_eq!(
        first_block_primitive(&store),
        lpk_format::PrimitiveId::Store
    );
    assert!(fs::metadata(&fast).unwrap().len() < fs::metadata(&store).unwrap().len());
}

#[test]
fn paths_starting_with_a_dash_work_for_x_and_t() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("-a.lpk");
    let a = run(&["a", arch.to_str().unwrap(), src.path().to_str().unwrap()]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    // Relative names starting with `-`, after `--`, in the archive's directory.
    let x = Command::new(env!("CARGO_BIN_EXE_lpk"))
        .current_dir(work.path())
        .args(["x", "--", "-a.lpk", "-out"])
        .output()
        .unwrap();
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&work.path().join("-out")), tree_hash(src.path()));
    let t = Command::new(env!("CARGO_BIN_EXE_lpk"))
        .current_dir(work.path())
        .args(["t", "--", "-a.lpk"])
        .output()
        .unwrap();
    assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
}

#[test]
fn version_matches_the_catalogue_pattern() {
    let v = run(&["--version"]);
    assert_eq!(v.status.code(), Some(0));
    let t = text(&v.stdout);
    let rest = t.trim().strip_prefix("lpk ").unwrap();
    let (ver, hash) = rest.split_once('+').unwrap();
    assert_eq!(ver, env!("CARGO_PKG_VERSION"));
    assert!(!hash.is_empty() && !hash.contains(' '), "{t}");
}

#[test]
fn usage_errors_exit_1_and_refusals_exit_2() {
    assert_eq!(run(&["frobnicate"]).status.code(), Some(1));
    assert_eq!(run(&["a", "only-one-arg"]).status.code(), Some(1));
    assert_eq!(
        run(&["a", "x", "y", "--fast", "--store"]).status.code(),
        Some(1)
    );
    let src = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("t.lpk");
    // An out-of-range option is a usage error and leaves no file.
    let bad = run(&[
        "a",
        arch.to_str().unwrap(),
        src.path().to_str().unwrap(),
        "--window-log",
        "40",
    ]);
    assert_eq!(bad.status.code(), Some(1), "{}", text(&bad.stderr));
    assert!(!arch.exists());
    // A missing input directory and a missing archive are refused operations.
    let gone = work.path().join("nope");
    let r = run(&["a", arch.to_str().unwrap(), gone.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(2));
    let r = run(&["t", gone.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(2));
    assert_eq!(run(&["--help"]).status.code(), Some(0));
}

#[test]
fn prior_files_are_layered_over_the_defaults() {
    let samples: Vec<Vec<u8>> = (1..400).map(|i| prose(i, 900)).collect();
    let dict = zstd::dict::from_samples(&samples, 8 * 1024).unwrap();
    let set = Arc::new(ProvidedDictionaries::new().with(DictionaryKind::Prose, dict.clone()));
    let src = tempfile::tempdir().unwrap();
    fs::write(src.path().join("a.txt"), prose(7, 5_000)).unwrap();
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("d.lpk");
    let options = FastOptions {
        dictionaries: DictionaryPolicy::Provided(set),
        ..FastOptions::default()
    };
    lpk_core::archive_fast_file(src.path(), &arch, options).unwrap();
    let dfile = work.path().join("prose.dict");
    fs::write(&dfile, &dict).unwrap();
    let (arch_s, out_s) = (arch.to_str().unwrap(), work.path().join("o"));
    let without = run(&["t", arch_s]);
    assert_eq!(without.status.code(), Some(2), "{}", text(&without.stdout));
    let with = run(&["t", arch_s, "--prior", dfile.to_str().unwrap()]);
    assert_eq!(with.status.code(), Some(0), "{}", text(&with.stderr));
    let x = run(&[
        "x",
        arch_s,
        out_s.to_str().unwrap(),
        "--prior",
        dfile.to_str().unwrap(),
    ]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out_s), tree_hash(src.path()));
}

#[test]
fn balanced_round_trips_bit_exact_and_uses_lzma() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    fs::write(src.path().join("big.txt"), prose(9, 400_000)).unwrap();
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("b.lpk");
    let out = work.path().join("out");
    let a = run(&[
        "a",
        arch.to_str().unwrap(),
        src.path().to_str().unwrap(),
        "--balanced",
        "--dict-size",
        "1048576",
        "-v",
    ]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    assert!(text(&a.stderr).contains("lzma"), "{}", text(&a.stderr));
    let x = run(&["x", arch.to_str().unwrap(), out.to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out), tree_hash(src.path()));
    let t = run(&["t", arch.to_str().unwrap()]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
    assert!(text(&t.stdout).starts_with("ok: "));
}

#[test]
fn balanced_flags_are_exclusive_and_checked() {
    let src = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("t.lpk");
    let (a, s) = (arch.to_str().unwrap(), src.path().to_str().unwrap());
    for extra in [
        &["--balanced", "--fast"][..],
        &["--balanced", "--store"][..],
        &["--dict-size", "1048576"][..],
        &["--balanced", "--dict-size", "10"][..],
        &["--balanced", "--window-log", "40"][..],
    ] {
        let mut args = vec!["a", a, s];
        args.extend_from_slice(extra);
        assert_eq!(run(&args).status.code(), Some(1), "{extra:?}");
        assert!(!arch.exists());
    }
    let h = run(&["a", "--help"]);
    assert!(text(&h.stdout).contains("--balanced"));
}

#[test]
fn ordering_values_are_checked_and_verbose_reports_the_pass() {
    let h = run(&["a", "--help"]);
    let help = text(&h.stdout);
    assert!(help.contains("--ordering"), "{help}");
    assert!(
        help.contains("extension") && !help.contains("similarity"),
        "{help}"
    );
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    for (i, order) in ["none", "extension"].iter().enumerate() {
        let arch = work.path().join(format!("{i}.lpk"));
        let a = run(&[
            "a",
            arch.to_str().unwrap(),
            src.path().to_str().unwrap(),
            "--ordering",
            order,
            "-v",
        ]);
        assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
        assert!(
            text(&a.stderr).contains(&format!("ordering: {order}\n")),
            "{}",
            text(&a.stderr)
        );
        let out = work.path().join(format!("out{i}"));
        let x = run(&["x", arch.to_str().unwrap(), out.to_str().unwrap()]);
        assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
        assert_eq!(tree_hash(&out), tree_hash(src.path()));
    }
    let arch = work.path().join("bad.lpk");
    let bad = run(&[
        "a",
        arch.to_str().unwrap(),
        src.path().to_str().unwrap(),
        "--ordering",
        "alphabet",
    ]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(!arch.exists());
    // similarity is not an order; an ignored mode is never printed
    let sim = run(&[
        "a",
        arch.to_str().unwrap(),
        src.path().to_str().unwrap(),
        "--ordering",
        "similarity",
    ]);
    assert_eq!(sim.status.code(), Some(1));
    for extra in ["--no-dedup", "--store"] {
        let arch = work.path().join("ignored.lpk");
        let a = run(&[
            "a",
            arch.to_str().unwrap(),
            src.path().to_str().unwrap(),
            "--ordering",
            "extension",
            extra,
            "-v",
        ]);
        assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
        assert!(!text(&a.stderr).contains("ordering:"), "{extra}");
        fs::remove_file(&arch).unwrap();
    }
}

#[test]
fn dedup_is_on_by_default_and_no_dedup_turns_it_off() {
    let src = tempfile::tempdir().unwrap();
    let big = random(300_000);
    fs::write(src.path().join("one.bin"), &big).unwrap();
    fs::write(src.path().join("two.bin"), &big).unwrap();
    fs::write(src.path().join("note.txt"), prose(4, 20_000)).unwrap();
    let work = tempfile::tempdir().unwrap();
    let (on, off) = (work.path().join("on.lpk"), work.path().join("off.lpk"));
    let s = src.path().to_str().unwrap();
    let a = run(&["a", on.to_str().unwrap(), s, "-v"]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let msg = text(&a.stderr);
    assert!(msg.contains("dedup: "), "{msg}");
    assert!(!msg.contains("dedup: 0 chunks"), "{msg}");
    let b = run(&["a", off.to_str().unwrap(), s, "--no-dedup", "-v"]);
    assert_eq!(b.status.code(), Some(0), "{}", text(&b.stderr));
    assert!(text(&b.stderr).contains("dedup: 0 chunks and 0 bytes"));
    let (n_on, n_off) = (
        fs::metadata(&on).unwrap().len(),
        fs::metadata(&off).unwrap().len(),
    );
    assert!(n_on + 250_000 < n_off, "{n_on} {n_off}");
    for (arch, name) in [(&on, "out-on"), (&off, "out-off")] {
        let out = work.path().join(name);
        let x = run(&["x", arch.to_str().unwrap(), out.to_str().unwrap()]);
        assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
        assert_eq!(tree_hash(&out), tree_hash(src.path()));
        let t = run(&["t", arch.to_str().unwrap()]);
        assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
    }
}
