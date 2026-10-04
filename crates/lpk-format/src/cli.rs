//! The logic of the `lpk-decode` reference tool, as a function the binary and
//! the tests both call.

use crate::archive::Archive;
use crate::entry::{Entry, EntryKind};
use crate::envelope::Resources;
use crate::error::FormatError;
use crate::trailer::TRAILER_FRAME_LEN;
use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// Reference decoder for `.lpk` archives.
#[derive(Debug, Parser)]
#[command(name = "lpk-decode", version, about)]
struct Cli {
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
}

const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

fn is_reserved_device(component: &str) -> bool {
    // Windows ignores everything from the first dot on.
    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.to_ascii_uppercase();
    if RESERVED.contains(&upper.as_str()) {
        return true;
    }
    for prefix in ["COM", "LPT"] {
        if let Some(d) = upper.strip_prefix(prefix) {
            if matches!(d.as_bytes(), [b'1'..=b'9']) {
                return true;
            }
        }
    }
    false
}

/// The extraction tool's path policy on top of the format's path rules: a
/// component may not be a Windows device name (`CON`, `PRN`, `AUX`, `NUL`,
/// `COM1`-`COM9`, `LPT1`-`LPT9`, in any case, with or without an extension),
/// contain `:`, or end in a dot or a space.
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

fn open(path: &Path) -> Result<Archive<File>, FormatError> {
    Archive::open(File::open(path)?, &Resources::default())
}

fn entries(a: &mut Archive<File>) -> Result<Vec<Entry>, FormatError> {
    let t = a.entry_table()?;
    t.table()?.iter().collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn list(path: &Path, out: &mut dyn Write) -> Result<(), FormatError> {
    let mut a = open(path)?;
    for e in entries(&mut a)? {
        writeln!(out, "{}\t{}\t{}", e.kind.name(), e.size, e.path)?;
    }
    Ok(())
}

fn verify(path: &Path, out: &mut dyn Write) -> Result<(), FormatError> {
    let mut a = open(path)?;
    let s = a.verify()?;
    writeln!(
        out,
        "ok: {} entries, {} chunks, {} blocks",
        s.entries, s.chunks, s.blocks
    )?;
    Ok(())
}

fn info(path: &Path, out: &mut dyn Write) -> Result<(), FormatError> {
    let mut a = open(path)?;
    let h = *a.header();
    let t = *a.trailer();
    let e = a.index().envelope;
    let entries = a.entry_table()?.len();
    let archive_len = t.index_offset + t.index_len + TRAILER_FRAME_LEN;
    writeln!(out, "format: {}.{}", h.version.major, h.version.minor)?;
    writeln!(out, "header flags: {:#x}", h.flags.bits())?;
    writeln!(out, "archive id: {}", hex(&h.archive_id))?;
    writeln!(out, "generation: {}", t.generation)?;
    writeln!(out, "length: {archive_len} bytes")?;
    writeln!(out, "entries: {entries}")?;
    writeln!(out, "chunks: {}", a.chunks().len())?;
    writeln!(out, "blocks: {}", a.index().blocks.len())?;
    writeln!(out, "records: {}", a.index().records.is_some())?;
    writeln!(out, "merkle root: {}", hex(&a.index().merkle_root))?;
    writeln!(out, "envelope max_window: {}", e.max_window)?;
    writeln!(out, "envelope max_bwt_block: {}", e.max_bwt_block)?;
    writeln!(out, "envelope max_block_plain: {}", e.max_block_plain)?;
    writeln!(out, "envelope max_frame_payload: {}", e.max_frame_payload)?;
    writeln!(out, "envelope decode_memory: {}", e.decode_memory)?;
    writeln!(out, "envelope threads_hint: {}", e.threads_hint)?;
    Ok(())
}

fn extract(path: &Path, dir: &Path, out: &mut dyn Write) -> Result<(), FormatError> {
    let mut a = open(path)?;
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
        let target = dir.join(&e.path);
        match e.kind {
            EntryKind::Directory => {
                std::fs::create_dir_all(&target)?;
                dirs += 1;
            }
            EntryKind::File => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                // Never overwrite: an existing file or link at the target is an error.
                let mut w = BufWriter::new(
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&target)?,
                );
                a.extract(e, &mut w)?;
                w.flush()?;
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
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let code = if e.use_stderr() { 2 } else { 0 };
            let text = e.to_string();
            let sink: &mut dyn Write = if code == 0 { out } else { err };
            let _ = sink.write_all(text.as_bytes());
            return code;
        }
    };
    let result = match &cli.command {
        Command::List { archive } => list(archive, out),
        Command::Verify { archive } => verify(archive, out),
        Command::Extract { archive, dir } => extract(archive, dir, out),
        Command::Info { archive } => info(archive, out),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            1
        }
    }
}
