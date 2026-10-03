//! Test helper for `lpk-procstat-sys`: allocate, burn CPU, spawn a grandchild, sleep, exit.
//!
//! Options (all `--name value`):
//! * `mem-mib N`: allocate and touch N MiB.
//! * `cpu-miters N`: burn a fixed N million loop iterations (CPU use independent of scheduling).
//! * `start-file PATH`: create the file immediately at start.
//! * `write-file PATH` + `write-after-ms MS`: create the file after the delay.
//! * `print-env NAME`: print `NAME=<value or <unset>>`.
//! * `sleep 1`: sleep forever (until killed). `no-wait 1`: do not wait for the grandchild.
//! * `count-stdin 1`: read standard input to the end and print `stdin-bytes=N`.
//! * `free-mem 1`: release the allocation before exiting.
//! * `exit-code C`.
//! * Any of the above prefixed `child-` spawns a copy of this program configured with them.
//! * `measure-child 1`: instead of `spawn`, call `lpk_procstat_sys::run` on that copy (so this
//!   probe runs `run` from inside whatever job it is in), print `inner exit=<code> rss=<bytes>`,
//!   and exit 0, or 99 after printing `inner error=...`.

use lpk_procstat_sys::{run, Spec};
use std::collections::HashMap;
use std::time::Duration;

fn main() {
    let mut o: HashMap<String, String> = HashMap::new();
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        o.insert(k.trim_start_matches("--").to_string(), v);
    }
    let num = |k: &str| o.get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);

    println!("probe-ok");

    let child_args: Vec<String> = o
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("child-").map(|r| (r, v)))
        .flat_map(|(r, v)| [format!("--{r}"), v.clone()])
        .collect();
    let exe = std::env::current_exe().expect("exe");

    if o.contains_key("measure-child") {
        match run(&Spec::new(&exe).args(&child_args)) {
            Ok(m) => {
                println!("inner exit={:?} rss={}", m.exit_code, m.peak_rss);
                std::process::exit(0);
            }
            Err(e) => {
                println!("inner error={e}");
                std::process::exit(99);
            }
        }
    }

    if o.contains_key("count-stdin") {
        let mut buf = Vec::new();
        let n = std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf).unwrap_or(0);
        println!("stdin-bytes={n}");
    }

    if let Some(name) = o.get("print-env") {
        match std::env::var(name) {
            Ok(v) => println!("{name}={v}"),
            Err(_) => println!("{name}=<unset>"),
        }
    }

    if let Some(path) = o.get("start-file") {
        let _ = std::fs::write(path, b"started");
    }

    let mut grandchild = None;
    if !child_args.is_empty() {
        let mut c = std::process::Command::new(&exe);
        c.args(&child_args);
        // We run detached (no console) under `lpk_procstat_sys::run`; without this the grandchild
        // would get a console host of its own, which would join the job and exit late.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0000_0008); // DETACHED_PROCESS
        }
        grandchild = Some(c.spawn().expect("spawn grandchild"));
    }

    let mem = num("mem-mib") as usize * 1024 * 1024;
    let buf = vec![0xA5u8; mem]; // non-zero fill commits and touches every page
    std::hint::black_box(&buf);

    let mut x = 1u64;
    for _ in 0..num("cpu-miters") * 1_000_000 {
        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
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
        if !o.contains_key("no-wait") {
            let _ = g.wait();
        }
    }
    if o.contains_key("free-mem") {
        drop(buf); // current usage falls back before exit; the peak must still be reported
    } else {
        std::hint::black_box(&buf);
    }
    std::process::exit(num("exit-code") as i32);
}
