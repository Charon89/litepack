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

fn list(path: &Path, out: &mut dyn Write) -> Result<(), FormatError> {
    let mut a = open(path)?;
    for e in entries(&mut a)? {
        let path = escape_path(&e.path);
        writeln!(out, "{}\t{}\t{}", e.kind.name(), e.size, path)?;
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

/// Write one file next to its final name and rename it into place when it is
/// complete; on any error the partial file is removed. An existing file or link
/// at the target is an error (nothing is overwritten).
fn extract_file(a: &mut Archive<File>, e: &Entry, target: &Path) -> Result<(), FormatError> {
    if std::fs::symlink_metadata(target).is_ok() {
        return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists).into());
    }
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".lpk-partial");
    let tmp = target.with_file_name(name);
    let result = (|| -> Result<(), FormatError> {
        let mut w = BufWriter::new(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?,
        );
        a.extract(e, &mut w)?;
        w.flush()?;
        drop(w);
        std::fs::rename(&tmp, target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
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
