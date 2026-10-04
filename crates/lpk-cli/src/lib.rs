//! The `lpk` command line: `a` creates an archive from a directory, `x` extracts, `t` tests.
//!
//! Extraction and testing go through the reference tool's code (`lpk_format::cli`), so the
//! extraction refusals (unsafe paths, devices, symlinks, existing files) are the format's own.
//! Exit codes: 0 ok, 1 usage error, 2 a refused or failed operation. Nothing is ever prompted;
//! progress goes to stderr and only with `-v`.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use lpk_core::{CoreError, DictionaryPolicy, FastOptions, Pipeline, RunSummary, StoreOptions};

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
    #[arg(long, conflicts_with = "store")]
    fast: bool,
    /// Store every block without compression.
    #[arg(long)]
    store: bool,
    /// Worker threads: accepted and ignored (shown with -v); parallel encoding comes with the
    /// next task.
    #[arg(long, value_name = "N")]
    threads: Option<usize>,
    /// zstd level of the Fast tier.
    #[arg(long, value_name = "L", conflicts_with = "store")]
    level: Option<i32>,
    /// log2 of the Fast tier's match window, 10 to 31.
    #[arg(long, value_name = "W", conflicts_with = "store")]
    window_log: Option<u32>,
    /// Use no dictionaries (what the Fast tier does today: none is bundled).
    #[arg(long)]
    no_dictionaries: bool,
    /// Print the counts and the stage times to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Args)]
struct ExtractArgs {
    /// The archive.
    archive: PathBuf,
    /// The directory to extract into; created if missing.
    outdir: PathBuf,
    /// Worker threads. Accepted and ignored; decoding is sequential for now.
    #[arg(long, value_name = "N")]
    threads: Option<usize>,
    /// A prior file (for example a zstd dictionary) the archive needs; may be repeated.
    #[arg(long = "prior", value_name = "FILE")]
    priors: Vec<PathBuf>,
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
    let pipeline = if a.store {
        Pipeline::store(StoreOptions::default())
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
    let _ = writeln!(
        err,
        "stage seconds: walk {:.3}, classify {:.3}, model {:.3}, seal {:.3}",
        t.walk.as_secs_f64(),
        t.classify.as_secs_f64(),
        t.model.as_secs_f64(),
        t.seal.as_secs_f64()
    );
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
    match lpk_format::cli::run(args, out, err) {
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
        Command::Extract(x) => {
            reference("extract", &x.archive, Some(&x.outdir), &x.priors, out, err)
        }
        Command::Test(t) => reference("verify", &t.archive, None, &t.priors, out, err),
    }
}
