//! The `lpk` command line: `a` creates an archive from a directory, `x` extracts, `t` tests.
//!
//! Extraction goes through `lpk-core`'s block-ordered engine (`lpk_core::extract_file`: each
//! needed block decoded once, blocks decoded in parallel under a memory bound) with the default
//! policy, which calls the format tool's own refusals (unsafe paths, devices, symlinks, existing
//! files). Testing goes through the reference tool's code (`lpk_format::cli::run_with`). Both use
//! `lpk-core`'s full reader (revision 1.1's `jpeg-reconstruct`), so peeled files extract and
//! verify.
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
}

#[derive(Debug, Args)]
struct AddArgs {
    /// The archive to create; must not exist.
    archive: PathBuf,
    /// The directory whose contents are archived.
    input: PathBuf,
    /// The Fast tier (zstd with a long window; the default).
    #[arg(long, conflicts_with_all = ["store", "balanced"])]
    fast: bool,
    /// The Balanced tier (LZMA with a large dictionary, or zstd --ultra --long per block by a
    /// trial on a sample). Memory: about the block size plus several times the dictionary to
    /// compress; the block size plus the dictionary to extract.
    #[arg(long, conflicts_with = "store")]
    balanced: bool,
    /// Store every block without compression.
    #[arg(long)]
    store: bool,
    /// Worker threads: accepted and ignored (shown with -v); parallel encoding comes with the
    /// next task.
    #[arg(long, value_name = "N")]
    threads: Option<usize>,
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
    /// that the decoded blocks in flight stay within the reader's decode memory.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    threads: Option<u64>,
    /// A prior file (for example a zstd dictionary) the archive needs; may be repeated.
    #[arg(long = "prior", value_name = "FILE")]
    priors: Vec<PathBuf>,
    /// Print the plan's counts and the pool size to stderr.
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
}

fn core_exit(e: &CoreError) -> i32 {
    match e {
        CoreError::InvalidOption(_) => EXIT_USAGE,
        _ => EXIT_FAILED,
    }
}

fn add(a: &AddArgs, err: &mut dyn Write) -> i32 {
    let mut pipeline = if a.store {
        Pipeline::store(StoreOptions::default())
    } else if a.balanced {
        let mut o = BalancedOptions::default();
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
        let mut o = FastOptions::default();
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
    if a.no_dedup {
        pipeline.fold = None;
    } else if pipeline.fold.is_some() {
        pipeline.fold = Some(Box::new(lpk_core::Dedup::new(lpk_core::FoldOptions {
            ordering: a.ordering.into(),
            ..lpk_core::FoldOptions::default()
        })));
    }
    match pipeline.run_file(&a.input, &a.archive) {
        Ok(s) => {
            if a.verbose {
                report(err, a, &s);
            }
            EXIT_OK
        }
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            core_exit(&e)
        }
    }
}

fn report(err: &mut dyn Write, a: &AddArgs, s: &RunSummary) {
    let t = &s.timings;
    let _ = writeln!(
        err,
        "{}: {} entries, {} blocks, {} bytes (threads requested: {})",
        a.archive.display(),
        s.writer.entries,
        s.writer.blocks,
        s.writer.archive_len,
        a.threads.map_or("default".to_string(), |n| n.to_string())
    );
    if let Some(f) = s.fast {
        let _ = writeln!(
            err,
            "blocks: {} zstd, {} stored by gate, {} stored by class, {} stored without gain",
            f.zstd_blocks, f.stored_by_gate, f.stored_by_class, f.stored_no_gain
        );
    }
    if let Some(b) = s.balanced {
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
    if !a.store && !a.no_dedup {
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

fn extract(x: &ExtractArgs, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
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
        ..lpk_core::ExtractOptions::default()
    };
    match lpk_core::extract_file(
        &x.archive,
        &x.outdir,
        &priors,
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

/// Run the reference tool with `sub` (`extract` or `verify`) and the prior files; its summary
/// goes to `out`, its error line to `err`, and any failure is exit 2.
fn reference(
    sub: &str,
    archive: &Path,
    outdir: Option<&Path>,
    priors: &[PathBuf],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let mut args: Vec<OsString> = vec!["lpk".into()];
    for p in priors {
        let mut a = OsString::from("--prior=");
        a.push(p);
        args.push(a);
    }
    args.push(sub.into());
    args.push("--".into());
    args.push(archive.into());
    if let Some(d) = outdir {
        args.push(d.into());
    }
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
    match &cli.command {
        Command::Add(a) => add(a, err),
        Command::Extract(x) => extract(x, out, err),
        Command::Test(t) => reference("verify", &t.archive, None, &t.priors, out, err),
    }
}
