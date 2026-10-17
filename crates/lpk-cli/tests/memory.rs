//! E2-20's acceptance case and `--memory` on the command line: an archive written with a
//! 256 MiB window is refused by `lpk x --memory 128M` and `lpk t --memory 128M` before any
//! output exists, and extracts with the default limit; `lpk a --memory` below one block in
//! flight is refused with the figure; `-v` prints the budget.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use lpk_core::{FastOptions, Pipeline};
use lpk_format::{Archive, Resources};

const MIB: u64 = 1 << 20;

fn run(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lpk"))
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn prose(seed: u64, len: usize) -> Vec<u8> {
    const WORDS: [&str; 8] = ["the", "king", "of", "France", "and", "army", "sea", "road"];
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(WORDS[(x % 8) as usize].as_bytes());
        out.push(if x & 0x700 == 0 { b'\n' } else { b' ' });
    }
    out.truncate(len);
    out
}

/// A Fast archive whose one block holds 129 MiB, so its zstd frame declares a 2^28 window
/// (`window_log` 28, block size 256 MiB; no dedup, so the repeated MiB is all written).
fn big_window_archive(src: &Path, arch: &Path) {
    let unit = prose(1, MIB as usize);
    let mut data = Vec::with_capacity(129 * MIB as usize);
    for _ in 0..129 {
        data.extend_from_slice(&unit);
    }
    fs::write(src.join("big.txt"), &data).unwrap();
    let mut p = Pipeline::fast(FastOptions {
        window_log: 28,
        block_size: 256 * MIB,
        level: 1,
        threads: Some(1),
        ..FastOptions::default()
    });
    p.fold = None;
    p.run_file(src, arch).unwrap();
}

#[test]
fn a_256_mib_window_is_refused_below_its_memory_and_extracts_by_default() {
    let src = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("w.lpk");
    big_window_archive(src.path(), &arch);
    let env = Archive::open(fs::File::open(&arch).unwrap(), &Resources::default())
        .unwrap()
        .index()
        .envelope;
    assert_eq!(env.max_window, 256 * MIB);
    let plain = 129 * MIB;
    assert_eq!(env.max_block_plain, plain);
    // The block, its window (the block, shorter than 2^28) and the decoder's allowance.
    let need = lpk_core::memory::zstd_decode_memory(28, plain) + plain;
    assert_eq!(env.decode_memory, need);

    let a = arch.as_os_str();
    let out = work.path().join("out");
    let x = run(&[
        "x".as_ref(),
        a,
        out.as_os_str(),
        "--memory".as_ref(),
        "128M".as_ref(),
    ]);
    assert_eq!(x.status.code(), Some(2), "{}", text(&x.stderr));
    let e = text(&x.stderr);
    assert!(e.contains("decode_memory"), "{e}");
    assert!(e.contains(&need.to_string()), "{e}");
    assert!(e.contains(&(128 * MIB).to_string()), "{e}");
    assert!(!out.exists(), "no output directory before the refusal");

    let t = run(&["t".as_ref(), a, "--memory".as_ref(), "128M".as_ref()]);
    assert_eq!(t.status.code(), Some(2), "{}", text(&t.stderr));
    let e = text(&t.stderr);
    assert!(
        e.contains("decode_memory") && e.contains(&need.to_string()),
        "{e}"
    );

    let t = run(&["t".as_ref(), a]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
    let x = run(&["x".as_ref(), a, out.as_os_str(), "-v".as_ref()]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(
        fs::read(out.join("big.txt")).unwrap(),
        fs::read(src.path().join("big.txt")).unwrap()
    );
}

#[test]
fn add_refuses_a_budget_below_one_block_and_v_prints_the_budget() {
    let src = tempfile::tempdir().unwrap();
    for i in 0..3u64 {
        fs::write(src.path().join(format!("t{i}.txt")), prose(i + 1, 200_000)).unwrap();
    }
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("a.lpk");
    let s = src.path().as_os_str();
    let a = run(&[
        "a".as_ref(),
        arch.as_os_str(),
        s,
        "--memory".as_ref(),
        "16M".as_ref(),
    ]);
    assert_eq!(a.status.code(), Some(1), "{}", text(&a.stderr));
    let e = text(&a.stderr);
    assert!(e.contains("below what one block in flight needs"), "{e}");
    assert!(!arch.exists(), "{e}");
    let bad = run(&[
        "a".as_ref(),
        arch.as_os_str(),
        s,
        "--memory".as_ref(),
        "lots".as_ref(),
    ]);
    assert_eq!(bad.status.code(), Some(1));

    // A budget that allows exactly one block in flight: sequential, and -v says so.
    let probe = run(&[
        "a".as_ref(),
        arch.as_os_str(),
        s,
        "--threads".as_ref(),
        "8".as_ref(),
        "-v".as_ref(),
    ]);
    assert_eq!(probe.status.code(), Some(0), "{}", text(&probe.stderr));
    let v = text(&probe.stderr);
    assert!(
        v.contains("memory: budget ") && v.contains("(default)"),
        "{v}"
    );
    let figure = |key: &str| -> u64 {
        let at = v.find(key).unwrap() + key.len();
        v[at..].split(' ').next().unwrap().parse().unwrap()
    };
    let per_block = figure("per block ");
    let first = fs::read(&arch).unwrap();
    fs::remove_file(&arch).unwrap();
    // The sequential path's fixed figure is below the pipelined one, so ask for the minimum
    // through the refusal's message.
    let r = run(&[
        "a".as_ref(),
        arch.as_os_str(),
        s,
        "--memory".as_ref(),
        per_block.to_string().as_ref(),
    ]);
    let e = text(&r.stderr);
    let at = e.find("needs: ").unwrap() + "needs: ".len();
    let need: u64 = e[at..].split(' ').next().unwrap().parse().unwrap();
    let one = run(&[
        "a".as_ref(),
        arch.as_os_str(),
        s,
        "--threads".as_ref(),
        "8".as_ref(),
        "--memory".as_ref(),
        need.to_string().as_ref(),
        "-v".as_ref(),
    ]);
    assert_eq!(one.status.code(), Some(0), "{}", text(&one.stderr));
    let v = text(&one.stderr);
    assert!(v.contains("(--memory)"), "{v}");
    assert!(
        v.contains("blocks in flight 1 (sequential: no reader or write thread)"),
        "{v}"
    );
    assert!(v.contains("I/O threads: 0"), "{v}");
    assert_eq!(
        fs::read(&arch).unwrap(),
        first,
        "the bytes do not depend on the budget"
    );
}
