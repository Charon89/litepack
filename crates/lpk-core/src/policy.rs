//! The extraction policy (E3-2): every rule an extraction applies before and while it writes,
//! in one audited place that the command line, the GUI and the shell extension call.
//!
//! **The rules.** The path rules of the reference tool, kept here as our own copy with the same
//! behaviour and messages (the reference tool keeps its own; it does not depend on this crate):
//! a component may not be a Windows device name ([`check_reserved_device`]), contain a colon or
//! end in a dot or a space ([`check_extraction_path`], which also refuses an absolute path and
//! a `.` or `..` component, which the format's own path rules refuse when the entry table is
//! parsed); a path one of whose parents is an entry that is not a directory is a conflicting
//! name ([`check_conflicting_names`]); an existing parent below the output directory that is a
//! symlink or a junction is a linked parent ([`refuse_linked_parent`], D-54). Two entries whose
//! paths differ only by case are refused before any output when the target is
//! case-insensitive ([`check_case_collisions`]; [`case_insensitive_target`] is Windows and
//! macOS). Symlink entries are refused ([`refuse_symlink`], the reference tool's rule), skipped
//! or created ([`Symlinks`]); one to create whose target is absolute, UNC, a device path,
//! drive-relative, rooted or escaping is skipped and recorded, its kind on Windows read from
//! the archive, never the file system ([`resolve_symlink_target`], E6-0). An existing target is refused, skipped, written beside under a
//! new name or replaced ([`Overwrite`]); the modes apply to files (and symlink entries); a
//! directory where a file goes, or a file where a directory goes, is in the way
//! ([`CoreError::InTheWay`]) in every mode but `skip`.
//!
//! **The hook.** [`ExtractPolicy`] is what the engine asks; [`DefaultPolicy`] answers with the
//! reference tool's rules (nothing replaced, no symlink), [`Policy`] with the configurable
//! modes. A policy of one's own needs only the five original hooks; the two added by E3-2
//! (`symlink_action`, `existing`) default to the original behaviour (symlinks skipped, an
//! accepted existing target still opened create-new).

use std::collections::HashMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lpk_format::{Entry, EntryKind, FormatError};

use crate::CoreError;

/// What an extraction may write. Every hook is called before anything is written; an `Err`
/// refuses the whole extraction with that error.
pub trait ExtractPolicy: Sync {
    /// The archive path of every entry (the format's own path rules were checked when the entry
    /// table was parsed).
    fn check_path(&self, path: &str) -> Result<(), FormatError>;
    /// Every symlink entry.
    fn symlink(&self, entry: &Entry) -> Result<(), FormatError>;
    /// A file target that already exists. Returning `Ok` accepts it, but the file is still
    /// opened create-new: a policy that replaces must remove the target itself (or answer
    /// [`Existing::Replace`] from [`ExtractPolicy::existing`]).
    fn overwrite(&self, target: &Path) -> Result<(), FormatError>;
    /// The archive path of every entry, for device names.
    fn device(&self, path: &str) -> Result<(), FormatError>;
    /// An existing directory below the output directory, on the way to a target, that is a
    /// symlink or a reparse point (writing through it could leave the output directory).
    fn linked_parent(&self, dir: &Path) -> Result<(), FormatError>;
    /// Whether a symlink entry that [`ExtractPolicy::symlink`] accepted is created or skipped.
    /// Default: skipped.
    fn symlink_action(&self, entry: &Entry) -> SymlinkAction {
        let _ = entry;
        SymlinkAction::Skip
    }
    /// A symlink entry to create whose target [`resolve_symlink_target`] refuses: it is skipped
    /// and counted, never created; the policy may record it for a warning. Default: nothing.
    fn symlink_target_refused(&self, entry: &Entry, refusal: TargetRefusal) {
        let _ = (entry, refusal);
    }
    /// Something exists where an entry goes: what to do. Default: [`ExtractPolicy::overwrite`]
    /// decides, and an accepted target is written as before (create-new, so the open fails).
    /// For a directory entry (or an implicit parent directory) only [`Existing::Write`] and
    /// [`Existing::Skip`] are told apart; the others act as `Write`.
    fn existing(&self, target: &Path, found: Found, wanted: Wanted) -> Result<Existing, CoreError> {
        let _ = (found, wanted);
        self.overwrite(target)?;
        Ok(Existing::Write)
    }
}

/// What a symlink entry becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkAction {
    /// Not created; counted as skipped.
    Skip,
    /// Created with the entry's target, as the archive holds it.
    Create,
}

/// What exists at a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// A regular file (or anything that is neither a directory nor a link).
    File,
    /// A directory.
    Directory,
    /// A symlink or, on Windows, a junction; never followed.
    Symlink,
}

impl Found {
    /// What `meta` (from `symlink_metadata`) says is there.
    pub fn of(meta: &Metadata) -> Self {
        let t = meta.file_type();
        if t.is_symlink() {
            Found::Symlink
        } else if t.is_dir() {
            Found::Directory
        } else {
            Found::File
        }
    }

    fn name(self) -> &'static str {
        match self {
            Found::File => "file",
            Found::Directory => "directory",
            Found::Symlink => "symlink",
        }
    }
}

/// What the entry at a target is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    /// A file entry.
    File,
    /// A directory entry, or an implicit parent directory of another entry.
    Directory,
    /// A symlink entry that is to be created.
    Symlink,
}

impl Wanted {
    fn name(self) -> &'static str {
        match self {
            Wanted::File => "file",
            Wanted::Directory => "directory",
            Wanted::Symlink => "symlink",
        }
    }
}

/// What to do with an existing target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Existing {
    /// Write as if nothing were there (create-new, so a file target fails with `AlreadyExists`
    /// at its open; a directory is reused).
    Write,
    /// Leave it as it is and count it.
    Skip,
    /// Write the entry beside it under the name [`rename_target`] gives, and count it.
    Rename,
    /// Remove it (a link as a link, never followed) and write the entry in its place.
    Replace,
}

/// `lpk x --overwrite`: what an existing file target gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overwrite {
    /// The extraction is refused before any output (the default).
    #[default]
    Refuse,
    /// The existing file is left as it is and counted; a directory in a file's way (or a file
    /// in a directory's way, with everything below it) is skipped too.
    Skip,
    /// The entry is written as `<stem> (<k>)<.ext>` with the smallest `k >= 1` free, and
    /// counted.
    Rename,
    /// The existing file is replaced; a link at the target is removed as a link, never
    /// followed, and a file written in its place.
    Replace,
}

impl Overwrite {
    /// The command-line names.
    pub const NAMES: [&'static str; 4] = ["refuse", "skip", "rename", "replace"];
}

/// What symlink entries get.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Symlinks {
    /// Refused before any output (the reference tool's rule).
    Refuse,
    /// Skipped, each one recorded for a warning (the command line's default).
    #[default]
    Skip,
    /// Created (`lpk x --symlinks`) when the target stays inside the destination
    /// ([`resolve_symlink_target`]; a refused target is skipped with a record); a creation
    /// that fails fails the extraction.
    Create,
}

/// The reference tool's refusals: unsafe paths, Windows device names, every symlink, every
/// existing file, every linked parent. Conflicting names and case collisions are refused by
/// the plan for every policy.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultPolicy;

impl ExtractPolicy for DefaultPolicy {
    fn check_path(&self, path: &str) -> Result<(), FormatError> {
        check_extraction_path(path)
    }

    fn symlink(&self, entry: &Entry) -> Result<(), FormatError> {
        refuse_symlink(entry)
    }

    fn overwrite(&self, target: &Path) -> Result<(), FormatError> {
        check_no_overwrite(target)
    }

    fn device(&self, path: &str) -> Result<(), FormatError> {
        check_reserved_device(path)
    }

    fn linked_parent(&self, dir: &Path) -> Result<(), FormatError> {
        refuse_linked_parent(dir)
    }
}

/// The configurable policy: the reference tool's path rules, an [`Overwrite`] mode and a
/// [`Symlinks`] mode. `Policy::default()` behaves as [`DefaultPolicy`] except that symlink
/// entries are skipped and recorded instead of refused.
#[derive(Debug, Default)]
pub struct Policy {
    /// What an existing file target gets.
    pub overwrite: Overwrite,
    /// What symlink entries get.
    pub symlinks: Symlinks,
    skipped_symlinks: Mutex<Vec<String>>,
    refused_targets: Mutex<Vec<(String, TargetRefusal)>>,
}

impl Policy {
    /// A policy with the given modes.
    pub fn new(overwrite: Overwrite, symlinks: Symlinks) -> Self {
        Policy {
            overwrite,
            symlinks,
            skipped_symlinks: Mutex::new(Vec::new()),
            refused_targets: Mutex::new(Vec::new()),
        }
    }

    /// The archive paths of the symlink entries skipped so far (for the warnings), in entry
    /// order.
    pub fn skipped_symlinks(&self) -> Vec<String> {
        self.skipped_symlinks
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// The archive paths of the symlink entries skipped because of their target, with the
    /// reason, in entry order (for the warnings).
    pub fn refused_symlink_targets(&self) -> Vec<(String, TargetRefusal)> {
        self.refused_targets
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default()
    }
}

impl ExtractPolicy for Policy {
    fn check_path(&self, path: &str) -> Result<(), FormatError> {
        check_extraction_path(path)
    }

    fn symlink(&self, entry: &Entry) -> Result<(), FormatError> {
        match self.symlinks {
            Symlinks::Refuse => refuse_symlink(entry),
            Symlinks::Skip => {
                if let Ok(mut v) = self.skipped_symlinks.lock() {
                    v.push(entry.path.clone());
                }
                Ok(())
            }
            Symlinks::Create => Ok(()),
        }
    }

    fn overwrite(&self, target: &Path) -> Result<(), FormatError> {
        check_no_overwrite(target)
    }

    fn device(&self, path: &str) -> Result<(), FormatError> {
        check_reserved_device(path)
    }

    fn linked_parent(&self, dir: &Path) -> Result<(), FormatError> {
        refuse_linked_parent(dir)
    }

    fn symlink_action(&self, _entry: &Entry) -> SymlinkAction {
        match self.symlinks {
            Symlinks::Create => SymlinkAction::Create,
            Symlinks::Refuse | Symlinks::Skip => SymlinkAction::Skip,
        }
    }

    fn symlink_target_refused(&self, entry: &Entry, refusal: TargetRefusal) {
        if let Ok(mut v) = self.refused_targets.lock() {
            v.push((entry.path.clone(), refusal));
        }
    }

    fn existing(&self, target: &Path, found: Found, wanted: Wanted) -> Result<Existing, CoreError> {
        let in_the_way = matches!(
            (found, wanted),
            (Found::Directory, Wanted::File | Wanted::Symlink)
                | (Found::File | Found::Symlink, Wanted::Directory)
        );
        if found == Found::Directory && wanted == Wanted::Directory {
            return Ok(Existing::Write);
        }
        if in_the_way {
            return if self.overwrite == Overwrite::Skip {
                Ok(Existing::Skip)
            } else {
                Err(CoreError::InTheWay {
                    path: target.to_path_buf(),
                    found: found.name(),
                    wanted: wanted.name(),
                })
            };
        }
        match self.overwrite {
            Overwrite::Refuse => {
                check_no_overwrite(target)?;
                Ok(Existing::Write)
            }
            Overwrite::Skip => Ok(Existing::Skip),
            Overwrite::Rename => Ok(Existing::Rename),
            Overwrite::Replace => Ok(Existing::Replace),
        }
    }
}

const RESERVED: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

/// Whether `component` is a Windows device name: `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`,
/// `COM` with a superscript 1, 2 or 3, `LPT1`-`LPT9`, `LPT` with a superscript, `CONIN$`,
/// `CONOUT$`; in any case, with or without an extension, and with spaces before the extension.
pub fn is_reserved_device(component: &str) -> bool {
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

/// The path rules on top of the format's: a component may not be a Windows device name,
/// contain `:`, or end in a dot or a space (`UnsafePath` with the reasons "reserved device
/// name", "colon", "trailing dot or space"); and, defensively (the format's own rules refuse
/// them when the entry table is parsed), an absolute path ("leading slash"), a backslash
/// ("backslash") and a `.` or `..` component ("dot component").
pub fn check_extraction_path(path: &str) -> Result<(), FormatError> {
    let unsafe_path = |reason| FormatError::UnsafePath {
        path: path.to_string(),
        reason,
    };
    if path.starts_with('/') {
        return Err(unsafe_path("leading slash"));
    }
    if path.contains('\\') {
        return Err(unsafe_path("backslash"));
    }
    for component in path.split('/') {
        if component == "." || component == ".." {
            return Err(unsafe_path("dot component"));
        }
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

/// The device rule of [`check_extraction_path`] alone: `UnsafePath` with the reason "reserved
/// device name".
pub fn check_reserved_device(path: &str) -> Result<(), FormatError> {
    if path.split('/').any(is_reserved_device) {
        return Err(FormatError::UnsafePath {
            path: path.to_string(),
            reason: "reserved device name",
        });
    }
    Ok(())
}

/// The reference tool extracts no symlink: a symlink entry is `SymlinkRefused`, any other
/// entry passes.
pub fn refuse_symlink(entry: &Entry) -> Result<(), FormatError> {
    if entry.kind == EntryKind::Symlink {
        return Err(FormatError::SymlinkRefused {
            path: entry.path.clone(),
        });
    }
    Ok(())
}

/// Why a symlink entry's target is not created (E6-0; `docs/notes/e6-design.md` section 5).
/// The same targets are refused on every operating system: an archive made on one is
/// extracted on another, and `\` is a separator for Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetRefusal {
    /// No target at all.
    Empty,
    /// A NUL byte, which no file system accepts in a path.
    Nul,
    /// A device path (`\\?\`, `\\.\`, `\??\`, or the same with `/`).
    DevicePath,
    /// A UNC path (`\\server\share`, `//server/share`): following it contacts a server.
    Unc,
    /// A drive-absolute path (`C:\x`).
    Absolute,
    /// A drive-relative path (`C:x`).
    DriveRelative,
    /// Rooted: starts with one `/` or `\` (an absolute path on Unix).
    Rooted,
    /// A component with a colon (an alternate data stream on Windows).
    Colon,
    /// A component ending in a dot or a space (Windows strips them, so the name the archive
    /// shows is not the name followed).
    TrailingDotOrSpace,
    /// A component that is a Windows device name (`CON`, `NUL`, `COM1`, ...).
    DeviceName,
    /// It climbs above the destination after lexical normalisation from the link's directory.
    Escapes,
    /// It goes through another symlink entry of the archive (the operating system would
    /// resolve what follows from that link's target, which lexical normalisation cannot see).
    ThroughLink,
}

impl TargetRefusal {
    /// The reason, as the warnings print it.
    pub fn reason(self) -> &'static str {
        match self {
            TargetRefusal::Empty => "the target is empty",
            TargetRefusal::Nul => "the target contains a NUL byte",
            TargetRefusal::DevicePath => "the target is a device path",
            TargetRefusal::Unc => "the target is a UNC path",
            TargetRefusal::Absolute => "the target is an absolute path",
            TargetRefusal::DriveRelative => "the target is drive-relative",
            TargetRefusal::Rooted => "the target is rooted",
            TargetRefusal::Colon => "the target contains a colon",
            TargetRefusal::TrailingDotOrSpace => {
                "a component of the target ends in a dot or a space"
            }
            TargetRefusal::DeviceName => "the target names a Windows device",
            TargetRefusal::Escapes => "the target escapes the destination",
            TargetRefusal::ThroughLink => "the target goes through another symlink entry",
        }
    }
}

impl std::fmt::Display for TargetRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

fn is_sep(b: u8) -> bool {
    b == b'/' || b == b'\\'
}

/// Resolve the target of the symlink entry at archive path `link` (`/`-separated) lexically,
/// from the link's own directory, within the archive's namespace: the path it names relative
/// to the destination (`""` for the destination itself), or why it is refused
/// ([`TargetRefusal`]). `is_link` says whether an archive path (resolved so far) is a symlink
/// entry; a target that goes on below one is refused. Pure: no file-system call, so a refused
/// target is never touched.
pub fn resolve_symlink_target(
    link: &str,
    target: &[u8],
    is_link: impl Fn(&str) -> bool,
) -> Result<String, TargetRefusal> {
    if target.is_empty() {
        return Err(TargetRefusal::Empty);
    }
    if target.contains(&0) {
        return Err(TargetRefusal::Nul);
    }
    if target.len() >= 4
        && is_sep(target[0])
        && is_sep(target[3])
        && ((is_sep(target[1]) && matches!(target[2], b'?' | b'.'))
            || (target[1] == b'?' && target[2] == b'?'))
    {
        return Err(TargetRefusal::DevicePath);
    }
    if target.len() >= 2 && is_sep(target[0]) && is_sep(target[1]) {
        return Err(TargetRefusal::Unc);
    }
    if is_sep(target[0]) {
        return Err(TargetRefusal::Rooted);
    }
    if target.len() >= 2 && target[0].is_ascii_alphabetic() && target[1] == b':' {
        return Err(if target.get(2).copied().is_some_and(is_sep) {
            TargetRefusal::Absolute
        } else {
            TargetRefusal::DriveRelative
        });
    }
    let components: Vec<String> = target
        .split(|&b| is_sep(b))
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect();
    for c in &components {
        if c.is_empty() || c == "." || c == ".." {
            continue;
        }
        if c.contains(':') {
            return Err(TargetRefusal::Colon);
        }
        if c.ends_with('.') || c.ends_with(' ') {
            return Err(TargetRefusal::TrailingDotOrSpace);
        }
        if is_reserved_device(c) {
            return Err(TargetRefusal::DeviceName);
        }
    }
    // The link's directory, then each component; a parent of the link is a directory entry
    // or implied (the conflicting-name rule refuses a link below a non-directory).
    let mut stack: Vec<&str> = link.split('/').collect();
    stack.pop();
    for c in &components {
        if c.is_empty() || c == "." {
            continue;
        }
        if !stack.is_empty() && is_link(&stack.join("/")) {
            return Err(TargetRefusal::ThroughLink);
        }
        if c == ".." {
            if stack.pop().is_none() {
                return Err(TargetRefusal::Escapes);
            }
        } else {
            stack.push(c);
        }
    }
    Ok(stack.join("/"))
}

/// Nothing is replaced: a file target that already exists (a file, a directory, a symlink,
/// even a dangling one) is an I/O error of kind `AlreadyExists`, the error a create-new open
/// reports. Checking first lets an extraction refuse before it writes anything.
pub fn check_no_overwrite(target: &Path) -> Result<(), FormatError> {
    if std::fs::symlink_metadata(target).is_ok() {
        return Err(FormatError::from(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{}: already exists", target.display()),
        )));
    }
    Ok(())
}

/// Refuse a path one of whose parents is an entry that is not a directory (such as a file `a`
/// beside `a/b`): `UnsafePath` with the reason "conflicting name". Missing parent directories
/// are created by the extraction.
pub fn check_conflicting_names(entries: &[Entry]) -> Result<(), FormatError> {
    let not_dirs: std::collections::HashSet<&str> = entries
        .iter()
        .filter(|e| e.kind != EntryKind::Directory)
        .map(|e| e.path.as_str())
        .collect();
    for e in entries {
        let conflict = e
            .path
            .match_indices('/')
            .any(|(i, _)| not_dirs.contains(&e.path[..i]));
        if conflict {
            return Err(FormatError::UnsafePath {
                path: e.path.clone(),
                reason: "conflicting name",
            });
        }
    }
    Ok(())
}

/// An existing parent that is a symlink or a junction: `UnsafePath` with the reason "linked
/// parent" (D-54).
pub fn refuse_linked_parent(dir: &Path) -> Result<(), FormatError> {
    Err(FormatError::UnsafePath {
        path: dir.display().to_string(),
        reason: "linked parent",
    })
}

/// Whether paths under `dir` are compared without regard to case: Windows and macOS (by
/// target, whatever the volume); elsewhere `false`. A probe of the directory may refine this
/// later.
pub fn case_insensitive_target(dir: &Path) -> bool {
    let _ = dir;
    cfg!(any(windows, target_os = "macos"))
}

/// Refuse two entries whose paths differ only by case: [`CoreError::CaseCollision`] naming
/// both, the first in entry order first; and, with the same folding, a path one of whose
/// parents is an entry that is not a directory ([`check_conflicting_names`]'s rule, so a
/// symlink `A` beside `a/x` is a conflicting name here as `a` beside `a/x` is everywhere).
/// Called when the target is case-insensitive. Folding is full Unicode lower-casing of the
/// whole path, which over-refuses some pairs NTFS keeps distinct (`ß` and `ẞ`, for one):
/// safe, since refusing is the failure mode, and simpler than the file system's own table.
pub fn check_case_collisions(entries: &[Entry]) -> Result<(), CoreError> {
    let mut seen: HashMap<String, &str> = HashMap::with_capacity(entries.len());
    let mut not_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in entries {
        let folded = e.path.to_lowercase();
        if let Some(first) = seen.get(folded.as_str()) {
            return Err(CoreError::CaseCollision {
                first: (*first).to_string(),
                second: e.path.clone(),
            });
        }
        if e.kind != EntryKind::Directory {
            not_dirs.insert(folded.clone());
        }
        seen.insert(folded, &e.path);
    }
    for e in entries {
        let folded = e.path.to_lowercase();
        let conflict = folded
            .match_indices('/')
            .any(|(i, _)| not_dirs.contains(&folded[..i]));
        if conflict {
            return Err(FormatError::UnsafePath {
                path: e.path.clone(),
                reason: "conflicting name",
            }
            .into());
        }
    }
    Ok(())
}

/// The name an entry is written under beside an existing `target`: `<stem> (<k>)<.ext>` in the
/// same directory with the smallest `k >= 1` for which `taken` is false (the convention of
/// Explorer; the extension is the part after the last dot, so `a.tar.gz` becomes
/// `a.tar (1).gz`, and a name starting with a dot, like `.bashrc`, has no extension).
pub fn rename_target(target: &Path, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let stem = target
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    let ext = target.extension().map(|e| e.to_os_string());
    let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
    for k in 1u64.. {
        let mut name = stem.clone();
        name.push(format!(" ({k})"));
        if let Some(e) = &ext {
            name.push(".");
            name.push(e);
        }
        let candidate = dir.join(name);
        if !taken(&candidate) {
            return candidate;
        }
    }
    unreachable!("every k was taken")
}

/// Remove what is at `target` so an entry can be written in its place: a link is removed as a
/// link (never followed; on Windows a directory symlink or a junction, which the metadata
/// already read says it is, is removed as a directory entry, with no fallback that could take
/// a real directory swapped in between), a file as a file. A directory is an error (nothing
/// is written into or over one).
pub fn remove_existing(target: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(target)?;
    match Found::of(&meta) {
        Found::Symlink => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::FileTypeExt;
                if meta.file_type().is_symlink_dir() {
                    return std::fs::remove_dir(target);
                }
            }
            std::fs::remove_file(target)
        }
        Found::File => std::fs::remove_file(target),
        Found::Directory => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{}: a directory is in the way", target.display()),
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use lpk_format::EntryFlags;

    fn file(path: &str) -> Entry {
        Entry {
            kind: EntryKind::File,
            flags: EntryFlags::EMPTY,
            path: path.to_string(),
            mtime_ns: 0,
            size: 0,
            symlink_target: None,
            chunks: Vec::new(),
        }
    }

    fn dir(path: &str) -> Entry {
        Entry {
            kind: EntryKind::Directory,
            ..file(path)
        }
    }

    fn link(path: &str) -> Entry {
        Entry {
            kind: EntryKind::Symlink,
            symlink_target: Some(b"x".to_vec()),
            size: 1,
            ..file(path)
        }
    }

    /// E6-0: every refused target form, with its reason, and the in-tree resolutions.
    #[test]
    fn symlink_target_table() {
        use TargetRefusal as R;
        let links = ["s", "d/s2"];
        let is_link = |p: &str| links.contains(&p);
        let refused: &[(&str, &[u8], R)] = &[
            ("l", b"\\\\server\\share\\x", R::Unc),
            ("l", b"//server/share", R::Unc),
            ("l", b"\\/server/share", R::Unc),
            ("l", b"C:\\Windows\\x", R::Absolute),
            ("l", b"c:/x", R::Absolute),
            ("l", b"C:x", R::DriveRelative),
            ("l", b"C:", R::DriveRelative),
            ("l", b"\\x", R::Rooted),
            ("l", b"/etc/passwd", R::Rooted),
            ("l", b"\\\\?\\C:\\x", R::DevicePath),
            ("l", b"\\\\.\\pipe\\x", R::DevicePath),
            ("l", b"//?/C:/x", R::DevicePath),
            ("l", b"\\??\\C:\\x", R::DevicePath),
            ("a/l", b"../../x", R::Escapes),
            ("l", b"..", R::Escapes),
            ("a/b/l", b"..\\..\\..\\x", R::Escapes),
            ("l", b"a/../../x", R::Escapes),
            ("l", b"x/file:stream", R::Colon),
            ("l", b"x./y", R::TrailingDotOrSpace),
            ("l", b"x ", R::TrailingDotOrSpace),
            ("l", b"...", R::TrailingDotOrSpace),
            ("l", b"sub/CON", R::DeviceName),
            ("l", b"nul.txt", R::DeviceName),
            ("l", b"s/../x", R::ThroughLink),
            ("l", b"s/x", R::ThroughLink),
            ("l", b"d/s2/..", R::ThroughLink),
            ("l", b"", R::Empty),
            ("l", b"a\0b", R::Nul),
        ];
        for (link, target, want) in refused {
            assert_eq!(
                resolve_symlink_target(link, target, is_link),
                Err(*want),
                "{link} -> {}",
                String::from_utf8_lossy(target)
            );
        }
        let fine: &[(&str, &[u8], &str)] = &[
            ("l", b"a.txt", "a.txt"),
            ("l", b"./d/x", "d/x"),
            ("d/l", b"../a.txt", "a.txt"),
            ("d/e/l", b"..\\..\\x", "x"),
            ("d/l", b"..", ""),
            ("l", b".", ""),
            ("l", b"d/", "d"),
            ("l", b"s", "s"),
            ("a/b/l", b"../../d/x/../y", "d/y"),
        ];
        for (link, target, want) in fine {
            assert_eq!(
                resolve_symlink_target(link, target, is_link).as_deref(),
                Ok(*want),
                "{link} -> {}",
                String::from_utf8_lossy(target)
            );
        }
        assert_eq!(R::Unc.to_string(), "the target is a UNC path");
        // The policy records each refusal for the warnings, in order.
        let p = Policy::new(Overwrite::Refuse, Symlinks::Create);
        p.symlink_target_refused(&link("a"), R::Unc);
        p.symlink_target_refused(&link("b"), R::Escapes);
        assert_eq!(
            p.refused_symlink_targets(),
            vec![("a".to_string(), R::Unc), ("b".to_string(), R::Escapes)]
        );
        assert!(p.skipped_symlinks().is_empty());
        // The default hook records nothing and does not panic.
        DefaultPolicy.symlink_target_refused(&link("a"), R::Unc);
    }

    fn reason(r: Result<(), FormatError>) -> Option<&'static str> {
        match r {
            Ok(()) => None,
            Err(FormatError::UnsafePath { reason, .. }) => Some(reason),
            Err(e) => panic!("{e:?}"),
        }
    }

    /// Every path rule, on both OSes (the rules are the same everywhere: the archive must
    /// extract on Windows whatever wrote it).
    #[test]
    fn path_rules_table() {
        let table: [(&str, Option<&str>); 23] = [
            ("a.txt", None),
            ("dir/sub/file", None),
            ("console/x", None),
            ("comet", None),
            ("lpt0", None),
            ("COM10", None),
            ("nul", Some("reserved device name")),
            ("a/CON.txt", Some("reserved device name")),
            ("con.sole/x", Some("reserved device name")),
            ("Prn .txt", Some("reserved device name")),
            ("COM1", Some("reserved device name")),
            ("lpt9.log", Some("reserved device name")),
            ("COM\u{B9}", Some("reserved device name")),
            ("conin$", Some("reserved device name")),
            ("a/b:c", Some("colon")),
            ("c:/x", Some("colon")),
            ("a./b", Some("trailing dot or space")),
            ("a /b", Some("trailing dot or space")),
            ("a/b.", Some("trailing dot or space")),
            ("/etc/passwd", Some("leading slash")),
            ("a\\b", Some("backslash")),
            ("../x", Some("dot component")),
            ("a/./b", Some("dot component")),
        ];
        for (path, want) in table {
            assert_eq!(reason(check_extraction_path(path)), want, "{path}");
            let device = reason(check_reserved_device(path));
            assert_eq!(
                device.is_some(),
                want == Some("reserved device name"),
                "{path}"
            );
        }
        let e = check_extraction_path("a/b:c").unwrap_err();
        assert_eq!(e.to_string(), "unsafe path \"a/b:c\" (colon)");
    }

    #[test]
    fn conflicting_names_table() {
        let ok = [dir("a"), file("a/b"), file("c")];
        assert!(check_conflicting_names(&ok).is_ok());
        let bad = [file("a"), file("a/b")];
        assert!(matches!(
            check_conflicting_names(&bad),
            Err(FormatError::UnsafePath {
                reason: "conflicting name",
                ..
            })
        ));
        let linked = [link("a"), file("a/b")];
        assert!(check_conflicting_names(&linked).is_err());
        // Case-insensitive target: a symlink `A` beside `a/x` is the same conflict.
        let folded = [link("A"), file("a/x")];
        assert!(check_conflicting_names(&folded).is_ok());
        assert!(matches!(
            check_case_collisions(&folded),
            Err(CoreError::Format(FormatError::UnsafePath {
                reason: "conflicting name",
                ..
            }))
        ));
        let folded_dir = [dir("A"), file("a/x")];
        assert!(check_case_collisions(&folded_dir).is_ok());
    }

    #[test]
    fn symlink_rules_table() {
        let l = link("l");
        assert!(matches!(
            refuse_symlink(&l),
            Err(FormatError::SymlinkRefused { path }) if path == "l"
        ));
        assert!(refuse_symlink(&file("f")).is_ok());
        assert_eq!(
            refuse_symlink(&l).unwrap_err().to_string(),
            "symlink entry \"l\" refused"
        );
        // The three modes.
        let refuse = Policy::new(Overwrite::Refuse, Symlinks::Refuse);
        assert!(refuse.symlink(&l).is_err());
        let skip = Policy::default();
        assert_eq!(skip.symlinks, Symlinks::Skip);
        assert!(skip.symlink(&l).is_ok());
        assert_eq!(skip.symlink_action(&l), SymlinkAction::Skip);
        assert_eq!(skip.skipped_symlinks(), vec!["l".to_string()]);
        let create = Policy::new(Overwrite::Refuse, Symlinks::Create);
        assert!(create.symlink(&l).is_ok());
        assert_eq!(create.symlink_action(&l), SymlinkAction::Create);
        assert!(create.skipped_symlinks().is_empty());
        // DefaultPolicy refuses; its added hooks default to skip and write.
        assert!(DefaultPolicy.symlink(&l).is_err());
        assert_eq!(DefaultPolicy.symlink_action(&l), SymlinkAction::Skip);
    }

    #[test]
    fn linked_parent_message() {
        let e = refuse_linked_parent(Path::new("out/a")).unwrap_err();
        assert!(matches!(
            &e,
            FormatError::UnsafePath {
                reason: "linked parent",
                ..
            }
        ));
        assert!(e.to_string().ends_with("(linked parent)"), "{e}");
    }

    #[test]
    fn no_overwrite_is_already_exists_naming_the_target() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x");
        assert!(check_no_overwrite(&p).is_ok());
        std::fs::write(&p, b"1").unwrap();
        let e = check_no_overwrite(&p).unwrap_err();
        assert!(
            matches!(&e, FormatError::Io(io) if io.kind() == std::io::ErrorKind::AlreadyExists)
        );
        assert!(e.to_string().contains("already exists"), "{e}");
        let dd = d.path().join("d");
        std::fs::create_dir(&dd).unwrap();
        assert!(check_no_overwrite(&dd).is_err());
    }

    /// Every mode against every (found, wanted) pair.
    #[test]
    fn overwrite_modes_table() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        std::fs::write(&f, b"1").unwrap();
        use Existing as E;
        use Found as F;
        use Overwrite as O;
        use Wanted as W;
        // (mode, found, wanted) -> Ok(action) or Err("in the way")
        let table: Vec<(O, F, W, Result<E, &str>)> = vec![
            (O::Refuse, F::File, W::File, Err("already exists")),
            (O::Refuse, F::Symlink, W::File, Err("already exists")),
            (O::Refuse, F::Directory, W::File, Err("in the way")),
            (O::Refuse, F::File, W::Directory, Err("in the way")),
            (O::Refuse, F::Symlink, W::Directory, Err("in the way")),
            (O::Refuse, F::File, W::Symlink, Err("already exists")),
            (O::Skip, F::File, W::File, Ok(E::Skip)),
            (O::Skip, F::Symlink, W::File, Ok(E::Skip)),
            (O::Skip, F::Directory, W::File, Ok(E::Skip)),
            (O::Skip, F::File, W::Directory, Ok(E::Skip)),
            (O::Skip, F::Symlink, W::Symlink, Ok(E::Skip)),
            (O::Rename, F::File, W::File, Ok(E::Rename)),
            (O::Rename, F::Symlink, W::File, Ok(E::Rename)),
            (O::Rename, F::Directory, W::File, Err("in the way")),
            (O::Rename, F::File, W::Directory, Err("in the way")),
            (O::Replace, F::File, W::File, Ok(E::Replace)),
            (O::Replace, F::Symlink, W::File, Ok(E::Replace)),
            (O::Replace, F::Symlink, W::Symlink, Ok(E::Replace)),
            (O::Replace, F::Directory, W::File, Err("in the way")),
            (O::Replace, F::Directory, W::Symlink, Err("in the way")),
            (O::Replace, F::File, W::Directory, Err("in the way")),
        ];
        for (mode, found, wanted, want) in table {
            let p = Policy::new(mode, Symlinks::Skip);
            let got = p.existing(&f, found, wanted);
            match (got, want) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "{mode:?} {found:?} {wanted:?}"),
                (
                    Err(CoreError::InTheWay {
                        path,
                        found: fo,
                        wanted: wa,
                    }),
                    Err("in the way"),
                ) => {
                    assert_eq!(path, f);
                    assert_eq!(fo, found.name());
                    assert_eq!(wa, wanted.name());
                }
                (Err(CoreError::Format(FormatError::Io(io))), Err("already exists")) => {
                    assert_eq!(io.kind(), std::io::ErrorKind::AlreadyExists);
                }
                (got, want) => panic!("{mode:?} {found:?} {wanted:?}: {got:?} wanted {want:?}"),
            }
        }
        let e = Policy::new(O::Replace, Symlinks::Skip)
            .existing(Path::new("out/x"), F::Directory, W::File)
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "{}: a directory exists where a file goes",
                Path::new("out/x").display()
            )
        );
        // The default hook: `overwrite` decides and the target is still written create-new.
        assert_eq!(
            DefaultPolicy
                .existing(&d.path().join("absent"), F::File, W::File)
                .unwrap(),
            E::Write
        );
        assert!(DefaultPolicy.existing(&f, F::File, W::File).is_err());
    }

    #[test]
    fn rename_pattern_table() {
        let never = |_: &Path| false;
        let table = [
            ("a.txt", "a (1).txt"),
            ("a.tar.gz", "a.tar (1).gz"),
            ("name", "name (1)"),
            (".bashrc", ".bashrc (1)"),
            ("a (1).txt", "a (1) (1).txt"),
        ];
        for (from, to) in table {
            let got = rename_target(&Path::new("d").join(from), never);
            assert_eq!(got, Path::new("d").join(to), "{from}");
        }
        // The smallest free k, against the file system.
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("r.bin");
        std::fs::write(&t, b"0").unwrap();
        std::fs::write(d.path().join("r (1).bin"), b"1").unwrap();
        std::fs::write(d.path().join("r (2).bin"), b"2").unwrap();
        let got = rename_target(&t, |p| std::fs::symlink_metadata(p).is_ok());
        assert_eq!(got, d.path().join("r (3).bin"));
    }

    #[test]
    fn case_collision_table() {
        let ok = [file("a"), file("b"), dir("Ab"), file("Ab/c"), file("ab")];
        // "Ab" (a directory) and "ab" (a file) collide: refused, naming both in entry order.
        let e = check_case_collisions(&ok).unwrap_err();
        assert!(
            matches!(&e, CoreError::CaseCollision { first, second } if first == "Ab" && second == "ab"),
            "{e:?}"
        );
        assert_eq!(
            e.to_string(),
            "entries \"Ab\" and \"ab\" differ only by case; the target is case-insensitive"
        );
        let rows: [(&[&str], bool); 5] = [
            (&["a", "b"], true),
            (&["dir/x", "DIR/X"], false),
            (&["\u{E9}", "\u{C9}"], false),
            (&["a/b", "a/b.txt"], true),
            (&[], true),
        ];
        for (paths, ok) in rows {
            let entries: Vec<Entry> = paths.iter().map(|p| file(p)).collect();
            assert_eq!(check_case_collisions(&entries).is_ok(), ok, "{paths:?}");
        }
        #[cfg(any(windows, target_os = "macos"))]
        assert!(case_insensitive_target(Path::new(".")));
        #[cfg(not(any(windows, target_os = "macos")))]
        assert!(!case_insensitive_target(Path::new(".")));
    }

    #[test]
    fn remove_existing_removes_a_link_as_a_link_and_never_a_directory() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        std::fs::write(&f, b"1").unwrap();
        remove_existing(&f).unwrap();
        assert!(!f.exists());
        let dd = d.path().join("dir");
        std::fs::create_dir(&dd).unwrap();
        std::fs::write(dd.join("inside"), b"2").unwrap();
        let e = remove_existing(&dd).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(dd.join("inside").exists());
        // A link to the directory: removed, the directory and its content untouched.
        let l = d.path().join("l");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dd, &l).unwrap();
        #[cfg(windows)]
        {
            let made = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&l)
                .arg(&dd)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !made {
                println!("SKIPPED: mklink /J refused on this machine");
                return;
            }
        }
        assert!(std::fs::symlink_metadata(&l)
            .unwrap()
            .file_type()
            .is_symlink());
        remove_existing(&l).unwrap();
        assert!(std::fs::symlink_metadata(&l).is_err());
        assert!(dd.join("inside").exists());
        // A dangling link to a file.
        #[cfg(unix)]
        {
            let dl = d.path().join("dangling");
            std::os::unix::fs::symlink("nowhere", &dl).unwrap();
            remove_existing(&dl).unwrap();
            assert!(std::fs::symlink_metadata(&dl).is_err());
        }
    }
}
