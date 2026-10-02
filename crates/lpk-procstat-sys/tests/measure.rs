#![allow(clippy::unwrap_used)]

use lpk_procstat_sys::{run, Output, Spec};
use std::time::Duration;

const MIB: u64 = 1024 * 1024;

fn probe(args: &[&str]) -> Spec {
    Spec::new(env!("CARGO_BIN_EXE_lpk-procstat-probe"))
        .args(args.iter().copied())
        .stdout(Output::Discard)
        .stderr(Output::Discard)
}

#[test]
fn peak_memory_is_counted() {
    let m = run(&probe(&["--mem-mib", "64"])).unwrap();
    assert_eq!(m.exit_code, Some(0));
    assert!(!m.timed_out);
    assert!(m.peak_process_memory >= 64 * MIB, "{m:?}");
    assert!(m.peak_process_memory < 400 * MIB, "{m:?}");
    if cfg!(windows) {
        assert!(m.peak_job_memory.unwrap() >= 64 * MIB, "{m:?}");
    }
}

#[test]
fn cpu_and_wall_time_are_counted() {
    let m = run(&probe(&["--cpu-ms", "300"])).unwrap();
    assert!(m.wall >= Duration::from_millis(300), "{m:?}");
    assert!(m.total_cpu() >= Duration::from_millis(150), "{m:?}");
}

#[test]
fn grandchild_is_counted() {
    let m = run(&probe(&[
        "--mem-mib",
        "1",
        "--child-mem-mib",
        "48",
        "--child-cpu-ms",
        "300",
    ]))
    .unwrap();
    assert_eq!(m.exit_code, Some(0));
    assert!(m.peak_process_memory >= 48 * MIB, "{m:?}");
    assert!(m.total_cpu() >= Duration::from_millis(150), "{m:?}");
}

#[test]
fn exit_code_propagates() {
    let m = run(&probe(&["--exit-code", "7"])).unwrap();
    assert_eq!(m.exit_code, Some(7));
    assert!(!m.timed_out);
}

#[test]
fn missing_program_is_an_error() {
    let r = run(&Spec::new("lpk-definitely-not-a-program-xyz"));
    assert!(r.is_err());
}

#[test]
fn timeout_kills_whole_tree() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("grandchild-alive");
    let m = run(&probe(&[
        "--sleep",
        "1",
        "--child-write-after-ms",
        "500",
        "--child-write-file",
        marker.to_str().unwrap(),
        "--child-sleep",
        "1",
    ])
    .timeout(Duration::from_millis(200)))
    .unwrap();
    assert!(m.timed_out, "{m:?}");
    assert_eq!(m.exit_code, None);
    assert!(m.wall < Duration::from_secs(20), "{m:?}");
    // A surviving grandchild would create the marker at ~500 ms.
    std::thread::sleep(Duration::from_millis(700));
    assert!(!marker.exists(), "grandchild survived the timeout");
}

#[test]
fn output_can_go_to_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.txt");
    let spec = Spec::new(env!("CARGO_BIN_EXE_lpk-procstat-probe"))
        .stdout(Output::File(out.clone()))
        .stderr(Output::Discard);
    assert_eq!(run(&spec).unwrap().exit_code, Some(0));
    assert_eq!(std::fs::read_to_string(&out).unwrap().trim(), "probe-ok");
}

#[test]
fn bad_cwd_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let bad = probe(&[]).cwd(dir.path().join("no-such-dir"));
    assert!(run(&bad).is_err());
}
