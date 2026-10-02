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

/// Describe this machine.
pub fn collect() -> HostFile {
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
    }
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
    fn directory_names_use_the_utc_date() {
        // 2026-10-01T23:59:59Z and one second later.
        assert_eq!(results_dir_name(1_790_899_199, "Box 1"), "2026-10-01-box-1");
        assert_eq!(results_dir_name(1_790_899_200, "Box 1"), "2026-10-02-box-1");
    }

    #[test]
    fn collected_host_info_is_filled_in_and_valid() {
        let h = collect();
        assert!(h.logical_cores >= 1 && h.ram_bytes > 0);
        assert!(
            !h.git_commit.is_empty() && h.rustc_version.starts_with("rustc")
                || h.rustc_version == "unknown"
        );
        assert_eq!(h.host, sanitize_host(&h.host));
    }
}
