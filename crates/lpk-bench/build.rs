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
        // Any change to tracked or untracked (non-ignored) files makes the build dirty.
        let status = Command::new("git").args(["status", "--porcelain"]).output();
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
    ] {
        watch(source);
    }

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = output(&rustc, &["--version"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=LPK_RUSTC_VERSION={version}");
}
