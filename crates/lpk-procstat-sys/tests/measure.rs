#![allow(clippy::unwrap_used)]

use lpk_procstat_sys::{run, Input, Measurement, Output, Spec};
use std::path::Path;
use std::time::Duration;

const MIB: u64 = 1024 * 1024;
const PROBE: &str = env!("CARGO_BIN_EXE_lpk-procstat-probe");

fn probe(args: &[&str]) -> Spec {
    Spec::new(PROBE)
        .args(args.iter().copied())
        .stdout(Output::Discard)
        .stderr(Output::Discard)
}

/// Run the probe with stdout captured in a file; returns the measurement and the output text.
fn run_capture(spec: Spec, dir: &Path) -> (Measurement, String) {
    let out = dir.join("out.txt");
    let m = run(&spec.stdout(Output::File(out.clone()))).unwrap();
    (m, std::fs::read_to_string(out).unwrap())
}

#[test]
fn peak_memory_is_counted() {
    let m = run(&probe(&["--mem-mib", "64"])).unwrap();
    assert_eq!(m.exit_code, Some(0));
    assert!(!m.timed_out);
    assert!(m.peak_rss >= 64 * MIB, "{m:?}");
    assert!(m.peak_rss < 400 * MIB, "{m:?}");
    if cfg!(windows) {
        assert!(m.peak_commit_process.unwrap() >= 64 * MIB, "{m:?}");
        assert!(m.peak_commit_job.unwrap() >= 64 * MIB, "{m:?}");
    } else {
        assert_eq!(m.peak_commit_process, None);
        assert_eq!(m.peak_commit_job, None);
    }
}

/// The working-set peak is read after the process has exited and must still be the real peak,
/// not the (small) value at exit: the probe frees its buffer before exiting.
#[test]
fn peak_rss_survives_process_exit() {
    let m = run(&probe(&["--mem-mib", "96", "--free-mem", "1"])).unwrap();
    assert_eq!(m.exit_code, Some(0));
    assert!(m.peak_rss >= 96 * MIB, "{m:?}");
}

const BURN_MITERS: &str = "40";

#[test]
fn cpu_and_wall_time_are_consistent() {
    let m = run(&probe(&["--cpu-miters", BURN_MITERS])).unwrap();
    // A fixed amount of single-threaded work: CPU time does not depend on scheduling, cannot
    // exceed wall time (plus accounting slack) and cannot be implausibly small.
    assert!(
        m.total_cpu() >= Duration::from_millis(MIN_BURN_CPU_MS),
        "{m:?}"
    );
    assert!(m.total_cpu() <= m.wall + Duration::from_millis(50), "{m:?}");
    assert!(
        m.wall >= m.total_cpu().saturating_sub(Duration::from_millis(50)),
        "{m:?}"
    );
}

const MIN_BURN_CPU_MS: u64 = 10;

#[test]
fn grandchild_is_counted() {
    let m = run(&probe(&[
        "--mem-mib",
        "1",
        "--child-mem-mib",
        "48",
        "--child-cpu-miters",
        BURN_MITERS,
    ]))
    .unwrap();
    assert_eq!(m.exit_code, Some(0));
    assert!(!m.descendants_killed, "{m:?}");
    // Windows peak_rss covers the main process only; the commit peak covers the whole job.
    if cfg!(windows) {
        assert!(m.peak_commit_process.unwrap() >= 48 * MIB, "{m:?}");
    } else {
        assert!(m.peak_rss >= 48 * MIB, "{m:?}");
    }
    // Only the grandchild burns CPU, so the sum is bounded by wall time.
    assert!(
        m.total_cpu() >= Duration::from_millis(MIN_BURN_CPU_MS),
        "{m:?}"
    );
    assert!(m.total_cpu() <= m.wall + Duration::from_millis(50), "{m:?}");
}

#[test]
fn exit_code_propagates() {
    let m = run(&probe(&["--exit-code", "7"])).unwrap();
    assert_eq!(m.exit_code, Some(7));
    assert!(!m.timed_out);
    assert!(!m.descendants_killed);
}

#[test]
fn missing_program_is_an_error() {
    let r = run(&Spec::new("lpk-definitely-not-a-program-xyz"));
    assert!(r.is_err());
}

#[test]
fn bad_cwd_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let bad = probe(&[]).cwd(dir.path().join("no-such-dir"));
    assert!(run(&bad).is_err());
}

#[test]
fn timeout_kills_whole_tree() {
    let dir = tempfile::tempdir().unwrap();
    let started = dir.path().join("grandchild-started");
    let alive = dir.path().join("grandchild-alive");
    let m = run(&probe(&[
        "--sleep",
        "1",
        "--child-start-file",
        started.to_str().unwrap(),
        // A surviving grandchild would create this marker 300 ms after the timeout.
        "--child-write-after-ms",
        "1800",
        "--child-write-file",
        alive.to_str().unwrap(),
        "--child-sleep",
        "1",
    ])
    .timeout(Duration::from_millis(1500)))
    .unwrap();
    assert!(m.timed_out, "{m:?}");
    assert_eq!(m.exit_code, None);
    assert!(m.descendants_killed, "{m:?}");
    assert!(m.wall >= Duration::from_millis(1500), "{m:?}");
    assert!(
        started.exists(),
        "grandchild never started: the tree kill was not exercised"
    );
    std::thread::sleep(Duration::from_millis(600));
    assert!(!alive.exists(), "grandchild survived the timeout");
}

#[test]
fn descendants_still_running_at_exit_are_flagged() {
    let m = run(&probe(&["--no-wait", "1", "--child-sleep", "1"])).unwrap();
    assert_eq!(m.exit_code, Some(0));
    assert!(m.descendants_killed, "{m:?}");
    assert!(!m.timed_out);
}

#[test]
fn run_works_from_inside_a_job() {
    // The outer probe runs under our job (Windows) and itself calls `run` on an inner probe,
    // so the inner assignment happens with the caller already inside a job.
    let dir = tempfile::tempdir().unwrap();
    let (m, out) = run_capture(
        Spec::new(PROBE)
            .args([
                "--measure-child",
                "1",
                "--child-mem-mib",
                "32",
                "--child-exit-code",
                "5",
            ])
            .stderr(Output::Discard),
        dir.path(),
    );
    assert_eq!(m.exit_code, Some(0), "{out}");
    assert!(out.contains("inner exit=Some(5)"), "{out}");
    let rss: u64 = out
        .split("rss=")
        .nth(1)
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| panic!("no rss in {out}"));
    assert!(rss >= 32 * MIB, "{out}");
}

#[test]
fn output_can_go_to_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let (m, out) = run_capture(Spec::new(PROBE).stderr(Output::Discard), dir.path());
    assert_eq!(m.exit_code, Some(0));
    assert_eq!(out.trim(), "probe-ok");
}

#[test]
fn stdin_can_come_from_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.bin");
    std::fs::write(&input, vec![7u8; 100_000]).unwrap();
    let base = || {
        Spec::new(PROBE)
            .args(["--count-stdin", "1"])
            .stderr(Output::Discard)
    };
    let (m, out) = run_capture(base().stdin(Input::File(input.clone())), dir.path());
    assert_eq!(m.exit_code, Some(0));
    assert!(out.contains("stdin-bytes=100000"), "{out}");
    // The default is an empty input.
    let (_, out) = run_capture(base(), dir.path());
    assert!(out.contains("stdin-bytes=0"), "{out}");
    // A missing file is an error, not a silent empty input.
    let missing = dir.path().join("absent.bin");
    assert!(run(&base().stdin(Input::File(missing))).is_err());
}

#[test]
fn env_set_remove_and_clear() {
    let dir = tempfile::tempdir().unwrap();
    let base = |name: &str| {
        Spec::new(PROBE)
            .args(["--print-env", name])
            .stderr(Output::Discard)
    };

    let (_, out) = run_capture(
        base("LPK_PROCSTAT_X").env("LPK_PROCSTAT_X", "42"),
        dir.path(),
    );
    assert!(out.contains("LPK_PROCSTAT_X=42"), "{out}");

    // PATH exists in every test environment.
    let (_, out) = run_capture(base("PATH"), dir.path());
    assert!(!out.contains("PATH=<unset>"), "{out}");
    let (_, out) = run_capture(base("PATH").env_remove("PATH"), dir.path());
    assert!(out.contains("PATH=<unset>"), "{out}");

    let (m, out) = run_capture(
        base("PATH").env_clear().env("LPK_PROCSTAT_X", "1"),
        dir.path(),
    );
    assert_eq!(m.exit_code, Some(0));
    assert!(out.contains("PATH=<unset>"), "{out}");
}

/// Deterministic guard against a console host (conhost.exe) joining the job: the timeout path
/// checks once, without any grace period, so any extra process in the job would show up here.
#[test]
fn no_helper_processes_without_real_descendants() {
    let m = run(&probe(&["--sleep", "1"]).timeout(Duration::from_millis(300))).unwrap();
    assert!(m.timed_out, "{m:?}");
    assert!(!m.descendants_killed, "{m:?}");
}
