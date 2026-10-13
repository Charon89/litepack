//! The `lpk` binary end to end on temporary trees.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::{Seek, SeekFrom};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use lpk_core::{DictionaryKind, DictionaryPolicy, FastOptions, ProvidedDictionaries};
use lpk_format::{Archive, Frame, ReadFrame, ReadLimits, RecoveryFrame, Resources};

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

// ---- E2-12: encryption through the tool ----

/// `a` with a password file (CRLF-terminated: the newline is not part of it) and a keyfile,
/// listable; `x` and `t` with the literal password and the keyfile; `t` without the key checks
/// the frame hashes only; the archive records the suite and the listable flag.
#[test]
fn encrypted_round_trip_with_password_file_keyfile_and_listable() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("e.lpk");
    let out = work.path().join("out");
    let pw_file = work.path().join("pw.txt");
    fs::write(&pw_file, b"open sesame\r\n").unwrap();
    let keyfile = work.path().join("key.bin");
    fs::write(&keyfile, random(1000)).unwrap();
    let a = run(&[
        "a",
        arch.to_str().unwrap(),
        src.path().to_str().unwrap(),
        "--password-file",
        pw_file.to_str().unwrap(),
        "--keyfile",
        keyfile.to_str().unwrap(),
        "--listable",
        "--suite",
        "xchacha20-poly1305",
        "-v",
    ]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let msg = text(&a.stderr);
    assert!(
        msg.contains("encrypted: XChaCha20-Poly1305, listable"),
        "{msg}"
    );
    assert!(!msg.contains("sesame"), "{msg}");
    let archive = Archive::open(fs::File::open(&arch).unwrap(), &Resources::default()).unwrap();
    assert!(archive.is_encrypted() && archive.is_listable() && archive.is_keyless());
    assert_eq!(archive.suite(), Some(lpk_format::Suite::XChaCha));
    assert!(archive.key_slot().unwrap().keyfile_required);
    drop(archive);
    // Without the key: the entry table lists, the hashes verify, nothing extracts.
    let t = run(&["t", arch.to_str().unwrap()]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
    assert!(
        text(&t.stdout).starts_with("ok (frame hashes only"),
        "{}",
        text(&t.stdout)
    );
    let x = run(&["x", arch.to_str().unwrap(), out.to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(2));
    assert!(!out.exists());
    // With the key, in the other password form.
    let x = run(&[
        "x",
        arch.to_str().unwrap(),
        out.to_str().unwrap(),
        "--password",
        "open sesame",
        "--keyfile",
        keyfile.to_str().unwrap(),
    ]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out), tree_hash(src.path()));
    let t = run(&[
        "t",
        arch.to_str().unwrap(),
        "--password-file",
        pw_file.to_str().unwrap(),
        "--keyfile",
        keyfile.to_str().unwrap(),
    ]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
    assert!(text(&t.stdout).starts_with("ok: "), "{}", text(&t.stdout));
}

#[test]
fn a_wrong_or_missing_password_leaves_no_output_and_is_never_echoed() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("e.lpk");
    let a = run(&[
        "a",
        arch.to_str().unwrap(),
        src.path().to_str().unwrap(),
        "--password",
        "hunter2",
        "-v",
    ]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let msg = text(&a.stderr);
    assert!(msg.contains("encrypted: AES-256-GCM\n"), "{msg}");
    assert!(!msg.contains("hunter"), "{msg}");
    let archive = Archive::open(fs::File::open(&arch).unwrap(), &Resources::default()).unwrap();
    assert!(archive.is_encrypted() && !archive.is_listable());
    assert_eq!(archive.suite(), Some(lpk_format::Suite::AesGcm));
    drop(archive);
    let out = work.path().join("nested/out");
    for extra in [vec!["--password", "hunter3"], vec![]] {
        let mut args = vec!["x", arch.to_str().unwrap(), out.to_str().unwrap()];
        args.extend(extra);
        let x = run(&args);
        assert_eq!(x.status.code(), Some(2), "{}", text(&x.stderr));
        assert!(!out.exists() && !out.parent().unwrap().exists());
        let err = text(&x.stderr);
        assert!(err.starts_with("error: "), "{err}");
        assert!(!err.contains("hunter"), "{err}");
    }
    // `t` with the wrong password is refused at the key slot too.
    let t = run(&["t", arch.to_str().unwrap(), "--password", "hunter3"]);
    assert_eq!(t.status.code(), Some(2));
    assert!(!text(&t.stderr).contains("hunter"));
    // A missing password file is a failed operation, not a usage error.
    let gone = work.path().join("no-such-file");
    let x = run(&[
        "x",
        arch.to_str().unwrap(),
        out.to_str().unwrap(),
        "--password-file",
        gone.to_str().unwrap(),
    ]);
    assert_eq!(x.status.code(), Some(2), "{}", text(&x.stderr));
    assert!(!out.exists());
    let x = run(&[
        "x",
        arch.to_str().unwrap(),
        out.to_str().unwrap(),
        "--password",
        "hunter2",
    ]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out), tree_hash(src.path()));
}

#[test]
fn encryption_flags_are_checked() {
    let src = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("e.lpk");
    let s = src.path().to_str().unwrap();
    for extra in [
        vec!["--listable"],
        vec!["--suite", "aes-256-gcm"],
        vec!["--password", "a", "--password-file", "b"],
        vec!["--password", "a", "--suite", "rot13"],
    ] {
        let mut args = vec!["a", arch.to_str().unwrap(), s];
        args.extend(extra.iter());
        let r = run(&args);
        assert_eq!(r.status.code(), Some(1), "{extra:?}: {}", text(&r.stderr));
        assert!(!arch.exists(), "{extra:?}");
    }
    // A keyfile alone is a credential (an empty password).
    let keyfile = work.path().join("key.bin");
    fs::write(&keyfile, random(64)).unwrap();
    make_tree(src.path());
    let a = run(&[
        "a",
        arch.to_str().unwrap(),
        s,
        "--keyfile",
        keyfile.to_str().unwrap(),
    ]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let out = work.path().join("out");
    let x = run(&[
        "x",
        arch.to_str().unwrap(),
        out.to_str().unwrap(),
        "--keyfile",
        keyfile.to_str().unwrap(),
    ]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out), tree_hash(src.path()));
}

// ---- E2-13: recovery records through the tool ----

/// `a --recovery 10`, a few bytes overwritten inside the covered data: `t` fails, `check`
/// reports the damaged shards (exit 2), `repair` writes a copy that tests and extracts.
#[test]
fn recovery_records_repair_a_damaged_archive() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("r.lpk");
    let s = src.path().to_str().unwrap();
    let bad = run(&["a", arch.to_str().unwrap(), s, "--recovery", "21"]);
    assert_eq!(bad.status.code(), Some(1), "{}", text(&bad.stderr));
    assert!(!arch.exists());
    let a = run(&["a", arch.to_str().unwrap(), s, "--recovery", "10", "-v"]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    assert!(text(&a.stderr).contains("recovery: 10 percent"));
    let good = fs::read(&arch).unwrap();
    let (cover_offset, cover_len) = {
        let archive = Archive::open(fs::File::open(&arch).unwrap(), &Resources::default()).unwrap();
        let frames = archive.recovery_frames();
        assert_eq!(frames.len(), 1, "{frames:?}");
        let mut r = std::io::Cursor::new(&good);
        r.seek(SeekFrom::Start(frames[0].offset)).unwrap();
        let limits = ReadLimits {
            max_payload: Resources::default().max_frame_payload,
        };
        let frame = match Frame::read(&mut r, &limits).unwrap().unwrap() {
            ReadFrame::Known(f) => f,
            ReadFrame::Unknown { kind, .. } => panic!("unknown frame kind {kind}"),
        };
        let f = RecoveryFrame::parse(&frame.payload, archive.trailer().index_offset).unwrap();
        (f.cover_offset, f.cover_len)
    };
    assert!(cover_len > 150_000, "{cover_len}");
    // Overwrite a run of bytes in the middle of the covered data (one shard of 64 KiB).
    let mut bytes = good.clone();
    let at = (cover_offset + cover_len / 2) as usize;
    for (i, b) in bytes[at..at + 40].iter_mut().enumerate() {
        *b ^= 0x80 | (i as u8);
    }
    fs::write(&arch, &bytes).unwrap();
    let t = run(&["t", arch.to_str().unwrap()]);
    assert_eq!(t.status.code(), Some(2), "{}", text(&t.stdout));
    let c = run(&["check", arch.to_str().unwrap()]);
    assert_eq!(c.status.code(), Some(2), "{}", text(&c.stderr));
    assert!(
        text(&c.stdout).contains("damaged shards: 1, repaired shards: 0"),
        "{}",
        text(&c.stdout)
    );
    let fixed = work.path().join("fixed.lpk");
    let r = run(&["repair", arch.to_str().unwrap(), fixed.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert!(
        text(&r.stdout).contains("damaged shards: 1, repaired shards: 1"),
        "{}",
        text(&r.stdout)
    );
    assert!(fs::read(&fixed).unwrap() == good);
    let t = run(&["t", fixed.to_str().unwrap()]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
    let out = work.path().join("out");
    let x = run(&["x", fixed.to_str().unwrap(), out.to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out), tree_hash(src.path()));
    // The copy must not exist already.
    let r = run(&["repair", arch.to_str().unwrap(), fixed.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(2));
    // The damaged archive is still refused by `x`: no silent repair on extraction.
    let out2 = work.path().join("out2");
    let x = run(&["x", arch.to_str().unwrap(), out2.to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(2));
}

#[test]
fn an_archive_without_recovery_checks_clean_and_an_encrypted_one_checks_keyless() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let s = src.path().to_str().unwrap();
    let plain = work.path().join("p.lpk");
    let a = run(&["a", plain.to_str().unwrap(), s]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let c = run(&["check", plain.to_str().unwrap()]);
    assert_eq!(c.status.code(), Some(0), "{}", text(&c.stderr));
    assert!(
        text(&c.stdout).starts_with("recovery frames: 0,"),
        "{}",
        text(&c.stdout)
    );
    // Encrypted with recovery: the shards are checked and repaired without the key.
    let sealed = work.path().join("s.lpk");
    let a = run(&[
        "a",
        sealed.to_str().unwrap(),
        s,
        "--password",
        "pw",
        "--recovery",
        "3",
    ]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let c = run(&["check", sealed.to_str().unwrap()]);
    assert_eq!(c.status.code(), Some(0), "{}", text(&c.stderr));
    assert!(
        text(&c.stdout).starts_with("recovery frames: 1,"),
        "{}",
        text(&c.stdout)
    );
    let copy = work.path().join("copy.lpk");
    let r = run(&["repair", sealed.to_str().unwrap(), copy.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert!(fs::read(&copy).unwrap() == fs::read(&sealed).unwrap());
    let out = work.path().join("out");
    let x = run(&[
        "x",
        copy.to_str().unwrap(),
        out.to_str().unwrap(),
        "--password",
        "pw",
    ]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out), tree_hash(src.path()));
}

/// Three versions of a tree as sibling directories `v1`, `v2`, `v3`: a file deleted, a file
/// changed in its tail, a file added, then another deleted.
fn make_versions(root: &Path) {
    let v1 = root.join("v1");
    make_tree(&v1);
    fs::write(v1.join("gone.bin"), random(30_000)).unwrap();
    let v2 = root.join("v2");
    make_tree(&v2);
    let mut r = random(150_000);
    r.truncate(120_000);
    r.extend_from_slice(&prose(5, 30_000));
    fs::write(v2.join("docs/deep/r.bin"), r).unwrap();
    fs::write(v2.join("added.txt"), prose(6, 40_000)).unwrap();
    let v3 = root.join("v3");
    make_tree(&v3);
    fs::write(
        v3.join("docs/deep/r.bin"),
        fs::read(v2.join("docs/deep/r.bin")).unwrap(),
    )
    .unwrap();
    fs::remove_file(v3.join("docs/b.md")).unwrap();
}

#[test]
fn append_rollback_and_info_through_the_binary() {
    let src = tempfile::tempdir().unwrap();
    make_versions(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("j.lpk");
    let archive = arch.to_str().unwrap();
    let v = |n: &str| src.path().join(n);
    let a = run(&["a", archive, v("v1").to_str().unwrap()]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let len0 = fs::metadata(&arch).unwrap().len();
    // Append v2 (-v reports the generation and the cross-generation reuse).
    let p = run(&["append", archive, v("v2").to_str().unwrap(), "-v"]);
    assert_eq!(p.status.code(), Some(0), "{}", text(&p.stderr));
    let e = text(&p.stderr);
    assert!(e.contains("generation 1,"), "{e}");
    let reused = e
        .lines()
        .find(|l| l.starts_with("dedup: "))
        .and_then(|l| {
            l.split(", ")
                .find_map(|s| s.strip_suffix(" reused from an earlier generation)"))
        })
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("no reuse count in {e}"));
    assert!(reused > 0, "{e}");
    let len1 = fs::metadata(&arch).unwrap().len();
    assert!(len1 > len0);
    // Append v3; the tiers' flags are accepted; an impossible combination is a usage error.
    let bad = run(&[
        "append",
        archive,
        v("v3").to_str().unwrap(),
        "--fast",
        "--store",
    ]);
    assert_eq!(bad.status.code(), Some(1));
    assert_eq!(fs::metadata(&arch).unwrap().len(), len1);
    let p = run(&["append", archive, v("v3").to_str().unwrap(), "--balanced"]);
    assert_eq!(p.status.code(), Some(0), "{}", text(&p.stderr));
    let len2 = fs::metadata(&arch).unwrap().len();
    // info lists the generations.
    let i = run(&["info", archive]);
    assert_eq!(i.status.code(), Some(0), "{}", text(&i.stderr));
    let o = text(&i.stdout);
    assert!(o.contains("generation: 2\n"), "{o}");
    assert!(o.contains("chain length: 3\n"), "{o}");
    // The latest generation is v3's snapshot: nothing of v1 or v2 that v3 lacks.
    let out = work.path().join("x3");
    let x = run(&["x", archive, out.to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out), tree_hash(&v("v3")));
    assert!(!out.join("gone.bin").exists() && !out.join("added.txt").exists());
    // t verifies the whole archive.
    let t = run(&["t", archive]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
    // Rollback above the latest is refused; to 1 reproduces v2; to 0 reproduces v1.
    let r = run(&["rollback", archive, "3"]);
    assert_eq!(r.status.code(), Some(2));
    assert_eq!(fs::metadata(&arch).unwrap().len(), len2);
    let r = run(&["rollback", archive, "1"]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert!(text(&r.stdout).starts_with("rolled back to generation 1:"));
    assert_eq!(fs::metadata(&arch).unwrap().len(), len1);
    let out = work.path().join("x2");
    let x = run(&["x", archive, out.to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out), tree_hash(&v("v2")));
    let r = run(&["rollback", archive, "0"]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(fs::metadata(&arch).unwrap().len(), len0);
    let out = work.path().join("x1");
    let x = run(&["x", archive, out.to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out), tree_hash(&v("v1")));
    let i = run(&["info", archive]);
    assert!(
        text(&i.stdout).contains("generation: 0\n"),
        "{}",
        text(&i.stdout)
    );
    // A missing archive or directory is a refused operation; the archive is untouched.
    let gone = work.path().join("nope");
    let r = run(&["append", gone.to_str().unwrap(), v("v1").to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(2));
    let r = run(&["append", archive, gone.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(2));
    assert_eq!(fs::metadata(&arch).unwrap().len(), len0);
}

#[test]
fn a_half_written_append_is_recovered_by_rollback_through_the_binary() {
    let src = tempfile::tempdir().unwrap();
    make_versions(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("j.lpk");
    let archive = arch.to_str().unwrap();
    let a = run(&["a", archive, src.path().join("v1").to_str().unwrap()]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let len0 = fs::metadata(&arch).unwrap().len();
    let p = run(&["append", archive, src.path().join("v2").to_str().unwrap()]);
    assert_eq!(p.status.code(), Some(0), "{}", text(&p.stderr));
    let len1 = fs::metadata(&arch).unwrap().len();
    // The second generation cut short, between the old trailer and the new one.
    let f = fs::OpenOptions::new().write(true).open(&arch).unwrap();
    f.set_len(len0 + (len1 - len0) / 2).unwrap();
    drop(f);
    let t = run(&["t", archive]);
    assert_eq!(t.status.code(), Some(2));
    let x = run(&["x", archive, work.path().join("none").to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(2));
    // Rollback to the generation the file still holds whole.
    let r = run(&["rollback", archive, "0"]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(fs::metadata(&arch).unwrap().len(), len0);
    let t = run(&["t", archive]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
    let out = work.path().join("x1");
    let x = run(&["x", archive, out.to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out), tree_hash(&src.path().join("v1")));
    // And it appends again.
    let p = run(&["append", archive, src.path().join("v2").to_str().unwrap()]);
    assert_eq!(p.status.code(), Some(0), "{}", text(&p.stderr));
    let i = run(&["info", archive]);
    assert!(
        text(&i.stdout).contains("generation: 1\n"),
        "{}",
        text(&i.stdout)
    );
}

#[test]
fn an_appended_generation_gets_its_own_recovery_frames() {
    let src = tempfile::tempdir().unwrap();
    make_versions(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("j.lpk");
    let archive = arch.to_str().unwrap();
    let a = run(&[
        "a",
        archive,
        src.path().join("v1").to_str().unwrap(),
        "--recovery",
        "5",
    ]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let c = run(&["check", archive]);
    assert_eq!(c.status.code(), Some(0), "{}", text(&c.stderr));
    assert!(
        text(&c.stdout).starts_with("recovery frames: 1,"),
        "{}",
        text(&c.stdout)
    );
    // The same bounds as `a`.
    let bad = run(&[
        "append",
        archive,
        src.path().join("v2").to_str().unwrap(),
        "--recovery",
        "21",
    ]);
    assert_eq!(bad.status.code(), Some(1));
    let p = run(&[
        "append",
        archive,
        src.path().join("v2").to_str().unwrap(),
        "--recovery",
        "5",
        "-v",
    ]);
    assert_eq!(p.status.code(), Some(0), "{}", text(&p.stderr));
    assert!(
        text(&p.stderr).contains("recovery: 5 percent"),
        "{}",
        text(&p.stderr)
    );
    // Both generations are covered: one frame each.
    let c = run(&["check", archive]);
    assert_eq!(c.status.code(), Some(0), "{}", text(&c.stderr));
    assert!(
        text(&c.stdout).starts_with("recovery frames: 2,"),
        "{}",
        text(&c.stdout)
    );
    let t = run(&["t", archive]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
    let i = run(&["info", archive]);
    assert!(
        text(&i.stdout).contains("recovery frames: 2\n"),
        "{}",
        text(&i.stdout)
    );
}

#[test]
fn an_encrypted_archive_appends_with_its_password_only() {
    let src = tempfile::tempdir().unwrap();
    make_versions(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("j.lpk");
    let archive = arch.to_str().unwrap();
    let v2 = src.path().join("v2");
    let a = run(&[
        "a",
        archive,
        src.path().join("v1").to_str().unwrap(),
        "--password",
        "pw",
    ]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let before = fs::read(&arch).unwrap();
    // Keyless and wrong password: refused, the archive untouched, the password never echoed.
    let p = run(&["append", archive, v2.to_str().unwrap()]);
    assert_eq!(p.status.code(), Some(2), "{}", text(&p.stderr));
    assert_eq!(fs::read(&arch).unwrap(), before);
    let p = run(&[
        "append",
        archive,
        v2.to_str().unwrap(),
        "--password",
        "not-the-secret",
    ]);
    assert_eq!(p.status.code(), Some(2), "{}", text(&p.stderr));
    assert!(
        !text(&p.stderr).contains("not-the-secret"),
        "{}",
        text(&p.stderr)
    );
    assert_eq!(fs::read(&arch).unwrap(), before);
    // The right password: the new generation is sealed; info and rollback need no key.
    let p = run(&["append", archive, v2.to_str().unwrap(), "--password", "pw"]);
    assert_eq!(p.status.code(), Some(0), "{}", text(&p.stderr));
    let i = run(&["info", archive]);
    assert_eq!(i.status.code(), Some(0), "{}", text(&i.stderr));
    let o = text(&i.stdout);
    assert!(
        o.contains("encrypted: yes\n") && o.contains("generation: 1\n"),
        "{o}"
    );
    assert!(o.contains("index: sealed"), "{o}");
    let i = run(&["info", archive, "--password", "pw"]);
    assert!(text(&i.stdout).contains("chunks: "), "{}", text(&i.stdout));
    let out = work.path().join("x2");
    let x = run(&["x", archive, out.to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(2));
    assert!(!out.exists());
    let x = run(&["x", archive, out.to_str().unwrap(), "--password", "pw"]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree_hash(&out), tree_hash(&v2));
    let r = run(&["rollback", archive, "0", "--password", "pw"]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(fs::read(&arch).unwrap(), before);
}
