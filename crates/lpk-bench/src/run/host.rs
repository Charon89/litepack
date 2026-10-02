//! `host.json` contents and results-directory naming.

use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

use super::result::{HostFile, SCHEMA_VERSION};
use crate::corpus::manifest::rfc3339_utc;

/// Lower-case and reduce to `[a-z0-9-]`: other characters become `-`, runs of `-` collapse, and
/// leading/trailing `-` are dropped. An empty result becomes `unknown`.
pub fn sanitize_host(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_alphanumeric() { c } else { '-' };
        if c == '-' && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        out.push(c);
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

/// `<YYYY-MM-DD>-<host>` with the date in UTC.
pub fn results_dir_name(unix_secs: u64, host: &str) -> String {
    let stamp = rfc3339_utc(unix_secs);
    format!("{}-{}", &stamp[..10], sanitize_host(host))
}

/// The first free directory name for a run: `<date>-<host>`, then `<date>-<host>-2`, `-3`, ...
/// (a repeat run on the same UTC day), judged by what exists under `root`.
pub fn free_results_dir_name(root: &std::path::Path, unix_secs: u64, host: &str) -> String {
    let base = results_dir_name(unix_secs, host);
    if !root.join(&base).exists() {
        return base;
    }
    (2u32..)
        .map(|n| format!("{base}-{n}"))
        .find(|name| !root.join(name).exists())
        .unwrap_or(base)
}

/// Is the commit stamp of a build usable for published results? A stamp with the `-dirty`
/// suffix, or `unknown`, is not.
pub fn build_is_clean(git_commit: &str) -> bool {
    git_commit != "unknown" && !git_commit.ends_with("-dirty") && !git_commit.is_empty()
}

/// The run loop's gate: refuse to write results from a dirty or unknown build unless the flag
/// allows it (the flag's use is recorded in `host.json` as `dirty_build_allowed`).
pub fn check_build(git_commit: &str, allow_dirty: bool) -> Result<(), String> {
    if build_is_clean(git_commit) || allow_dirty {
        Ok(())
    } else {
        Err(format!(
            "this binary was built from `{git_commit}`, not a clean commit: results would not be \
             reproducible. Commit your changes and rebuild, or pass --allow-dirty-build"
        ))
    }
}

/// Describe this machine. `allow_dirty` records that the dirty-build flag was used.
pub fn collect(allow_dirty: bool) -> HostFile {
    let sys = System::new_with_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing())
            .with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let cores = if sys.cpus().is_empty() {
        std::thread::available_parallelism().map_or(1, usize::from)
    } else {
        sys.cpus().len()
    };
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let or_unknown = |v: Option<String>| {
        v.filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".into())
    };
    HostFile {
        schema_version: SCHEMA_VERSION,
        host: sanitize_host(&System::host_name().unwrap_or_default()),
        os: std::env::consts::OS.to_string(),
        os_version: or_unknown(System::long_os_version().or_else(System::os_version)),
        cpu_model: cpu,
        logical_cores: u32::try_from(cores).unwrap_or(u32::MAX).max(1),
        ram_bytes: sys.total_memory(),
        lpk_bench_version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: env!("LPK_GIT_COMMIT").to_string(),
        rustc_version: env!("LPK_RUSTC_VERSION").to_string(),
        defender_realtime: defender_realtime().to_string(),
        dirty_build_allowed: allow_dirty,
    }
}

/// Read `reg query` output for `DisableRealtimeMonitoring`: `0x1` means real-time protection is
/// off, `0x0` on, anything else `unknown`. (The value is absent on a default installation, in
/// which case the answer is `unknown`, not a guess.)
pub fn parse_defender(reg_output: &str) -> &'static str {
    for line in reg_output.lines() {
        let mut words = line.split_whitespace();
        if words.next() == Some("DisableRealtimeMonitoring") && words.next() == Some("REG_DWORD") {
            return match words.next() {
                Some("0x0") => "on",
                Some("0x1") => "off",
                _ => "unknown",
            };
        }
    }
    "unknown"
}

/// Whether Windows Defender real-time protection is on, best effort: `unknown` on any failure
/// and outside Windows.
#[cfg(windows)]
pub fn defender_realtime() -> &'static str {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let Ok(mut child) = Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Windows Defender\Real-Time Protection",
            "/v",
            "DisableRealtimeMonitoring",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return "unknown";
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return "unknown";
            }
        }
    }
    let mut text = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut out, &mut text);
    }
    parse_defender(&text)
}

#[cfg(not(windows))]
pub fn defender_realtime() -> &'static str {
    "unknown"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_names_are_reduced_to_lowercase_alphanumerics_and_dashes() {
        assert_eq!(sanitize_host("DESKTOP-ABC_01"), "desktop-abc-01");
        assert_eq!(sanitize_host("  My PC!! "), "my-pc");
        assert_eq!(sanitize_host("Ünï.local"), "n-local");
        assert_eq!(sanitize_host("***"), "unknown");
        assert_eq!(sanitize_host(""), "unknown");
    }

    #[test]
    fn defender_state_is_read_from_the_registry_output_or_unknown() {
        let q = |v: &str| {
            format!(
                "\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows Defender\\Real-Time Protection\n    DisableRealtimeMonitoring    REG_DWORD    {v}\n\n"
            )
        };
        assert_eq!(parse_defender(&q("0x0")), "on");
        assert_eq!(parse_defender(&q("0x1")), "off");
        assert_eq!(parse_defender(&q("0x7")), "unknown");
        assert_eq!(parse_defender(""), "unknown");
        assert_eq!(
            parse_defender("ERROR: The system was unable to find the specified registry key"),
            "unknown"
        );
        assert!(["on", "off", "unknown"].contains(&defender_realtime()));
        if !cfg!(windows) {
            assert_eq!(defender_realtime(), "unknown");
        }
    }

    #[test]
    fn directory_names_use_the_utc_date() {
        // 2026-10-01T23:59:59Z and one second later.
        assert_eq!(results_dir_name(1_790_899_199, "Box 1"), "2026-10-01-box-1");
        assert_eq!(results_dir_name(1_790_899_200, "Box 1"), "2026-10-02-box-1");
    }

    #[test]
    fn repeat_runs_on_one_day_get_a_numeric_suffix() {
        let tmp = tempfile::tempdir().expect("tmp");
        let day = 1_790_899_200; // 2026-10-02 UTC
        assert_eq!(
            free_results_dir_name(tmp.path(), day, "Box"),
            "2026-10-02-box"
        );
        std::fs::create_dir(tmp.path().join("2026-10-02-box")).expect("dir");
        assert_eq!(
            free_results_dir_name(tmp.path(), day, "Box"),
            "2026-10-02-box-2"
        );
        std::fs::create_dir(tmp.path().join("2026-10-02-box-2")).expect("dir");
        assert_eq!(
            free_results_dir_name(tmp.path(), day, "Box"),
            "2026-10-02-box-3"
        );
    }

    #[test]
    fn the_build_stamp_is_a_commit_in_a_git_checkout() {
        let stamp = env!("LPK_GIT_COMMIT");
        eprintln!("build stamp: {stamp}");
        let in_checkout = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .any(|p| p.join(".git").exists());
        if in_checkout {
            let hex = stamp.strip_suffix("-dirty").unwrap_or(stamp);
            assert!(
                hex.len() == 12 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
                "stamp `{stamp}`"
            );
        }
    }

    #[test]
    fn dirty_and_unknown_builds_are_refused_unless_allowed() {
        assert!(check_build("0123456789ab", false).is_ok());
        for stamp in ["0123456789ab-dirty", "unknown"] {
            let err = check_build(stamp, false).expect_err("refused");
            assert!(err.contains("--allow-dirty-build"), "{err}");
            assert!(check_build(stamp, true).is_ok());
        }
        assert!(collect(true).dirty_build_allowed);
        assert!(!collect(false).dirty_build_allowed);
    }

    #[test]
    fn collected_host_info_is_filled_in_and_valid() {
        let h = collect(false);
        assert!(h.logical_cores >= 1 && h.ram_bytes > 0);
        assert!(
            !h.git_commit.is_empty() && h.rustc_version.starts_with("rustc")
                || h.rustc_version == "unknown"
        );
        assert_eq!(h.host, sanitize_host(&h.host));
    }
}
