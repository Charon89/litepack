//! The `deflate_recreate` fuzz finding of run 40270390275 (E4-6): a crafted record made the
//! limited recreation (D-66) resize the literal and distance code-length vectors to a decoded
//! count (`tree_predictor::recreate_tree_for_block`), up to 4 GiB each, past the writer's
//! `limit + 8` cap. Vendor change 12 (`vendor/preflate-rs-0.7.6/LITEPACK-CHANGES.md`) checks
//! each count against the range a Deflate header can carry before it sizes anything.
//!
//! A counting global allocator measures the peak heap of each recreation. Its own test binary,
//! so no other test allocates meanwhile.
//!
//! The fuzz inputs reach the resize only with `--cfg fuzzing` (which turns off bitcode's
//! trailing-bytes check, `bitcode::consume::expect_eof`) and a wrapping `decode_difference` (E4-6's
//! vendor change 9 on the fuzzed branch, or a build without overflow checks), which lets the
//! decoded count wrap below zero to about 2^32. Without the fix,
//! `RUSTFLAGS="--cfg fuzzing" cargo test --release -p lpk-core --test deflate_recreate_fuzz`
//! measured a peak of 12,985,859,157 bytes on the out-of-memory input. In a normal build they are
//! refused earlier, so the crafted record `counts-*.bin` (an addition, no wrap, no trailing bytes)
//! reaches the same resize in every build.
#![allow(clippy::unwrap_used)]

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, PoisonError};

use lpk_procstat_sys::alloc::CountingAlloc;
use preflate_rs::{recreate_whole_deflate_stream_limited_report, ExitCode};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

/// The fuzz target's cap on the plain text (`fuzz/fuzz_targets/deflate_recreate.rs`).
const MAX_PLAIN: usize = 1 << 16;

/// The heap a run may take beyond the writer's `limit + 8`: the decoded record, the predictor's
/// tables and hash chains and the panic machinery (measured: under 300 KB). Far below any
/// allocation sized from a crafted count (the crafted record asks for 64 MiB).
const BOOKKEEPING: usize = 8 << 20;

struct Outcome {
    peak: usize,
    /// `None`: the recreation panicked (the decoder catches it: `corrections`).
    result: Option<(Result<Vec<u8>, ExitCode>, usize)>,
}

/// The peak counter is process-wide: one measurement at a time, so one test's `reset_peak`
/// cannot erase another's peak.
static MEASURE: Mutex<()> = Mutex::new(());

fn recreate(plain: &[u8], corrections: &[u8], limit: usize) -> Outcome {
    let _guard = MEASURE.lock().unwrap_or_else(PoisonError::into_inner);
    let base = ALLOC.current();
    ALLOC.reset_peak();
    let r = catch_unwind(AssertUnwindSafe(|| {
        recreate_whole_deflate_stream_limited_report(plain, corrections, limit)
    }));
    let peak = ALLOC.peak().saturating_sub(base);
    Outcome {
        peak,
        result: r
            .ok()
            .map(|(res, capacity)| (res.map_err(|e| e.exit_code()), capacity)),
    }
}

fn check_bounded(name: &str, o: &Outcome, limit: usize) {
    assert!(
        o.peak <= limit + 8 + BOOKKEEPING,
        "{name}: peak heap {} bytes over limit {limit} + 8 + {BOOKKEEPING}",
        o.peak
    );
    if let Some((res, capacity)) = &o.result {
        assert!(*capacity <= limit + 8, "{name}: capacity {capacity}");
        assert!(res.is_err(), "{name}: a crafted record was accepted");
    }
}

/// The fuzz target's body.
fn check_fuzz_input(name: &str, data: &[u8]) {
    let limit = usize::from(u16::from_le_bytes([data[0], data[1]]));
    let rest = &data[6..];
    let declared = u32::from_le_bytes([data[2], data[3], data[4], data[5]]);
    let plain_len = usize::try_from(declared)
        .unwrap_or(usize::MAX)
        .min(MAX_PLAIN)
        .min(rest.len());
    let (plain, corrections) = rest.split_at(plain_len);
    let o = recreate(plain, corrections, limit);
    check_bounded(name, &o, limit);
}

/// Fuzz run 40270390275's out-of-memory input (libFuzzer: `malloc(4294967295)`). An
/// early-refusal regression: in a normal build bitcode refuses it (trailing bytes) before the
/// resize, so it does not guard change 12 (the crafted-record test below does).
#[test]
fn fuzz_oom_input_is_refused_early() {
    check_fuzz_input(
        "oom-e80ee409",
        include_bytes!("fixtures/fuzz/deflate_recreate-oom-e80ee409"),
    );
}

/// The base unit libFuzzer mutated into it (an earlier crash input of the target). An
/// early-refusal regression too: refused before the resize in a normal build.
#[test]
fn fuzz_crash_input_is_refused_early() {
    check_fuzz_input(
        "crash-35aa2eee",
        include_bytes!("fixtures/fuzz/deflate_recreate-crash-35aa2eee"),
    );
}

/// The crafted record (written by the vendored crate's ignored test
/// `litepack_crafted::write_crafted_counts_record`): a valid dynamic-block record whose literal
/// count correction asks for the predicted count plus 2^26. Refused as `InvalidPredictionData`
/// (the decoder's `corrections`) before the vector is resized; without change 12 it took 64 MiB.
#[test]
fn a_crafted_literal_count_is_refused_before_it_allocates() {
    let plain = include_bytes!("fixtures/deflate/crafted/counts-plain.bin");
    let corr = include_bytes!("fixtures/deflate/crafted/counts-corrections.bin");
    let limit = 1 << 10;
    let o = recreate(plain, corr, limit);
    check_bounded("counts", &o, limit);
    let (res, _) = o.result.expect("refused, not panicked");
    assert_eq!(res.unwrap_err(), ExitCode::InvalidPredictionData);
}
