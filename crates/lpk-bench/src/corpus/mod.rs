//! Corpus builder (PLAN P0-2, spec in `docs/CORPUS.md`).
//!
//! # Commands
//!
//! ```text
//! lpk-bench corpus build --profile <small|full> [--out DIR] [--cache DIR] [--only a,b] [--update-lock]
//! lpk-bench corpus scan  --private DIR --out DIR      (see [`scan`] and [`classify`])
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
//! | `optional` | `true`: a failed download is listed under `skipped` in `build-info.json`;   |
//! |            | lock mismatches and missing pins stay fatal                                |
//! | `inputs`   | classes this source derives from; each must have a source earlier in the   |
//! |            | file (see "`--only` and derived classes")                                  |
//! | `kind`     | selects the kind-specific keys below                                       |
//!
//! Differences between profiles are expressed as separate sources (different ids), not as
//! overrides. Unknown keys and unknown kinds are errors (typos must not pass silently).
//!
//! Kinds implemented:
//! * `file`: `url`, optional `filename` (default: last URL segment). One URL becomes one file.
//! * `archive`: `url`, optional `format` (`zip` | `tar.gz` | `7z` | `gz`, default guessed from the
//!   URL; `7z` also reads a self-extracting `.7z.exe`; `gz` is one stream decompressed to a
//!   single file named like the URL without `.gz`), `include`/`exclude` glob lists (matched on
//!   the `/`-separated path after `strip_components`; `*` stays within a directory, `**`
//!   crosses), `max_files`, `max_bytes`, `strip_components`, and `truncate_files = N`: keep only
//!   the first N bytes of every larger file, cut after the last line break before N (the
//!   manifest lists the truncated size).
//! * `files`: `files = [{ url, path?, licence?, attribution? }, ...]`, one source listing many
//!   URLs with output names (default: last URL segment), each pinned separately. It uses the
//!   listed-pin path: the whole listing is validated up front (portable names, no
//!   case-insensitive duplicates, no file/directory clash), and a normal build requires the
//!   registry's list to equal the pins in the lock.
//!
//! Top-level `[[host]]` tables set politeness per host (`name`, `min_interval_ms`,
//! `max_mbit_per_s`); see "Downloader".
//!
//! Archive selection (see [`extract`]): *every* entry is validated first, whether or not a
//! glob would select it, and any of these fails the whole extraction on every OS: absolute
//! path, `..`, drive prefix or `:`, symlink/hardlink/device entry, Windows-reserved or
//! non-portable name (`NUL`, trailing dot or space, `<>"|?*`, control characters),
//! case-insensitive duplicate, or a file that shares a name with another entry's directory.
//! The survivors are filtered by the globs, sorted by path bytes and capped: `max_files` keeps
//! the first N; `max_bytes` keeps files while the running total of *declared* sizes fits and
//! stops at the first file that would exceed it. The same archive always yields the same
//! files.
//!
//! # Output directory
//!
//! Files go to `<out>/<class>/<source-id>/<path>`, plus `manifest.json` (deterministic: sorted
//! keys, `/` paths, no timestamps) and `build-info.json` (`built_at`, tool version, host,
//! manifest BLAKE3, `skipped`).
//! * `<out>` is owned by the tool through a `.lpk-corpus` marker written on first use. A
//!   non-empty directory without the marker is an error and is never modified. The cache must
//!   not live inside `<out>`.
//! * Every build first deletes `manifest*.json` and `build-info*.json`, so a failed build
//!   never leaves a stale manifest.
//! * After a successful *full* build, every file below `<out>` that the manifest does not list
//!   (output of removed or renamed sources and classes) is deleted, empty directories too, and
//!   the paths are reported.
//! * `--only a,b` builds just those classes. It is a pinning and debugging aid: it writes
//!   `manifest.partial.json` / `build-info.partial.json`, never `manifest.json`, and removes
//!   nothing else. A directory without `manifest.json` is not a usable corpus.
//!
//! # `--only` and derived classes
//!
//! A source with `inputs` is derived from other classes. `--only <class>` for such a class is
//! an error naming the missing input classes unless each input is also selected (and so built
//! earlier in the same run) or `<out>/<input>` already holds files. This is checked before
//! anything is modified. Derivation steps read their inputs below [`build::Ctx::out`].
//!
//! # Lock semantics (`bench/corpus.lock`)
//!
//! JSON, committed, format version 2 (version 1 files are still read). Per profile a sorted
//! list of pins keyed by `(source, url)`; a pin has `bytes` and `blake3` for a download, or
//! `commit` for a git pin, and optional `path`, `licence`, `attribution` and a free-form
//! `extra` map for list-type sources (see [`lock::LockEntry`]). Regenerating without upstream
//! changes gives identical bytes.
//! * Normal build: every URL of a static source must have a lock entry (checked before any
//!   network traffic) and the downloaded bytes must match it; either failure is a hard error
//!   naming the source and the re-pin command. A cache file is reused only after re-hashing
//!   it against the lock; otherwise it is downloaded again. Pins of built sources that the
//!   build did not use are reported.
//! * `--update-lock`: downloads everything selected, ignoring the cache (there is nothing to
//!   verify it against), records the result and rewrites the entries of those sources; other
//!   sources' entries are kept. With `--only` it pins just those classes.
//! * List-type sources (URLs resolved from an API): the lock is the listing. Only
//!   `--update-lock` may call the API; a normal build makes no API call and builds exactly the
//!   source's pinned entries ([`build::Ctx::listed_pins`]). Per-file licence and author live
//!   in the pin; the manifest carries the per-file licence when the pin has one and the
//!   source-level string otherwise.
//!
//! # Downloader
//!
//! In-process HTTPS (`reqwest` blocking + native TLS: Schannel on Windows, system OpenSSL on
//! Linux, so no bundled root store; Linux builds need `libssl-dev` and `pkg-config`).
//! HTTPS only, including redirects. Strictly sequential, hence one request at a time per
//! host. The 90-second timeout is an idle timeout (headers and each body read). 429, 5xx,
//! timeouts and mid-body failures are retried with exponential backoff (honouring
//! `Retry-After`, capped). Bytes stream into `<cache>/<id>-<urlhash>.part` (the cache file
//! name plus `.part`), are hashed on the way, and are renamed into place only after
//! verification. A retry after a mid-body failure resumes with a `Range` request when the
//! server honours it and restarts from zero when it does not.
//!
//! # Adding a source kind
//!
//! 1. Add a variant wrapping a new struct to [`registry::SourceSpec`] (the struct carries
//!    `#[serde(deny_unknown_fields)]`; the variant name in kebab-case is the `kind`), extend
//!    `SourceSpec::static_urls` (URLs known without network; they are pre-flight checked
//!    against the lock) and the validation in `Registry::validate`.
//! 2. Add a match arm in [`build::build_source`]. Static kinds obtain artifacts through
//!    `Ctx::artifact(source, url)` (lock verification, pin recording). List kinds: when
//!    `Ctx::update_lock()` resolve the API into `ListedFile`s, otherwise take them from
//!    `Ctx::listed_pins`; fetch each with `Ctx::fetch_listed`. Derived kinds read files below
//!    `Ctx::out()` (and `Ctx::produced` for this run's files). Write files into the directory
//!    given and return one `ManifestFile` per file.
//! 3. New archive formats: add a variant to `registry::ArchiveFormat` (+ `guess`) and a
//!    branch in `extract::extract` that lists entries through the private `Lister` (which
//!    applies `sanitize_path` and rejects duplicates; also reject link/special entries),
//!    then reuses `pick` and `write_entry`, so the safety and cap rules stay shared.
//! 4. Tests: serve bytes from `fetch::fake::FakeFetcher`; never hit the network in tests.

pub mod build;
pub mod classify;
pub mod extract;
pub mod fetch;
pub mod lock;
pub mod manifest;
pub mod registry;
pub mod scan;

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
    /// Only build these classes (comma separated). A partial build: writes
    /// manifest.partial.json (never manifest.json), removes nothing, and a derived class needs
    /// its input classes selected too or already present in the output directory
    #[arg(long, value_delimiter = ',')]
    pub only: Vec<String>,
    /// Re-download the selected sources (the cache is ignored), record their pins and rewrite
    /// the lock instead of verifying against it; other sources' pins are kept
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
                    for p in &r.removed {
                        println!("removed unlisted file: {p}");
                    }
                    for p in &r.unused_pins {
                        println!("unused lock pin: {p}");
                    }
                    if r.partial {
                        println!("partial build (--only): not a usable corpus, no manifest.json");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        CorpusCommand::Scan(s) => scan::run(&s),
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
