//! Records the git commit (with a `-dirty` suffix when the sources have uncommitted changes) for
//! `lpk --version`, the way `lpk-bench` records its build: `unknown` when git cannot be run.

use std::path::Path;
use std::process::Command;

fn output(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// `rerun-if-changed` only for paths that exist (a missing path means "always changed").
fn watch(path: &str) {
    if Path::new(path).exists() {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn git_path(what: &str) -> Option<String> {
    output(&["rev-parse", "--git-path", what])
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let mut commit =
        output(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    if commit != "unknown" {
        // Same inputs as lpk-bench's build: a change under these paths makes the build dirty.
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
    for what in ["HEAD", "packed-refs", "refs/heads"] {
        if let Some(p) = git_path(what) {
            watch(&p);
        }
    }
    if let Some(reference) = output(&["symbolic-ref", "-q", "HEAD"]) {
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
        "../../crates",
        "../../bench/tools.toml",
    ] {
        watch(source);
    }
}
