//! The `lpk` command line: `a` creates an archive from a directory, `x` extracts, `t` tests,
//! `check` and `repair` drive the recovery records, `append` adds a generation (a snapshot of
//! a directory, deduplicated against the archive's chunks), `rollback` truncates to an earlier
//! generation and `info` lists them.
//!
//! Extraction goes through `lpk-core`'s block-ordered engine (`lpk_core::extract_file`: each
//! needed block decoded once, blocks decoded in parallel under a memory bound) with the
//! engine's policy module (`lpk_core::policy`: the reference tool's path rules, case
//! collisions on a case-insensitive target, `--overwrite refuse|skip|rename|replace`, symlink
//! entries skipped with a warning unless `--symlinks`). Testing goes through the reference
//! tool's code (`lpk_format::cli::run_with`). Both use
//! `lpk-core`'s full reader (revision 1.1's `jpeg-reconstruct`), so peeled files extract and
//! verify.
//! Encryption: `--password`, `--password-file`, `--password-prompt` and `--keyfile` are read
//! here for every command (the format tool's rules: one trailing newline is not part of a
//! password file; a keyfile alone means an empty password) into memory that is wiped
//! ([`zeroize::Zeroizing`], then the format's `Credentials`) and handed to the reference tool
//! as a typed value (`lpk_format::cli::run_with_credentials`), never as an argument string. A
//! password is never printed or logged. `x` opens the key slot before anything is written: a
//! wrong or missing password leaves no output directory. A `--keyfile` on an archive whose
//! key slot takes none is refused (exit 2, `UnneededKeyfile`).
//! Prompting ([`prompt`]): `--password-prompt` reads the password from the terminal without
//! echo (twice on `a`); without it a prompt happens only when stdin and stderr are terminals
//! and the archive needs a credential that was not given; through a pipe (the benchmark
//! runner) there is never a prompt, the command fails with `PasswordRequired`.
//! Tiers: `--fast` (the default), `--balanced`, `--store`, or `--tier <name>` as their long form
//! (what the GUI mirrors); `--strong` and `--tier strong` are reserved for the Strong tier
//! (Phase 2) and refused as a usage error.
//! Listing: `l` streams the entry table ([`list`]): kind, size, mtime and path per line,
//! `--long` for the flags and the generation, `--json` for one object per entry per line; a
//! listable sealed archive lists without its key, a sealed one that is not says
//! `PasswordRequired`.
//! Export: `export <archive> <out.zip|out.7z>` streams the archive's entries, in entry order,
//! into a ZIP or a 7z file through `lpk-core`'s export engine ([`export`]: the same plan and
//! memory bound as `x`, the blocks decoded in parallel and cached by a schedule computed from
//! the whole plan) without a tree on disk; `--method`, `--encrypt` with the archive's
//! password, `--force` to replace an existing output.
//! Exit codes: 0 ok, 1 usage error, 2 a refused or failed operation ([`failure`]: one line
//! `error: ...` on stderr, or with the global `--json` flag one JSON object line).
//! Console ([`console`]): a progress line on stderr, rewritten in place, only when stderr is
//! a terminal (never through a pipe) and not under `--quiet`, `--no-progress` or `--json`;
//! `--quiet` leaves nothing on stderr but errors (stdout untouched); Ctrl-C cancels `a`,
//! `append`, `x` and `export` through the engine's token (exit 2, `cancelled`; a second
//! Ctrl-C exits at once); the `-v` reports are unchanged.
#![forbid(unsafe_code)]

pub mod console;
/// The ZIP and 7z writers of `lpk export` (in `lpk-api` since E5-1).
pub use lpk_api::export;
pub mod failure;
pub mod list;
pub mod prompt;

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, CommandFactory, Parser, Subcommand};
use lpk_api::{Session, Tier};
use lpk_core::{CancelToken, RunSummary};
use lpk_format::{Credentials, FormatError, Suite};
use zeroize::Zeroizing;

pub use console::Console;
pub use export::{Container, Method};
pub use failure::{core_exit, format_exit, EntryFailure, Failure};
pub use list::{list_archive, ListError, ListOptions};
pub use prompt::Terminal;

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
    after_help = "Exit codes: 0 ok, 1 usage error, 2 an operation was refused or failed. A failure \
                  is one line on stderr, `error: <message>`; with --json it is one JSON object \
                  line instead: {\"error\":{\"code\":1|2,\"kind\":\"<the error's variant name; \
                  Usage for a command-line error; UnneededKeyfile for a --keyfile the archive \
                  does not take>\",\"message\":\"...\",\"path\":\"<the path the error \
                  names, else the archive>\",\"entries\":[{\"path\",\"kind\",\"message\"}, the \
                  per-entry errors of a partial extraction, when there are some]}}. A password \
                  is asked for on the terminal only with --password-prompt, or when stdin and \
                  stderr are terminals and the archive needs one that was not given; never \
                  through a pipe. Ctrl-C during a, append, x or export cancels the command: \
                  it cleans up (a new archive removed, an append cut back, unfinished files \
                  removed, an export's output removed) and exits 2 with `cancelled`; a second \
                  Ctrl-C exits at once with code 2, skipping the cleanup (an append cut this way is \n                  restored with `lpk rollback`). The other commands \
                  end at Ctrl-C as any process does."
)]
struct Cli {
    /// Machine-readable output: `l` prints one JSON object per entry per line, and any failing
    /// command prints exactly one JSON object line on stderr instead of `error: ...`.
    #[arg(long, global = true)]
    json: bool,
    /// Nothing on stderr but errors: no progress line, no warning, no note, no -v report.
    /// Stdout is untouched (the listing, the counts, info).
    #[arg(long, global = true)]
    quiet: bool,
    /// No progress line. (The line is shown only when stderr is a terminal: never through a
    /// pipe, and not with --json or --quiet.)
    #[arg(long, global = true)]
    no_progress: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create an archive: from the contents of one directory (not the directory itself), or
    /// from several inputs, files and directories, each entered under its own name.
    #[command(name = "a", long_about = ADD_ABOUT)]
    Add(AddArgs),
    /// List an archive, one line per entry: kind, size, mtime (UTC), path (tab-separated).
    #[command(name = "l", long_about = LIST_ABOUT)]
    List(ListArgs),
    /// Extract an archive (every entry, or those the patterns select) into a directory; an
    /// existing file is refused unless --overwrite says otherwise, and symlink entries are
    /// skipped unless --symlinks is given.
    #[command(name = "x", long_about = EXTRACT_ABOUT)]
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
    /// Also recovers an archive whose last append was cut short. No key is needed: the
    /// credential flags, --password-prompt included, are accepted and ignored.
    Rollback(RollbackArgs),
    /// Print an archive's header, envelope, counts and generations.
    Info(InfoArgs),
    /// Write every entry of an archive into a ZIP or 7z file (by the output's extension),
    /// streamed in entry order: no tree on disk, the blocks decoded as for `x`.
    #[command(long_about = EXPORT_ABOUT)]
    Export(ExportArgs),
}

/// The long help of `export`.
const EXPORT_ABOUT: &str = "\
Write every entry of an archive into a ZIP or a 7z file, chosen by the output's extension \
(.zip or .7z; anything else is a usage error). The entries are streamed in entry order \
through the extraction engine (the same plan, thread budget and memory budget as `x`; the \
decoded blocks are cached under the budget by a schedule computed from the whole plan, so a \
deduplicated chunk referring back to an earlier block is decoded again only when the cache \
cannot hold it): no tree is written on disk and no whole file is held in memory.

ZIP: one entry per archive entry, Zip64 where an entry or the directory needs it, UTF-8 \
names, the modification time as DOS time and as the extended-timestamp field, a Unix mode \
with the executable flag and the DOS read-only, hidden and system bits; a symlink entry is a \
ZIP symlink (its target as the stored content); --method deflate (default), zstd (method 93) \
or store; --encrypt writes AES-256 (AE-2) entries with the archive's password.

7z: one entry per archive entry, each in its own stream (an empty file as a stream-less \
entry); --method lzma2 (default), deflate or store (zstd inside 7z is a fork extension 7-Zip \
does not read: refused); --encrypt writes AES-256 with an encrypted header.

Names are written as the archive stores them: the format's reader already refuses absolute \
paths and `..`; a colon, a device name or a trailing dot or space goes through as-is for the \
consumer's extractor to sanitise. Both writers run in process. The archive, its key slot and \
the plan are checked before the output exists; the output is refused when it is the archive \
itself, and when it exists unless --force replaces it (touched only once the export writes). \
A failed export removes the output; a panic leaves it in place. Outside the memory budget: \
the writer's directory, kept until the end (ZIP's central directory about 200 bytes plus the \
name per entry, 7z's entry table about 100 plus the name), and its compressor (Deflate under \
1 MiB, zstd a few MiB, LZMA2 about eleven times its 16 MiB dictionary); -v prints the figures.";

/// The long help of `a`: the naming rule of several inputs, the list file and the excludes.
const ADD_ABOUT: &str = "\
Create an archive. With one directory as the input, its contents are archived (not the \
directory itself: `lpk a out.lpk D:\\x\\photos` holds `IMG_1.jpg`, not `photos/IMG_1.jpg`). \
With several inputs, or a file, or --list, every input is entered under its own name: a \
relative path as given (`sub/photos` becomes `sub/photos/...`, `notes.txt` becomes \
`notes.txt`), an absolute path or one leaving the current directory through `..` under its \
last component (`D:\\x\\photos` becomes `photos/...`). Two inputs that would be entered \
under the same name, or one under the other (`a` and `a/b`), are refused before any output \
(exit 1, naming both).

--list FILE reads more inputs from FILE, one path per line (UTF-8, CRLF tolerated; a line \
starting with `#` and an empty line are ignored), each treated as above, a relative path \
resolved against the current directory.

--exclude GLOB (repeatable) leaves out every entry whose archive path matches: globs over \
the entry path with `/` separators, `*` and `?` within one component, `**` across \
components, a backslash read as `/`, case-sensitive; an excluded directory is not walked \
into. The exclude applies to the inputs' own names too.";

/// The long help of `x`: the patterns.
const EXTRACT_ABOUT: &str = "\
Extract an archive into a directory, created if missing. Without a pattern every entry is \
extracted. Each PATTERN is a glob over entry paths (`/` separators; `*` and `?` within one \
component, `**` across components, `[...]` a class, `{a,b}` an alternation, a backslash read \
as `/`, case-sensitive) and selects every entry it matches and, for a directory, the subtree \
below it: `docs` selects `docs/b.md` and `docs/deep/r.bin`, `**/*.jpg` every JPEG. Only the \
blocks the selected entries need are decoded; the directories a selected path implies are \
created. A pattern that matches no entry is refused before any output (exit 2, naming it). An \
existing file is refused unless --overwrite says otherwise; symlink entries are skipped \
unless --symlinks is given.";

/// The long help of `l`: the columns and the keys of a `--json` line.
const LIST_ABOUT: &str = "\
List an archive, one line per entry, streamed in table order: kind (File, Directory or \
Symlink), size (bytes; a symlink's target length), mtime (UTC, ISO 8601 to the second; `-` \
when the archive records none) and path, tab-separated, control characters in the path \
escaped as the reference tool escapes them (`\\n`, `\\t`, `\\u{1}`). --long adds the flags \
column (`xhrs`: executable, hidden, read-only, system; `-` for a flag not set) before the \
path, a symlink's target after ` -> `, and a first line `generation<TAB>N` with the archive's \
generation (the format records one per archive, not per entry).

With --json each line is one object: \"kind\", \"size\", \"mtime\" (the string above, or \
null), \"mtime_ns\" (nanoseconds since the Unix epoch, or null), \"flags\" (an array of \
\"executable\", \"hidden\", \"read-only\", \"system\"), \"flag_bits\" (the raw flag word), \
\"path\" (unescaped: a JSON string), \"symlink_target\" (a symlink's target, else null), \
\"chunks\" (the number of chunks the file references) and \"generation\" (the archive's).

Each PATTERN is a glob over entry paths, as for `x` (a directory's match lists its subtree); \
only the matching entries are listed, and a pattern that matches no entry is refused before \
any line is written (exit 2, naming it).

A sealed archive written with --listable lists without its key; one that was not is refused \
with PasswordRequired (exit 2) unless a credential is given.";

/// The key material of an encrypted archive; `a` seals with it, every other command opens
/// with it. A password (one of the three forms) or a keyfile or both.
#[derive(Args)]
#[group(id = "credential", multiple = true)]
struct CredentialArgs {
    /// The password. Visible in the process list and the shell history; prefer
    /// --password-file or --password-prompt.
    #[arg(long, value_name = "STR", value_parser = secret,
          conflicts_with_all = ["password_file", "password_prompt"])]
    password: Option<Zeroizing<String>>,
    /// A file holding the password; one trailing newline is not part of it.
    #[arg(long, value_name = "PATH", conflicts_with = "password_prompt")]
    password_file: Option<PathBuf>,
    /// Ask for the password on the terminal, typed without echo (twice for `a`). Needs stdin
    /// and stderr to be terminals, else refused (exit 1). Without this flag the password is
    /// asked for only when the archive needs one that was not given and stdin and stderr are
    /// terminals; through a pipe the command fails with PasswordRequired instead.
    #[arg(long)]
    password_prompt: bool,
    /// A keyfile as a second factor (alone: the password is empty). Refused on an archive
    /// whose key slot takes none.
    #[arg(long, value_name = "PATH")]
    keyfile: Option<PathBuf>,
}

/// `--password`'s value in memory that is wiped when it is dropped.
fn secret(s: &str) -> Result<Zeroizing<String>, String> {
    Ok(Zeroizing::new(s.to_string()))
}

/// The password is redacted (as `lpk_format::Credentials` does); the two paths are shown.
impl std::fmt::Debug for CredentialArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialArgs")
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("password_file", &self.password_file)
            .field("password_prompt", &self.password_prompt)
            .field("keyfile", &self.keyfile)
            .finish()
    }
}

/// Whether the credentials seal a new archive (`a`) or open an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Use {
    /// `a`: a prompted password is confirmed; no archive to check the keyfile against.
    Create,
    /// Every other command: a `--keyfile` the archive's key slot does not take is refused.
    Open,
}

/// The kind of the refusal of a `--keyfile` on an archive whose key slot takes none.
pub const UNNEEDED_KEYFILE: &str = "UnneededKeyfile";

impl CredentialArgs {
    /// True when any credential was given (a prompt asked for counts: it is read before the
    /// command runs).
    fn given(&self) -> bool {
        self.password.is_some()
            || self.password_file.is_some()
            || self.password_prompt
            || self.keyfile.is_some()
    }

    /// The credentials, read as the format tool reads them, in memory that is wiped: the
    /// password moves out of the parsed arguments (or is read from its file, or asked for on
    /// `terminal` with `--password-prompt`, confirmed for [`Use::Create`]) into
    /// `Credentials`, which wipes it on drop. `None` when nothing was given. A credential file
    /// that cannot be read names that file; a `--keyfile` on an existing archive whose key
    /// slot takes none is refused ([`UNNEEDED_KEYFILE`], exit 2) before the command runs.
    fn resolve(
        &mut self,
        terminal: &Terminal,
        archive: &Path,
        purpose: Use,
    ) -> Result<Option<Credentials>, Failure> {
        let unreadable = |f: &Path, e: std::io::Error| {
            Failure::failed("Io", format!("{}: {e}", f.display()), Some(f))
        };
        let keyfile = match &self.keyfile {
            Some(f) => {
                let bytes = std::fs::read(f).map_err(|e| unreadable(f, e))?;
                if purpose == Use::Open {
                    check_keyfile_needed(archive)?;
                }
                Some(bytes)
            }
            None => None,
        };
        let password: Option<Zeroizing<Vec<u8>>> = if self.password_prompt {
            Some(terminal.read_password(archive, purpose == Use::Create)?)
        } else {
            match (self.password.take(), &self.password_file) {
                (Some(p), _) => {
                    // The bytes move; the wrapper left behind is wiped as it drops.
                    let mut p = p;
                    Some(Zeroizing::new(std::mem::take(&mut *p).into_bytes()))
                }
                (None, Some(f)) => {
                    let mut b = Zeroizing::new(std::fs::read(f).map_err(|e| unreadable(f, e))?);
                    if b.ends_with(b"\n") {
                        b.pop();
                        if b.ends_with(b"\r") {
                            b.pop();
                        }
                    }
                    Some(b)
                }
                (None, None) => None,
            }
        };
        Ok(match (password, keyfile) {
            (None, None) => None,
            (p, keyfile) => Some(Credentials {
                password: p.map(|mut p| std::mem::take(&mut *p)).unwrap_or_default(),
                keyfile,
            }),
        })
    }
}

/// The refusal of a `--keyfile` on an archive that is encrypted with a password alone: the
/// header and the key slot are read, nothing more (a plain archive, or one that cannot be
/// read, passes: the command reports it). Exit 2 as a refused operation naming the archive
/// (D-77's table: the command line alone is not at fault, the archive decides).
fn check_keyfile_needed(archive: &Path) -> Result<(), Failure> {
    let Ok(mut file) = std::fs::File::open(archive) else {
        return Ok(());
    };
    let Ok(header) = lpk_format::Header::read(&mut file) else {
        return Ok(());
    };
    if !header.flags.contains(lpk_format::HeaderFlags::ENCRYPTED) {
        return Ok(());
    }
    match lpk_format::read_key_slot(&mut file) {
        Ok((slot, _)) if !slot.keyfile_required => Err(Failure::failed(
            UNNEEDED_KEYFILE,
            format!(
                "{}: the archive's key slot takes no keyfile; --keyfile is not needed",
                archive.display()
            ),
            Some(archive),
        )),
        _ => Ok(()),
    }
}

/// Run `f` with the credentials the command line gave; when it reports that the archive
/// needs a credential (`PasswordRequired`, `AppendNeedsCredentials`), none was given and
/// `terminal` is interactive, ask for the password once and run `f` again with it. Every
/// command this covers refuses before it writes anything, so the second run starts clean.
fn with_credentials(
    given: Option<Credentials>,
    args: &CredentialArgs,
    archive: &Path,
    terminal: &Terminal,
    mut f: impl FnMut(Option<&Credentials>) -> Result<(), Failure>,
) -> Result<(), Failure> {
    match f(given.as_ref()) {
        Err(e)
            if given.is_none()
                && !args.given()
                && terminal.interactive()
                && matches!(
                    e.kind.as_str(),
                    "PasswordRequired" | "AppendNeedsCredentials"
                ) =>
        {
            let mut password = terminal.read_password(archive, false)?;
            let credentials = Credentials {
                password: std::mem::take(&mut *password),
                keyfile: None,
            };
            f(Some(&credentials))
        }
        r => r,
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

/// The refusal of `--dict-size` under a tier other than Balanced, whatever its spelling.
const DICT_SIZE_BALANCED: &str =
    "the argument '--dict-size' requires the Balanced tier (--balanced or --tier balanced)";

/// The refusal of `--bwt` under a tier other than Balanced (the Fast tier never uses it).
const BWT_BALANCED: &str =
    "the argument '--bwt' requires the Balanced tier (--balanced or --tier balanced)";

/// The refusal of `--png-filter` under a tier other than Balanced.
const PNG_FILTER_BALANCED: &str =
    "the argument '--png-filter' requires the Balanced tier (--balanced or --tier balanced)";

/// The refusal of `--png-filter on` with the Deflate peel off (the re-encoding is part of it).
const PNG_FILTER_NEEDS_PEEL: &str =
    "the argument '--png-filter on' needs the Deflate peel (it cannot be used with '--deflate-peel off')";

/// The value of `--tier`: the long form of the tier flags, which the GUI mirrors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum TierArg {
    Fast,
    Balanced,
    Store,
    /// Reserved (Phase 2): refused.
    Strong,
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
    #[arg(long, value_name = "BYTES")]
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
    /// path order, which keeps directory locality), `extension` (by extension, then name, the
    /// order 7-Zip uses for solid archives) or `versions` (sibling folders that share at least
    /// half of the smaller one's files are versions: a file's versions are written next to each
    /// other, in the natural order of the folder names; without such folders, path order).
    /// Ignored with --no-dedup and --store.
    #[arg(long, value_enum, value_name = "ORDER", default_value = "none")]
    ordering: OrderingArg,
    /// The Deflate peel: Deflate streams inside ZIP, Office, JAR, PNG, gzip and PDF files (and
    /// zlib streams found in other binary files) stored as their plain text plus a
    /// reconstruction record that restores them bit for bit (revision 1.2). Default: on with
    /// --balanced, off with --fast. An append runs it only on a revision 1.2 archive.
    #[arg(long, value_enum, value_name = "ON|OFF", conflicts_with = "store")]
    deflate_peel: Option<OnOff>,
    /// The JPEG peel: baseline and progressive JPEGs stored as a Lepton stream plus a
    /// reconstruction record that restores them bit for bit (revision 1.1). Default: on with
    /// --fast and --balanced. Off stores JPEGs as they are, so extracting them is a copy
    /// (E2-25, the Fast tier's extraction on photographs).
    #[arg(long, value_enum, value_name = "ON|OFF", conflicts_with = "store")]
    jpeg_peel: Option<OnOff>,
    /// The Balanced tier's BWT stage (revision 1.3): a block of text is also tried as a
    /// Burrows-Wheeler transform followed by LZMA and written so when that is smaller. Default:
    /// on with --balanced; the Fast tier never uses it. An append runs it only on a revision
    /// 1.3 archive.
    #[arg(long, value_enum, value_name = "ON|OFF", conflicts_with = "store")]
    bwt: Option<OnOff>,
    /// The Balanced tier's PNG row-filter re-encoding (revision 1.4), part of the Deflate peel:
    /// a PNG's image data is also tried with every row filtered with Up, and written so (with
    /// the original filter bytes in a record that restores them) when that is smaller. Default:
    /// on with --balanced; the Fast tier never uses it. An append runs it only on a revision 1.4
    /// archive.
    #[arg(long, value_enum, value_name = "ON|OFF", conflicts_with = "store")]
    png_filter: Option<OnOff>,
}

impl TierArgs {
    /// The tier chosen, or the usage error that refuses the command line before anything is
    /// written: `--strong` and `--tier strong` (the Strong tier arrives in Phase 2), and a
    /// setting of one tier given with `--tier` naming another (the rules clap applies to the
    /// tier flags, so `--tier X` and `--X` accept the same command lines); `--dict-size`, a
    /// setting of the Balanced tier alone, is checked here for every spelling.
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
        if self.dict_size.is_some() && tier != Tier::Balanced {
            return Err(DICT_SIZE_BALANCED.to_string());
        }
        if self.bwt.is_some() && tier != Tier::Balanced {
            return Err(BWT_BALANCED.to_string());
        }
        if self.png_filter.is_some() && tier != Tier::Balanced {
            return Err(PNG_FILTER_BALANCED.to_string());
        }
        if self.png_filter == Some(OnOff::On) && self.deflate_peel == Some(OnOff::Off) {
            return Err(PNG_FILTER_NEEDS_PEEL.to_string());
        }
        if self.tier.is_some() {
            let settings: &[(&str, bool)] = match tier {
                Tier::Fast => &[],
                Tier::Balanced => &[("--no-dictionaries", self.no_dictionaries)],
                Tier::Store => &[
                    ("--level", self.level.is_some()),
                    ("--window-log", self.window_log.is_some()),
                    ("--deflate-peel", self.deflate_peel.is_some()),
                    ("--jpeg-peel", self.jpeg_peel.is_some()),
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
    /// The inputs: one directory whose contents are archived, or several files and
    /// directories each entered under its own name (see the long help, --help).
    #[arg(value_name = "PATH", required_unless_present = "list")]
    inputs: Vec<PathBuf>,
    /// A file naming more inputs, one path per line (`#` lines ignored), each entered under
    /// its own name.
    #[arg(long, value_name = "FILE")]
    list: Option<PathBuf>,
    /// Leave out every entry whose archive path matches this glob (repeatable; a matched
    /// directory is not walked into).
    #[arg(long, value_name = "GLOB")]
    exclude: Vec<String>,
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
    Versions,
}

impl From<OrderingArg> for lpk_core::FileOrder {
    fn from(o: OrderingArg) -> Self {
        match o {
            OrderingArg::None => lpk_core::FileOrder::None,
            OrderingArg::Extension => lpk_core::FileOrder::Extension,
            OrderingArg::Versions => lpk_core::FileOrder::Versions,
        }
    }
}

#[derive(Debug, Args)]
struct ExtractArgs {
    /// The archive.
    archive: PathBuf,
    /// The directory to extract into; created if missing.
    outdir: PathBuf,
    /// Globs over entry paths selecting what to extract (a directory's match selects its
    /// subtree); none selects everything.
    #[arg(value_name = "PATTERN")]
    patterns: Vec<String>,
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
    /// What an existing file at a target gets. refuse: the extraction is refused before any
    /// output (the default). skip: the file is left as it is and counted (`skipped <n> existing
    /// entries` on stderr; with --json one `{"summary":{...}}` line). rename: the entry is
    /// written beside it as `<stem> (<k>)<.ext>` with the smallest free k, and counted.
    /// replace: the file is replaced (a link at the target is removed as a link, never
    /// followed). A directory where a file goes, or a file where a directory goes, is refused
    /// in every mode but skip, which skips it (and everything below a skipped directory).
    /// Directories are created or reused whatever the mode.
    #[arg(long, value_enum, value_name = "MODE", default_value_t = OverwriteArg::Refuse)]
    overwrite: OverwriteArg,
    /// Create symlink entries with the target the archive holds. Without this flag every
    /// symlink entry is skipped with a warning on stderr (with --json one
    /// `{"warning":{"kind":"SymlinkSkipped","path":...}}` line). Even with it, a link whose
    /// target is absolute, a UNC path, a device path (`\\?\`, `\\.\`), drive-relative (`C:x`),
    /// rooted (`\x` or `/x`), names a Windows device, has a colon or a component ending in a dot or
    /// a space, has `..` after a name (`a/../x`), has more leading `..` than the link is deep
    /// below the output directory, or (outside Windows, where `\` is not a separator) contains
    /// a backslash is not created: it is skipped with a warning naming the entry and the
    /// reason (with --json one
    /// `{"warning":{"kind":"SymlinkTargetRefused","path":...,"reason":...}}` line) and counted
    /// in symlinks_skipped; such a target is never read or opened. On Windows a link is a
    /// directory link when its target names a directory of the archive, else a file link; so a
    /// link whose target goes through another link (`l -> s/sub`, `s` a link) is a file link
    /// unless the archive itself names that target as a directory. On Windows links are
    /// created only when the process token holds SeCreateSymbolicLinkPrivilege (an elevated
    /// administrator; Developer Mode alone does not count); without it every link is skipped
    /// with a warning (with --json one `{"warning":{"kind":"SymlinkNoPrivilege","path":...}}`
    /// line) and counted in symlinks_skipped, and the extraction goes on. A link that cannot be
    /// created for any other reason fails the extraction.
    #[arg(long)]
    symlinks: bool,
    /// Mark of the Web (Windows): with `on` (the default), when the archive's
    /// `Zone.Identifier` stream gives zone 3 (Internet) or 4 (Restricted), or cannot be parsed
    /// (read as 3), every extracted file gets a `Zone.Identifier` stream with that `ZoneId`
    /// only (no `HostUrl` or `ReferrerUrl`), written before its first byte; zones 0-2 and
    /// archives without the stream: nothing. A stream that exists and cannot be read (access
    /// denied, a sharing violation) is read as 3 with one warning (with --json one
    /// `{"warning":{"kind":"MotwZoneUnreadable"}}` line). A destination whose volume has no
    /// named streams cannot hold the mark: the extraction goes on and prints one warning (with
    /// --json one `{"warning":{"kind":"MotwUnmarked","zone":N}}` line). `off` writes no stream.
    #[arg(long, value_enum, value_name = "on|off", default_value_t = OnOff::On)]
    motw: OnOff,
    /// Print the plan's counts, the memory budget and its terms, and the pool size to stderr.
    #[arg(short, long)]
    verbose: bool,
    /// The limits of a foreign archive's sandboxed worker.
    #[command(flatten)]
    foreign: ForeignArgs,
}

/// `lpk x --overwrite`: the modes of [`lpk_core::Overwrite`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum OverwriteArg {
    Refuse,
    Skip,
    Rename,
    Replace,
}

impl From<OverwriteArg> for lpk_core::Overwrite {
    fn from(o: OverwriteArg) -> Self {
        match o {
            OverwriteArg::Refuse => lpk_core::Overwrite::Refuse,
            OverwriteArg::Skip => lpk_core::Overwrite::Skip,
            OverwriteArg::Rename => lpk_core::Overwrite::Rename,
            OverwriteArg::Replace => lpk_core::Overwrite::Replace,
        }
    }
}

#[derive(Debug, Args)]
struct ListArgs {
    /// The archive.
    archive: PathBuf,
    /// Globs over entry paths selecting what to list (a directory's match lists its
    /// subtree); none lists everything.
    #[arg(value_name = "PATTERN")]
    patterns: Vec<String>,
    /// Add the flags column and the symlink targets, after a first line with the archive's
    /// generation.
    #[arg(long)]
    long: bool,
    /// The key material of a sealed archive that was not written with --listable.
    #[command(flatten)]
    credentials: CredentialArgs,
    /// The limits of a foreign archive's sandboxed worker.
    #[command(flatten)]
    foreign: ForeignArgs,
}

/// A limit's value, or `off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrOff {
    Off,
    Value(u64),
}

impl OrOff {
    fn value(self) -> Option<u64> {
        match self {
            OrOff::Off => None,
            OrOff::Value(v) => Some(v),
        }
    }
}

/// `--max-entries`, `--max-expansion`, `--max-cpu`: a positive integer or `off`.
fn or_off(s: &str) -> Result<OrOff, String> {
    if s.eq_ignore_ascii_case("off") {
        return Ok(OrOff::Off);
    }
    match s.parse::<u64>() {
        Ok(v) if v > 0 => Ok(OrOff::Value(v)),
        _ => Err(format!("{s:?}: a positive integer or `off`")),
    }
}

/// The limits of the sandboxed worker that reads a foreign archive (`x` and `l` route a file
/// that is not an `.lpk` to it by its magic; E4-2 recognises only the test fixture, E4-3 adds
/// ZIP, 7z and tar). The worker's progress is what the host observes (bytes it read of the
/// archive, member bytes received); it is stopped after 60 s without progress.
#[derive(Debug, Args)]
struct ForeignArgs {
    /// Foreign archives: the most entries a listing may have (default 10000000; the archive's
    /// length over the format's smallest header always caps it).
    #[arg(long, value_name = "N|off", value_parser = or_off)]
    max_entries: Option<OrOff>,
    /// Foreign archives: the most bytes an extraction writes, as a multiple of the archive's
    /// length (default 10000); refused from the declared sizes before anything is written.
    #[arg(long, value_name = "FACTOR|off", value_parser = or_off)]
    max_expansion: Option<OrOff>,
    /// Foreign archives: the worker's CPU-time ceiling in minutes (default 60); it may use 60 s
    /// past its CPU time at its last progress, up to this.
    #[arg(long, value_name = "MINUTES|off", value_parser = or_off)]
    max_cpu: Option<OrOff>,
}

impl ForeignArgs {
    /// The broker's options: these limits, `memory` as the worker's budget. A debug build
    /// (tests only) also takes the worker, its AppContainer name and extra worker arguments
    /// from `LPK_TEST_WORKER`, `LPK_TEST_APPCONTAINER` and `LPK_TEST_WORKER_ARGS`.
    fn options(&self, memory: Option<u64>) -> lpk_core::foreign::Options {
        let mut limits = lpk_core::foreign::Limits {
            memory,
            ..lpk_core::foreign::Limits::default()
        };
        if let Some(v) = self.max_entries {
            limits.max_entries = v.value();
        }
        if let Some(v) = self.max_expansion {
            limits.max_expansion = v.value();
        }
        if let Some(v) = self.max_cpu {
            limits.max_cpu = v
                .value()
                .map(|m| std::time::Duration::from_secs(m.saturating_mul(60)));
        }
        #[allow(unused_mut)]
        let mut o = lpk_core::foreign::Options {
            limits,
            ..lpk_core::foreign::Options::default()
        };
        #[cfg(debug_assertions)]
        {
            if let Some(w) = std::env::var_os("LPK_TEST_WORKER") {
                o.worker = Some(PathBuf::from(w));
            }
            if let Ok(n) = std::env::var("LPK_TEST_APPCONTAINER") {
                o.appcontainer = Some(n);
            }
            if let Ok(a) = std::env::var("LPK_TEST_WORKER_ARGS") {
                o.worker_args = a.split_whitespace().map(str::to_string).collect();
            }
        }
        o
    }
}

/// True when `archive` is a foreign archive the broker reads (detection by magic).
fn is_foreign(archive: &Path) -> bool {
    matches!(lpk_core::foreign::detect_file(archive), Ok(Some(_)))
}

/// A `--keyfile` on a foreign archive: refused as on an `.lpk` whose key slot takes none.
fn refuse_foreign_keyfile(
    archive: &Path,
    credentials: Option<&Credentials>,
) -> Result<(), Failure> {
    if credentials.is_some_and(|c| c.keyfile.is_some()) {
        return Err(Failure::failed(
            UNNEEDED_KEYFILE,
            format!(
                "{}: a foreign archive takes no keyfile; --keyfile is not needed",
                archive.display()
            ),
            Some(archive),
        ));
    }
    Ok(())
}

/// `lpk x` on a foreign archive: through the sandboxed worker (`lpk_core::foreign`).
fn foreign_extract(
    x: &ExtractArgs,
    credentials: Option<&Credentials>,
    json: bool,
    console: &Console,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    refuse_foreign_keyfile(&x.archive, credentials)?;
    let select =
        lpk_core::Selection::new(&x.patterns).map_err(|e| Failure::core(&e, Some(&x.archive)))?;
    let session = Session::open(
        &x.archive,
        &lpk_api::OpenOptions {
            credentials,
            foreign: x.foreign.options(x.memory),
            motw_off: x.motw == OnOff::Off,
            ..lpk_api::OpenOptions::default()
        },
    )?;
    let zone_warning = session.zone_warning();
    let r = session.extract(
        &lpk_api::Selection::Globs(select),
        &lpk_api::Destination::Dir(x.outdir.clone()),
        &extract_options(x),
        &console.monitor(),
    );
    console.clear();
    let report = r?;
    let lpk_api::Extracted::Foreign(s) = report.outcome else {
        return Err(Failure::failed(
            "Internal",
            "a foreign archive extracted as an .lpk",
            Some(&x.archive),
        ));
    };
    sandbox_warnings(&s.warnings, json, err);
    zone_warning_line(zone_warning.as_deref(), json, err);
    if report.motw_unmarked {
        motw_warning(report.zone, json, err);
    }
    for p in report.skipped_symlinks {
        if json {
            let line = serde_json::json!({
                "warning": { "kind": "SymlinkSkipped", "path": p }
            });
            let _ = writeln!(err, "{line}");
        } else {
            let _ = writeln!(
                err,
                "warning: {}: symlink entry skipped (--symlinks creates it)",
                lpk_format::cli::escape_path(&p)
            );
        }
    }
    unprivileged_warnings(&report.unprivileged_symlinks, json, err);
    refused_target_warnings(&report.refused_symlink_targets, json, err);
    if s.skipped > 0 {
        let _ = writeln!(err, "skipped {} existing entries", s.skipped);
    }
    if s.renamed > 0 {
        let _ = writeln!(err, "renamed {} existing entries", s.renamed);
    }
    if x.verbose {
        let _ = writeln!(
            err,
            "{}: {} archive through the sandboxed worker: {} entries, {} files, {} directories, {} symlinks, {} bytes; listing charge {} bytes",
            x.archive.display(),
            s.format,
            s.entries,
            s.files,
            s.directories,
            s.symlinks,
            s.bytes,
            s.listing_charge
        );
    }
    let _ = writeln!(
        out,
        "extracted {} files, {} directories",
        s.files, s.directories
    );
    Ok(())
}

/// Mark of the Web's one warning (E6-5): files derived from an archive zoned `zone` could not
/// carry the zone (the destination's volume has no named streams, or the write failed):
/// `warning: Mark of the Web: ...`, or with --json
/// `{"warning":{"kind":"MotwUnmarked","zone":N}}`.
fn motw_warning(zone: Option<lpk_core::motw::Zone>, json: bool, err: &mut dyn Write) {
    let Some(z) = zone else { return };
    if json {
        let line = serde_json::json!({
            "warning": { "kind": "MotwUnmarked", "zone": z.id() }
        });
        let _ = writeln!(err, "{line}");
    } else {
        let _ = writeln!(err, "warning: {}", lpk_core::motw::unmarked_warning(z));
    }
}

/// The archive's zone could not be read and was taken as the Internet zone (fail safe): one
/// line, `warning: Mark of the Web: ...`, or with --json
/// `{"warning":{"kind":"MotwZoneUnreadable"}}`.
fn zone_warning_line(warning: Option<&str>, json: bool, err: &mut dyn Write) {
    let Some(w) = warning else { return };
    if json {
        let line = serde_json::json!({ "warning": { "kind": "MotwZoneUnreadable" } });
        let _ = writeln!(err, "{line}");
    } else {
        let _ = writeln!(err, "warning: {w}");
    }
}

/// After an export or a repair (one output file): the warning of [`motw_warning`] when the
/// archive is zoned and the output does not carry the zone.
fn motw_check(zone: Option<lpk_core::motw::Zone>, output: &Path, json: bool, err: &mut dyn Write) {
    if zone.is_some() && lpk_core::motw::read_zone(output) != zone {
        motw_warning(zone, json, err);
    }
}

/// The symlink entries `--symlinks` did not create because this process cannot create links
/// (E6-6: Windows, no SeCreateSymbolicLinkPrivilege), one line each: `warning: PATH: symlink
/// entry skipped: this process lacks SeCreateSymbolicLinkPrivilege`, or with --json
/// `{"warning":{"kind":"SymlinkNoPrivilege","path":...}}`.
fn unprivileged_warnings(paths: &[String], json: bool, err: &mut dyn Write) {
    for p in paths {
        if json {
            let line = serde_json::json!({
                "warning": { "kind": "SymlinkNoPrivilege", "path": p }
            });
            let _ = writeln!(err, "{line}");
        } else {
            let _ = writeln!(
                err,
                "warning: {}: symlink entry skipped: this process lacks SeCreateSymbolicLinkPrivilege",
                lpk_format::cli::escape_path(p)
            );
        }
    }
}

/// The symlink entries `--symlinks` did not create because of their target (E6-0), one line
/// each: `warning: PATH: symlink entry skipped: REASON`, or with --json
/// `{"warning":{"kind":"SymlinkTargetRefused","path":...,"reason":...}}`.
fn refused_target_warnings(
    refused: &[(String, lpk_api::TargetRefusal)],
    json: bool,
    err: &mut dyn Write,
) {
    for (p, why) in refused {
        if json {
            let line = serde_json::json!({
                "warning": { "kind": "SymlinkTargetRefused", "path": p, "reason": why.reason() }
            });
            let _ = writeln!(err, "{line}");
        } else {
            let _ = writeln!(
                err,
                "warning: {}: symlink entry skipped: {why}",
                lpk_format::cli::escape_path(p)
            );
        }
    }
}

/// The broker's launch warnings (an AppContainer temporary folder it could not empty), one
/// line each: `warning: ...`, or with --json `{"warning":{"kind":"SandboxCleanup",...}}`.
fn sandbox_warnings(warnings: &[String], json: bool, err: &mut dyn Write) {
    for w in warnings {
        if json {
            let line = serde_json::json!({
                "warning": { "kind": "SandboxCleanup", "message": w }
            });
            let _ = writeln!(err, "{line}");
        } else {
            let _ = writeln!(err, "warning: {w}");
        }
    }
}

/// `lpk l` on a foreign archive: the checked listing from the sandboxed worker.
fn foreign_list(
    l: &ListArgs,
    credentials: Option<&Credentials>,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    refuse_foreign_keyfile(&l.archive, credentials)?;
    let select =
        lpk_core::Selection::new(&l.patterns).map_err(|e| Failure::core(&e, Some(&l.archive)))?;
    let session = Session::open(
        &l.archive,
        &lpk_api::OpenOptions {
            credentials,
            foreign: l.foreign.options(None),
            ..lpk_api::OpenOptions::default()
        },
    )?;
    sandbox_warnings(&session.warnings()?, json, err);
    let options = ListOptions { long: l.long, json };
    list::list_session(&session, options, select, out, Failure::from)
}

/// The API's extraction options of `lpk x`.
fn extract_options(x: &ExtractArgs) -> lpk_api::ExtractOptions {
    lpk_api::ExtractOptions {
        overwrite: x.overwrite.into(),
        symlinks: if x.symlinks {
            lpk_core::Symlinks::Create
        } else {
            lpk_core::Symlinks::Skip
        },
        threads: x.threads.map(|n| usize::try_from(n).unwrap_or(usize::MAX)),
        memory: x.memory,
    }
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
    /// Accepted for symmetry and ignored (--password-prompt included: nothing is asked for);
    /// no key is needed to roll back.
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
struct ExportArgs {
    /// The archive.
    archive: PathBuf,
    /// The file to write: `.zip` or `.7z` by its extension; must not exist unless --force.
    out: PathBuf,
    /// Globs over entry paths selecting what to export, as for `x`; none exports everything.
    #[arg(value_name = "PATTERN")]
    patterns: Vec<String>,
    /// The compression method of the entries: deflate (ZIP's default), zstd (ZIP only),
    /// store, or lzma2 (7z's default; 7z only).
    #[arg(long, value_enum, value_name = "METHOD")]
    method: Option<MethodArg>,
    /// Encrypt every entry with the archive's password (AES-256: ZIP's AE-2, 7z's with the
    /// header encrypted). Needs a credential with a password; a keyfile alone is refused.
    #[arg(long, requires = "credential")]
    encrypt: bool,
    /// Replace an existing output file.
    #[arg(long)]
    force: bool,
    /// Mark of the Web (Windows), as for `x`: with `on` (the default) the output file gets the
    /// zone of an archive zoned 3 or 4 before its first byte; `off` writes no stream.
    #[arg(long, value_enum, value_name = "on|off", default_value_t = OnOff::On)]
    motw: OnOff,
    /// The thread budget: 1 decodes on the calling thread; from 2 on, N - 1 threads decode
    /// blocks ahead of the writer (capped by the memory budget), as for `x`.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    threads: Option<u64>,
    /// A prior file (for example a zstd dictionary) the archive needs; may be repeated.
    #[arg(long = "prior", value_name = "FILE")]
    priors: Vec<PathBuf>,
    /// The key material of an encrypted archive; checked before anything is written.
    #[command(flatten)]
    credentials: CredentialArgs,
    /// The memory budget, as for `x`: the decoded blocks held at once (cached or in flight)
    /// are derived from it; the writer's own memory (its compressor) is outside the count.
    #[arg(long, value_name = "SIZE", value_parser = parse_memory)]
    memory: Option<u64>,
    /// Print the plan's counts, the cache schedule's outcome and the memory terms to stderr.
    #[arg(short, long)]
    verbose: bool,
}

/// `lpk export --method`: the methods of [`Method`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum MethodArg {
    Deflate,
    Zstd,
    Store,
    /// 7z only.
    Lzma2,
}

impl From<MethodArg> for Method {
    fn from(m: MethodArg) -> Self {
        match m {
            MethodArg::Deflate => Method::Deflate,
            MethodArg::Zstd => Method::Zstd,
            MethodArg::Store => Method::Store,
            MethodArg::Lzma2 => Method::Lzma2,
        }
    }
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
    /// Mark of the Web (Windows), as for `x`: with `on` (the default) the repaired copy gets
    /// the zone of an archive zoned 3 or 4 before its first byte; `off` writes no stream.
    #[arg(long, value_enum, value_name = "on|off", default_value_t = OnOff::On)]
    motw: OnOff,
}

/// The API's settings of a tier and its flags (E5-1: what `build_pipeline` built is now
/// [`lpk_api::CreateSettings::pipeline`]); no seal, recovery 0.
fn build_settings(a: &TierArgs, tier: Tier) -> lpk_api::CreateSettings {
    let on = |o: Option<OnOff>| o.map(|v| v == OnOff::On);
    lpk_api::CreateSettings {
        tier,
        threads: a.threads,
        memory: a.memory,
        level: a.level,
        window_log: a.window_log,
        dict_size: a.dict_size,
        no_dictionaries: a.no_dictionaries,
        no_dedup: a.no_dedup,
        ordering: a.ordering.into(),
        deflate_peel: on(a.deflate_peel),
        jpeg_peel: on(a.jpeg_peel),
        bwt: on(a.bwt),
        png_filter: on(a.png_filter),
        seal: None,
        recovery: 0,
    }
}

fn add(
    a: &AddArgs,
    credentials: Option<Credentials>,
    console: &Console,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let tier = a.tier.tier().map_err(Failure::usage)?;
    // What the `-v` report says the archive is sealed with.
    let sealing = credentials.is_some().then(|| {
        format!(
            "{}{}",
            a.suite.map_or(DEFAULT_SUITE, Suite::from).name(),
            if a.listable { ", listable" } else { "" }
        )
    });
    let mut settings = build_settings(&a.tier, tier);
    settings.seal = credentials.map(|credentials| lpk_api::Seal {
        suite: a.suite.map_or(DEFAULT_SUITE, Suite::from),
        listable: a.listable,
        credentials,
    });
    settings.recovery = a.recovery;
    let sources = sources(a)?;
    let r = lpk_api::create(&sources, &a.archive, &settings, &console.monitor());
    console.clear();
    let s = r?;
    records_cap_note(err, &s.peel.deflate);
    if a.verbose {
        report(err, &a.archive, &a.tier, tier, sealing, a.recovery, &s);
    }
    Ok(())
}

/// The inputs of `lpk a` (the rules of `ADD_ABOUT`): one directory given on the command line
/// alone is archived by its contents; otherwise every input, the list file's included, is
/// entered under its own name. The list file is read here, so an unreadable one is refused
/// before any output; a leading UTF-8 BOM (PowerShell 5's `Out-File`) is dropped, so are
/// empty and `#` lines (`str::lines` drops a trailing CR). No input at all (a list naming
/// none) is a usage error: never a silent empty archive.
fn sources(a: &AddArgs) -> Result<lpk_core::Sources, Failure> {
    let core = |e: lpk_core::CoreError| Failure::core(&e, None);
    let exclude = lpk_core::Selection::new(&a.exclude).map_err(core)?;
    let mut paths: Vec<PathBuf> = a.inputs.clone();
    if let Some(list) = &a.list {
        let text = std::fs::read(list)
            .map_err(|e| Failure::failed("Io", format!("{}: {e}", list.display()), Some(list)))?;
        let text = String::from_utf8(text)
            .map_err(|_| Failure::usage(format!("--list {}: not UTF-8", list.display())))?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            paths.push(PathBuf::from(line));
        }
    }
    if paths.is_empty() {
        return Err(Failure::usage(match &a.list {
            Some(list) => format!(
                "--list {}: no inputs (the file names no path)",
                list.display()
            ),
            None => "no inputs".to_string(),
        }));
    }
    if a.list.is_none() && paths.len() == 1 && paths[0].is_dir() {
        return Ok(lpk_core::Sources {
            roots: vec![lpk_core::Root::contents(&paths[0])],
            exclude,
        });
    }
    let mut roots = Vec::with_capacity(paths.len());
    for p in &paths {
        roots.push(lpk_core::Root::named(p).map_err(core)?);
    }
    lpk_core::Sources::new(roots, exclude).map_err(core)
}

fn append(
    a: &AppendArgs,
    credentials: Option<&Credentials>,
    console: &Console,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let tier = a.tier.tier().map_err(Failure::usage)?;
    // The new generation is sealed with the archive's own key (the suite is the archive's).
    let sealing = credentials
        .is_some()
        .then(|| "the archive's key".to_string());
    let mut settings = build_settings(&a.tier, tier);
    settings.recovery = a.recovery;
    // Through the API's free `append` (what `Session::append` runs once its session is
    // closed): an archive whose last append was cut short must still reach the engine's own
    // refusal, which a session's open would replace.
    let r = lpk_api::append(
        &a.archive,
        &a.input,
        &settings,
        credentials,
        &console.monitor(),
    );
    console.clear();
    let s = r?;
    records_cap_note(err, &s.peel.deflate);
    if a.verbose {
        report(err, &a.archive, &a.tier, tier, sealing, a.recovery, &s);
    }
    Ok(())
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
            "blocks: {} lzma, {} zstd, {} bwt, {} stored by class, {} stored without gain; trial sample bytes {} (bwt candidate {}); encoder memory about {} bytes (approximate: liblzma LZMA2 encoder query at one thread, not measured)",
            b.lzma_blocks,
            b.zstd_blocks,
            b.bwt_blocks,
            b.stored_by_class,
            b.stored_no_gain,
            b.sample_bytes,
            b.bwt_sample_bytes,
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
            "append: stages not run, the archive's revision does not allow them: {}",
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
        "  png filters: {} streams tried with every row Up, {} written re-filtered (png-filter records {} bytes)",
        d.png_tried, d.png_filtered, d.png_record_bytes
    );
    let _ = writeln!(
        err,
        "  seconds: analyse {:.3}, recreate {:.3}, encode {:.3}",
        s.analyse_seconds, s.recreate_seconds, d.encode_seconds
    );
}

fn extract(
    x: &ExtractArgs,
    credentials: Option<&Credentials>,
    json: bool,
    console: &Console,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    if is_foreign(&x.archive) {
        return foreign_extract(x, credentials, json, console, out, err);
    }
    let mut priors = Vec::with_capacity(x.priors.len());
    for p in &x.priors {
        let b = std::fs::read(p)
            .map_err(|e| Failure::failed("Io", format!("{}: {e}", p.display()), Some(p)))?;
        priors.push(b);
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let max = u64::try_from(cores.saturating_mul(4)).unwrap_or(u64::MAX);
    if x.threads.is_some_and(|n| n > max) {
        return Err(Failure::usage(format!(
            "--threads: at most {max} (four times the logical cores) on this machine"
        )));
    }
    let select =
        lpk_core::Selection::new(&x.patterns).map_err(|e| Failure::core(&e, Some(&x.archive)))?;
    let mut zone_warning = None;
    // E5-1: open (the key slot, the envelope against `--memory`, the priors), then extract;
    // every refusal of the open comes before any output exists, as before.
    let r = Session::open(
        &x.archive,
        &lpk_api::OpenOptions {
            credentials,
            memory: x.memory,
            priors,
            motw_off: x.motw == OnOff::Off,
            ..lpk_api::OpenOptions::default()
        },
    )
    .and_then(|session| {
        zone_warning = session.zone_warning();
        session.extract(
            &lpk_api::Selection::Globs(select),
            &lpk_api::Destination::Dir(x.outdir.clone()),
            &extract_options(x),
            &console.monitor(),
        )
    });
    console.clear();
    // On failure the error line is the only stderr output (D-77 (3)): the warnings are
    // printed only after success, one line per skipped symlink entry, then the counts.
    let report = r?;
    let lpk_api::Extracted::Lpk(s) = report.outcome else {
        return Err(Failure::failed(
            "Internal",
            "an .lpk extracted as a foreign archive",
            Some(&x.archive),
        ));
    };
    zone_warning_line(zone_warning.as_deref(), json, err);
    if report.motw_unmarked {
        motw_warning(report.zone, json, err);
    }
    for p in report.skipped_symlinks {
        if json {
            let line = serde_json::json!({
                "warning": { "kind": "SymlinkSkipped", "path": p }
            });
            let _ = writeln!(err, "{line}");
        } else {
            let _ = writeln!(
                err,
                "warning: {}: symlink entry skipped (--symlinks creates it)",
                lpk_format::cli::escape_path(&p)
            );
        }
    }
    unprivileged_warnings(&report.unprivileged_symlinks, json, err);
    refused_target_warnings(&report.refused_symlink_targets, json, err);
    if json {
        if s.skipped > 0 || s.renamed > 0 || s.symlinks_skipped > 0 {
            let line = serde_json::json!({
                "summary": {
                    "skipped": s.skipped,
                    "renamed": s.renamed,
                    "symlinks_skipped": s.symlinks_skipped,
                }
            });
            let _ = writeln!(err, "{line}");
        }
    } else {
        if s.skipped > 0 {
            let _ = writeln!(err, "skipped {} existing entries", s.skipped);
        }
        if s.renamed > 0 {
            let _ = writeln!(err, "renamed {} existing entries", s.renamed);
        }
    }
    if x.verbose {
        let _ = writeln!(
            err,
            "{}: {} files, {} directories, {} symlinks, {} bytes; plan: {} chunk placements over {} of {} blocks; pool: {} workers, {} blocks in flight, {} writers; {} blocks decoded, {} nested decodes, {} files reopened",
            x.archive.display(),
            s.files,
            s.directories,
            s.symlinks,
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
                m.peak_in_flight(s.workers, s.in_flight)
            )
        } else {
            String::new()
        };
        let _ = writeln!(
            err,
            "memory: budget {} bytes ({source}); envelope tier {}; per block {} bytes (encoded {}, plain {}, decoder {}, step input {}, nested {}); fixed {} bytes (shared {}, per worker {}); workers {}; held block {} bytes, blocks in flight {}{key}",
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
            s.workers,
            m.held,
            s.in_flight
        );
    }
    let _ = writeln!(
        out,
        "extracted {} files, {} directories",
        s.files, s.directories
    );
    Ok(())
}

/// `lpk export`: the container by the output's extension, the method by the container, the
/// archive opened with the credentials (its key slot, then the plan and the pool, every
/// refusal before the output exists), the output refused when it is the archive or when it
/// exists without `--force`, then created at the first entry and removed on failure (an
/// existing output replaced under `--force` is touched only once the export writes).
fn export(
    x: &ExportArgs,
    credentials: Option<&Credentials>,
    json: bool,
    console: &Console,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let container = Container::of(&x.out).ok_or_else(|| {
        Failure::usage(format!(
            "{}: the output must end in .zip or .7z",
            x.out.display()
        ))
    })?;
    let method = x
        .method
        .map(Method::from)
        .unwrap_or_else(|| container.default_method());
    if container == Container::Zip && method == Method::Lzma2 {
        return Err(Failure::usage(
            "--method lzma2 is a 7z method; ZIP takes deflate, zstd or store",
        ));
    }
    if container == Container::SevenZ && method == Method::Zstd {
        return Err(Failure::usage(
            "--method zstd inside 7z is a 7-Zip-ZS extension that 7-Zip does not read; 7z takes lzma2, deflate or store",
        ));
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let max = u64::try_from(cores.saturating_mul(4)).unwrap_or(u64::MAX);
    if x.threads.is_some_and(|n| n > max) {
        return Err(Failure::usage(format!(
            "--threads: at most {max} (four times the logical cores) on this machine"
        )));
    }
    let password = if x.encrypt {
        match credentials.map(|c| c.password.as_slice()) {
            Some(p) if !p.is_empty() => Some(p),
            _ => {
                return Err(Failure::usage(
                    "--encrypt needs a password (a keyfile alone gives none)",
                ))
            }
        }
    } else {
        None
    };
    let mut priors = Vec::with_capacity(x.priors.len());
    for p in &x.priors {
        let b = std::fs::read(p)
            .map_err(|e| Failure::failed("Io", format!("{}: {e}", p.display()), Some(p)))?;
        priors.push(b);
    }
    let select =
        lpk_core::Selection::new(&x.patterns).map_err(|e| Failure::core(&e, Some(&x.archive)))?;
    // The output must not be the archive (whatever the spelling of either path), and an
    // existing one is refused unless --force; both before the archive is opened.
    lpk_api::check_export_target(&x.archive, &x.out, x.force)?;
    // The archive, its key slot and its priors: a wrong or missing password, an unreadable
    // archive or an envelope over the budget are refused here, before any output exists.
    let session = Session::open(
        &x.archive,
        &lpk_api::OpenOptions {
            credentials,
            memory: x.memory,
            priors,
            motw_off: x.motw == OnOff::Off,
            ..lpk_api::OpenOptions::default()
        },
    )?;
    let zone = if x.motw == OnOff::On {
        session.zone()
    } else {
        None
    };
    let options = lpk_api::ExportOptions {
        container,
        method,
        password,
        force: x.force,
        threads: x.threads.map(|n| usize::try_from(n).unwrap_or(usize::MAX)),
        memory: x.memory,
    };
    let r = session.export(
        &lpk_api::Selection::Globs(select),
        &x.out,
        &options,
        &console.monitor(),
    );
    console.clear();
    let s = r?;
    if x.verbose {
        let _ = writeln!(
            err,
            "{}: {} files, {} directories, {} symlinks, {} bytes into {} ({}, {}{}); plan: {} chunk placements, {} block visits over {} of {} blocks; {} workers, {} blocks held at most (peak {}); {} blocks decoded ({} again for back references), {} nested decodes",
            x.archive.display(),
            s.files,
            s.directories,
            s.symlinks,
            s.bytes,
            x.out.display(),
            container.name(),
            method.name(),
            if password.is_some() { ", AES-256" } else { "" },
            s.placements,
            s.visits,
            s.blocks_needed,
            s.blocks,
            s.workers,
            s.in_flight,
            s.peak_in_flight,
            s.blocks_decoded,
            s.redecodes,
            s.nested_decodes
        );
        let m = &s.memory;
        let entries = s.files + s.directories + s.symlinks;
        let _ = writeln!(
            err,
            "memory: budget {} bytes ({}); envelope tier {}; per block {} bytes; fixed {} bytes (shared {}, per worker {}); counted {} bytes for {} blocks held and {} workers; not counted: the writer's own memory, {} (about {} bytes for {} entries) plus its compressor ({})",
            m.budget.bytes,
            m.budget.source.name(),
            m.tier.name(),
            m.per_block,
            m.fixed,
            m.fixed_shared,
            m.per_worker,
            s.counted(),
            s.in_flight,
            s.workers,
            match container {
                Container::Zip => "the ZIP central directory kept until the end",
                Container::SevenZ => "the 7z entry table kept until the end",
            },
            export::directory_estimate(container, entries, s.name_bytes),
            entries,
            export::compressor_memory(container, method)
        );
    }
    zone_warning_line(session.zone_warning().as_deref(), json, err);
    motw_check(zone, &x.out, json, err);
    let _ = writeln!(
        out,
        "exported {} files, {} directories, {} symlinks to {}",
        s.files,
        s.directories,
        s.symlinks,
        x.out.display()
    );
    Ok(())
}

/// Run one typed command of the reference tool (`verify`, `check`, `repair`, `rollback` or
/// `info`) with the prior files, the decode memory limit and the credentials as typed values
/// (`lpk_format::cli::run_with_credentials`: no password passes through an argument string);
/// its summary goes to `out`, its `FormatError` becomes a [`Failure`] of that variant's kind,
/// naming the archive unless the error names a path itself.
fn reference(
    command: &lpk_format::cli::Command,
    priors: &[PathBuf],
    memory: Option<u64>,
    credentials: Option<&Credentials>,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let options = lpk_format::cli::ToolOptions {
        priors: priors.to_vec(),
        memory,
        credentials,
        hook: Some(lpk_core::register_full_reader::<std::fs::File>),
    };
    lpk_format::cli::run_with_credentials(command, options, out)
        .map_err(|e| Failure::format(&e, Some(command.archive())))
}

/// The session of `check` and `repair`: the archive opened as the reference tool opened it
/// (the reader's default limit, no priors), its open failure a `FormatError` naming it.
fn maintenance_session(
    archive: &Path,
    credentials: Option<&Credentials>,
    motw_off: bool,
) -> Result<Session, Failure> {
    let file = std::fs::File::open(archive)
        .map_err(|e| Failure::format(&FormatError::Io(e), Some(archive)))?;
    let open = lpk_api::OpenOptions {
        credentials,
        memory: Some(lpk_format::Resources::default().memory),
        motw_off,
        ..lpk_api::OpenOptions::default()
    };
    Ok(Session::open_file(file, archive, &open)?)
}

/// The recovery report line of `check` and `repair` (the reference tool's).
fn recovery_line(
    out: &mut dyn Write,
    archive: &Path,
    r: &lpk_api::RecoveryReport,
) -> Result<(), Failure> {
    writeln!(
        out,
        "recovery frames: {}, unusable: {}, damaged shards: {}, repaired shards: {}",
        r.frames, r.frames_unusable, r.shards_damaged, r.shards_repaired
    )
    .map_err(|e| Failure::format(&FormatError::Io(e), Some(archive)))
}

/// `lpk check`: `Session::check`, the report line, `DamageFound` when a shard is damaged or a
/// frame unusable.
fn check(
    archive: &Path,
    credentials: Option<&Credentials>,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let session = maintenance_session(archive, credentials, false)?;
    let r = session.check(&lpk_core::Monitor::default())?;
    recovery_line(out, archive, &r)?;
    if r.shards_damaged > 0 || r.frames_unusable > 0 {
        return Err(Failure::format(
            &FormatError::DamageFound {
                damaged: r.shards_damaged,
                unusable: r.frames_unusable,
            },
            Some(archive),
        ));
    }
    Ok(())
}

/// `lpk repair`: `Session::repair` into `target` (which must not exist); the copy is kept
/// with every possible repair when a frame cannot be rebuilt (exit 2, `Unrepairable`).
fn repair(
    r: &RepairArgs,
    credentials: Option<&Credentials>,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let (archive, target) = (r.archive.as_path(), r.out.as_path());
    let motw_off = r.motw == OnOff::Off;
    let session = maintenance_session(archive, credentials, motw_off)?;
    let (report, unrepairable) = session.repair(target, &lpk_core::Monitor::default())?;
    zone_warning_line(session.zone_warning().as_deref(), json, err);
    motw_check(
        if motw_off { None } else { session.zone() },
        target,
        json,
        err,
    );
    recovery_line(out, archive, &report)?;
    match unrepairable {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

/// `lpk l`: open with the credentials (a sealed archive without them opens keyless, which
/// lists only when it is listable) and stream the entry table.
fn list(
    l: &ListArgs,
    credentials: Option<&Credentials>,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    if is_foreign(&l.archive) {
        return foreign_list(l, credentials, json, out, err);
    }
    let failed = |e: FormatError| {
        let mut f = Failure::format(&e, Some(&l.archive));
        f.message = format!("{}: {} ({})", l.archive.display(), f.message, f.kind);
        f
    };
    let select =
        lpk_core::Selection::new(&l.patterns).map_err(|e| Failure::core(&e, Some(&l.archive)))?;
    let file = std::fs::File::open(&l.archive).map_err(|e| failed(FormatError::Io(e)))?;
    // The reader's default limit (a listing decodes no block).
    let open = lpk_api::OpenOptions {
        credentials,
        memory: Some(lpk_format::Resources::default().memory),
        ..lpk_api::OpenOptions::default()
    };
    let api_failed = |e: lpk_api::Error| {
        let format = e.is_format();
        let mut f = Failure::from(e);
        if format {
            f.message = format!("{}: {} ({})", l.archive.display(), f.message, f.kind);
        }
        f
    };
    let session = Session::open_file(file, &l.archive, &open).map_err(api_failed)?;
    let options = ListOptions { long: l.long, json };
    list::list_session(&session, options, select, out, api_failed)
}

/// True when the raw command line carries `--json` (before any `--`), so a usage error clap
/// reports is printed as JSON too.
fn wants_json(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|a| a.as_os_str() != "--")
        .any(|a| a.as_os_str() == "--json")
}

/// The command line as clap defines it, for the generated reference (`docs/cli/lpk.md`, the
/// man page): the version is the crate's alone, without the build hash `--version` prints,
/// so the text is the same for every build of one version.
pub fn command() -> clap::Command {
    Cli::command().version(env!("CARGO_PKG_VERSION"))
}

/// Run the tool with `args` (the first is the program name); returns the exit code. Nothing
/// cancels the command: `main` uses [`run_with_cancel`] with the Ctrl-C handler.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    run_with_cancel(args, out, err, |_| {})
}

/// [`run`] with `install` called, with the token the command polls, only for a command that
/// polls one (`a`, `append`, `x`, `export`), before it runs: `main` installs the Ctrl-C
/// handler there, so the other commands keep the process default. Cancelling the token stops
/// the engine at its next file, read or block, cleans up as a failure does, and exits 2 with
/// `cancelled`.
pub fn run_with_cancel<I, T>(
    args: I,
    out: &mut dyn Write,
    err: &mut dyn Write,
    install: impl FnOnce(CancelToken),
) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let cli = match Cli::try_parse_from(&args) {
        Ok(c) => c,
        Err(e) => {
            let text = e.to_string();
            if e.use_stderr() {
                if wants_json(&args) {
                    let message = text.trim_end_matches(['\n', '\r']);
                    let message = message.strip_prefix("error: ").unwrap_or(message);
                    Failure::usage(message).write_json(err);
                } else {
                    let _ = err.write_all(text.as_bytes());
                }
                return EXIT_USAGE;
            }
            let _ = out.write_all(text.as_bytes());
            return EXIT_OK;
        }
    };
    let json = cli.json;
    let terminal = Terminal::detect();
    let cancel = CancelToken::new();
    if matches!(
        cli.command,
        Command::Add(_) | Command::Append(_) | Command::Extract(_) | Command::Export(_)
    ) {
        install(cancel.clone());
    }
    let console = Console::new(cancel, !cli.quiet && !cli.no_progress && !json);
    // Under --quiet the command's own stderr output (warnings, notes, -v reports) goes
    // nowhere; stdout is untouched and a failure's line still goes to `err`.
    let mut quiet_err = std::io::sink();
    let e: &mut dyn Write = if cli.quiet { &mut quiet_err } else { err };
    let r = dispatch(cli.command, json, &terminal, &console, out, e);
    console.clear();
    match r {
        Ok(()) => EXIT_OK,
        Err(f) => {
            f.write(json, err);
            f.code
        }
    }
}

/// Resolve the command's credentials (read, or asked for on `terminal`), then run it; the
/// commands that open an archive run through [`with_credentials`], which asks for the
/// password on an interactive terminal when the archive turns out to need one.
fn dispatch(
    command: Command,
    json: bool,
    terminal: &Terminal,
    console: &Console,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    use lpk_format::cli::Command as Tool;
    match command {
        Command::Add(mut a) => {
            let credentials = a.credentials.resolve(terminal, &a.archive, Use::Create)?;
            add(&a, credentials, console, err)
        }
        Command::List(mut l) => {
            let given = l.credentials.resolve(terminal, &l.archive, Use::Open)?;
            with_credentials(given, &l.credentials, &l.archive, terminal, |c| {
                list(&l, c, json, out, err)
            })
        }
        Command::Extract(mut x) => {
            let given = x.credentials.resolve(terminal, &x.archive, Use::Open)?;
            with_credentials(given, &x.credentials, &x.archive, terminal, |c| {
                extract(&x, c, json, console, out, err)
            })
        }
        Command::Test(mut t) => {
            let given = t.credentials.resolve(terminal, &t.archive, Use::Open)?;
            let tool = Tool::Verify {
                archive: t.archive.clone(),
            };
            with_credentials(given, &t.credentials, &t.archive, terminal, |c| {
                reference(&tool, &t.priors, Some(decode_memory(t.memory)), c, out)
            })
        }
        Command::Check(mut c) => {
            let given = c.credentials.resolve(terminal, &c.archive, Use::Open)?;
            with_credentials(given, &c.credentials, &c.archive, terminal, |k| {
                check(&c.archive, k, out)
            })
        }
        Command::Repair(mut r) => {
            let given = r.credentials.resolve(terminal, &r.archive, Use::Open)?;
            with_credentials(given, &r.credentials, &r.archive, terminal, |c| {
                repair(&r, c, json, out, err)
            })
        }
        Command::Append(mut a) => {
            let given = a.credentials.resolve(terminal, &a.archive, Use::Open)?;
            with_credentials(given, &a.credentials, &a.archive, terminal, |c| {
                append(&a, c, console, err)
            })
        }
        Command::Rollback(r) => {
            // No key is needed: the credential flags are accepted and ignored, nothing is
            // read or asked for (`--password-prompt` does not prompt here).
            let g = lpk_api::rollback(&r.archive, r.generation)?;
            writeln!(
                out,
                "rolled back to generation {}: {} bytes",
                g.generation, g.end
            )
            .map_err(|e| Failure::format(&FormatError::Io(e), Some(&r.archive)))
        }
        Command::Info(mut i) => {
            let given = i.credentials.resolve(terminal, &i.archive, Use::Open)?;
            let tool = Tool::Info {
                archive: i.archive.clone(),
            };
            with_credentials(given, &i.credentials, &i.archive, terminal, |c| {
                reference(&tool, &i.priors, None, c, out)
            })
        }
        Command::Export(mut x) => {
            let given = x.credentials.resolve(terminal, &x.archive, Use::Open)?;
            with_credentials(given, &x.credentials, &x.archive, terminal, |c| {
                export(&x, c, json, console, out, err)
            })
        }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod jpeg_peel_tests {
    use clap::Parser;

    /// The stages that peel JPEGs in the pipeline `lpk a <args>` builds.
    fn jpeg_stages(args: &[&str]) -> usize {
        let cli = super::Cli::try_parse_from(
            ["lpk", "a"]
                .iter()
                .chain(args)
                .chain(["out.lpk", "in"].iter()),
        )
        .unwrap();
        let super::Command::Add(a) = cli.command else {
            panic!("not an add");
        };
        let tier = a.tier.tier().unwrap();
        super::build_settings(&a.tier, tier)
            .pipeline(&lpk_core::Monitor::default())
            .peel
            .iter()
            .filter(|p| p.applies_to(lpk_core::Class::Jpeg))
            .count()
    }

    #[test]
    fn jpeg_peel_off_removes_the_stage_and_on_keeps_it_in_both_tiers() {
        for tier in ["--fast", "--balanced"] {
            assert_eq!(jpeg_stages(&[tier]), 1, "{tier}");
            assert_eq!(jpeg_stages(&[tier, "--jpeg-peel", "on"]), 1, "{tier}");
            assert_eq!(jpeg_stages(&[tier, "--jpeg-peel", "off"]), 0, "{tier}");
        }
        assert_eq!(jpeg_stages(&["--store"]), 0);
    }
}
