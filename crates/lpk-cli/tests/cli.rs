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

/// E3-5: no output of a credential test carries the password's bytes (stdout and stderr,
/// `-v` lines included).
fn assert_no_secret(r: &Output, secret: &[u8]) {
    for (name, bytes) in [("stdout", &r.stdout), ("stderr", &r.stderr)] {
        assert!(
            !bytes.windows(secret.len()).any(|w| w == secret),
            "the password appears on {name}: {}",
            text(bytes)
        );
    }
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

/// `--deflate-peel on|off`: on by default with --balanced (header minor 2, `-v` reports the
/// stage), off by default with --fast (minor 1), and either way on request.
#[test]
fn the_deflate_peel_flag_defaults_per_tier() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let zip = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../lpk-core/tests/fixtures/deflate/doc.zip");
    fs::write(src.path().join("doc.zip"), fs::read(zip).unwrap()).unwrap();
    let work = tempfile::tempdir().unwrap();
    // Each row: the flags, the header's minor (4 whenever the Balanced tier's PNG row-filter
    // re-encoding is on, revision 1.4; else 3 whenever its BWT stage is, revision 1.3; else 2
    // with the Deflate peel, else 1) and whether the peel walked.
    let cases: [(&str, &[&str], u16, bool); 8] = [
        ("b", &["--balanced", "--dict-size", "1048576"], 4, true),
        (
            "b-no-png",
            &[
                "--balanced",
                "--dict-size",
                "1048576",
                "--png-filter",
                "off",
            ],
            3,
            true,
        ),
        (
            "b-png-on",
            &["--balanced", "--dict-size", "1048576", "--png-filter", "on"],
            4,
            true,
        ),
        (
            "b-off",
            &[
                "--balanced",
                "--dict-size",
                "1048576",
                "--deflate-peel",
                "off",
            ],
            3,
            false,
        ),
        (
            "b-no-bwt",
            &["--balanced", "--dict-size", "1048576", "--bwt", "off"],
            4,
            true,
        ),
        (
            "b-neither",
            &[
                "--balanced",
                "--dict-size",
                "1048576",
                "--bwt",
                "off",
                "--deflate-peel",
                "off",
            ],
            1,
            false,
        ),
        ("f", &["--fast"], 1, false),
        ("f-on", &["--fast", "--deflate-peel", "on"], 2, true),
    ];
    for (name, flags, minor, walks) in cases {
        let arch = work.path().join(format!("{name}.lpk"));
        let mut args = vec![
            "a",
            arch.to_str().unwrap(),
            src.path().to_str().unwrap(),
            "-v",
        ];
        args.extend_from_slice(flags);
        let a = run(&args);
        assert_eq!(a.status.code(), Some(0), "{name}: {}", text(&a.stderr));
        let walked = text(&a.stderr).contains("deflate peel: ");
        assert_eq!(walked, walks, "{name}: {}", text(&a.stderr));
        let archive = Archive::open(fs::File::open(&arch).unwrap(), &Resources::default()).unwrap();
        assert_eq!(archive.header().version.minor, minor, "{name}");
        let out = work.path().join(format!("{name}-out"));
        let x = run(&["x", arch.to_str().unwrap(), out.to_str().unwrap()]);
        assert_eq!(x.status.code(), Some(0), "{name}: {}", text(&x.stderr));
        assert_eq!(tree_hash(&out), tree_hash(src.path()), "{name}");
    }
    let bad = run(&["a", "x.lpk", ".", "--store", "--deflate-peel", "on"]);
    assert_eq!(bad.status.code(), Some(1));
    let bad = run(&["a", "x.lpk", ".", "--deflate-peel", "maybe"]);
    assert_eq!(bad.status.code(), Some(1));
    // `--png-filter` needs the Balanced tier, and `on` needs the Deflate peel.
    for flags in [
        &["--fast", "--png-filter", "on"][..],
        &["--tier", "fast", "--png-filter", "off"],
        &["--balanced", "--deflate-peel", "off", "--png-filter", "on"],
    ] {
        let mut args = vec!["a", "x.lpk", "."];
        args.extend_from_slice(flags);
        let bad = run(&args);
        assert_eq!(bad.status.code(), Some(1), "{flags:?}");
        assert!(text(&bad.stderr).contains("--png-filter"), "{flags:?}");
    }
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

/// E3-8: `--strong` and `--tier strong` are reserved for Phase 2 and refused as a usage error
/// (exit 1, one line) before anything exists, on `a` and on `append`.
#[test]
fn strong_is_reserved_and_refused_before_any_output() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("t.lpk");
    let (a, s) = (arch.to_str().unwrap(), src.path().to_str().unwrap());
    for args in [
        vec!["a", a, s, "--strong"],
        vec!["a", a, s, "--tier", "strong"],
        vec!["a", a, s, "--tier=strong", "-v"],
        // The refusal comes before the archive is looked at: the archive does not exist.
        vec!["append", a, s, "--strong"],
        vec!["append", a, s, "--tier", "strong"],
    ] {
        let r = run(&args);
        let e = text(&r.stderr);
        assert_eq!(r.status.code(), Some(1), "{args:?}: {e}");
        assert!(
            e.contains("Strong tier arrives in Phase 2"),
            "{args:?}: {e}"
        );
        assert_eq!(e.trim_end().lines().count(), 1, "{args:?}: {e}");
        assert!(r.stdout.is_empty(), "{args:?}: {}", text(&r.stdout));
        assert!(!arch.exists(), "{args:?}");
    }
}

/// E3-8: `--tier` is exclusive with each tier flag and with `--strong`, takes only the four
/// names, and refuses a setting of another tier as the flags do.
#[test]
fn tier_conflicts_with_the_tier_flags_and_checks_the_settings() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("t.lpk");
    let (a, s) = (arch.to_str().unwrap(), src.path().to_str().unwrap());
    // Each row: the extra arguments and the text the refusal must name (clap's wording for
    // the conflicts, `lpk`'s own for a setting of another tier under `--tier`).
    const STORE: &str = "cannot be used with '--tier store'";
    const DICT: &str = "'--dict-size' requires the Balanced tier";
    for (extra, reason) in [
        (&["--tier", "fast", "--fast"][..], "cannot be used with"),
        (&["--tier", "fast", "--balanced"][..], "cannot be used with"),
        (
            &["--tier", "balanced", "--store"][..],
            "cannot be used with",
        ),
        (&["--tier", "store", "--strong"][..], "cannot be used with"),
        (&["--tier", "strong", "--fast"][..], "cannot be used with"),
        (&["--fast", "--strong"][..], "cannot be used with"),
        (&["--balanced", "--strong"][..], "cannot be used with"),
        (&["--store", "--strong"][..], "cannot be used with"),
        (&["--tier", "bogus"][..], "invalid value 'bogus'"),
        (&["--tier"][..], "--tier <TIER>"),
        (&["--tier", "store", "--level", "3"][..], STORE),
        (&["--tier", "store", "--window-log", "20"][..], STORE),
        (&["--tier", "store", "--deflate-peel", "on"][..], STORE),
        (&["--tier", "store", "--dict-size", "1048576"][..], DICT),
        (&["--tier", "fast", "--dict-size", "1048576"][..], DICT),
        (&["--fast", "--dict-size", "1048576"][..], DICT),
        (&["--store", "--dict-size", "1048576"][..], DICT),
        (&["--dict-size", "1048576"][..], DICT),
        (
            &["--fast", "--bwt", "on"][..],
            "'--bwt' requires the Balanced tier",
        ),
        (
            &["--tier", "fast", "--bwt", "off"][..],
            "'--bwt' requires the Balanced tier",
        ),
        (&["--store", "--bwt", "on"][..], "cannot be used with"),
        (
            &["--tier", "balanced", "--no-dictionaries"][..],
            "'--no-dictionaries' cannot be used with '--tier balanced'",
        ),
    ] {
        let mut args = vec!["a", a, s];
        args.extend_from_slice(extra);
        let r = run(&args);
        let e = text(&r.stderr);
        assert_eq!(r.status.code(), Some(1), "{extra:?}: {e}");
        assert!(e.contains(reason), "{extra:?}: {e}");
        assert!(!arch.exists(), "{extra:?}");
    }
}

/// E3-8: the help of every command that writes an archive names the reservation.
#[test]
fn help_says_strong_is_reserved_for_phase_2() {
    for cmd in ["a", "append"] {
        let h = run(&[cmd, "--help"]);
        assert_eq!(h.status.code(), Some(0));
        let t = text(&h.stdout);
        assert!(t.contains("--strong"), "{cmd}: {t}");
        assert!(t.contains("Strong tier (Phase 2)"), "{cmd}: {t}");
        assert!(t.contains("--tier <TIER>"), "{cmd}: {t}");
        assert!(t.contains("long form of --fast"), "{cmd}: {t}");
    }
}

/// E3-8: `--tier X` writes the archive `--X` writes, byte for byte, for each of the three.
#[test]
fn tier_long_form_writes_the_same_archive_as_the_flag() {
    // Flat: a directory entry carries a modification time the file system moves between walks.
    let src = tempfile::tempdir().unwrap();
    for i in 0..3u64 {
        fs::write(src.path().join(format!("t{i}.txt")), prose(i + 1, 200_000)).unwrap();
    }
    fs::write(src.path().join("r.bin"), random(100_000)).unwrap();
    let work = tempfile::tempdir().unwrap();
    let mut archives = Vec::new();
    for tier in ["fast", "balanced", "store"] {
        let flag = format!("--{tier}");
        let flag_form = [flag.as_str()];
        let long_form = ["--tier", tier];
        let mut pair = Vec::new();
        for (form, spelling) in [(&flag_form[..], "flag"), (&long_form[..], "long")] {
            let arch = work.path().join(format!("{tier}-{spelling}.lpk"));
            let mut args = vec!["a", arch.to_str().unwrap(), src.path().to_str().unwrap()];
            args.extend_from_slice(form);
            if tier == "balanced" {
                // A setting of the tier, accepted with its long form too.
                args.extend(["--dict-size", "1048576"]);
            }
            let r = run(&args);
            assert_eq!(r.status.code(), Some(0), "{args:?}: {}", text(&r.stderr));
            pair.push(fs::read(&arch).unwrap());
        }
        assert_eq!(pair[0], pair[1], "{tier}");
        archives.push(pair.remove(0));
    }
    assert_ne!(archives[0], archives[1], "fast and balanced differ");
    assert_ne!(archives[0], archives[2], "fast and store differ");
    // `-v` names the tier's ordering line for both forms alike (store has none).
    let arch = work.path().join("v.lpk");
    let r = run(&[
        "a",
        arch.to_str().unwrap(),
        src.path().to_str().unwrap(),
        "--tier",
        "store",
        "-v",
    ]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert!(
        !text(&r.stderr).contains("ordering:"),
        "{}",
        text(&r.stderr)
    );
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

/// The first chunk index of each file in the archive at `path` (the order the fold wrote the
/// files in), each file checked against `src`.
fn first_chunks(path: &Path, src: &Path) -> std::collections::BTreeMap<String, u64> {
    let mut a = Archive::open(fs::File::open(path).unwrap(), &Resources::default()).unwrap();
    lpk_core::register_full_reader(&mut a);
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    let mut out = std::collections::BTreeMap::new();
    for e in entries
        .iter()
        .filter(|e| e.kind == lpk_format::EntryKind::File)
    {
        let mut got = Vec::new();
        a.extract(e, &mut got).unwrap();
        assert!(got == fs::read(src.join(&e.path)).unwrap(), "{}", e.path);
        out.insert(e.path.clone(), e.chunks[0]);
    }
    out
}

/// Two versions of a two-file tree as sibling folders `r1`, `r2`, beside an unrelated folder
/// whose path sorts between the versions' files (`m/notes.txt`).
fn make_sibling_roots(root: &Path) {
    for (v, seed) in [("r1", 10), ("r2", 20)] {
        fs::create_dir_all(root.join(v).join("sub")).unwrap();
        fs::write(root.join(v).join("a.txt"), prose(seed, 80_000)).unwrap();
        fs::write(root.join(v).join("sub/z.txt"), prose(seed + 1, 80_000)).unwrap();
    }
    fs::create_dir_all(root.join("m")).unwrap();
    fs::write(root.join("m/notes.txt"), prose(99, 80_000)).unwrap();
}

#[test]
fn the_balanced_tier_orders_versions_by_default_and_ordering_none_restores_path_order() {
    let src = tempfile::tempdir().unwrap();
    make_sibling_roots(src.path());
    let work = tempfile::tempdir().unwrap();
    let s = src.path().to_str().unwrap();
    let make = |name: &str, extra: &[&str]| {
        let arch = work.path().join(name);
        let mut args = vec!["a", arch.to_str().unwrap(), s, "--balanced", "-v"];
        args.extend_from_slice(extra);
        let r = run(&args);
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        (arch, text(&r.stderr))
    };
    // A first walk can see other directory times than the next: settle before comparing.
    let _settle = make("settle.lpk", &[]);
    let (default, e) = make("default.lpk", &[]);
    assert!(e.contains("ordering: versions\n"), "{e}");
    let (versions, _) = make("versions.lpk", &["--ordering", "versions"]);
    let (none, e) = make("none.lpk", &["--ordering", "none"]);
    assert!(e.contains("ordering: none\n"), "{e}");
    // The default is the versions order, byte for byte.
    assert!(fs::read(&default).unwrap() == fs::read(&versions).unwrap());
    // Default: a file's versions next to each other.
    let d = first_chunks(&default, src.path());
    let order = [
        "r1/a.txt",
        "r2/a.txt",
        "m/notes.txt",
        "r1/sub/z.txt",
        "r2/sub/z.txt",
    ];
    for w in order.windows(2) {
        assert!(
            d[w[0]] < d[w[1]],
            "default: {} before {}: {d:?}",
            w[0],
            w[1]
        );
    }
    // --ordering none: path order, as before E2-26.
    let n = first_chunks(&none, src.path());
    let order = [
        "m/notes.txt",
        "r1/a.txt",
        "r1/sub/z.txt",
        "r2/a.txt",
        "r2/sub/z.txt",
    ];
    for w in order.windows(2) {
        assert!(n[w[0]] < n[w[1]], "none: {} before {}: {n:?}", w[0], w[1]);
    }
    // The Fast tier keeps path order by default.
    let arch = work.path().join("fast.lpk");
    let r = run(&["a", arch.to_str().unwrap(), s, "--fast", "-v"]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert!(
        text(&r.stderr).contains("ordering: none\n"),
        "{}",
        text(&r.stderr)
    );
    let f = first_chunks(&arch, src.path());
    assert!(f["m/notes.txt"] < f["r1/a.txt"] && f["r1/sub/z.txt"] < f["r2/a.txt"]);
}

#[test]
fn the_balanced_default_without_sibling_roots_is_byte_identical_to_ordering_none() {
    let src = tempfile::tempdir().unwrap();
    fs::create_dir_all(src.path().join("docs/deep")).unwrap();
    fs::write(src.path().join("a.txt"), prose(1, 200_000)).unwrap();
    fs::write(src.path().join("docs/b.md"), prose(2, 70_000)).unwrap();
    fs::write(src.path().join("docs/deep/r.bin"), random(150_000)).unwrap();
    let work = tempfile::tempdir().unwrap();
    let s = src.path().to_str().unwrap();
    let make = |name: &str, extra: &[&str]| {
        let arch = work.path().join(name);
        let mut args = vec!["a", arch.to_str().unwrap(), s, "--balanced"];
        args.extend_from_slice(extra);
        let r = run(&args);
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        fs::read(&arch).unwrap()
    };
    let _settle = make("settle.lpk", &[]);
    let (default, none) = (make("d.lpk", &[]), make("n.lpk", &["--ordering", "none"]));
    assert!(default == none, "the archives differ");
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
    assert_no_secret(&a, b"open sesame");
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
    assert_no_secret(&x, b"open sesame");
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
    assert_no_secret(&t, b"open sesame");
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
    assert_no_secret(&a, b"hunter");
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
        assert_no_secret(&x, b"hunter");
    }
    // `t` with the wrong password is refused at the key slot too, with the key slot's own
    // error as the kind (E3-5: the reference tool's error reaches the tool typed).
    let t = run(&["t", arch.to_str().unwrap(), "--password", "hunter3"]);
    assert_eq!(t.status.code(), Some(2));
    assert_no_secret(&t, b"hunter");
    let t = run(&[
        "t",
        arch.to_str().unwrap(),
        "--password",
        "hunter3",
        "--json",
    ]);
    assert_eq!(t.status.code(), Some(2));
    assert_no_secret(&t, b"hunter");
    let e = json_error(&t);
    assert_eq!(e["kind"], "WrongKey", "{e}");
    assert_eq!(e["path"], arch.to_str().unwrap());
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

#[test]
fn add_threads_1_and_8_write_the_same_archive_and_v_prints_the_blocks_in_flight() {
    // Flat: a directory entry carries a modification time the file system moves between walks.
    let src = tempfile::tempdir().unwrap();
    for i in 0..4u64 {
        fs::write(src.path().join(format!("t{i}.txt")), prose(i + 1, 300_000)).unwrap();
    }
    fs::write(src.path().join("r.bin"), random(200_000)).unwrap();
    let work = tempfile::tempdir().unwrap();
    let mut archives = Vec::new();
    for (tier, threads, expect, hashes) in [
        (
            "--fast",
            "1",
            "threads: 1, blocks in flight: 1",
            "chunk hash threads: up to 1)",
        ),
        (
            "--fast",
            "8",
            "threads: 8, blocks in flight: 8",
            "chunk hash threads: up to 4)",
        ),
        (
            "--balanced",
            "1",
            "threads: 1, blocks in flight: 1",
            "chunk hash threads: up to 1)",
        ),
        (
            "--balanced",
            "8",
            "threads: 8, blocks in flight: 3",
            "chunk hash threads: up to 3)",
        ),
        (
            "--store",
            "1",
            "threads: 1, blocks in flight: 1",
            "chunk hash threads: up to 1)",
        ),
        (
            "--store",
            "8",
            "threads: 8, blocks in flight: 8",
            "chunk hash threads: up to 4)",
        ),
    ] {
        let arch = work.path().join(format!("{}-{threads}.lpk", &tier[2..]));
        let mut args = vec![
            "a",
            arch.to_str().unwrap(),
            src.path().to_str().unwrap(),
            tier,
            "--threads",
            threads,
            // A budget that cannot bind, so the blocks in flight are the threads on any machine.
            "--memory",
            "64G",
            "-v",
        ];
        if tier == "--balanced" {
            args.extend(["--dict-size", "1048576"]);
        }
        let a = run(&args);
        assert_eq!(a.status.code(), Some(0), "{tier}: {}", text(&a.stderr));
        assert!(
            text(&a.stderr).contains(expect) && text(&a.stderr).contains(hashes),
            "{tier}: {}",
            text(&a.stderr)
        );
        // The Balanced tier peels r.bin (a class the Deflate peel scans): at eight threads the
        // peel pool takes five, the blocks three (design `docs/notes/e2-10-peel-pool-design.md`, section 2).
        assert_eq!(
            text(&a.stderr).contains("peel pool: 5 workers"),
            tier == "--balanced" && threads == "8",
            "{tier}: {}",
            text(&a.stderr)
        );
        archives.push(fs::read(&arch).unwrap());
    }
    assert_eq!(archives[0], archives[1], "fast");
    assert_eq!(archives[2], archives[3], "balanced");
    assert_eq!(archives[4], archives[5], "store");
    assert_ne!(archives[0], archives[2]);
}

/// The conformance vectors of the format crate.
fn vectors() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-format/tests/vectors")
}

/// The reference tool's `list` of a vector, run in-process (its own escaping, its own order).
fn reference_list(args: &[&str]) -> String {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut argv = vec!["lpk-decode"];
    argv.extend_from_slice(args);
    let code = lpk_format::cli::run(argv, &mut out, &mut err);
    assert_eq!(code, 0, "{}", text(&err));
    text(&out)
}

/// `lpk l`'s default output with its mtime column removed: the reference tool's `list`.
fn without_mtime(listing: &str) -> String {
    listing
        .lines()
        .map(|l| {
            let mut cols: Vec<&str> = l.split('\t').collect();
            assert_eq!(cols.len(), 4, "{l}");
            cols.remove(2);
            cols.join("\t") + "\n"
        })
        .collect()
}

#[test]
fn list_prints_the_vectors_as_the_reference_tool_does_with_an_mtime_column() {
    let v = vectors();
    for (name, generation) in [
        ("zstd-basic.lpk", 0u64),
        ("sealed-listable.lpk", 0),
        ("journal-3gen.lpk", 2),
    ] {
        let path = v.join(name);
        let p = path.to_str().unwrap();
        let r = run(&["l", p]);
        assert_eq!(r.status.code(), Some(0), "{name}: {}", text(&r.stderr));
        assert!(r.stderr.is_empty(), "{name}: {}", text(&r.stderr));
        let listing = text(&r.stdout);
        assert_eq!(
            without_mtime(&listing),
            reference_list(&["list", p]),
            "{name}: {listing}"
        );
        for line in listing.lines() {
            let mtime = line.split('\t').nth(2).unwrap();
            assert!(
                mtime.len() == 20 && mtime.ends_with('Z') && &mtime[10..11] == "T",
                "{name}: {line}"
            );
        }
        // --long: the generation header, then the flags column before the path.
        let r = run(&["l", "--long", p]);
        assert_eq!(r.status.code(), Some(0), "{name}: {}", text(&r.stderr));
        let long = text(&r.stdout);
        let mut lines = long.lines();
        assert_eq!(lines.next().unwrap(), format!("generation\t{generation}"));
        let rest: Vec<&str> = lines.collect();
        assert_eq!(rest.len(), listing.lines().count());
        for (long_line, line) in rest.iter().zip(listing.lines()) {
            let cols: Vec<&str> = long_line.split('\t').collect();
            assert_eq!(cols.len(), 5, "{long_line}");
            assert_eq!(cols[3], "----", "{long_line}");
            assert_eq!(
                [cols[0], cols[1], cols[2], cols[4]].join("\t"),
                line,
                "{name}"
            );
        }
        // --json: one object per entry per line, every line parses, every key present.
        for argv in [vec!["l", "--json", p], vec!["--json", "l", p]] {
            let r = run(&argv);
            assert_eq!(r.status.code(), Some(0), "{name}: {}", text(&r.stderr));
            let json = text(&r.stdout);
            assert_eq!(json.lines().count(), listing.lines().count());
            for (object, line) in json.lines().zip(listing.lines()) {
                let v: serde_json::Value = serde_json::from_str(object).unwrap();
                let cols: Vec<&str> = line.split('\t').collect();
                assert_eq!(v["kind"], cols[0]);
                assert_eq!(v["size"].to_string(), cols[1]);
                assert_eq!(v["mtime"], cols[2]);
                assert_eq!(v["path"], cols[3]);
                assert_eq!(v["generation"], generation);
                let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
                assert_eq!(
                    keys,
                    [
                        "chunks",
                        "flag_bits",
                        "flags",
                        "generation",
                        "kind",
                        "mtime",
                        "mtime_ns",
                        "path",
                        "size",
                        "symlink_target"
                    ],
                    "{object}"
                );
            }
        }
    }
}

#[test]
fn a_sealed_archive_lists_only_with_its_key_unless_listable() {
    let v = vectors();
    let sealed = v.join("sealed-aes.lpk");
    let sealed = sealed.to_str().unwrap();
    // Without the key: refused before any line, exit 2, the variant named.
    let r = run(&["l", sealed]);
    assert_eq!(r.status.code(), Some(2));
    assert!(r.stdout.is_empty());
    let err = text(&r.stderr);
    assert!(
        err.starts_with("error: ") && err.contains("PasswordRequired") && err.contains(sealed),
        "{err}"
    );
    let r = run(&["--json", "l", sealed]);
    assert_eq!(r.status.code(), Some(2));
    assert!(r.stdout.is_empty());
    let err = text(&r.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    let e: serde_json::Value = serde_json::from_str(&err).unwrap();
    assert_eq!(e["error"]["code"], 2);
    assert_eq!(e["error"]["kind"], "PasswordRequired");
    assert_eq!(e["error"]["path"], sealed);
    // A wrong key is refused at the key slot.
    let r = run(&["l", sealed, "--password", "wrong"]);
    assert_eq!(r.status.code(), Some(2));
    assert!(text(&r.stderr).contains("WrongKey"), "{}", text(&r.stderr));
    // With the key (the vector's test password, `vectors.toml`): the reference's list.
    let r = run(&["l", sealed, "--password", "correct horse"]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(
        without_mtime(&text(&r.stdout)),
        reference_list(&["--password=correct horse", "list", sealed])
    );
    // The listable vector needs no key (checked above with the others) and takes one.
    let listable = v.join("sealed-listable.lpk");
    let listable = listable.to_str().unwrap();
    let keyless = run(&["l", listable]);
    let keyed = run(&["l", listable, "--password", "list me"]);
    assert_eq!(keyed.status.code(), Some(0), "{}", text(&keyed.stderr));
    assert_eq!(keyed.stdout, keyless.stdout);
}

#[test]
fn list_escapes_control_characters_as_the_reference_tool_and_shows_symlinks_in_long() {
    use lpk_format::{EntryFlags, Writer, WriterOptions};
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("odd.lpk");
    {
        let mut out = std::fs::File::create(&arch).unwrap();
        let mut w = Writer::new(&mut out, WriterOptions::default()).unwrap();
        w.add_file(
            "a\nb\tc\u{1}",
            EntryFlags::EXECUTABLE.union(EntryFlags::READ_ONLY),
            1_760_745_600_000_000_000 + 5,
            &mut &b"xyz"[..],
        )
        .unwrap();
        w.add_directory("dir", EntryFlags::HIDDEN, i64::MIN)
            .unwrap();
        w.add_symlink("link", EntryFlags::EMPTY, 0, "a\nb\tc\u{1}".as_bytes())
            .unwrap();
        w.finish().unwrap();
    }
    let p = arch.to_str().unwrap();
    let r = run(&["l", p]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    let listing = text(&r.stdout);
    assert_eq!(listing.lines().count(), 3, "{listing}");
    assert_eq!(without_mtime(&listing), reference_list(&["list", p]));
    assert!(
        listing.contains("File\t3\t2025-10-18T00:00:00Z\ta\\nb\\tc\\u{1}\n"),
        "{listing}"
    );
    assert!(listing.contains("Directory\t0\t-\tdir\n"), "{listing}");
    let r = run(&["l", "--long", p]);
    let long = text(&r.stdout);
    assert!(long.starts_with("generation\t0\n"), "{long}");
    assert!(long.contains("\tx-r-\ta\\nb\\tc\\u{1}\n"), "{long}");
    assert!(long.contains("\t-h--\tdir\n"), "{long}");
    assert!(
        long.contains("Symlink\t6\t1970-01-01T00:00:00Z\t----\tlink -> a\\nb\\tc\\u{1}\n"),
        "{long}"
    );
    // JSON carries the raw path, the names of the flags and the symlink target.
    let r = run(&["l", "--json", p]);
    let json = text(&r.stdout);
    let objects: Vec<serde_json::Value> = json
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(objects[0]["path"], "a\nb\tc\u{1}");
    assert_eq!(
        objects[0]["flags"],
        serde_json::json!(["executable", "read-only"])
    );
    assert_eq!(objects[0]["flag_bits"], 5);
    assert_eq!(objects[0]["mtime_ns"], 1_760_745_600_000_000_005i64);
    assert_eq!(objects[1]["mtime"], serde_json::Value::Null);
    assert_eq!(objects[1]["mtime_ns"], serde_json::Value::Null);
    assert_eq!(objects[2]["kind"], "Symlink");
    assert_eq!(objects[2]["symlink_target"], "a\nb\tc\u{1}");
    assert_eq!(objects[2]["size"], 6);
}

/// The one JSON object line a failing command writes on stderr with `--json`.
fn json_error(r: &Output) -> serde_json::Value {
    let err = text(&r.stderr);
    assert_eq!(err.lines().count(), 1, "exactly one line: {err}");
    let v: serde_json::Value = serde_json::from_str(&err).unwrap();
    assert_eq!(v.as_object().unwrap().len(), 1, "{err}");
    v["error"].clone()
}

#[test]
fn json_errors_are_one_object_line_and_text_errors_are_unchanged() {
    let v = vectors();
    let work = tempfile::tempdir().unwrap();
    let gone = work.path().join("nope");
    let gone = gone.to_str().unwrap();
    let arch = work.path().join("t.lpk");
    let arch = arch.to_str().unwrap();

    // A clap usage error: code 1, kind Usage, the message without clap's `error: ` prefix.
    let r = run(&["--json", "a", "only-one-arg"]);
    assert_eq!(r.status.code(), Some(1));
    let e = json_error(&r);
    assert_eq!(e["code"], 1);
    assert_eq!(e["kind"], "Usage");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .starts_with("the following required arguments"),
        "{e}"
    );
    assert_eq!(e.as_object().unwrap().len(), 3, "{e}");
    let plain = run(&["a", "only-one-arg"]);
    assert_eq!(plain.status.code(), Some(1));
    assert!(
        text(&plain.stderr).starts_with("error: "),
        "{}",
        text(&plain.stderr)
    );

    // A refused tier (E3-8) and an engine-refused option: Usage, exit 1, no file.
    let r = run(&["a", arch, gone, "--strong", "--json"]);
    assert_eq!(r.status.code(), Some(1));
    assert_eq!(json_error(&r)["kind"], "Usage");
    let r = run(&["a", arch, gone, "--json", "--window-log", "40"]);
    assert_eq!(r.status.code(), Some(1));
    let e = json_error(&r);
    assert_eq!(e["kind"], "InvalidOption", "{e}");
    assert!(!Path::new(arch).exists());

    // A pipeline error naming its path: code 2, kind Io, the path the error names.
    let r = run(&["--json", "a", arch, gone]);
    assert_eq!(r.status.code(), Some(2));
    let e = json_error(&r);
    assert_eq!(e["code"], 2);
    assert_eq!(e["kind"], "Io");
    assert_eq!(e["path"], gone);
    assert!(e.get("entries").is_none(), "{e}");
    let plain = run(&["a", arch, gone]);
    assert_eq!(plain.status.code(), Some(2));
    assert_eq!(
        text(&plain.stderr),
        format!("error: {}\n", e["message"].as_str().unwrap())
    );

    // The reference tool's commands (E3-5): the error's own variant as the kind, the archive
    // as the path, the same message as the text line.
    let bad = v.join("malformed-hashflip.lpk");
    let bad = bad.to_str().unwrap();
    let r = run(&["t", bad, "--json"]);
    assert_eq!(r.status.code(), Some(2));
    let e = json_error(&r);
    assert_eq!(e["kind"], "HashMismatch");
    assert_eq!(e["path"], bad);
    assert_eq!(e["message"], "payload hash mismatch in frame kind 2");
    let plain = run(&["t", bad]);
    assert_eq!(plain.status.code(), Some(2));
    assert_eq!(
        text(&plain.stderr),
        "error: payload hash mismatch in frame kind 2\n"
    );
    assert_eq!(plain.stdout, r.stdout);

    // Extraction refused before anything is written (a sealed archive without its key).
    let sealed = v.join("sealed-aes.lpk");
    let outdir = work.path().join("out");
    let r = run(&[
        "--json",
        "x",
        sealed.to_str().unwrap(),
        outdir.to_str().unwrap(),
    ]);
    assert_eq!(r.status.code(), Some(2));
    let e = json_error(&r);
    assert_eq!(e["kind"], "PasswordRequired", "{e}");
    assert_eq!(e["path"], sealed.to_str().unwrap());
    assert!(!outdir.exists());

    // Success is unchanged by the flag (stdout as before, nothing on stderr).
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let ok = run(&["--json", "a", arch, src.path().to_str().unwrap()]);
    assert_eq!(ok.status.code(), Some(0), "{}", text(&ok.stderr));
    assert!(ok.stderr.is_empty() && ok.stdout.is_empty());
    let ok = run(&["t", arch, "--json"]);
    assert_eq!(ok.status.code(), Some(0));
    assert!(text(&ok.stdout).starts_with("ok: "), "{}", text(&ok.stdout));
    assert!(ok.stderr.is_empty());
}

#[test]
fn extract_with_a_memory_limit_below_one_block_is_a_usage_error() {
    // E3-7: an invalid option of `x` (`--memory` that the archive's envelope allows but below
    // what one block in flight needs) is `CoreError::InvalidOption`, exit 1 as the same option
    // on `a` (before E3-7 `x` gave 2); a limit below the envelope stays a refusal, exit 2.
    let arch = vectors().join("zstd-basic.lpk");
    let arch = arch.to_str().unwrap();
    let work = tempfile::tempdir().unwrap();
    let out = work.path().join("out");
    let out = out.to_str().unwrap();
    let r = run(&["x", arch, out, "--memory", "20000"]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
    assert!(
        text(&r.stderr).contains("invalid option"),
        "{}",
        text(&r.stderr)
    );
    let r = run(&["--json", "x", arch, out, "--memory", "20000"]);
    assert_eq!(r.status.code(), Some(1));
    assert_eq!(json_error(&r)["kind"], "InvalidOption");
}

/// E3-2: `lpk x --overwrite skip` over a half-extracted tree completes, restores what is
/// missing, leaves what exists and prints the count; the other modes and a bad mode.
#[test]
fn overwrite_skip_over_a_half_extracted_tree_prints_the_skipped_count() {
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
    assert_eq!(run(&["x", arch_s, out_s]).status.code(), Some(0));
    // Half extracted: two of the four files gone, one changed.
    fs::remove_file(out.join("docs/b.md")).unwrap();
    fs::remove_file(out.join("docs/deep/r.bin")).unwrap();
    fs::write(out.join("a.txt"), b"changed").unwrap();
    // The default refuses (as before) and leaves the tree as it was.
    let refused = run(&["x", arch_s, out_s]);
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused.stderr));
    assert!(!out.join("docs/b.md").exists());
    // skip: completes, exit 0, the count on stderr, the changed file left alone.
    let skip = run(&["x", "--overwrite", "skip", arch_s, out_s]);
    assert_eq!(skip.status.code(), Some(0), "{}", text(&skip.stderr));
    assert_eq!(text(&skip.stderr), "skipped 2 existing entries\n");
    assert_eq!(text(&skip.stdout), "extracted 2 files, 3 directories\n");
    assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"changed");
    assert_eq!(
        fs::read(out.join("docs/b.md")).unwrap(),
        fs::read(src.path().join("docs/b.md")).unwrap()
    );
    // replace: the changed file is restored; nothing on stderr.
    let replace = run(&["x", "--overwrite", "replace", arch_s, out_s]);
    assert_eq!(replace.status.code(), Some(0), "{}", text(&replace.stderr));
    assert!(replace.stderr.is_empty(), "{}", text(&replace.stderr));
    assert_eq!(tree_hash(&out), tree_hash(src.path()));
    // rename: every file written beside its existing twin.
    let rename = run(&["x", "--overwrite", "rename", arch_s, out_s]);
    assert_eq!(rename.status.code(), Some(0), "{}", text(&rename.stderr));
    assert_eq!(text(&rename.stderr), "renamed 4 existing entries\n");
    assert_eq!(
        fs::read(out.join("a (1).txt")).unwrap(),
        fs::read(src.path().join("a.txt")).unwrap()
    );
    assert!(out.join("docs/deep/r (1).bin").exists());
    assert!(out.join("zero (1)").exists());
    // A mode that does not exist is a usage error.
    let bad = run(&["x", "--overwrite", "clobber", arch_s, out_s]);
    assert_eq!(bad.status.code(), Some(1), "{}", text(&bad.stderr));
    // A directory where a file goes: refused, naming it, kind InTheWay.
    let blocked = work.path().join("blocked");
    fs::create_dir_all(blocked.join("a.txt")).unwrap();
    let r = run(&["--json", "x", arch_s, blocked.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(2));
    let e = json_error(&r);
    assert_eq!(e["kind"], "InTheWay", "{e}");
    assert!(e["path"].as_str().unwrap().ends_with("a.txt"), "{e}");
    assert!(!blocked.join("docs").exists());
    // Under --json the counts are one summary line on stderr (every file exists now, after
    // the replace run; the rename run added the twins, which are not entries).
    let skip = run(&["--json", "x", "--overwrite", "skip", arch_s, out_s]);
    assert_eq!(skip.status.code(), Some(0), "{}", text(&skip.stderr));
    let lines: Vec<serde_json::Value> = text(&skip.stderr)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "{}", text(&skip.stderr));
    assert_eq!(lines[0]["summary"]["skipped"], 4, "{}", lines[0]);
    assert_eq!(lines[0]["summary"]["renamed"], 0);
    assert_eq!(lines[0]["summary"]["symlinks_skipped"], 0);
    assert_eq!(text(&skip.stdout), "extracted 0 files, 3 directories\n");
}

/// E3-2: a symlink entry is skipped with one warning line unless `--symlinks` creates it.
#[test]
fn symlink_entries_are_skipped_with_a_warning_unless_symlinks_is_given() {
    use lpk_format::{EntryFlags, Writer, WriterOptions};
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("links.lpk");
    {
        let mut out = std::fs::File::create(&arch).unwrap();
        let mut w = Writer::new(&mut out, WriterOptions::default()).unwrap();
        w.add_file("a.txt", EntryFlags::EMPTY, 0, &mut &b"target"[..])
            .unwrap();
        w.add_symlink("link", EntryFlags::EMPTY, 0, b"a.txt")
            .unwrap();
        w.finish().unwrap();
    }
    let arch_s = arch.to_str().unwrap();
    let out = work.path().join("out");
    let r = run(&["x", arch_s, out.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(
        text(&r.stderr),
        "warning: link: symlink entry skipped (--symlinks creates it)\n"
    );
    assert_eq!(text(&r.stdout), "extracted 1 files, 0 directories\n");
    assert!(fs::symlink_metadata(out.join("link")).is_err());
    // --json: the warning and the summary are one object line each.
    let jout = work.path().join("jout");
    let r = run(&["--json", "x", arch_s, jout.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    let lines: Vec<serde_json::Value> = text(&r.stderr)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{}", text(&r.stderr));
    assert_eq!(lines[0]["warning"]["kind"], "SymlinkSkipped");
    assert_eq!(lines[0]["warning"]["path"], "link");
    assert_eq!(lines[1]["summary"]["symlinks_skipped"], 1);
    assert_eq!(lines[1]["summary"]["skipped"], 0);
    // A failure under --json: the error line is the only stderr output, no warning.
    let r = run(&["--json", "x", arch_s, jout.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(2));
    let e = json_error(&r);
    assert_eq!(e["kind"], "Io", "{e}");
    // And without --json a failure prints the error line only, too.
    let r = run(&["x", arch_s, jout.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(2));
    assert_eq!(text(&r.stderr).lines().count(), 1, "{}", text(&r.stderr));
    let with = work.path().join("with");
    let r = run(&["x", "--symlinks", arch_s, with.to_str().unwrap()]);
    match r.status.code() {
        Some(0) => {
            assert!(r.stderr.is_empty(), "{}", text(&r.stderr));
            let l = with.join("link");
            assert!(fs::symlink_metadata(&l).unwrap().file_type().is_symlink());
            assert_eq!(fs::read(&l).unwrap(), b"target");
        }
        code => {
            // Windows without the symlink privilege: the extraction fails naming the link,
            // before any file is written.
            if !cfg!(windows) {
                panic!("{code:?}: {}", text(&r.stderr));
            }
            assert_eq!(code, Some(2), "{}", text(&r.stderr));
            assert!(text(&r.stderr).contains("link"), "{}", text(&r.stderr));
            assert!(!with.join("a.txt").exists());
        }
    }
}

/// E6-0: with `--symlinks` a link whose target leaves the output directory (here a UNC path
/// and an escaping one) is skipped with a warning naming the entry and the reason, counted in
/// symlinks_skipped, and the extraction goes on.
#[test]
fn symlinks_with_targets_leaving_the_output_are_skipped_with_a_reason() {
    use lpk_format::{EntryFlags, Writer, WriterOptions};
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("hostile.lpk");
    {
        let mut out = std::fs::File::create(&arch).unwrap();
        let mut w = Writer::new(&mut out, WriterOptions::default()).unwrap();
        w.add_file("a.txt", EntryFlags::EMPTY, 0, &mut &b"target"[..])
            .unwrap();
        w.add_symlink("sub/up", EntryFlags::EMPTY, 0, b"../../outside")
            .unwrap();
        w.add_symlink("unc", EntryFlags::EMPTY, 0, b"\\\\192.0.2.1\\share\\x")
            .unwrap();
        w.finish().unwrap();
    }
    let arch_s = arch.to_str().unwrap();
    let out = work.path().join("out");
    let r = run(&["x", "--symlinks", arch_s, out.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(
        text(&r.stderr),
        "warning: sub/up: symlink entry skipped: the target escapes the destination\n\
         warning: unc: symlink entry skipped: the target is a UNC path\n"
    );
    assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"target");
    assert!(fs::symlink_metadata(out.join("unc")).is_err());
    assert!(fs::symlink_metadata(out.join("sub/up")).is_err());
    let jout = work.path().join("jout");
    let r = run(&["--json", "x", "--symlinks", arch_s, jout.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    let lines: Vec<serde_json::Value> = text(&r.stderr)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "{}", text(&r.stderr));
    assert_eq!(lines[0]["warning"]["kind"], "SymlinkTargetRefused");
    assert_eq!(lines[0]["warning"]["path"], "sub/up");
    assert_eq!(
        lines[0]["warning"]["reason"],
        "the target escapes the destination"
    );
    assert_eq!(lines[1]["warning"]["path"], "unc");
    assert_eq!(lines[1]["warning"]["reason"], "the target is a UNC path");
    assert_eq!(lines[2]["summary"]["symlinks_skipped"], 2);
}
