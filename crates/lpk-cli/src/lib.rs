//! The `lpk` command line: `a` creates an archive from a directory, `x` extracts, `t` tests,
//! `check` and `repair` drive the recovery records, `append` adds a generation (a snapshot of
//! a directory, deduplicated against the archive's chunks), `rollback` truncates to an earlier
//! generation and `info` lists them.
//!
//! Extraction goes through `lpk-core`'s block-ordered engine (`lpk_core::extract_file`: each
//! needed block decoded once, blocks decoded in parallel under a memory bound) with the default
//! policy, which calls the format tool's own refusals (unsafe paths, devices, symlinks, existing
//! files). Testing goes through the reference tool's code (`lpk_format::cli::run_with`). Both use
//! `lpk-core`'s full reader (revision 1.1's `jpeg-reconstruct`), so peeled files extract and
//! verify.
//! Encryption: `--password`, `--password-file` and `--keyfile` are read here for `a` and `x`
//! (the format tool's rules: one trailing newline is not part of a password file; a keyfile
//! alone means an empty password) and passed through to the reference tool for `t`, `check`
//! and `repair`. A password is never printed or logged. `x` opens the key slot before anything
//! is written: a wrong or missing password leaves no output directory.
//! Tiers: `--fast` (the default), `--balanced`, `--store`, or `--tier <name>` as their long form
//! (what the GUI mirrors); `--strong` and `--tier strong` are reserved for the Strong tier
//! (Phase 2) and refused as a usage error.
//! Exit codes: 0 ok, 1 usage error, 2 a refused or failed operation. Nothing is ever prompted;
//! progress goes to stderr and only with `-v`.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use lpk_core::{
    BalancedOptions, CoreError, DictionaryPolicy, FastOptions, Pipeline, RunSummary, StoreOptions,
};
use lpk_format::{Argon2Params, Credentials, Suite};

/// Exit code: success.
pub const EXIT_OK: i32 = 0;
/// Exit code: the command line was wrong.
pub const EXIT_USAGE: i32 = 1;
/// Exit code: the operation was refused or failed.
pub const EXIT_FAILED: i32 = 2;

/// The version line's payload: crate version and build hash.
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("LPK_GIT_COMMIT"));

#[derive(Debug, Parser)]
#[command(
    name = "lpk",
    version = VERSION,
    about = "LitePack archiver: create, extract and test .lpk archives",
    after_help = "Exit codes: 0 ok, 1 usage error, 2 an operation was refused or failed."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create an archive from the contents of a directory (not the directory itself).
    #[command(name = "a")]
    Add(AddArgs),
    /// Extract every entry of an archive into a directory; never overwrites a file.
    #[command(name = "x")]
    Extract(ExtractArgs),
    /// Test an archive: frame hashes, chunk hashes, Merkle root, records, recovery shards.
    #[command(name = "t")]
    Test(TestArgs),
    /// Hash the data shards the recovery frames cover and report the damage (exit 2 if any).
    Check(CheckArgs),
    /// Write a copy of an archive with its damaged shards rebuilt from the recovery frames.
    Repair(RepairArgs),
    /// Append the contents of a directory to an archive as its next generation: a snapshot
    /// (paths the directory no longer has are removed), deduplicated against the archive's
    /// chunks. A failed append leaves the archive as it was.
    Append(AppendArgs),
    /// Truncate an archive to the end of an earlier generation (in place; `info` lists them).
    /// Also recovers an archive whose last append was cut short.
    Rollback(RollbackArgs),
    /// Print an archive's header, envelope, counts and generations.
    Info(InfoArgs),
}

/// The key material of an encrypted archive; `a` seals with it, every other command opens
/// with it. Either a password (one of the two forms) or a keyfile or both.
#[derive(Args)]
#[group(id = "credential", multiple = true)]
struct CredentialArgs {
    /// The password. Visible in the process list and the shell history; prefer
    /// --password-file.
    #[arg(long, value_name = "STR", conflicts_with = "password_file")]
    password: Option<String>,
    /// A file holding the password; one trailing newline is not part of it.
    #[arg(long, value_name = "PATH")]
    password_file: Option<PathBuf>,
    /// A keyfile as a second factor (alone: the password is empty).
    #[arg(long, value_name = "PATH")]
    keyfile: Option<PathBuf>,
}

/// The password is redacted (as `lpk_format::Credentials` does); the two paths are shown.
impl std::fmt::Debug for CredentialArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialArgs")
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("password_file", &self.password_file)
            .field("keyfile", &self.keyfile)
            .finish()
    }
}

impl CredentialArgs {
    /// True when any credential was given.
    fn given(&self) -> bool {
        self.password.is_some() || self.password_file.is_some() || self.keyfile.is_some()
    }

    /// The credentials, read as the format tool reads them (the password moves out of the
    /// parsed arguments into `Credentials`, which wipes it on drop). `None` when nothing was
    /// given.
    fn take(&mut self) -> Result<Option<Credentials>, String> {
        let password = match (self.password.take(), &self.password_file) {
            (Some(p), _) => Some(p.into_bytes()),
            (None, Some(f)) => {
                let mut b = std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
                if b.ends_with(b"\n") {
                    b.pop();
                    if b.ends_with(b"\r") {
                        b.pop();
                    }
                }
                Some(b)
            }
            (None, None) => None,
        };
        let keyfile = match &self.keyfile {
            Some(f) => Some(std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?),
            None => None,
        };
        Ok(match (password, keyfile) {
            (None, None) => None,
            (p, keyfile) => Some(Credentials {
                password: p.unwrap_or_default(),
                keyfile,
            }),
        })
    }

    /// The same credentials as global arguments of the reference tool (`--password=STR`,
    /// `--password-file=PATH`, `--keyfile=PATH`), which applies the same rules itself.
    fn reference_args(&self) -> Vec<OsString> {
        let mut v = Vec::new();
        if let Some(p) = &self.password {
            v.push(OsString::from(format!("--password={p}")));
        }
        for (flag, path) in [
            ("--password-file=", &self.password_file),
            ("--keyfile=", &self.keyfile),
        ] {
            if let Some(path) = path {
                let mut a = OsString::from(flag);
                a.push(path);
                v.push(a);
            }
        }
        v
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum SuiteArg {
    #[value(name = "aes-256-gcm")]
    Aes256Gcm,
    #[value(name = "xchacha20-poly1305")]
    XChaCha20Poly1305,
}

impl From<SuiteArg> for Suite {
    fn from(s: SuiteArg) -> Self {
        match s {
            SuiteArg::Aes256Gcm => Suite::AesGcm,
            SuiteArg::XChaCha20Poly1305 => Suite::XChaCha,
        }
    }
}

/// The format's default suite (D-39: AES-256-GCM).
const DEFAULT_SUITE: Suite = Suite::AesGcm;

/// The one-line refusal of `--strong` and `--tier strong` (PLAN E3: reserved for Phase 2).
const STRONG_RESERVED: &str =
    "the Strong tier arrives in Phase 2; --strong and --tier strong are reserved until then";

/// The value of `--tier`: the long form of the tier flags, which the GUI mirrors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum TierArg {
    Fast,
    Balanced,
    Store,
    /// Reserved (Phase 2): refused.
    Strong,
}

/// A tier the tool can run today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Fast,
    Balanced,
    Store,
}

impl Tier {
    /// The value `--tier` takes for this tier.
    fn name(self) -> &'static str {
        match self {
            Tier::Fast => "fast",
            Tier::Balanced => "balanced",
            Tier::Store => "store",
        }
    }
}

/// The tier and its settings; `a` and `append` take the same ones.
#[derive(Debug, Args)]
struct TierArgs {
    /// The tier by name: the long form of --fast, --balanced and --store (one or the other;
    /// `strong` is reserved for the Strong tier, Phase 2, and refused).
    #[arg(long, value_enum, value_name = "TIER",
          conflicts_with_all = ["fast", "balanced", "store", "strong"])]
    tier: Option<TierArg>,
    /// The Fast tier (zstd with a long window; the default).
    #[arg(long, conflicts_with_all = ["store", "balanced", "strong"])]
    fast: bool,
    /// The Balanced tier (LZMA with a large dictionary, or zstd --ultra --long per block by a
    /// trial on a sample). Memory: about the block size plus several times the dictionary to
    /// compress; the block size plus the dictionary to extract.
    #[arg(long, conflicts_with_all = ["store", "strong"])]
    balanced: bool,
    /// Store every block without compression.
    #[arg(long, conflicts_with = "strong")]
    store: bool,
    /// Reserved for the Strong tier (Phase 2): refused with exit 1 until it arrives.
    #[arg(long)]
    strong: bool,
    /// Blocks encoded at once at most (default: the machine's logical cores; 1 is sequential).
    /// The archive is the same whatever the count. The memory budget may allow fewer (shown
    /// with -v).
    #[arg(long, value_name = "N")]
    threads: Option<usize>,
    /// The memory budget of the whole run, for example 512M or 2G (binary units) or plain
    /// bytes. Default: half of the physical memory, at most 1 GiB (--store), 2 GiB (--fast) or
    /// 4 GiB (--balanced). The blocks in flight are derived from it; a budget below what one
    /// block in flight needs is refused with that figure.
    #[arg(long, value_name = "SIZE", value_parser = parse_memory)]
    memory: Option<u64>,
    /// zstd level (of the Fast tier, or of the Balanced tier's zstd candidate; default 22 there).
    #[arg(long, value_name = "L", conflicts_with = "store")]
    level: Option<i32>,
    /// log2 of the zstd match window, 10 to 28 (the reader's default limit caps it).
    #[arg(long, value_name = "W", conflicts_with = "store")]
    window_log: Option<u32>,
    /// The Balanced tier's LZMA dictionary in bytes (default 64 MiB).
    #[arg(long, value_name = "BYTES", requires = "balanced")]
    dict_size: Option<u32>,
    /// Use no dictionaries (what the Fast tier does today: none is bundled).
    #[arg(long, conflicts_with = "balanced")]
    no_dictionaries: bool,
    /// Do not deduplicate chunks (the Fast and Balanced tiers cut files with content-defined
    /// chunking and store each distinct chunk once by default, across files and within a file).
    /// This also returns to the fixed 1 MiB cut.
    #[arg(long)]
    no_dedup: bool,
    /// How files are ordered inside a cluster before they are written: `none` (the default:
    /// path order, which keeps directory locality) or `extension` (by extension, then name, the
    /// order 7-Zip uses for solid archives). Ignored with --no-dedup and --store.
    #[arg(long, value_enum, value_name = "ORDER", default_value = "none")]
    ordering: OrderingArg,
    /// The Deflate peel: Deflate streams inside ZIP, Office, JAR, PNG, gzip and PDF files (and
    /// zlib streams found in other binary files) stored as their plain text plus a
    /// reconstruction record that restores them bit for bit (revision 1.2). Default: on with
    /// --balanced, off with --fast. An append runs it only on a revision 1.2 archive.
    #[arg(long, value_enum, value_name = "ON|OFF", conflicts_with = "store")]
    deflate_peel: Option<OnOff>,
}

impl TierArgs {
    /// The tier chosen, or the usage error that refuses the command line before anything is
    /// written: `--strong` and `--tier strong` (the Strong tier arrives in Phase 2), and a
    /// setting of one tier given with `--tier` naming another (the rules clap applies to the
    /// tier flags, so `--tier X` and `--X` accept the same command lines).
    fn tier(&self) -> Result<Tier, String> {
        if self.strong {
            return Err(STRONG_RESERVED.to_string());
        }
        let tier = match self.tier {
            Some(TierArg::Strong) => return Err(STRONG_RESERVED.to_string()),
            Some(TierArg::Fast) => Tier::Fast,
            Some(TierArg::Balanced) => Tier::Balanced,
            Some(TierArg::Store) => Tier::Store,
            None if self.store => Tier::Store,
            None if self.balanced => Tier::Balanced,
            None => Tier::Fast,
        };
        if self.tier.is_some() {
            let settings: &[(&str, bool)] = match tier {
                Tier::Fast => &[("--dict-size", self.dict_size.is_some())],
                Tier::Balanced => &[("--no-dictionaries", self.no_dictionaries)],
                Tier::Store => &[
                    ("--level", self.level.is_some()),
                    ("--window-log", self.window_log.is_some()),
                    ("--dict-size", self.dict_size.is_some()),
                    ("--deflate-peel", self.deflate_peel.is_some()),
                ],
            };
            if let Some((flag, _)) = settings.iter().find(|(_, given)| *given) {
                return Err(format!(
                    "the argument '{flag}' cannot be used with '--tier {}'",
                    tier.name()
                ));
            }
        }
        Ok(tier)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Debug, Args)]
struct AddArgs {
    /// The archive to create; must not exist.
    archive: PathBuf,
    /// The directory whose contents are archived.
    input: PathBuf,
    #[command(flatten)]
    tier: TierArgs,
    /// Encrypt the archive with these (AEAD per frame; the key is derived with Argon2id at the
    /// format's default cost).
    #[command(flatten)]
    credentials: CredentialArgs,
    /// The cipher suite of an encrypted archive (default aes-256-gcm).
    #[arg(long, value_enum, value_name = "SUITE", requires = "credential")]
    suite: Option<SuiteArg>,
    /// Leave the entry table in clear, so the encrypted archive lists without the key.
    #[arg(long, requires = "credential")]
    listable: bool,
    /// Recovery records as a percentage of the archived data, 0 (none, the default) to 20:
    /// Reed-Solomon frames interleaved with the data they protect; `lpk repair` rebuilds
    /// damaged shards up to that share of each group.
    #[arg(long, value_name = "PERCENT", default_value_t = 0,
          value_parser = clap::value_parser!(u8).range(0..=i64::from(lpk_format::MAX_PERCENT)))]
    recovery: u8,
    /// Print the counts and the stage times to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum OrderingArg {
    None,
    Extension,
}

impl From<OrderingArg> for lpk_core::FileOrder {
    fn from(o: OrderingArg) -> Self {
        match o {
            OrderingArg::None => lpk_core::FileOrder::None,
            OrderingArg::Extension => lpk_core::FileOrder::Extension,
        }
    }
}

#[derive(Debug, Args)]
struct ExtractArgs {
    /// The archive.
    archive: PathBuf,
    /// The directory to extract into; created if missing.
    outdir: PathBuf,
    /// The total thread budget, decoders plus file writers (default: the machine's logical
    /// cores; at most four times that). 1 decodes and writes on one thread; from 2 on,
    /// clamp(N/4, 1, 4) threads write files and the rest decode blocks, the decoders capped so
    /// that the extraction stays within its memory budget (see --memory).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    threads: Option<u64>,
    /// A prior file (for example a zstd dictionary) the archive needs; may be repeated.
    #[arg(long = "prior", value_name = "FILE")]
    priors: Vec<PathBuf>,
    /// The key material of an encrypted archive; checked before anything is written.
    #[command(flatten)]
    credentials: CredentialArgs,
    /// The memory budget of the extraction, for example 512M or 2G or plain bytes; also the
    /// reader's decode memory limit (an archive whose envelope needs more is refused before
    /// anything is written). Default: half of the physical memory, at most the ceiling of the
    /// tier the archive's envelope shows: 1 GiB (stored blocks only), 2 GiB (Fast-sized blocks
    /// and windows) or 4 GiB (larger); the reader is then limited to half of the physical
    /// memory, at most 4 GiB. The decoders in flight are derived from the budget (shown with -v);
    /// a budget below what one block in flight needs is refused with that figure.
    #[arg(long, value_name = "SIZE", value_parser = parse_memory)]
    memory: Option<u64>,
    /// Print the plan's counts, the memory budget and its terms, and the pool size to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Args)]
struct TestArgs {
    /// The archive.
    archive: PathBuf,
    /// A prior file (for example a zstd dictionary) the archive needs; may be repeated.
    #[arg(long = "prior", value_name = "FILE")]
    priors: Vec<PathBuf>,
    /// The key material of an encrypted archive; without it only the frame hashes (and the
    /// recovery shards) of such an archive are checked.
    #[command(flatten)]
    credentials: CredentialArgs,
    /// The decode memory limit, as for `x`: an archive whose envelope needs more is refused.
    #[arg(long, value_name = "SIZE", value_parser = parse_memory)]
    memory: Option<u64>,
}

/// `--memory`'s value: `512M`, `2G`, plain bytes ([`lpk_core::memory::parse_size`]).
fn parse_memory(s: &str) -> Result<u64, String> {
    lpk_core::memory::parse_size(s)
}

/// The reader's decode memory limit of `t`: `--memory`, else `min(4 GiB, half the physical
/// memory)` (`x` passes `--memory` on; the engine applies the same default to its reader).
fn decode_memory(given: Option<u64>) -> u64 {
    lpk_core::memory::Budget::resolve(given, lpk_core::memory::EXTRACT_CEILING).bytes
}

#[derive(Debug, Args)]
struct CheckArgs {
    /// The archive.
    archive: PathBuf,
    /// The key material of an encrypted archive (not needed: the shards are checked sealed).
    #[command(flatten)]
    credentials: CredentialArgs,
}

#[derive(Debug, Args)]
struct AppendArgs {
    /// The archive; must exist.
    archive: PathBuf,
    /// The directory whose contents become the new generation.
    input: PathBuf,
    #[command(flatten)]
    tier: TierArgs,
    /// The key material of an encrypted archive (required for one: the new generation is sealed
    /// with the archive's key); checked before anything is written.
    #[command(flatten)]
    credentials: CredentialArgs,
    /// Recovery records for the new generation as a percentage of its data, 0 (none, the
    /// default) to 20, as for `a`.
    #[arg(long, value_name = "PERCENT", default_value_t = 0,
          value_parser = clap::value_parser!(u8).range(0..=i64::from(lpk_format::MAX_PERCENT)))]
    recovery: u8,
    /// Print the counts and the stage times to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Args)]
struct RollbackArgs {
    /// The archive; changed in place.
    archive: PathBuf,
    /// The generation to keep (0 is the first write; `info` shows the latest).
    generation: u64,
    /// Accepted for symmetry; no key is needed to roll back.
    #[command(flatten)]
    credentials: CredentialArgs,
}

#[derive(Debug, Args)]
struct InfoArgs {
    /// The archive.
    archive: PathBuf,
    /// A prior file (for example a zstd dictionary) the archive needs; may be repeated.
    #[arg(long = "prior", value_name = "FILE")]
    priors: Vec<PathBuf>,
    /// The key material of an encrypted archive; without it the sealed counts are not shown.
    #[command(flatten)]
    credentials: CredentialArgs,
}

#[derive(Debug, Args)]
struct RepairArgs {
    /// The archive.
    archive: PathBuf,
    /// The repaired copy; must not exist. Kept with every repair that was possible when a
    /// frame had more damage than it can rebuild (exit 2 then).
    out: PathBuf,
    /// The key material of an encrypted archive (not needed: the shards are rebuilt sealed).
    #[command(flatten)]
    credentials: CredentialArgs,
}

fn core_exit(e: &CoreError) -> i32 {
    match e {
        CoreError::InvalidOption(_) => EXIT_USAGE,
        _ => EXIT_FAILED,
    }
}

/// The pipeline of a tier and its settings (the seal stage untouched).
fn build_pipeline(a: &TierArgs, tier: Tier) -> Pipeline {
    let mut pipeline = if tier == Tier::Store {
        Pipeline::store(StoreOptions {
            threads: a.threads,
            ..StoreOptions::default()
        })
    } else if tier == Tier::Balanced {
        let mut o = BalancedOptions {
            threads: a.threads,
            ..BalancedOptions::default()
        };
        if let Some(l) = a.level {
            o.zstd_level = l;
        }
        if let Some(w) = a.window_log {
            o.zstd_window_log = Some(w);
        }
        if let Some(d) = a.dict_size {
            o.dict_size = d;
        }
        Pipeline::balanced(o)
    } else {
        let mut o = FastOptions {
            threads: a.threads,
            ..FastOptions::default()
        };
        if let Some(l) = a.level {
            o.level = l;
        }
        if a.no_dictionaries {
            o.dictionaries = DictionaryPolicy::None;
        }
        if let Some(w) = a.window_log {
            o.window_log = w;
        }
        Pipeline::fast(o)
    };
    pipeline.seal.memory = a.memory;
    match a.deflate_peel {
        Some(OnOff::On) => pipeline.deflate = Some(lpk_core::peel::deflate::DeflatePeel::default()),
        Some(OnOff::Off) => pipeline.deflate = None,
        None => {}
    }
    if a.no_dedup {
        pipeline.fold = None;
    } else if pipeline.fold.is_some() {
        pipeline.fold = Some(Box::new(lpk_core::Dedup::new(lpk_core::FoldOptions {
            ordering: a.ordering.into(),
            ..lpk_core::FoldOptions::default()
        })));
    }
    pipeline
}

fn add(a: &mut AddArgs, err: &mut dyn Write) -> i32 {
    let tier = match a.tier.tier() {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            return EXIT_USAGE;
        }
    };
    // Decided before `take` moves the password out of the arguments: what the `-v` report
    // says the archive is sealed with.
    let sealing = a.credentials.given().then(|| {
        format!(
            "{}{}",
            a.suite.map_or(DEFAULT_SUITE, Suite::from).name(),
            if a.listable { ", listable" } else { "" }
        )
    });
    let credentials = match a.credentials.take() {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            return EXIT_FAILED;
        }
    };
    let mut pipeline = build_pipeline(&a.tier, tier);
    if let Some(credentials) = credentials {
        pipeline.seal.writer.seal = Some(lpk_format::SealOptions {
            suite: a.suite.map_or(DEFAULT_SUITE, Suite::from),
            argon2: Argon2Params::default(),
            listable: a.listable,
            credentials,
        });
    }
    pipeline.seal.writer.recovery.percent = a.recovery;
    match pipeline.run_file(&a.input, &a.archive) {
        Ok(s) => {
            records_cap_note(err, &s.peel.deflate);
            if a.verbose {
                report(err, &a.archive, &a.tier, tier, sealing, a.recovery, &s);
            }
            EXIT_OK
        }
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            core_exit(&e)
        }
    }
}

fn append(a: &mut AppendArgs, err: &mut dyn Write) -> i32 {
    let tier = match a.tier.tier() {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            return EXIT_USAGE;
        }
    };
    // The new generation is sealed with the archive's own key (the suite is the archive's).
    let sealing = a
        .credentials
        .given()
        .then(|| "the archive's key".to_string());
    let credentials = match a.credentials.take() {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            return EXIT_FAILED;
        }
    };
    let mut pipeline = build_pipeline(&a.tier, tier);
    pipeline.seal.writer.recovery.percent = a.recovery;
    match pipeline.append_file(&a.input, &a.archive, credentials.as_ref()) {
        Ok(s) => {
            records_cap_note(err, &s.peel.deflate);
            if a.verbose {
                report(err, &a.archive, &a.tier, tier, sealing, a.recovery, &s);
            }
            EXIT_OK
        }
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            core_exit(&e)
        }
    }
}

fn report(
    err: &mut dyn Write,
    archive: &Path,
    a: &TierArgs,
    tier: Tier,
    sealing: Option<String>,
    recovery: u8,
    s: &RunSummary,
) {
    let t = &s.timings;
    let _ = writeln!(
        err,
        "{}: generation {}, {} entries, {} blocks, {} bytes (threads: {}, blocks in flight: {}, I/O threads: {}, chunk hash threads: up to {})",
        archive.display(),
        s.writer.generation,
        s.writer.entries,
        s.writer.blocks,
        s.writer.archive_len,
        s.threads,
        s.blocks_in_flight,
        s.io_threads,
        s.hash_threads
    );
    let m = &s.memory;
    let _ = writeln!(
        err,
        "memory: budget {} bytes ({}), per block {} bytes, fixed {} bytes, blocks in flight {}{}",
        m.budget.bytes,
        m.budget.source.name(),
        m.per_block,
        m.fixed(),
        m.blocks_in_flight,
        if m.blocks_in_flight > 1 {
            ""
        } else {
            " (sequential: no reader or write thread)"
        }
    );
    let p = &s.peel_pool;
    if p.workers > 0 {
        let _ = writeln!(
            err,
            "peel pool: {} workers, {} bytes (reserve {} bytes), {} files prepared, peak {} bytes reserved, waits {} by workers and {} by the feeding thread; unused work: {} files deduplicated whole, {} encodes of stored streams, {} files prepared again",
            p.workers,
            p.pool_bytes,
            p.reserve,
            p.files,
            p.peak_bytes,
            p.worker_waits,
            p.feeding_waits,
            p.waste.files,
            p.waste.encodes,
            p.waste.reruns
        );
    }
    if let Some(f) = &s.fast {
        let _ = writeln!(
            err,
            "blocks: {} zstd, {} stored by gate, {} stored by class, {} stored without gain",
            f.zstd_blocks, f.stored_by_gate, f.stored_by_class, f.stored_no_gain
        );
    }
    if let Some(b) = &s.balanced {
        let _ = writeln!(
            err,
            "blocks: {} lzma, {} zstd, {} stored by class, {} stored without gain; trial sample bytes {}; encoder memory about {} bytes (approximate: liblzma LZMA2 encoder query at one thread, not measured)",
            b.lzma_blocks,
            b.zstd_blocks,
            b.stored_by_class,
            b.stored_no_gain,
            b.sample_bytes,
            b.lzma_encoder_memory
        );
    }
    let _ = writeln!(
        err,
        "dedup: {} chunks and {} bytes referenced instead of stored ({} new chunks in the table, {} reused from an earlier generation)",
        s.writer.deduped_chunks, s.writer.deduped_bytes, s.writer.new_chunks, s.writer.reused_chunks
    );
    // Only a mode that was applied: --store and --no-dedup have no fold stage to order by.
    if tier != Tier::Store && !a.no_dedup {
        let _ = writeln!(
            err,
            "ordering: {}",
            lpk_core::FileOrder::from(a.ordering).name()
        );
    }
    let p = &s.peel;
    let _ = writeln!(
        err,
        "deduplicated whole: {} files ({} bytes), never peeled; peeled but the peeled part was already stored: {} files ({} bytes)",
        p.deduplicated.files, p.deduplicated.bytes, p.primary_deduplicated.files, p.primary_deduplicated.bytes
    );
    let _ = writeln!(
        err,
        "peel: {} files peeled ({} bytes in, {} bytes out), {} stored as-is",
        p.peeled.files,
        p.peeled.bytes,
        p.peeled_output_bytes,
        p.fallback_total().files
    );
    for c in lpk_core::Fallback::ALL {
        let n = p.fallback(c);
        if n.files > 0 {
            let _ = writeln!(
                err,
                "  as-is, {}: {} files, {} bytes",
                c.label(),
                n.files,
                n.bytes
            );
        }
    }
    if !p.dropped_stages.is_empty() {
        let _ = writeln!(
            err,
            "append: peel stages not run, the archive's revision does not allow them: {}",
            p.dropped_stages.join(", ")
        );
    }
    deflate_report(err, &p.deflate);
    if let Some(sealing) = sealing {
        let _ = writeln!(err, "encrypted: {sealing}");
    }
    if recovery > 0 {
        let _ = writeln!(
            err,
            "recovery: {} percent, peak {} bytes held for the open group",
            recovery, s.writer.recovery_peak
        );
    }
    let _ = writeln!(
        err,
        "stage seconds: walk {:.3}, classify {:.3}, peel {:.3}, model {:.3}, seal {:.3}",
        t.walk.as_secs_f64(),
        t.classify.as_secs_f64(),
        t.peel.as_secs_f64(),
        t.model.as_secs_f64(),
        t.seal.as_secs_f64()
    );
}

/// The summary line, with or without `-v`, when the run reached the Deflate peel's records cap
/// (a whole-run cap: later streams were stored as found).
fn records_cap_note(err: &mut dyn Write, d: &lpk_core::peel::deflate::DeflateSummary) {
    let n = d.outcomes.records_cap;
    if n > 0 {
        let _ = writeln!(
            err,
            "records cap: {n} Deflate streams stored as found, the run's records reached the cap of {} bytes",
            lpk_core::peel::deflate::stage::RECORDS_CAP
        );
    }
}

/// The Deflate peel's lines of `-v` (nothing when the stage saw no file).
fn deflate_report(err: &mut dyn Write, d: &lpk_core::peel::deflate::DeflateSummary) {
    let walked: u64 = d.walked_by_kind.values().sum();
    if walked == 0 && d.size_cap.files == 0 && d.deduplicated.files == 0 {
        return;
    }
    let kinds: Vec<String> = d
        .walked_by_kind
        .iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect();
    let s = &d.streams;
    let failed: Vec<String> = s.failed.iter().map(|(k, n)| format!("{k} {n}")).collect();
    let o = &d.outcomes;
    let classes: Vec<String> = d
        .kept_by_class
        .iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect();
    let _ = writeln!(
        err,
        "deflate peel: {walked} files walked ({}), {} peeled ({} bytes), {} over the size cap ({} bytes), {} deduplicated whole, {} without streams, {} with none kept, {} sent whole by the file gate, {} by the file share rule, {} by their bound without encodes, {} walker panics",
        kinds.join(", "),
        d.peeled.files,
        d.peeled.bytes,
        d.size_cap.files,
        d.size_cap.bytes,
        d.deduplicated.files,
        d.no_streams,
        d.none_kept,
        d.file_gate,
        d.file_share,
        d.file_bound,
        s.walker_panics
    );
    let _ = writeln!(
        err,
        "  streams: {} found, {} reconstructed, {} skipped, {} failed ({}), {} structural refusals, {} scan false hits; {} files reached the work budget ({} candidates not examined)",
        s.found,
        s.reconstructed,
        s.skipped,
        s.failed_total(),
        failed.join(", "),
        s.structural_refusals,
        s.scan_false_hits,
        s.work_budget_stopped,
        s.unexamined
    );
    let _ = writeln!(
        err,
        "  reconstructed: {} kept ({}), {} no gain, {} block cap, {} memory cap, {} records cap, {} already stored, {} file gate, {} file share, {} file bound",
        o.kept,
        classes.join(", "),
        o.no_gain,
        o.block_cap,
        o.memory_cap,
        o.records_cap,
        o.already_stored,
        o.file_gate,
        o.file_share,
        o.file_bound
    );
    let _ = writeln!(
        err,
        "  kept bytes: stream {}, plain {}, model {}, corrections {}, records {}; part blocks: {} lzma, {} zstd; ZIP members not Deflate: {} Deflate64, {} other",
        d.stream_bytes,
        d.plain_bytes,
        d.model_bytes,
        d.correction_bytes,
        d.record_bytes,
        d.lzma_blocks,
        d.zstd_blocks,
        s.zip_deflate64_members,
        s.zip_other_method_members
    );
    let _ = writeln!(
        err,
        "  seconds: analyse {:.3}, recreate {:.3}, encode {:.3}",
        s.analyse_seconds, s.recreate_seconds, d.encode_seconds
    );
}

fn extract(x: &mut ExtractArgs, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let credentials = match x.credentials.take() {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            return EXIT_FAILED;
        }
    };
    let mut priors = Vec::with_capacity(x.priors.len());
    for p in &x.priors {
        match std::fs::read(p) {
            Ok(b) => priors.push(b),
            Err(e) => {
                let _ = writeln!(err, "error: {}: {e}", p.display());
                return EXIT_FAILED;
            }
        }
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let max = u64::try_from(cores.saturating_mul(4)).unwrap_or(u64::MAX);
    if x.threads.is_some_and(|n| n > max) {
        let _ = writeln!(
            err,
            "error: --threads: at most {max} (four times the logical cores) on this machine"
        );
        return EXIT_USAGE;
    }
    let opts = lpk_core::ExtractOptions {
        threads: x.threads.map(|n| usize::try_from(n).unwrap_or(usize::MAX)),
        memory: x.memory,
        ..lpk_core::ExtractOptions::default()
    };
    match lpk_core::extract_file(
        &x.archive,
        &x.outdir,
        &priors,
        credentials.as_ref(),
        &lpk_core::DefaultPolicy,
        &opts,
    ) {
        Ok(s) => {
            if x.verbose {
                let _ = writeln!(
                    err,
                    "{}: {} files, {} directories, {} bytes; plan: {} chunk placements over {} of {} blocks; pool: {} workers, {} blocks in flight, {} writers; {} blocks decoded, {} nested decodes, {} files reopened",
                    x.archive.display(),
                    s.files,
                    s.directories,
                    s.bytes,
                    s.placements,
                    s.blocks_needed,
                    s.blocks,
                    s.workers,
                    s.in_flight,
                    s.writers,
                    s.blocks_decoded,
                    s.nested_decodes,
                    s.reopened
                );
                let m = &s.memory;
                let source = match m.budget.source {
                    lpk_core::memory::BudgetSource::Given => "--memory".to_string(),
                    lpk_core::memory::BudgetSource::Default {
                        ceiling,
                        physical: Some(p),
                    } => format!(
                        "default: the {} tier's ceiling of {ceiling} bytes, at most half the physical memory of {p} bytes",
                        m.tier.name()
                    ),
                    lpk_core::memory::BudgetSource::Default {
                        ceiling,
                        physical: None,
                    } => format!(
                        "default: the {} tier's ceiling of {ceiling} bytes; physical memory unknown",
                        m.tier.name()
                    ),
                };
                // A sealed archive's key is derived at open, before its envelope can be read:
                // its Argon2 memory is limited by the reader's limit, not the tier's budget.
                let key = if m.key_derivation > 0 {
                    format!(
                        "; key derivation at open: argon2_m {} bytes (process peak {} bytes)",
                        m.key_derivation,
                        m.peak(s.workers)
                    )
                } else {
                    String::new()
                };
                let _ = writeln!(
                    err,
                    "memory: budget {} bytes ({source}); envelope tier {}; per block {} bytes (encoded {}, plain {}, decoder {}, step input {}, nested {}); fixed {} bytes (shared {}, per worker {}); workers {}{key}",
                    m.budget.bytes,
                    m.tier.name(),
                    m.per_block,
                    m.encoded,
                    m.plain,
                    m.decoder,
                    m.step_input,
                    m.nested,
                    m.fixed,
                    m.fixed_shared,
                    m.per_worker,
                    s.workers
                );
            }
            let _ = writeln!(
                out,
                "extracted {} files, {} directories",
                s.files, s.directories
            );
            EXIT_OK
        }
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            EXIT_FAILED
        }
    }
}

/// Run the reference tool with `sub` (`verify`, `check` or `repair`), the prior files, the
/// credentials and the positional paths; its summary goes to `out`, its error line to `err`,
/// and any failure is exit 2.
fn reference(
    sub: &str,
    priors: &[PathBuf],
    memory: Option<u64>,
    credentials: &CredentialArgs,
    positional: &[OsString],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let mut args: Vec<OsString> = vec!["lpk".into()];
    for p in priors {
        let mut a = OsString::from("--prior=");
        a.push(p);
        args.push(a);
    }
    args.extend(credentials.reference_args());
    if let Some(m) = memory {
        args.push(format!("--memory={m}").into());
    }
    args.push(sub.into());
    args.push("--".into());
    args.extend(positional.iter().cloned());
    match lpk_format::cli::run_with(
        args,
        out,
        err,
        lpk_core::register_full_reader::<std::fs::File>,
    ) {
        0 => EXIT_OK,
        _ => EXIT_FAILED,
    }
}

/// Run the tool with `args` (the first is the program name); returns the exit code.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let text = e.to_string();
            if e.use_stderr() {
                let _ = err.write_all(text.as_bytes());
                return EXIT_USAGE;
            }
            let _ = out.write_all(text.as_bytes());
            return EXIT_OK;
        }
    };
    match cli.command {
        Command::Add(mut a) => add(&mut a, err),
        Command::Extract(mut x) => extract(&mut x, out, err),
        Command::Test(t) => reference(
            "verify",
            &t.priors,
            Some(decode_memory(t.memory)),
            &t.credentials,
            &[t.archive.into()],
            out,
            err,
        ),
        Command::Check(c) => reference(
            "check",
            &[],
            None,
            &c.credentials,
            &[c.archive.into()],
            out,
            err,
        ),
        Command::Repair(r) => reference(
            "repair",
            &[],
            None,
            &r.credentials,
            &[r.archive.into(), r.out.into()],
            out,
            err,
        ),
        Command::Append(mut a) => append(&mut a, err),
        Command::Rollback(r) => reference(
            "rollback",
            &[],
            None,
            &r.credentials,
            &[r.archive.into(), r.generation.to_string().into()],
            out,
            err,
        ),
        Command::Info(i) => reference(
            "info",
            &i.priors,
            None,
            &i.credentials,
            &[i.archive.into()],
            out,
            err,
        ),
    }
}

#[cfg(test)]
mod records_cap_tests {
    #[test]
    fn the_records_cap_line_shows_only_when_reached() {
        let mut d = lpk_core::peel::deflate::DeflateSummary::default();
        let mut out = Vec::new();
        super::records_cap_note(&mut out, &d);
        assert!(out.is_empty());
        d.outcomes.records_cap = 3;
        super::records_cap_note(&mut out, &d);
        let text = String::from_utf8_lossy(&out);
        assert!(text.starts_with("records cap: 3 Deflate streams"), "{text}");
    }
}
