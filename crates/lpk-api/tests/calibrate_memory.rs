//! E5-5 review round 1: `calibrate` reads at most the sample from any input and keeps no
//! output. One 64 MiB file calibrates, for each tier, with `bytes_in` equal to the 16 MiB
//! sample and a peak Rust heap growth well under the file's size.
//!
//! The bound is on the Rust allocator (the counting allocator): zstd's and LZMA's own C
//! allocations are not counted, so the test shows that nothing buffers the file or the archive
//! on the Rust side (the old `Vec` output and whole-file reads), which is what grew with the
//! input. Its own test binary, so no other test allocates meanwhile.
#![allow(clippy::unwrap_used)]

use lpk_api::{CreateSettings, Monitor, Sources, Tier};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

const SAMPLE: u64 = 16 << 20;
const FILE: usize = 64 << 20;

#[test]
fn a_huge_file_is_sampled_inside_the_file_and_never_buffered() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    // Compressible text with some variety, so that the output is not trivially small.
    let body: Vec<u8> = (0..FILE as u64)
        .map(|i| b"abcdefghij klmnop\n"[((i * 7 + (i >> 9)) % 18) as usize])
        .collect();
    std::fs::write(src.join("big.bin"), &body).unwrap();
    // The reference: a file of exactly the sample, which needs no cut.
    let exact = dir.path().join("exact");
    std::fs::create_dir(&exact).unwrap();
    std::fs::write(exact.join("exact.bin"), &body[..SAMPLE as usize]).unwrap();
    drop(body);
    let sources = Sources::contents_of(&src);
    let reference = Sources::contents_of(&exact);
    for tier in [Tier::Fast, Tier::Balanced, Tier::Store] {
        let s = CreateSettings {
            tier,
            ..CreateSettings::default()
        };
        // What the pipeline itself holds for a sample-sized input.
        let before = ALLOC.current();
        ALLOC.reset_peak();
        let base = lpk_api::calibrate(&reference, &s, SAMPLE, &Monitor::default()).unwrap();
        let floor = ALLOC.peak().saturating_sub(before);
        assert_eq!(base.bytes_in, SAMPLE);
        let before = ALLOC.current();
        ALLOC.reset_peak();
        let rate = lpk_api::calibrate(&sources, &s, SAMPLE, &Monitor::default()).unwrap();
        let growth = ALLOC.peak().saturating_sub(before);
        assert_eq!(rate.bytes_in, SAMPLE, "{tier:?}");
        assert!(rate.bytes_out > 0 && rate.seconds > 0.0, "{tier:?}");
        assert!(
            growth <= floor + (16 << 20),
            "{tier:?}: the heap grew by {growth} bytes for a {FILE}-byte file, {floor} for the sample"
        );
    }
    // Nothing is written anywhere: the same calibrations in a child process whose temporary
    // folder is empty (TEMP, TMP and TMPDIR) leave it empty.
    let scratch = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_calibrates",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("LPK_CALIBRATE_CHILD", &src)
        .env("TEMP", scratch.path())
        .env("TMP", scratch.path())
        .env("TMPDIR", scratch.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
}

/// The child of the test above: calibrates the folder `LPK_CALIBRATE_CHILD` names, per tier.
#[test]
fn child_calibrates() {
    let Some(src) = std::env::var_os("LPK_CALIBRATE_CHILD") else {
        return;
    };
    let sources = Sources::contents_of(std::path::Path::new(&src));
    for tier in [Tier::Fast, Tier::Balanced, Tier::Store] {
        let s = CreateSettings {
            tier,
            ..CreateSettings::default()
        };
        let rate = lpk_api::calibrate(&sources, &s, SAMPLE, &Monitor::default()).unwrap();
        assert_eq!(rate.bytes_in, SAMPLE, "{tier:?}");
    }
}
