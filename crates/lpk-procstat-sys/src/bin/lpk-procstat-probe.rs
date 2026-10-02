//! Test helper for `lpk-procstat-sys`: allocate, burn CPU, spawn a grandchild, sleep, exit.
//!
//! Options (all `--name value`): `mem-mib`, `cpu-ms`, `exit-code`, `sleep` (any value),
//! `write-file` + `write-after-ms` (after that delay, create the file), and the same options
//! prefixed `child-` to spawn and wait for a copy of this program configured with them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

fn main() {
    let mut o: HashMap<String, String> = HashMap::new();
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        o.insert(k.trim_start_matches("--").to_string(), v);
    }
    let num = |k: &str| o.get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);

    println!("probe-ok");

    let mut grandchild = None;
    if o.keys().any(|k| k.starts_with("child-")) {
        let mut c = std::process::Command::new(std::env::current_exe().expect("exe"));
        for (k, v) in &o {
            if let Some(rest) = k.strip_prefix("child-") {
                c.arg(format!("--{rest}")).arg(v);
            }
        }
        grandchild = Some(c.spawn().expect("spawn grandchild"));
    }

    let mem = num("mem-mib") as usize * 1024 * 1024;
    let buf = vec![0xA5u8; mem]; // non-zero fill commits and touches every page
    std::hint::black_box(&buf);

    let cpu = Duration::from_millis(num("cpu-ms"));
    let t = Instant::now();
    let mut x = 1u64;
    while t.elapsed() < cpu {
        for _ in 0..10_000 {
            x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
    }

    if let Some(path) = o.get("write-file") {
        std::thread::sleep(Duration::from_millis(num("write-after-ms")));
        let _ = std::fs::write(path, b"alive");
    }
    if o.contains_key("sleep") {
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    if let Some(mut g) = grandchild {
        let _ = g.wait();
    }
    std::hint::black_box(&buf);
    std::process::exit(num("exit-code") as i32);
}
