//! Records the git commit (with a `-dirty` suffix when the working tree has uncommitted changes)
//! and the rustc version the binary was built from, for `host.json`. Both fall back to `unknown`
//! when git or rustc cannot be run.

use std::path::Path;
use std::process::Command;

fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Emit `rerun-if-changed` for a path only when it exists: cargo treats a missing path as
/// "always changed", which would re-run this script on every build (packed refs have no loose
/// ref file).
fn watch(path: &str) {
    if Path::new(path).exists() {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn git_path(what: &str) -> Option<String> {
    output("git", &["rev-parse", "--git-path", what])
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RUSTC");

    let mut commit = output("git", &["rev-parse", "--short=12", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());
    if commit != "unknown" {
        // A change to a tracked or untracked file of the build's inputs makes the build dirty.
        // Results directories live outside these paths, so a new results directory does not.
        // (`-C ../..`: the repository root, whatever the directory cargo runs this script in.)
        let status = Command::new("git")
            .args([
                "-C",
                "../..",
                "status",
                "--porcelain",
                "--",
                "crates",
                "Cargo.toml",
                "Cargo.lock",
                "bench/tools.toml",
            ])
            .output();
        match status {
            Ok(s) if s.status.success() => {
                if !s.stdout.iter().all(u8::is_ascii_whitespace) {
                    commit.push_str("-dirty");
                }
            }
            _ => commit = "unknown".to_string(),
        }
    }
    println!("cargo:rustc-env=LPK_GIT_COMMIT={commit}");

    // Re-run when HEAD moves (branch switch; a new commit on a branch with a loose ref; a
    // packed ref) and when the sources change, so the dirty marker is re-evaluated.
    for what in ["HEAD", "packed-refs", "refs/heads"] {
        if let Some(p) = git_path(what) {
            watch(&p);
        }
    }
    if let Some(reference) = output("git", &["symbolic-ref", "-q", "HEAD"]) {
        if let Some(p) = git_path(&reference) {
            watch(&p);
        }
    }
    for source in [
        "src",
        "tests",
        "Cargo.toml",
        "../../Cargo.toml",
        "../../Cargo.lock",
        // lpk-bench depends on lpk-procstat-sys: watch every crate, and the catalogue read at
        // run time.
        "../../crates",
        "../../bench/tools.toml",
    ] {
        watch(source);
    }

    // How this binary is built, for the probe files: the speeds of an unoptimised build mean
    // nothing, so every probe file records it.
    for (var, out) in [("PROFILE", "LPK_PROFILE"), ("OPT_LEVEL", "LPK_OPT_LEVEL")] {
        let value = std::env::var(var).unwrap_or_else(|_| "unknown".to_string());
        println!("cargo:rustc-env={out}={value}");
    }
    // The xz release linked in: the numbers in the version.h of the include directory that
    // liblzma-sys exposes (`links = "lzma"`, bundled-source path); the crate version comes from
    // Cargo.lock. Without the header the label says that only the crate version is known.
    let lock = std::fs::read_to_string("../../Cargo.lock").unwrap_or_default();
    let crate_version = lock
        .split("[[package]]")
        .find(|p| p.contains("name = \"liblzma-sys\""))
        .and_then(|p| p.lines().find_map(|l| l.strip_prefix("version = \"")))
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or("unknown")
        .to_string();
    println!("cargo:rerun-if-env-changed=DEP_LZMA_INCLUDE");
    let header = std::env::var("DEP_LZMA_INCLUDE")
        .ok()
        .map(|d| Path::new(&d).join("lzma").join("version.h"));
    let number = |text: &str, name: &str| -> Option<u32> {
        text.lines().find_map(|l| {
            let mut w = l.split_whitespace();
            (w.next() == Some("#define") && w.next() == Some(name))
                .then(|| w.next().and_then(|v| v.parse().ok()))
                .flatten()
        })
    };
    let release = header.as_ref().and_then(|h| {
        watch(&h.to_string_lossy());
        let text = std::fs::read_to_string(h).ok()?;
        Some(format!(
            "{}.{}.{}",
            number(&text, "LZMA_VERSION_MAJOR")?,
            number(&text, "LZMA_VERSION_MINOR")?,
            number(&text, "LZMA_VERSION_PATCH")?
        ))
    });
    let label = match release {
        Some(r) => format!(
            "liblzma {r} (bundled by liblzma-sys {crate_version}; generic C build, no SIMD or unaligned-access paths)"
        ),
        None => format!(
            "liblzma (release unknown: version.h not found or not parsed; generic C build of liblzma-sys {crate_version})"
        ),
    };
    println!("cargo:rustc-env=LPK_XZ_VERSION={label}");

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = output(&rustc, &["--version"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=LPK_RUSTC_VERSION={version}");
}
