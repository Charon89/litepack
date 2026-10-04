//! `lpk-check`: the command-line tool of the independent decoder.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lpk_check::extract::{escape, extract_all};
use lpk_check::{journal, keyless, recovery, Archive, Error, Options};

#[derive(Debug, Parser)]
#[command(name = "lpk-check", about = "Independent .lpk v1 decoder (spec-only)")]
struct Cli {
    /// Password (for tests; prefer --password-file).
    #[arg(long, global = true)]
    password: Option<String>,
    /// File holding the password (one trailing newline is stripped).
    #[arg(long, global = true)]
    password_file: Option<PathBuf>,
    /// Keyfile.
    #[arg(long, global = true)]
    keyfile: Option<PathBuf>,
    /// A prior (repeatable); matched by its BLAKE3.
    #[arg(long, global = true)]
    prior: Vec<PathBuf>,
    /// Behave as a revision 1.0 reader (`jpeg-reconstruct` is not run).
    #[arg(long, global = true)]
    revision_1_0: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Entries: kind, size, path.
    List { archive: PathBuf },
    /// Verify everything.
    Verify { archive: PathBuf },
    /// Extract every entry into a directory.
    Extract { archive: PathBuf, dir: PathBuf },
    /// Recovery report.
    Check { archive: PathBuf },
    /// Header, trailer, index and envelope facts.
    Info { archive: PathBuf },
    /// Write a repaired copy to a new file.
    Repair { archive: PathBuf, out: PathBuf },
    /// Write a copy truncated to a generation.
    Rollback {
        archive: PathBuf,
        generation: u64,
        out: PathBuf,
    },
}

fn read(p: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(p).map_err(|e| Error::io(&format!("read {}", p.display()), &e))
}

fn options(cli: &Cli) -> Result<Options, Error> {
    let mut o = Options {
        revision_1_0: cli.revision_1_0,
        ..Options::default()
    };
    if let Some(p) = &cli.password {
        o.password = Some(p.as_bytes().to_vec());
    }
    if let Some(f) = &cli.password_file {
        let mut b = read(f)?;
        if b.ends_with(b"\r\n") {
            b.truncate(b.len() - 2);
        } else if b.ends_with(b"\n") {
            b.truncate(b.len() - 1);
        }
        o.password = Some(b);
    }
    if let Some(k) = &cli.keyfile {
        o.keyfile = Some(read(k)?);
    }
    for p in &cli.prior {
        o.add_prior(read(p)?);
    }
    Ok(o)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| Error::io("create output", &e))?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|e| Error::io("write output", &e))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The `info` lines of the reference format (CONFORMANCE "Output formats").
fn info(data: Vec<u8>, opts: &Options) -> Result<String, Error> {
    use std::fmt::Write;
    let len = data.len();
    let chain = journal::history(&data).map(|c| c.len());
    let a = Archive::open(data, opts.clone())?;
    let entries = a.entries()?.len();
    let (h, ix, env) = (&a.header, &a.index, a.index.envelope);
    let mut s = String::new();
    let mut line = |t: String| {
        let _ = writeln!(s, "{t}");
    };
    line(format!("format: 1.{}", h.version_minor));
    line(format!("header flags: {:#x}", h.flags));
    line(format!("archive id: {}", hex(&h.archive_id)));
    line(format!("generation: {}", a.trailer.generation));
    line(match chain {
        Ok(n) => format!("chain length: {n}"),
        Err(e) => format!("chain: error: {e}"),
    });
    line(format!("length: {len} bytes"));
    line(format!("entries: {entries}"));
    line(format!("chunks: {}", ix.chunks.len()));
    line(format!("blocks: {}", ix.blocks.len()));
    line(format!("records: {}", ix.records.is_some()));
    line(format!("recovery frames: {}", ix.recovery.len()));
    line(format!("priors: {}", ix.priors.len()));
    for p in &ix.priors {
        line(format!("prior: {}", hex(p)));
    }
    line(format!("merkle root: {}", hex(&ix.merkle_root)));
    for (k, v) in [
        ("max_window", env.max_window),
        ("max_bwt_block", env.max_bwt_block),
        ("max_block_plain", env.max_block_plain),
        ("max_frame_payload", env.max_frame_payload),
        ("decode_memory", env.decode_memory),
        ("threads_hint", env.threads_hint),
    ] {
        line(format!("envelope {k}: {v}"));
    }
    Ok(s)
}

fn run(cli: &Cli) -> Result<u8, Error> {
    let opts = options(cli)?;
    let keyless_encrypted = |data: &[u8]| {
        opts.password.is_none() && lpk_check::wire::parse_header(data).is_ok_and(|h| h.encrypted())
    };
    match &cli.cmd {
        Cmd::List { archive } => {
            let data = read(archive)?;
            let entries = if keyless_encrypted(&data) {
                keyless::list(&data, &opts)?
            } else {
                Archive::open(data, opts.clone())?.entries()?
            };
            for e in entries {
                println!("{}\t{}\t{}", e.kind.name(), e.size, escape(&e.path));
            }
            Ok(0)
        }
        Cmd::Verify { archive } => {
            let data = read(archive)?;
            if keyless_encrypted(&data) {
                // Spec problem: `<entries>` of a non-listable archive is not defined; 0 is printed.
                let (_, entries) = keyless::verify(&data, &opts)?;
                println!(
                    "ok (frame hashes only, chunks not checked without the password): {} entries",
                    entries.unwrap_or(0)
                );
                return Ok(0);
            }
            let s = Archive::open(data, opts.clone())?.verify()?;
            println!(
                "ok: {} entries, {} chunks, {} blocks",
                s.entries, s.chunks, s.blocks
            );
            Ok(0)
        }
        Cmd::Extract { archive, dir } => {
            let mut a = Archive::open(read(archive)?, opts.clone())?;
            std::fs::create_dir_all(dir).map_err(|e| Error::io("create directory", &e))?;
            let (done, err) = extract_all(&mut a, dir)?;
            println!("extracted {} entries", done.len());
            match err {
                Some(e) => Err(e),
                None => Ok(0),
            }
        }
        Cmd::Info { archive } => {
            print!("{}", info(read(archive)?, &opts)?);
            Ok(0)
        }
        Cmd::Check { archive } | Cmd::Repair { archive, .. } => {
            let data = read(archive)?;
            let mut copy = data.clone();
            let want_repair = matches!(cli.cmd, Cmd::Repair { .. });
            let rep_target = if want_repair { Some(&mut copy) } else { None };
            let rep = if keyless_encrypted(&data) {
                keyless::recovery_scan(&data, &opts, rep_target)?
            } else {
                let a = Archive::open(data.clone(), opts.clone())?;
                recovery::scan(
                    &data,
                    &a.index.recovery,
                    &a.index.generations,
                    a.trailer.index_offset,
                    rep_target,
                    opts.resources.memory,
                )?
            };
            if let Cmd::Repair { out, .. } = &cli.cmd {
                let keep = rep.error.as_ref().is_none_or(|e| e.class == "Unrepairable");
                if keep {
                    write_new(out, &copy)?;
                }
                println!("{}", rep.line());
                return match rep.error {
                    Some(e) => Err(e),
                    None => Ok(0),
                };
            }
            println!("{}", rep.line());
            if rep.damaged > 0 || rep.unusable > 0 {
                return Err(Error::new(
                    "DamageFound",
                    format!(
                        "damage found: {} shards damaged, {} recovery frames unusable",
                        rep.damaged, rep.unusable
                    ),
                ));
            }
            Ok(0)
        }
        Cmd::Rollback {
            archive,
            generation,
            out,
        } => {
            let data = read(archive)?;
            let len = journal::rollback_len(&data, *generation)?;
            write_new(out, &data[..len])?;
            println!("rolled back to generation {generation}: {len} bytes");
            Ok(0)
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
