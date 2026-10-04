//! The logic of the `lpk-decode` reference tool, as a function the binary and
//! the tests both call.

use crate::archive::Archive;
use crate::crypto::Credentials;
use crate::entry::{Entry, EntryKind};
use crate::envelope::Resources;
use crate::error::FormatError;
use crate::priors::MemoryPriors;
use crate::recovery::{repair_with_credentials, RepairReport};
use crate::trailer::TRAILER_FRAME_LEN;
use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// Reference decoder for `.lpk` archives.
#[derive(Debug, Parser)]
#[command(name = "lpk-decode", version, about)]
struct Cli {
    /// A prior file (for example a zstd dictionary) the archive needs; may be
    /// repeated. It is matched to the archive by the BLAKE3 of its content.
    #[arg(long = "prior", global = true, value_name = "FILE")]
    priors: Vec<PathBuf>,
    /// The password of an encrypted archive (visible in the process list; prefer
    /// `--password-file`).
    #[arg(
        long,
        global = true,
        value_name = "STR",
        conflicts_with = "password_file"
    )]
    password: Option<String>,
    /// A file holding the password; one trailing newline is not part of it.
    #[arg(long, global = true, value_name = "PATH")]
    password_file: Option<PathBuf>,
    /// The keyfile of an archive that uses one as a second factor.
    #[arg(long, global = true, value_name = "PATH")]
    keyfile: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// One line per entry: kind, size, path.
    List {
        /// The archive.
        archive: PathBuf,
    },
    /// Check every block and every entry; exit 1 on the first error.
    Verify {
        /// The archive.
        archive: PathBuf,
    },
    /// Write the archive's files and directories under a directory.
    Extract {
        /// The archive.
        archive: PathBuf,
        /// Where to extract; created if missing.
        dir: PathBuf,
    },
    /// Header, envelope, counts and generation.
    Info {
        /// The archive.
        archive: PathBuf,
    },
    /// Hash the data shards the recovery frames cover and report the damage; exit 1 if any.
    Check {
        /// The archive.
        archive: PathBuf,
    },
    /// Truncate the archive to the end of an earlier generation (an append is undone;
    /// the file is changed in place, no password is needed).
    Rollback {
        /// The archive.
        archive: PathBuf,
        /// The generation to keep (0 is the first write; `info` shows the latest).
        generation: u64,
    },
    /// Write a copy of the archive with the damaged shards rebuilt from its recovery frames.
    Repair {
        /// The archive.
        archive: PathBuf,
        /// The repaired copy; must not exist.
        out: PathBuf,
    },
}

const RESERVED: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

fn is_reserved_device(component: &str) -> bool {
    // Windows ignores everything from the first dot on, and the spaces before it.
    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.trim_end_matches([' ', '.']).to_uppercase();
    if RESERVED.contains(&upper.as_str()) {
        return true;
    }
    for prefix in ["COM", "LPT"] {
        if let Some(d) = upper.strip_prefix(prefix) {
            let mut it = d.chars();
            if let (Some('1'..='9' | '\u{B9}' | '\u{B2}' | '\u{B3}'), None) = (it.next(), it.next())
            {
                return true;
            }
        }
    }
    false
}

/// The extraction tool's path policy on top of the format's path rules: a
/// component may not be a Windows device name (`CON`, `PRN`, `AUX`, `NUL`,
/// `COM1`-`COM9`, `COM` with a superscript 1, 2 or 3, `LPT1`-`LPT9`, `LPT`
/// with a superscript 1, 2 or 3, `CONIN$`, `CONOUT$`; in any case, with or
/// without an extension, and with spaces before the extension), contain `:`,
/// or end in a dot or a space.
pub fn check_extraction_path(path: &str) -> Result<(), FormatError> {
    let unsafe_path = |reason| FormatError::UnsafePath {
        path: path.to_string(),
        reason,
    };
    for component in path.split('/') {
        if component.contains(':') {
            return Err(unsafe_path("colon"));
        }
        if component.ends_with('.') || component.ends_with(' ') {
            return Err(unsafe_path("trailing dot or space"));
        }
        if is_reserved_device(component) {
            return Err(unsafe_path("reserved device name"));
        }
    }
    Ok(())
}

/// The key material the command line gave for an encrypted archive.
#[derive(Debug, Default)]
struct Keys {
    credentials: Option<Credentials>,
}

fn credentials(cli: &mut Cli) -> Result<Keys, FormatError> {
    // The password string is moved out of the parsed arguments into a wiping
    // wrapper; the bytes handed on live in `Credentials`, which wipes them too.
    let given: Option<Zeroizing<String>> = cli.password.take().map(Zeroizing::new);
    let password = match (&given, &cli.password_file) {
        (Some(p), _) => Some(p.as_bytes().to_vec()),
        (None, Some(f)) => {
            let mut b = std::fs::read(f)?;
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
    let keyfile = match &cli.keyfile {
        Some(f) => Some(std::fs::read(f)?),
        None => None,
    };
    let credentials = match (password, keyfile) {
        (None, None) => None,
        (p, keyfile) => Some(Credentials {
            password: p.unwrap_or_default(),
            keyfile,
        }),
    };
    Ok(Keys { credentials })
}

fn open(path: &Path, priors: &[PathBuf], keys: &Keys) -> Result<Archive<File>, FormatError> {
    let mut a = Archive::open_with(
        File::open(path)?,
        &Resources::default(),
        keys.credentials.as_ref(),
    )?;
    if !priors.is_empty() {
        // The prior files are the caller's: named on the command line, each
        // filed under the BLAKE3 of its content.
        let mut store = MemoryPriors::new();
        for p in priors {
            store.insert(std::fs::read(p)?);
        }
        a.set_priors(Box::new(store));
    }
    Ok(a)
}

fn entries(a: &mut Archive<File>) -> Result<Vec<Entry>, FormatError> {
    let t = a.entry_table()?;
    t.table()?.iter().collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Control characters (newlines included) as Rust-style escapes, so one entry is one line.
fn escape_path(p: &str) -> String {
    p.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// `dir` with the archive path's components pushed one at a time (a `/` inside
/// one `join` argument is not converted under a `\\?\` prefix on Windows).
fn join_components(dir: &Path, path: &str) -> PathBuf {
    let mut p = dir.to_path_buf();
    for c in path.split('/') {
        p.push(c);
    }
    p
}

fn list(
    path: &Path,
    priors: &[PathBuf],
    keys: &Keys,
    out: &mut dyn Write,
) -> Result<(), FormatError> {
    let mut a = open(path, priors, keys)?;
    for e in entries(&mut a)? {
        let path = escape_path(&e.path);
        writeln!(out, "{}\t{}\t{}", e.kind.name(), e.size, path)?;
    }
    Ok(())
}

fn verify(
    path: &Path,
    priors: &[PathBuf],
    keys: &Keys,
    out: &mut dyn Write,
) -> Result<(), FormatError> {
    let mut a = open(path, priors, keys)?;
    let s = a.verify()?;
    if s.chunks_checked {
        writeln!(
            out,
            "ok: {} entries, {} chunks, {} blocks",
            s.entries, s.chunks, s.blocks
        )?;
    } else {
        writeln!(
            out,
            "ok (frame hashes and recovery frames only, chunks not checked without the password): {} entries",
            s.entries
        )?;
    }
    Ok(())
}

fn print_report(out: &mut dyn Write, r: &RepairReport) -> Result<(), FormatError> {
    writeln!(
        out,
        "recovery frames: {}, unusable: {}, damaged shards: {}, repaired shards: {}",
        r.frames, r.frames_unusable, r.shards_damaged, r.shards_repaired
    )?;
    Ok(())
}

fn check(path: &Path, keys: &Keys, out: &mut dyn Write) -> Result<(), FormatError> {
    let mut a = open(path, &[], keys)?;
    let r = a.check_recovery()?;
    print_report(out, &r)?;
    if r.shards_damaged > 0 || r.frames_unusable > 0 {
        return Err(FormatError::DamageFound {
            damaged: r.shards_damaged,
            unusable: r.frames_unusable,
        });
    }
    Ok(())
}

fn repair_cmd(
    path: &Path,
    target: &Path,
    keys: &Keys,
    out: &mut dyn Write,
) -> Result<(), FormatError> {
    let input = File::open(path)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(target)?;
    match repair_with_credentials(
        input,
        file,
        &Resources::default(),
        keys.credentials.as_ref(),
    ) {
        // The copy carries the repairs that were possible; keep it.
        Ok((report, unrepairable)) => {
            print_report(out, &report)?;
            unrepairable.map_or(Ok(()), Err)
        }
        Err(e) => {
            let _ = std::fs::remove_file(target);
            Err(e)
        }
    }
}

fn rollback_cmd(path: &Path, generation: u64, out: &mut dyn Write) -> Result<(), FormatError> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let g = crate::archive::rollback(&mut file, generation)?;
    writeln!(
        out,
        "rolled back to generation {}: {} bytes",
        g.generation,
        g.end()
    )?;
    Ok(())
}

fn info(
    path: &Path,
    priors: &[PathBuf],
    keys: &Keys,
    out: &mut dyn Write,
) -> Result<(), FormatError> {
    let mut a = open(path, priors, keys)?;
    let h = *a.header();
    let t = *a.trailer();
    let e = a.index().envelope;
    let entries = if a.is_keyless() && !a.is_listable() {
        None
    } else {
        Some(a.entry_table()?.len())
    };
    let archive_len = t.index_offset + t.index_len + TRAILER_FRAME_LEN;
    writeln!(out, "format: {}.{}", h.version.major, h.version.minor)?;
    writeln!(out, "header flags: {:#x}", h.flags.bits())?;
    writeln!(out, "archive id: {}", hex(&h.archive_id))?;
    if let (Some(suite), Some(slot)) = (a.suite(), a.key_slot()) {
        writeln!(out, "encrypted: yes")?;
        writeln!(out, "listable: {}", a.is_listable())?;
        writeln!(out, "suite: {}", suite.name())?;
        writeln!(
            out,
            "argon2id: t={} m_kib={} p={}",
            slot.argon2.t, slot.argon2.m_kib, slot.argon2.p
        )?;
        writeln!(out, "keyfile required: {}", slot.keyfile_required)?;
    }
    writeln!(out, "generation: {}", t.generation)?;
    match a.history() {
        Ok(h) => writeln!(out, "chain length: {}", h.len())?,
        Err(e) => writeln!(out, "chain: error: {e}")?,
    }
    writeln!(out, "length: {archive_len} bytes")?;
    if let Some(n) = entries {
        writeln!(out, "entries: {n}")?;
    }
    if a.is_keyless() {
        writeln!(out, "index: sealed (a password is required)")?;
        writeln!(out, "recovery frames: {}", a.recovery_frames().len())?;
        return Ok(());
    }
    writeln!(out, "chunks: {}", a.chunks().len())?;
    writeln!(out, "blocks: {}", a.index().blocks.len())?;
    writeln!(out, "records: {}", a.index().records.is_some())?;
    writeln!(out, "recovery frames: {}", a.recovery_frames().len())?;
    writeln!(out, "priors: {}", a.priors().len())?;
    for id in a.priors() {
        writeln!(out, "prior: {}", hex(id))?;
    }
    writeln!(out, "merkle root: {}", hex(&a.index().merkle_root))?;
    writeln!(out, "envelope max_window: {}", e.max_window)?;
    writeln!(out, "envelope max_bwt_block: {}", e.max_bwt_block)?;
    writeln!(out, "envelope max_block_plain: {}", e.max_block_plain)?;
    writeln!(out, "envelope max_frame_payload: {}", e.max_frame_payload)?;
    writeln!(out, "envelope decode_memory: {}", e.decode_memory)?;
    writeln!(out, "envelope threads_hint: {}", e.threads_hint)?;
    Ok(())
}

/// Create the file at `target` (never replacing anything: `create_new`) and
/// write the entry into it. If this call created the file and cannot finish it,
/// the file is removed; a file that was already there is never touched.
fn extract_file(a: &mut Archive<File>, e: &Entry, target: &Path) -> Result<(), FormatError> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let mut w = BufWriter::new(file);
    let result = a
        .extract(e, &mut w)
        .and_then(|()| w.flush().map_err(FormatError::from));
    drop(w);
    if result.is_err() {
        let _ = std::fs::remove_file(target);
    }
    result
}

fn extract(
    path: &Path,
    dir: &Path,
    priors: &[PathBuf],
    keys: &Keys,
    out: &mut dyn Write,
) -> Result<(), FormatError> {
    let mut a = open(path, priors, keys)?;
    let all = entries(&mut a)?;
    // Refuse before writing anything.
    for e in &all {
        if e.kind == EntryKind::Symlink {
            return Err(FormatError::SymlinkRefused {
                path: e.path.clone(),
            });
        }
        check_extraction_path(&e.path)?;
    }
    std::fs::create_dir_all(dir)?;
    let (mut files, mut dirs) = (0u64, 0u64);
    for e in &all {
        let target = join_components(dir, &e.path);
        match e.kind {
            EntryKind::Directory => {
                std::fs::create_dir_all(&target)?;
                dirs += 1;
            }
            EntryKind::File => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                extract_file(&mut a, e, &target)?;
                files += 1;
            }
            EntryKind::Symlink => {}
        }
    }
    writeln!(out, "extracted {files} files, {dirs} directories")?;
    Ok(())
}

/// Run the tool with `args` (the first is the program name); returns the exit
/// status: 0 on success, 1 on an archive or I/O error, 2 on a usage error.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let mut cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let code = if e.use_stderr() { 2 } else { 0 };
            let text = e.to_string();
            let sink: &mut dyn Write = if code == 0 { out } else { err };
            let _ = sink.write_all(text.as_bytes());
            return code;
        }
    };
    let keys = match credentials(&mut cli) {
        Ok(k) => k,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            return 1;
        }
    };
    let result = match &cli.command {
        Command::List { archive } => list(archive, &cli.priors, &keys, out),
        Command::Verify { archive } => verify(archive, &cli.priors, &keys, out),
        Command::Extract { archive, dir } => extract(archive, dir, &cli.priors, &keys, out),
        Command::Info { archive } => info(archive, &cli.priors, &keys, out),
        Command::Check { archive } => check(archive, &keys, out),
        Command::Rollback {
            archive,
            generation,
        } => rollback_cmd(archive, *generation, out),
        Command::Repair {
            archive,
            out: target,
        } => repair_cmd(archive, target, &keys, out),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            1
        }
    }
}
