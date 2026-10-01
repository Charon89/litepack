//! Corpus builder (PLAN P0-2, spec in `docs/CORPUS.md`).
//!
//! # Commands
//!
//! ```text
//! lpk-bench corpus build --profile <small|full> [--out DIR] [--cache DIR] [--only a,b] [--update-lock]
//! lpk-bench corpus scan  --private DIR --out DIR      (stub until sub-task P0-2d)
//! ```
//!
//! Defaults are relative to the working directory, which is expected to be the repository root:
//! registry `bench/corpus-sources.toml`, lock `bench/corpus.lock`, out `bench/corpus/<profile>`,
//! cache `bench/corpus/.cache` (the last two are git-ignored). `--sources` and `--lock` override
//! the first two.
//!
//! # Registry format (`bench/corpus-sources.toml`)
//!
//! A list of `[[source]]` tables, built in file order. Common keys (all required except
//! `optional`):
//!
//! | key        | meaning                                                                    |
//! |------------|----------------------------------------------------------------------------|
//! | `id`       | unique, `[a-z0-9._-]+`; names the output dir and the lock entries          |
//! | `class`    | corpus class (`[a-z0-9._-]+`); the key under `classes` in the manifest     |
//! | `licence`  | SPDX id or short statement; copied to every manifest file entry            |
//! | `origin`   | human description of where the data comes from                             |
//! | `profiles` | `["small"]`, `["full"]` or both                                             |
//! | `optional` | `true`: a failed download is listed under `skipped` in `build-info.json`   |
//! | `kind`     | selects the kind-specific keys below                                       |
//!
//! Differences between profiles are expressed as separate sources (different ids), not as
//! overrides. Unknown keys and unknown kinds are errors (typos must not pass silently).
//!
//! Kinds implemented:
//! * `file`: `url`, optional `filename` (default: last URL segment). One URL becomes one file.
//! * `archive`: `url`, optional `format` (`zip` | `tar.gz`, default guessed from the URL),
//!   `include`/`exclude` glob lists (matched on the `/`-separated path after
//!   `strip_components`; `*` stays within a directory, `**` crosses), `max_files`,
//!   `max_bytes`, `strip_components`. Caps apply in sorted-path order (see [`extract`]).
//!
//! Output: `<out>/<class>/<source-id>/<path>`, plus `<out>/manifest.json` (deterministic:
//! sorted keys, `/` paths, no timestamps) and `<out>/build-info.json` (`built_at`, tool
//! version, host, manifest BLAKE3, `skipped`). `--only` builds just those classes and the
//! manifest then covers only them.
//!
//! # Lock semantics (`bench/corpus.lock`)
//!
//! JSON, committed. Per profile a sorted list of `{blake3, bytes, source, url}` for every
//! downloaded artifact (the download, not the extracted files). Regenerating without upstream
//! changes gives identical bytes.
//! * Normal build: every URL must have a lock entry (checked before any network traffic) and
//!   the downloaded bytes must match it; either failure is a hard error naming the source and
//!   the `--update-lock` command. A cache file is reused only after re-hashing it against the
//!   lock; otherwise it is downloaded again.
//! * `--update-lock`: downloads everything selected (the cache is not trusted), records the
//!   result and rewrites the entries of those sources; other sources' entries are kept.
//!
//! # Downloader
//!
//! In-process HTTPS (`reqwest` blocking + native TLS: Schannel on Windows, system OpenSSL on
//! Linux, so no bundled root store). Strictly sequential, hence one request at a time per
//! host. 429, 5xx, timeouts and mid-body failures are retried with exponential backoff
//! (honouring `Retry-After`, capped). Bytes stream into `<cache>/<id>-<urlhash>.part`, are
//! hashed on the way, and are renamed into place only after verification.
//!
//! # Adding a source kind
//!
//! 1. Add a variant wrapping a new struct to [`registry::SourceSpec`] (the struct carries
//!    `#[serde(deny_unknown_fields)]`; the variant name in kebab-case is the `kind`), extend
//!    `SourceSpec::static_urls` (URLs known without network; they are pre-flight checked
//!    against the lock) and the validation in `Registry::validate`.
//! 2. Add a match arm in [`build::build_source`] that obtains artifacts through
//!    `Ctx::artifact(source, url)` (this applies lock verification and records pins; call it
//!    once per URL, including URLs resolved from an API at build time) and writes files into
//!    the directory it is given, returning one `ManifestFile` per file. Derived kinds read
//!    `Ctx::produced` (all files of earlier sources, in registry order) instead of downloading.
//! 3. New archive formats: add a variant to `registry::ArchiveFormat` (+ `guess`) and a
//!    branch in `extract::extract` that lists entries through the private `Lister` (which
//!    applies `sanitize_path` and rejects duplicates; also reject link/special entries),
//!    then reuses `pick` and `write_entry`, so the safety and cap rules stay shared.
//! 4. Tests: serve bytes from `fetch::fake::FakeFetcher`; never hit the network in tests.

pub mod build;
pub mod extract;
pub mod fetch;
pub mod lock;
pub mod manifest;
pub mod registry;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};

use self::build::BuildOptions;
use self::fetch::{HttpFetcher, RetryPolicy};
use self::registry::Profile;

/// Arguments of `lpk-bench corpus`.
#[derive(Debug, Args)]
pub struct CorpusArgs {
    #[command(subcommand)]
    pub command: CorpusCommand,
}

#[derive(Debug, Subcommand)]
pub enum CorpusCommand {
    /// Download and extract the public corpus, writing manifest.json and build-info.json
    Build(BuildArgs),
    /// Hash a private folder into a manifest without copying anything (PLAN P0-2d)
    Scan(ScanArgs),
}

#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Corpus profile
    #[arg(long, value_enum)]
    pub profile: Profile,
    /// Output directory [default: bench/corpus/<profile>]
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Download cache directory
    #[arg(long, default_value = "bench/corpus/.cache")]
    pub cache: PathBuf,
    /// Only build these classes (comma separated)
    #[arg(long, value_delimiter = ',')]
    pub only: Vec<String>,
    /// Download, record and rewrite the lock instead of verifying against it
    #[arg(long)]
    pub update_lock: bool,
    /// Source registry file
    #[arg(long, default_value = "bench/corpus-sources.toml")]
    pub sources: PathBuf,
    /// Lock file (committed)
    #[arg(long, default_value = "bench/corpus.lock")]
    pub lock: PathBuf,
}

#[derive(Debug, Args)]
pub struct ScanArgs {
    /// Folder to scan (read-only)
    #[arg(long)]
    pub private: PathBuf,
    /// Directory for the manifest
    #[arg(long)]
    pub out: PathBuf,
}

impl BuildArgs {
    /// Translate CLI arguments into build options.
    pub fn options(&self) -> BuildOptions {
        BuildOptions {
            profile: self.profile,
            out: self
                .out
                .clone()
                .unwrap_or_else(|| PathBuf::from("bench/corpus").join(self.profile.name())),
            cache: self.cache.clone(),
            only: self.only.clone(),
            update_lock: self.update_lock,
            sources_path: self.sources.clone(),
            lock_path: self.lock.clone(),
            retry: RetryPolicy::default(),
        }
    }
}

/// Entry point for `lpk-bench corpus ...`.
pub fn run(args: CorpusArgs) -> ExitCode {
    match args.command {
        CorpusCommand::Build(b) => {
            let result = HttpFetcher::new().and_then(|f| build::build(&b.options(), &f));
            match result {
                Ok(r) => {
                    println!(
                        "{} files, {} bytes; manifest {} (blake3 {})",
                        r.files,
                        r.bytes_total,
                        r.manifest_path.display(),
                        r.manifest_blake3
                    );
                    for s in &r.skipped {
                        println!("skipped {}: {}", s.source, s.reason);
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        CorpusCommand::Scan(_) => {
            eprintln!(
                "error: `lpk-bench corpus scan` is not implemented yet (PLAN task P0-2d); \
                 no manifest was written"
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_out_follows_profile() {
        let a = BuildArgs {
            profile: Profile::Full,
            out: None,
            cache: PathBuf::from("c"),
            only: vec![],
            update_lock: false,
            sources: PathBuf::from("s"),
            lock: PathBuf::from("l"),
        };
        assert_eq!(a.options().out, PathBuf::from("bench/corpus").join("full"));
    }
}
