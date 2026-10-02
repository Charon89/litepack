//! External programs for probes: lookup by name and a runner with the baseline runner's rules.
//!
//! Rules (the same as `run/exec.rs`): the program runs through `lpk-procstat-sys`, never through
//! a shell; the caller passes paths relative to the working directory (see
//! [`crate::run::exec::relative`]) so that no absolute path reaches a result file; the
//! tool-configuration variables in [`STRIPPED_ENV`] are removed from the child's environment;
//! stderr goes to a file; a timeout, a leftover descendant process or a non-zero exit is a
//! [`ToolFailure`] whose reason never contains a path.

#![allow(dead_code)] // shared helpers for probes that land in later tasks

use std::path::{Path, PathBuf};
use std::time::Duration;

use lpk_procstat_sys as ps;

pub use crate::run::exec::STRIPPED_ENV;

/// The skip reason for a program that cannot be found.
pub const NOT_INSTALLED: &str = "not installed";

/// What to run.
#[derive(Debug)]
pub struct ToolSpec<'a> {
    pub exe: &'a Path,
    pub args: &'a [String],
    pub cwd: &'a Path,
    pub stdin: ps::Input,
    pub stdout: ps::Output,
    /// File that receives the program's standard error.
    pub stderr_file: &'a Path,
    pub timeout: Duration,
}

/// Why a run did not end normally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFailure {
    pub reason: String,
    pub timed_out: bool,
    pub descendants_killed: bool,
}

/// Run a program as one measured step. `what` names it in the failure reason (for example
/// `zstd --patch-from`). Success means exit code 0, no timeout and no leftover descendants.
pub fn run_tool(spec: &ToolSpec<'_>, what: &str) -> Result<ps::Measurement, ToolFailure> {
    let mut s = ps::Spec::new(spec.exe)
        .args(spec.args)
        .cwd(spec.cwd)
        .stdin(match &spec.stdin {
            ps::Input::File(p) => ps::Input::File(p.clone()),
            _ => ps::Input::Null,
        })
        .stdout(match &spec.stdout {
            ps::Output::File(p) => ps::Output::File(p.clone()),
            ps::Output::Inherit => ps::Output::Inherit,
            _ => ps::Output::Discard,
        })
        .stderr(ps::Output::File(spec.stderr_file.to_path_buf()))
        .timeout(spec.timeout);
    for name in STRIPPED_ENV {
        s = s.env_remove(*name);
    }
    let fail = |reason: String, timed_out, descendants_killed| ToolFailure {
        reason,
        timed_out,
        descendants_killed,
    };
    let m = ps::run(&s).map_err(|e| {
        fail(
            format!("{what}: could not run the program ({:?})", e.kind()),
            false,
            false,
        )
    })?;
    if m.timed_out {
        return Err(fail(
            format!("{what}: timed out after {} s", spec.timeout.as_secs()),
            true,
            m.descendants_killed,
        ));
    }
    if m.descendants_killed {
        return Err(fail(
            format!("{what}: started processes that were still running when it exited (killed)"),
            false,
            true,
        ));
    }
    match m.exit_code {
        Some(0) => Ok(m),
        Some(c) => Err(fail(format!("{what}: exit code {c}"), false, false)),
        None => Err(fail(
            format!("{what}: ended without an exit code"),
            false,
            false,
        )),
    }
}

fn candidates(name: &str, windows: bool) -> Vec<String> {
    if windows {
        [".exe", ".cmd", ".bat", ".com"]
            .iter()
            .map(|e| format!("{name}{e}"))
            .collect()
    } else {
        vec![name.to_string()]
    }
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Find a program by name: the `path` of a `[[tool]]` entry with that `id` in the given text of
/// `bench/tools.local.toml` first, then the directories given. Returns the skip reason
/// ([`NOT_INSTALLED`]) when there is none.
pub fn find_tool_in(
    name: &str,
    local_toml: Option<&str>,
    path_dirs: &[PathBuf],
    windows: bool,
) -> Result<PathBuf, String> {
    if let Some(value) = local_toml.and_then(|t| t.parse::<toml::Table>().ok()) {
        let entries = value.get("tool").and_then(|t| t.as_array());
        for entry in entries.into_iter().flatten() {
            let id = entry.get("id").and_then(|v| v.as_str());
            let path = entry.get("path").and_then(|v| v.as_str());
            if let (Some(id), Some(path)) = (id, path) {
                let p = PathBuf::from(path);
                if id == name && is_executable(&p) {
                    return Ok(p);
                }
            }
        }
    }
    for dir in path_dirs {
        for cand in candidates(name, windows) {
            let p = dir.join(cand);
            if is_executable(&p) {
                return Ok(p);
            }
        }
    }
    Err(NOT_INSTALLED.to_string())
}

/// [`find_tool_in`] with this machine's `PATH` and the local override file at `local` (missing
/// file: no overrides). Names used by the probes: `zstd`, `bsc`, `kanzi`, `hdiffz`, `hpatchz`.
pub fn find_tool(name: &str, local: &Path) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(local).ok();
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    find_tool_in(name, text.as_deref(), &dirs, cfg!(windows))
}

/// The first non-empty line a program prints for `args` (for example `--version`), or `None`.
/// `work` is a scratch directory the program's output files go to.
pub fn tool_version(exe: &Path, args: &[&str], work: &Path) -> Option<String> {
    let out = work.join("version-out.txt");
    let err = work.join("version-err.txt");
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let spec = ToolSpec {
        exe,
        args: &args,
        cwd: work,
        stdin: ps::Input::Null,
        stdout: ps::Output::File(out.clone()),
        stderr_file: &err,
        timeout: Duration::from_secs(15),
    };
    run_tool(&spec, "version").ok()?;
    let text = std::fs::read_to_string(&out).ok()?;
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_prefers_the_local_entry_then_path_and_reports_not_installed() {
        let tmp = tempfile::tempdir().expect("tmp");
        let name = if cfg!(windows) {
            "faketool.exe"
        } else {
            "faketool"
        };
        let exe = tmp.path().join(name);
        std::fs::write(&exe, b"x").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let dirs = [tmp.path().to_path_buf()];
        assert_eq!(
            find_tool_in("faketool", None, &dirs, cfg!(windows)),
            Ok(exe.clone())
        );
        assert_eq!(
            find_tool_in("othertool", None, &dirs, cfg!(windows)),
            Err(NOT_INSTALLED.to_string())
        );
        let local = format!(
            "[[tool]]\nid = \"othertool\"\npath = '{}'\n",
            exe.to_string_lossy()
        );
        assert_eq!(
            find_tool_in("othertool", Some(&local), &[], cfg!(windows)),
            Ok(exe)
        );
        assert!(find_tool_in("othertool", Some("not [ toml"), &[], cfg!(windows)).is_err());
    }

    #[test]
    fn the_runner_reports_success_and_failure_without_paths() {
        let tmp = tempfile::tempdir().expect("tmp");
        let me = std::env::current_exe().expect("exe");
        let err = tmp.path().join("stderr.txt");
        let run = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            run_tool(
                &ToolSpec {
                    exe: &me,
                    args: &args,
                    cwd: tmp.path(),
                    stdin: ps::Input::Null,
                    stdout: ps::Output::Discard,
                    stderr_file: &err,
                    timeout: Duration::from_secs(120),
                },
                "test program",
            )
        };
        assert!(run(&["--list"]).is_ok());
        let f = run(&["--definitely-not-a-flag"]).expect_err("fails");
        assert!(
            f.reason.starts_with("test program: exit code"),
            "{}",
            f.reason
        );
        assert!(!f.timed_out && !f.descendants_killed);
        assert!(!f.reason.contains(tmp.path().to_string_lossy().as_ref()));
    }
}
