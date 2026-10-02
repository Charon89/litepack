//! Corpus builder (PLAN P0-2, spec in `docs/CORPUS.md`).
//!
//! # Commands
//!
//! ```text
//! lpk-bench corpus build --profile <small|full> [--out DIR] [--cache DIR] [--only a,b] [--update-lock [--repin a,b|all]] [--allow-unavailable]
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
//!   manifest lists the truncated size), `skip_links = true` (`tar.gz`: skip symlink and
//!   hardlink entries instead of failing).
//! * `files`: `files = [{ url, path?, licence?, attribution? }, ...]`, one source listing many
//!   URLs with output names (default: last URL segment), each pinned separately. It uses the
//!   listed-pin path: the whole listing is validated up front (portable names, no
//!   case-insensitive duplicates, no file/directory clash), and a normal build requires the
//!   registry's list to equal the pins in the lock.
//! * `commons-photos`: `category`, `count`, `min_bytes`, `max_bytes`. A list kind (see "Lock
//!   semantics"): under `--update-lock` it pages through the Wikimedia Commons API and keeps, in
//!   API order, the first `count` camera JPEGs (Exif Make and Model present) licensed CC0,
//!   CC BY or CC BY-SA (no NonCommercial or NoDerivs; names mapped onto a fixed SPDX table) whose
//!   size lies in the window. Names are
//!   an ASCII slug plus a short title hash. Details in [`commons`].
//! * `arxiv-papers`: `from`, `until`, optional `set`, `count`. A list kind: under `--update-lock`
//!   it lists the OAI-PMH window, keeps CC BY 4.0 records, sorts by identifier and takes the
//!   first `count`, each at its newest listed version. Details in [`arxiv`].
//! * `git-repo`: `repo`, `commit` (40 hex), `mode` (`clone`, default, or `export`), `depth`
//!   (`clone` only; absent = full history). Runs the external `git` program (name injectable
//!   through `BuildOptions::git_program`); without it the source is skipped when `optional`.
//!   `clone` yields the working tree plus a normalised `.git`, `export` the working tree only.
//!   The lock pins the commit. The git version goes to `tools` in `build-info.json`.
//!   Details and the determinism rules in [`gitsrc`].
//!
//! * Derived kinds (no download, nothing pinned in the lock; they need `inputs`, except
//!   `encrypted-random`, and take an optional `from = [source ids]`): `encrypted-random`
//!   (`bytes`), `small-files` (`count`, `max_file_bytes`, `json_percent`, `csv_percent`),
//!   `flac-to-wav`, `png-to-jpeg` (`baseline_quality`, `progressive_quality`), `jpeg-crop`
//!   (`every`, `crop_permille`, `quality`), `photo-convert` (`max_files`, `max_bytes`),
//!   `built-zips` (`encoders`, `bundles`) and `ffmpeg-encode` (`clip_seconds`, `codecs`; runs the
//!   external `ffmpeg`, name injectable through `BuildOptions::ffmpeg_program`, version in
//!   `tools`). An input class may be the source's own class when an earlier source has it.
//!   Details, determinism rules and the skip behaviour in [`derive`].
//!
//! Top-level `[[host]]` tables set politeness per host (`name`, `min_interval_ms`,
//! `max_mbit_per_s`, `max_attempts`, `max_delay_ms`); see "Downloader".
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
//! * `build-info.json` also holds `accounting` (per source: files extraction produced versus files
//!   that still exist with the right size when the manifest is written; the missing ones, e.g.
//!   quarantined by antivirus, or changed in place (the bytes on disk are re-hashed), are
//!   listed, left out of the manifest and do not fail the build)
//!   and `summary` (files and bytes per class and in total), which the build also prints.
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
//! * `--update-lock`: pins the selected sources and rewrites their entries; other sources'
//!   entries are kept. With `--only` it pins just those classes. It is *resumable* (a full
//!   profile is many gigabytes at the hosts' rate limits): an artifact that is already pinned
//!   for this profile (with a hash) is checked against its pin, so a cached file that matches is
//!   reused without any request and only unpinned or mismatching artifacts are fetched (a
//!   download of an already pinned URL must match the pin, so an existing pin never changes
//!   silently). The lock is written atomically (temporary file, then rename) after every
//!   source and, while a list source is pinned, every 16 files or 10 seconds and when a source fails, so an
//!   interrupted run keeps its progress. Run the same command again to continue.
//! * `--repin <source-id,...|all>` (with `--update-lock`): forget the pins of those sources
//!   (the lock is saved at once) and resolve and download them again, ignoring the cache. After
//!   an interruption continue with plain `--update-lock`; repeating `--repin` starts over.
//! * List-type sources (URLs resolved from an API): the lock is the listing. Only
//!   `--update-lock` may call the API; a normal build makes no API call and builds exactly the
//!   source's pinned entries ([`build::Ctx::listed_pins`]). Per-file licence and author live
//!   in the pin; the manifest carries the per-file licence when the pin has one and the
//!   source-level string otherwise. For the API-resolved kinds the resolved listing is saved
//!   to the lock before the first download: entries carry the URL and the API's metadata
//!   (path, licence, attribution, sha1, timestamp) but no `bytes`/`blake3`. A resumed
//!   `--update-lock` continues from a saved listing of the registry's `count` without calling
//!   the API (a listing of another size is resolved again; changing other settings of such a
//!   source, e.g. the size window, needs `--repin`). A normal build refuses a listing with
//!   unhashed entries, naming the source and saying the pin run was not finished.
//! * A listed file that is gone (404/410) or no longer matches the API's sha1 or its pin makes
//!   a pin run list that source again in the same run (at most three times): hashed pins whose
//!   URL and sha1 reappear are kept, a cached file with the listed sha1 is taken without a
//!   download, and only the rest is fetched.
//! * Every pin written for an API-listed source carries `extra.spec`, a fingerprint of the
//!   registry entry's selection settings plus a version number of the selection rules
//!   (`LIST_RULES_VERSION` in `build.rs`), and commons pins carry `extra.size`, the API size.
//!   `--update-lock` refuses to continue a listing whose fingerprint differs, naming
//!   `--repin <id>`; a pin without `spec` (made earlier) is accepted, and a normal build never
//!   looks at it, so existing pins stay valid and the lock only gains the keys when a source is
//!   listed again.
//! * `--update-lock --list-only` resolves and saves the listings of the API-listed sources and
//!   prints files and API-size sums per source without downloading anything.
//! * An `--update-lock` run holds `<lock>.run` (created with create-new, removed at exit) so two
//!   pin runs cannot overwrite each other's pins; a second run fails naming the file, which is
//!   to be deleted by hand if a run was killed. The lock itself is written through a file
//!   handle, synced, and renamed over the old one, retrying briefly on sharing violations.
//!
//! * Listed files of the API-resolved kinds (`commons-photos`, `arxiv-papers`; never a static
//!   `files` source): two conditions make a file *unavailable* in a normal build, an HTTP 404 or
//!   410, or bytes that no longer match the pin. By default the build fails, names each such file
//!   and the two ways forward. With `--allow-unavailable` it continues, leaves them out of the
//!   manifest, records them under `unavailable` in `build-info.json` and prints a warning. Timeouts,
//!   5xx, connection failures and a refused `Retry-After` stay fatal. Under `--update-lock`
//!   every failure is fatal. Re-pin procedure: the pin names
//!   the URL of the file as it was when listed. If a Commons file was re-uploaded, its pin goes
//!   stale (no archive-URL fallback: the archive name of an old version carries the time it was
//!   replaced, not the pinned upload time, so it cannot be computed from the lock). To refresh,
//!   run `--update-lock` for the source; this re-runs the API listing and may change several
//!   files, so review the lock diff. When pinning, the API's `sha1` is compared with the
//!   downloaded bytes and a difference is an error.
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
//! verification. A retry after a mid-body failure resumes with a `Range` request only when it is
//! safe: the request carries `If-Range` with the `ETag` (else `Last-Modified`) of the first
//! response; an unpinned download (`--update-lock`) whose first response had neither restarts
//! from zero, and so does any `206` whose `Content-Range` is not exactly the rest of the file
//! (`end + 1 == total`) or a server that ignores the range. User-Agent:
//! `lpk-bench/<version> (https://github.com/Charon89/litepack; corpus-builder bot)`.
//!
//! Politeness is set per host in `[[host]]` tables and applies to every request the
//! `Downloader` makes to that host (the host of the URL as written, not of redirect targets):
//! `min_interval_ms` between the end of one request and the start of the next, and an optional
//! `max_mbit_per_s` throughput cap. Two optional keys set the patience for that host only:
//! `max_attempts` (attempts per request, at least 1) and `max_delay_ms` (cap on one back-off
//! wait, at least 1). Without them the defaults apply (5 attempts, waits of 2, 4, 8 and 16 s,
//! capped at 60 s). `Retry-After` is still honoured, and a value above the global limit still
//! fails the download. Values outside 1..=20 attempts and 1..=900000 ms are rejected when the registry is loaded. `archive.org` is
//! configured to ride out an outage of some ten minutes in total.
//!
//! API requests of list resolvers (`Ctx::api_get`) go through the same pacing and retry rules
//! (`Downloader::get_bytes`), are never cached, and are refused outside `--update-lock`. Under
//! `maxlag` the Commons resolver backs off and asks again.
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

pub mod arxiv;
pub mod build;
pub mod classify;
pub mod commons;
pub mod derive;
pub mod extract;
pub mod fetch;
pub mod gitsrc;
pub mod lock;
pub mod manifest;
pub mod registry;
pub mod scan;

#[cfg(test)]
mod leftover_tests;

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
    /// Pin the selected sources and write the lock instead of verifying against it. Resumable:
    /// an artifact that is already pinned and whose cached file matches its pin is reused without
    /// a request, a list source continues from the listing saved in the lock (no API call), and
    /// the lock is saved after every source and every few files, so an interrupted run keeps its
    /// progress: just run the same command again (without --repin). Other sources' pins are
    /// kept. To change an existing pin use --repin
    #[arg(long)]
    pub update_lock: bool,
    /// With --update-lock: forget the pins of these sources (ids, comma separated, or `all` for
    /// every selected source) and resolve and download them again. After an interruption,
    /// continue with plain --update-lock; repeating --repin starts over
    #[arg(long, value_delimiter = ',', requires = "update_lock")]
    pub repin: Vec<String>,
    /// With --update-lock: only resolve and save the listings of the API-listed sources
    /// (commons-photos, arxiv-papers), download nothing, and print per source the number of
    /// accepted files and the sum of their API sizes
    #[arg(long, requires = "update_lock")]
    pub list_only: bool,
    /// Continue without listed files (commons-photos, arxiv-papers) that are gone upstream
    /// (404/410) or no longer match their pin; they are recorded under `unavailable` in
    /// build-info.json. Default: the build fails and names them
    #[arg(long)]
    pub allow_unavailable: bool,
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
            git_program: None,
            allow_unavailable: self.allow_unavailable,
            ffmpeg_program: None,
            repin: self.repin.clone(),
            list_only: self.list_only,
        }
    }
}

/// Entry point for `lpk-bench corpus ...`.
pub fn run(args: CorpusArgs) -> ExitCode {
    match args.command {
        CorpusCommand::Build(b) if b.list_only => {
            match HttpFetcher::new().and_then(|f| build::list_only(&b.options(), &f)) {
                Ok(list) => {
                    for s in &list {
                        match s.bytes {
                            Some(n) => {
                                println!("{}: {} files, {n} bytes (API sizes)", s.source, s.files)
                            }
                            None => println!("{}: {} files, sizes not reported", s.source, s.files),
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
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
                    for (class, t) in &r.summary.classes {
                        println!("  {class}: {} files, {} bytes", t.files, t.bytes);
                    }
                    println!(
                        "  total: {} files, {} bytes",
                        r.summary.total_files, r.summary.total_bytes
                    );
                    for p in &r.missing {
                        println!("missing after extraction (not in the manifest): {p}");
                    }
                    for p in &r.altered {
                        println!("altered after extraction (not in the manifest): {p}");
                    }
                    for s in &r.skipped {
                        println!(
                            "WARNING: source `{}` was SKIPPED, the corpus lacks it: {}",
                            s.source, s.reason
                        );
                    }
                    for u in &r.unavailable {
                        println!(
                            "WARNING: unavailable, left out of the manifest: {} ({}): {}",
                            u.url, u.source, u.reason
                        );
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
            repin: vec![],
            list_only: false,
            allow_unavailable: false,
            sources: PathBuf::from("s"),
            lock: PathBuf::from("l"),
        };
        assert_eq!(a.options().out, PathBuf::from("bench/corpus").join("full"));
    }
}
