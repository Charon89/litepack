//! `kind = "git-repo"`: a pinned commit of a git repository, produced with the external `git`
//! program (which is optional: without it the source is skipped, see below).
//!
//! The commit is fetched explicitly (`git init`, `git fetch [--depth N] <repo> <commit>`), never
//! a moving branch, and the lock pins the commit id (`commit` field). Two modes:
//! * `clone`: the checked-out working tree plus a `.git` directory normalised for determinism:
//!   no template files (hooks, `info/exclude`, description), no reflogs, no `FETCH_HEAD` or
//!   `ORIG_HEAD`, no index (it stores timestamps), refs packed into `packed-refs`, and all objects
//!   repacked into one pack (`repack -a -d -F`, one thread, fixed window, depth and compression).
//!   After the clean-up only `HEAD`, `config` (rewritten from a fixed text), `packed-refs`,
//!   `shallow` (history depth only) and `objects/pack/pack-*.{pack,idx}` remain.
//! * `export`: the working tree of the commit only, without `.git`.
//!
//! Both check out with `core.autocrlf=false`, `core.eol=lf`, `core.symlinks=false` (a symlink
//! becomes a small file holding its target) and `core.filemode=false`, so Windows and Linux
//! write the same bytes. The user's global git configuration is replaced by an empty file; the
//! system configuration is left alone (Git for Windows keeps its TLS settings there) and every
//! relevant setting is overridden on the command line. The git version is recorded under
//! `tools` in `build-info.json`.
//!
//! Same git version and same settings give the same bytes. Pack bytes can still differ between
//! git versions (delta search, zlib builds, pack and index format defaults); working-tree files
//! are unaffected.
//!
//! Only a missing `git` program is a fetch failure, so an `optional = true` source is listed
//! under `skipped` with the reason instead of failing the build. Any other git error (the commit
//! is gone upstream, a failed fetch, a tree that is not portable to every OS) fails the build.
//! git runs with: no credential helper, no hooks, no git-lfs filters, no inherited `GIT_*`
//! variables, `https` as the only protocol (the file protocol only in tests), and
//! `transfer.fsckObjects=true`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{bail, ensure, Context, Result};

use super::build::Ctx;
use super::fetch::{hash_file, DownloadError};
use super::manifest::ManifestFile;
use super::registry::{GitMode, GitSpec, Source};

/// Fixed `.git/config` of a `clone` output (the real one differs by OS and git version).
fn fixed_config(repo: &str) -> String {
    format!(
        "[core]\n\trepositoryformatversion = 0\n\tfilemode = false\n\tbare = false\n\
         \tautocrlf = false\n\teol = lf\n\tsymlinks = false\n\
         [remote \"origin\"]\n\turl = {repo}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"
    )
}

/// Settings passed to every git command (command-line values beat every config file).
const SETTINGS: &[&str] = &[
    "core.autocrlf=false",
    "core.eol=lf",
    "core.symlinks=false",
    "core.filemode=false",
    "core.longpaths=true",
    "core.fsmonitor=false",
    "core.logAllRefUpdates=false",
    "gc.auto=0",
    "maintenance.auto=false",
    "advice.detachedHead=false",
    "protocol.allow=never",
    "protocol.https.allow=always",
    "credential.helper=",
    "filter.lfs.smudge=",
    "filter.lfs.process=",
    "filter.lfs.clean=",
    "filter.lfs.required=false",
    "pack.threads=1",
    "pack.compression=6",
    "pack.writeReverseIndex=false",
    "repack.writeBitmaps=false",
    "pack.window=10",
    "pack.depth=50",
    "transfer.fsckObjects=true",
];

/// A git program bound to a working directory.
struct Git<'a> {
    program: &'a str,
    cwd: &'a Path,
    global_config: &'a Path,
}

fn not_found(program: &str) -> DownloadError {
    DownloadError::Fetch(format!(
        "the `{program}` program was not found; install git to build this source"
    ))
}

impl Git<'_> {
    fn command(&self) -> Command {
        let mut c = Command::new(self.program);
        for s in SETTINGS.iter() {
            c.arg("-c").arg(s);
        }
        // Hooks come from an empty directory; the file protocol exists only for tests.
        c.arg("-c").arg(format!(
            "core.hooksPath={}",
            self.global_config.with_file_name("empty-hooks").display()
        ));
        if cfg!(test) {
            c.arg("-c").arg("protocol.file.allow=always");
        }
        // No inherited GIT_* variable may steer git (repository, config, object format, ...).
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().to_ascii_uppercase().starts_with("GIT_") {
                c.env_remove(&k);
            }
        }
        c.current_dir(self.cwd)
            .env("GIT_CONFIG_GLOBAL", self.global_config)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .env("GCM_INTERACTIVE", "never")
            .env("LC_ALL", "C");
        c
    }

    fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Output> {
        let mut c = self.command();
        c.args(args);
        let out = c.output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::Error::from(not_found(self.program))
            } else {
                anyhow::Error::from(e).context(format!("running `{}`", self.program))
            }
        })?;
        if !out.status.success() {
            let what: Vec<String> = args
                .iter()
                .map(|a| a.as_ref().to_string_lossy().into_owned())
                .collect();
            bail!(
                "`git {}` failed ({}): {}",
                what.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out)
    }

    fn stdout<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.run(args)?.stdout)
            .trim()
            .to_string())
    }
}

/// `git --version` of `program`; an absent program is a [`DownloadError::Fetch`].
pub fn version(program: &str) -> Result<String> {
    let out = Command::new(program)
        .arg("--version")
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::Error::from(not_found(program))
            } else {
                anyhow::Error::from(e).context(format!("running `{program} --version`"))
            }
        })?;
    ensure!(out.status.success(), "`{program} --version` failed");
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Produce the output of `spec` in `dir` (existing and empty). `scratch` is a directory for
/// temporary files (the download cache); nothing in it outlives the call except an empty git
/// configuration file.
pub fn materialise(
    program: &str,
    spec: &GitSpec,
    dir: &Path,
    scratch: &Path,
    id: &str,
) -> Result<()> {
    std::fs::create_dir_all(scratch)?;
    let global = scratch.join("empty.gitconfig");
    std::fs::write(&global, b"")?;
    std::fs::create_dir_all(scratch.join("empty-hooks"))?;
    let abs = std::path::absolute(dir).context("resolving the output directory")?;
    match spec.mode {
        GitMode::Clone => {
            let git = Git {
                program,
                cwd: &abs,
                global_config: &global,
            };
            init_repo(&git)?;
            fetch_commit(&git, spec, spec.depth)?;
            git.run(&["update-ref", "refs/heads/main", &spec.commit])?;
            git.run(&["symbolic-ref", "HEAD", "refs/heads/main"])?;
            git.run(&["checkout", "-q", "--force", "main"])?;
            let gitdir = abs.join(".git");
            remove_if_exists(&gitdir.join("index"))?;
            remove_keep_files(&gitdir.join("objects").join("pack"))?;
            git.run(&["pack-refs", "--all", "--prune"])?;
            git.run(&["repack", "-a", "-d", "-F", "-q"])?;
            let head = git.stdout(&["rev-parse", "HEAD"])?;
            ensure!(
                head == spec.commit,
                "source `{id}`: HEAD is {head}, expected {}",
                spec.commit
            );
            normalise_git_dir(&gitdir, &spec.repo)?;
        }
        GitMode::Export => {
            let tmp = scratch.join(format!("{id}-git-scratch"));
            remove_tree(&tmp)?;
            std::fs::create_dir_all(&tmp)?;
            let result = (|| -> Result<()> {
                let git = Git {
                    program,
                    cwd: &tmp,
                    global_config: &global,
                };
                init_repo(&git)?;
                fetch_commit(&git, spec, Some(1))?;
                let wt = format!("--work-tree={}", abs.display());
                git.run(&[
                    wt.as_str(),
                    "checkout",
                    "-q",
                    "--force",
                    spec.commit.as_str(),
                ])?;
                Ok(())
            })();
            remove_tree(&tmp)?;
            result?;
            ensure!(
                !abs.join(".git").exists(),
                "source `{id}`: export left a .git entry"
            );
        }
    }
    Ok(())
}

/// `git init` with a fixed object format and ref storage (the user's defaults must not matter).
/// `--ref-format` needs git 2.45; older versions only know the files format anyway.
fn init_repo(git: &Git<'_>) -> Result<()> {
    if git
        .run(&["init", "-q", "--object-format=sha1", "--ref-format=files"])
        .is_ok()
    {
        return Ok(());
    }
    git.run(&["init", "-q", "--object-format=sha1"])?;
    Ok(())
}

/// Every path of the commit's tree must pass the same portability rules as archive entries
/// (no case-insensitive duplicates, no `aux`, no trailing dot or space, ...), so every OS
/// produces the same tree or fails the same way.
fn check_tree(git: &Git<'_>, commit: &str) -> Result<()> {
    let out = git.run(&["ls-tree", "-r", "-z", "--name-only", commit])?;
    let paths = tree_paths(&out.stdout, commit)?;
    super::extract::check_listing(&paths)
        .with_context(|| format!("the tree of commit {commit} is not portable"))
}

/// Split `ls-tree -z` output into paths. A path that is not valid UTF-8 is an error: git
/// writes the raw bytes as the name, and Windows and Linux would not agree on it.
fn tree_paths(stdout: &[u8], commit: &str) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for p in stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let name = std::str::from_utf8(p).map_err(|_| {
            anyhow::anyhow!(
                "the tree of commit {commit} holds a path that is not valid UTF-8: {}",
                String::from_utf8_lossy(p)
            )
        })?;
        paths.push(name.to_string());
    }
    Ok(paths)
}

/// `git fetch` of one commit, then check that it arrived and that its tree is portable. Any git
/// failure other than a missing program is a hard error (a commit that vanished upstream must
/// not turn into a quiet skip).
fn fetch_commit(git: &Git<'_>, spec: &GitSpec, depth: Option<u32>) -> Result<()> {
    let mut args: Vec<String> = vec!["fetch".into(), "-q".into(), "--no-tags".into()];
    if let Some(d) = depth {
        args.push(format!("--depth={d}"));
    }
    args.push(spec.repo.clone());
    args.push(spec.commit.clone());
    git.run(&args)
        .with_context(|| format!("fetching commit {} of {}", spec.commit, spec.repo))?;
    let ty = git.stdout(&["cat-file", "-t", &spec.commit])?;
    ensure!(ty == "commit", "{} is a {ty}, not a commit", spec.commit);
    check_tree(git, &spec.commit)?;
    Ok(())
}

fn remove_if_exists(p: &Path) -> Result<()> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", p.display())),
    }
}

fn remove_tree(p: &Path) -> Result<()> {
    match std::fs::remove_dir_all(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", p.display())),
    }
}

/// `.keep` files make `repack -a` leave a pack alone; fetch can leave them.
fn remove_keep_files(pack_dir: &Path) -> Result<()> {
    let Ok(rd) = std::fs::read_dir(pack_dir) else {
        return Ok(());
    };
    for e in rd.flatten() {
        if e.path().extension().is_some_and(|x| x == "keep") {
            remove_if_exists(&e.path())?;
        }
    }
    Ok(())
}

/// Keep only the deterministic part of `.git` and write the fixed `config`.
fn normalise_git_dir(gitdir: &Path, repo: &str) -> Result<()> {
    fn keep(rel: &str) -> bool {
        matches!(rel, "HEAD" | "packed-refs" | "shallow")
            || rel
                .strip_prefix("objects/pack/pack-")
                .is_some_and(|r| r.ends_with(".pack") || r.ends_with(".idx"))
    }
    fn walk(root: &Path, dir: &Path) -> Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let path = e.path();
            if e.file_type()?.is_dir() {
                walk(root, &path)?;
                continue;
            }
            let rel: Vec<String> = path
                .strip_prefix(root)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if !keep(&rel.join("/")) {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
        }
        Ok(())
    }
    walk(gitdir, gitdir)?;
    // Drop directories that are now empty, except the ones a repository needs.
    fn prune(root: &Path, dir: &Path) -> Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            if e.file_type()?.is_dir() {
                prune(root, &e.path())?;
                let rel = e.path().strip_prefix(root).map(|p| {
                    p.components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/")
                });
                let needed = matches!(
                    rel.as_deref(),
                    Ok("objects" | "objects/pack" | "refs" | "refs/heads" | "refs/tags")
                );
                if !needed && std::fs::read_dir(e.path())?.next().is_none() {
                    std::fs::remove_dir(e.path())?;
                }
            }
        }
        Ok(())
    }
    prune(gitdir, gitdir)?;
    for d in ["objects/pack", "refs/heads", "refs/tags"] {
        std::fs::create_dir_all(gitdir.join(d))?;
    }
    std::fs::write(gitdir.join("config"), fixed_config(repo))?;
    Ok(())
}

/// Every regular file below `dir` as `(path with '/', bytes, blake3)`, sorted by path.
pub fn list_files(dir: &Path) -> Result<Vec<(String, u64, String)>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u64, String)>) -> Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let ty = e.file_type()?;
            let path = e.path();
            if ty.is_dir() {
                walk(root, &path, out)?;
            } else if ty.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                let (bytes, blake3) =
                    hash_file(&path).with_context(|| format!("hashing {}", path.display()))?;
                out.push((rel, bytes, blake3));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(out)
}

/// Build one git source: pin or verify the commit, run git, list the files.
pub fn build(
    ctx: &mut Ctx<'_>,
    source: &Source,
    spec: &GitSpec,
    dir: &Path,
) -> Result<Vec<ManifestFile>> {
    ctx.pin_commit(source, &spec.repo, &spec.commit)?;
    let program = ctx.git_program().to_string();
    let v = version(&program)?;
    ctx.note_tool("git", &v);
    let scratch: PathBuf = ctx.cache_dir().to_path_buf();
    materialise(&program, spec, dir, &scratch, &source.id)
        .with_context(|| format!("source `{}`", source.id))?;
    Ok(list_files(dir)?
        .into_iter()
        .map(|(rel, bytes, blake3)| ManifestFile {
            blake3,
            bytes,
            licence: source.licence.clone(),
            path: format!("{}/{}/{rel}", source.class, source.id),
            source: source.id.clone(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::build::{build as build_corpus, BuildOptions};
    use super::super::fetch::fake::{fast_retry, FakeFetcher};
    use super::super::registry::Profile;
    use super::*;

    fn have_git() -> bool {
        version("git").is_ok()
    }

    /// A local repository with three commits; returns `(repo path, [c1, c2, c3])`.
    fn make_repo(root: &Path) -> (PathBuf, Vec<String>) {
        let repo = root.join("origin");
        std::fs::create_dir_all(&repo).expect("mkdir");
        let global = root.join("empty.gitconfig");
        std::fs::write(&global, b"").expect("cfg");
        let git = Git {
            program: "git",
            cwd: &repo,
            global_config: &global,
        };
        let id = ["-c", "user.name=T", "-c", "user.email=t@example.org"];
        git.run(&["init", "-q"]).expect("init");
        git.run(&["config", "uploadpack.allowAnySHA1InWant", "true"])
            .expect("cfg");
        let mut commits = Vec::new();
        for n in 1..=3 {
            std::fs::create_dir_all(repo.join("src/Sub")).expect("dirs");
            std::fs::write(repo.join("README.md"), format!("readme {n}\n")).expect("w");
            std::fs::write(
                repo.join("src/Sub/mod.c"),
                format!("int v = {n};\r\nline\r\n"),
            )
            .expect("w");
            std::fs::write(
                repo.join(format!("file{n}.txt")),
                vec![b'a' + n as u8; 3000],
            )
            .expect("w");
            git.run(&["add", "-A"]).expect("add");
            let mut args: Vec<&str> = id.to_vec();
            args.extend(["commit", "-q", "-m", "c"]);
            git.run(&args).expect("commit");
            commits.push(git.stdout(&["rev-parse", "HEAD"]).expect("rev"));
        }
        (repo, commits)
    }

    fn file_url(p: &Path) -> String {
        let s = p.to_string_lossy().replace('\\', "/");
        if s.starts_with('/') {
            format!("file://{s}")
        } else {
            format!("file:///{s}")
        }
    }

    fn spec(repo: &Path, commit: &str, mode: GitMode, depth: Option<u32>) -> GitSpec {
        GitSpec {
            repo: file_url(repo),
            commit: commit.to_string(),
            mode,
            depth,
        }
    }

    fn run(root: &Path, name: &str, spec: &GitSpec) -> (PathBuf, Vec<(String, u64, String)>) {
        let out = root.join(name);
        std::fs::create_dir_all(&out).expect("out");
        materialise("git", spec, &out, &root.join("scratch"), "t").expect("materialise");
        let files = list_files(&out).expect("list");
        (out, files)
    }

    #[test]
    fn clone_is_reproducible_normalised_and_shallow() {
        if !have_git() {
            eprintln!("git not installed: skipping");
            return;
        }
        let dir = tempfile::tempdir().expect("tmp");
        let (repo, commits) = make_repo(dir.path());
        let s = spec(&repo, &commits[2], GitMode::Clone, Some(2));
        let (out1, f1) = run(dir.path(), "a", &s);
        let (_out2, f2) = run(dir.path(), "b", &s);
        assert_eq!(f1, f2, "two from-scratch builds are byte-identical");
        let names: Vec<&str> = f1.iter().map(|f| f.0.as_str()).collect();
        for want in [
            "README.md",
            "src/Sub/mod.c",
            "file3.txt",
            ".git/HEAD",
            ".git/config",
            ".git/packed-refs",
            ".git/shallow",
        ] {
            assert!(names.contains(&want), "{want} in {names:?}");
        }
        let git_files: Vec<&&str> = names.iter().filter(|n| n.starts_with(".git/")).collect();
        let packs = git_files.iter().filter(|n| n.ends_with(".pack")).count();
        assert_eq!(packs, 1, "one pack: {git_files:?}");
        for banned in [
            "hooks",
            "info/",
            "logs",
            "FETCH_HEAD",
            "ORIG_HEAD",
            "index",
            "description",
            ".keep",
            ".rev",
        ] {
            assert!(
                !git_files.iter().any(|n| n.contains(banned)),
                "{banned} must not remain: {git_files:?}"
            );
        }
        // Working tree is exact: LF/CRLF untouched, newest commit.
        assert_eq!(
            std::fs::read(out1.join("README.md")).expect("r"),
            b"readme 3\n"
        );
        assert_eq!(
            std::fs::read(out1.join("src/Sub/mod.c")).expect("r"),
            b"int v = 3;\r\nline\r\n"
        );
        // The repository is usable: depth 2 history, head at the pinned commit.
        let global = dir.path().join("empty.gitconfig");
        let git = Git {
            program: "git",
            cwd: &out1,
            global_config: &global,
        };
        assert_eq!(
            git.stdout(&["rev-parse", "HEAD"]).expect("head"),
            commits[2]
        );
        assert_eq!(
            git.stdout(&["rev-list", "--count", "HEAD"]).expect("n"),
            "2"
        );
        // Full history when no depth is given.
        let (out3, _) = run(
            dir.path(),
            "c",
            &spec(&repo, &commits[2], GitMode::Clone, None),
        );
        let git3 = Git {
            program: "git",
            cwd: &out3,
            global_config: &global,
        };
        assert_eq!(
            git3.stdout(&["rev-list", "--count", "HEAD"]).expect("n"),
            "3"
        );
        assert!(!out3.join(".git/shallow").exists());
    }

    #[test]
    fn export_has_the_tree_of_an_old_commit_and_no_git_dir() {
        if !have_git() {
            eprintln!("git not installed: skipping");
            return;
        }
        let dir = tempfile::tempdir().expect("tmp");
        let (repo, commits) = make_repo(dir.path());
        let s = spec(&repo, &commits[0], GitMode::Export, None);
        let (out1, f1) = run(dir.path(), "a", &s);
        let (_o2, f2) = run(dir.path(), "b", &s);
        assert_eq!(f1, f2, "two exports are byte-identical");
        assert!(!out1.join(".git").exists());
        assert_eq!(
            std::fs::read(out1.join("README.md")).expect("r"),
            b"readme 1\n"
        );
        assert!(out1.join("file1.txt").is_file() && !out1.join("file2.txt").exists());
        assert!(
            !dir.path().join("scratch/t-git-scratch").exists(),
            "scratch removed"
        );
    }

    #[test]
    fn missing_git_is_a_fetch_failure_and_skips_an_optional_source() {
        let dir = tempfile::tempdir().expect("tmp");
        let spec = GitSpec {
            repo: "https://example.org/r.git".into(),
            commit: "a".repeat(40),
            mode: GitMode::Export,
            depth: None,
        };
        let err = materialise(
            "no-such-git-program-xyz",
            &spec,
            dir.path(),
            dir.path(),
            "t",
        )
        .expect_err("missing");
        assert!(matches!(
            err.downcast_ref::<DownloadError>(),
            Some(DownloadError::Fetch(m)) if m.contains("not found")
        ));
        // Through a whole build.
        let root = dir.path();
        std::fs::write(
            root.join("sources.toml"),
            format!(
                "[[source]]\nid = \"g\"\nclass = \"source-git\"\nlicence = \"MIT\"\norigin = \"t\"\n\
                 profiles = [\"small\"]\noptional = true\nkind = \"git-repo\"\n\
                 repo = \"https://example.org/r.git\"\ncommit = \"{}\"\nmode = \"export\"\n\n\
                 [[source]]\nid = \"f\"\nclass = \"other\"\nlicence = \"MIT\"\norigin = \"t\"\n\
                 profiles = [\"small\"]\nkind = \"file\"\nurl = \"https://example.org/f.bin\"\n",
                "a".repeat(40)
            ),
        )
        .expect("w");
        let fetcher = FakeFetcher::with("https://example.org/f.bin", b"data".to_vec());
        let opts = BuildOptions {
            profile: Profile::Small,
            out: root.join("out"),
            cache: root.join("cache"),
            only: vec![],
            update_lock: true,
            sources_path: root.join("sources.toml"),
            lock_path: root.join("corpus.lock"),
            retry: fast_retry(),
            git_program: Some("no-such-git-program-xyz".into()),
            allow_unavailable: false,
        };
        let report = build_corpus(&opts, &fetcher).expect("build");
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].source, "g");
        assert!(report.skipped[0].reason.contains("not found"));
        assert_eq!(report.files, 1);
    }

    #[test]
    fn registry_validation_and_lock_commit_check() {
        use super::super::registry::Registry;
        let good = "[[source]]\nid = \"g\"\nclass = \"c\"\nlicence = \"MIT\"\norigin = \"t\"\n\
                    profiles = [\"small\"]\nkind = \"git-repo\"\nrepo = \"https://example.org/r.git\"\n\
                    commit = \"0123456789abcdef0123456789abcdef01234567\"\ndepth = 200\n";
        Registry::parse(good).expect("valid");
        for bad in [
            good.replace("0123456789abcdef0123456789abcdef01234567", "abc"),
            good.replace("depth = 200", "depth = 0"),
            good.replace("depth = 200", "mode = \"export\"\ndepth = 5"),
            good.replace("https://", "http://"),
            good.replace("depth = 200", "depth = 200\nbogus = 1"),
        ] {
            assert!(Registry::parse(&bad).is_err(), "{bad}");
        }
    }

    /// A repository whose only commit holds the given `(name, content)` entries, built with
    /// plumbing so that names a Windows checkout could not create are possible.
    fn plumbing_repo(root: &Path, entries: &[(&str, &str)]) -> (PathBuf, String) {
        let repo = root.join("plumb");
        std::fs::create_dir_all(&repo).expect("mkdir");
        let global = root.join("empty.gitconfig");
        std::fs::write(&global, b"").expect("cfg");
        let git = Git {
            program: "git",
            cwd: &repo,
            global_config: &global,
        };
        git.run(&["init", "-q"]).expect("init");
        git.run(&["config", "uploadpack.allowAnySHA1InWant", "true"])
            .expect("cfg");
        for (name, content) in entries {
            std::fs::write(repo.join("blob.tmp"), content).expect("w");
            let blob = git
                .stdout(&["hash-object", "-w", "blob.tmp"])
                .expect("hash-object");
            git.run(&[
                "-c",
                "core.protectNTFS=false",
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},{name}"),
            ])
            .expect("update-index");
        }
        let tree = git.stdout(&["write-tree"]).expect("write-tree");
        let commit = git
            .stdout(&[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.org",
                "commit-tree",
                &tree,
                "-m",
                "c",
            ])
            .expect("commit-tree");
        (repo, commit)
    }

    #[test]
    fn tree_names_must_be_portable_and_a_missing_commit_is_a_hard_error() {
        if !have_git() {
            return;
        }
        let dir = tempfile::tempdir().expect("tmp");
        for (entries, what) in [
            (vec![("README", "a"), ("readme", "b")], "case"),
            (vec![("aux.h", "a")], "reserved"),
            (vec![("trail.", "a")], "dot"),
        ] {
            let (repo, commit) = plumbing_repo(dir.path(), &entries);
            let s = spec(&repo, &commit, GitMode::Export, None);
            let out = dir.path().join(format!("out-{what}"));
            std::fs::create_dir_all(&out).expect("out");
            let err =
                materialise("git", &s, &out, &dir.path().join("scratch"), "t").expect_err(what);
            assert!(
                format!("{err:#}").contains("not portable"),
                "{what}: {err:#}"
            );
            assert!(err.downcast_ref::<DownloadError>().is_none());
            std::fs::remove_dir_all(dir.path().join("plumb")).expect("rm");
        }
        // A commit the origin does not have is a hard error, not a skip.
        let (repo, _) = plumbing_repo(dir.path(), &[("ok.txt", "a")]);
        let s = spec(&repo, &"b".repeat(40), GitMode::Export, None);
        let out = dir.path().join("out-missing");
        std::fs::create_dir_all(&out).expect("out");
        let err = materialise("git", &s, &out, &dir.path().join("scratch"), "t")
            .expect_err("missing commit");
        assert!(err.downcast_ref::<DownloadError>().is_none(), "{err:#}");
    }

    #[test]
    fn lfs_attributes_do_not_run_filters_and_the_tree_is_exact() {
        if !have_git() {
            return;
        }
        let dir = tempfile::tempdir().expect("tmp");
        let (repo, commit) = plumbing_repo(
            dir.path(),
            &[
                (
                    ".gitattributes",
                    "* filter=lfs diff=lfs merge=lfs -text
",
                ),
                (
                    "big.bin",
                    "version https://git-lfs.github.com/spec/v1
",
                ),
            ],
        );
        let (out, files) = run(
            dir.path(),
            "a",
            &spec(&repo, &commit, GitMode::Export, None),
        );
        assert_eq!(files.len(), 2);
        assert_eq!(
            std::fs::read_to_string(out.join("big.bin")).expect("r"),
            "version https://git-lfs.github.com/spec/v1
",
            "the pointer text is kept as is"
        );
    }

    #[test]
    fn non_utf8_tree_paths_are_rejected() {
        let ok = tree_paths(b"a/b.txt\0c\xc3\xa9.txt\0", "c").expect("valid");
        assert_eq!(ok, ["a/b.txt", "c\u{e9}.txt"]);
        let err = tree_paths(b"fine.txt\0bad\xff\xfe.txt\0", "c").expect_err("invalid");
        assert!(format!("{err:#}").contains("not valid UTF-8"), "{err:#}");
    }
}
