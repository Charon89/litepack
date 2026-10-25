//! E4-1 on Linux (decision (d)): the worker is a plain child under `setrlimit`, NOT sandboxed.
//! These tests prove the limits and the fd passing only.
#![cfg(target_os = "linux")]

use lpk_sandbox_sys::{roles, Access, Launch, Limits};
use std::io::Read;
use std::os::fd::AsFd;
use std::time::Duration;

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_lpk-sandbox-testworker")
}

fn limits() -> Limits {
    Limits {
        memory: 256 << 20,
        cpu_time: Duration::from_secs(30),
        cpu_ceiling: Duration::from_secs(60),
    }
}

fn run(lim: Limits, args: &[&str]) -> (u32, String) {
    let (mut r, w) = std::io::pipe().expect("pipe");
    let worker = Launch::new(exe())
        .limits(lim)
        .args(args)
        .handle(roles::RESPONSES, w.as_fd(), Access::Same)
        .spawn()
        .expect("spawn");
    drop(w);
    let mut text = String::new();
    r.read_to_string(&mut text).expect("read");
    let code = worker
        .wait(Some(Duration::from_secs(60)))
        .expect("wait")
        .expect("exited");
    (code, text)
}

fn limit_line<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    text.lines()
        .find(|l| l.starts_with(&format!("limit {name}")))
        .unwrap_or_else(|| panic!("no {name} in {text}"))
        .split_whitespace()
        .skip(1)
        .collect()
}

#[test]
fn setrlimit_limits_and_fds() {
    let (code, text) = run(limits(), &["report"]);
    assert_eq!(code, 0, "{text}");
    // /proc/self/limits columns: name words, soft, hard, units.
    let cpu = limit_line(&text, "Max cpu time");
    assert_eq!(&cpu[3..5], ["30", "60"], "{text}");
    let asl = limit_line(&text, "Max address space");
    assert_eq!(&asl[3..5], ["268435456", "268435456"], "{text}");
    let core = limit_line(&text, "Max core file size");
    assert_eq!(&core[4..6], ["0", "0"], "{text}");
    let fsize = limit_line(&text, "Max file size");
    assert_eq!(&fsize[3..5], ["0", "0"], "{text}");
    let nofile = limit_line(&text, "Max open files");
    assert_eq!(&nofile[3..5], ["36", "36"], "{text}");
    assert!(text.contains("passed 2=3"), "{text}");
}

#[test]
fn memory_limit_kills() {
    let lim = Limits {
        memory: 128 << 20,
        ..limits()
    };
    let (code, text) = run(lim, &["alloc", "512"]);
    assert!(!text.contains("alloc ok"), "{text}");
    assert_ne!(code, 0);
}

#[test]
fn cpu_limit_can_be_raised_while_running() {
    let (r, w) = std::io::pipe().expect("pipe");
    let lim = Limits {
        cpu_time: Duration::from_secs(1),
        ..limits()
    };
    let worker = Launch::new(exe())
        .limits(lim)
        .args(["spin", "2500"])
        .handle(roles::RESPONSES, w.as_fd(), Access::Same)
        .spawn()
        .expect("spawn");
    drop(w);
    worker
        .set_cpu_limit(Duration::from_secs(30))
        .expect("raise");
    let mut text = String::new();
    let mut r = r;
    r.read_to_string(&mut text).expect("read");
    assert!(text.contains("spin done"), "{text}");
    assert_eq!(
        worker.wait(Some(Duration::from_secs(60))).expect("wait"),
        Some(0)
    );
}

#[test]
fn worker_dies_with_its_handle() {
    let (_r, w) = std::io::pipe().expect("pipe");
    let worker = Launch::new(exe())
        .limits(limits())
        .args(["sleep", "60000"])
        .handle(roles::RESPONSES, w.as_fd(), Access::Same)
        .spawn()
        .expect("spawn");
    let pid = worker.id();
    drop(worker); // kills and reaps the worker
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}
