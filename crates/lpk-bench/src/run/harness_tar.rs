//! The runner's own tar stream (E2-23 (b), D-89 (4)): where the system `tar` cannot take a
//! class's names (the Windows bsdtar crashes on a name outside the ANSI code page and rewrites
//! one inside it, so the nine emoji and CJK names of `backup-versions-large` left `store`,
//! `zstd` and `xz` without rows), `lpk-bench` writes the tar stream itself, with the `tar`
//! crate (MIT OR Apache-2.0): ustar headers, and a PAX extended header (`path`, and `linkpath`
//! for a symlink) for every name that is not ASCII or does not fit the ustar fields. The same
//! entries in the same order the system tar writes for `-cf A -C DIR .` (the entry `./`, then
//! the directory's entries in file-system order, a directory's header before its contents) or
//! for `-cf A -T LIST` (the listed paths, in list order), and the same file bytes. One known
//! difference: a hard link is written as a full regular file where GNU tar and bsdtar write a
//! link entry (the file data is the same; the stream differs on a class with hard links; none
//! is known in `full`). The stream is flushed to the OS and not fsynced, like the system tar's:
//! the runner flushes between steps, outside the timing.
//!
//! The stream is produced by a child process, `lpk-bench tar ...`, with the store row's own
//! argument shapes (`-cf {archive} -C {input} .`, `-cf {archive} -T {list}`,
//! `-xf {archive} -C {outdir}`), so that the step is measured exactly as the system tar's is
//! (process tree wall and CPU time, peak memory, the same stripped environment). Extraction
//! uses the `tar` crate's reader.
//!
//! The choice is per class and automatic ([`choose`]): a class without a non-ASCII name always
//! gets the system tar (nothing changes on such classes: G3 is measured against the system
//! tar's rate); with the Windows bsdtar a non-ASCII name is a known rejection (no process is
//! run); with any other tar a dry run over the class's non-ASCII relative paths decides. A dry
//! run that cannot be set up (the tar missing, the temporary directory unwritable) is an error
//! that aborts the run: the control is never changed silently. The row records the provenance
//! in `measurement.tar.tool` ([`HARNESS_TAR`], or the system tar's version as before) and the
//! reason in `measurement.tar.note`. No reason text carries a file name (D-20: a private
//! corpus's names reach neither a result file nor the console).

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tar::{Builder, EntryType, Header};

/// What `measurement.tar.tool` records when the runner wrote the stream itself.
pub const HARNESS_TAR: &str = "lpk-bench";

/// The tar format the harness writes (`measurement.tar.format`).
pub const HARNESS_FORMAT: &str = "pax";

/// The ustar `name` field.
const USTAR_NAME: usize = 100;
/// The ustar `prefix` field.
const USTAR_PREFIX: usize = 155;
/// Where the `prefix` field starts in a 512-byte header.
const PREFIX_AT: usize = 345;

// ---------------------------------------------------------------------------------------------
// Selection

/// Which tar writes a class's stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TarChoice {
    /// The system tar (the `store` tool as discovered).
    System,
    /// The runner's own writer, with why the system tar was set aside (`reason`, for the
    /// console) and the short form recorded in the row (`note`, `measurement.tar.note`).
    Harness { reason: String, note: String },
}

/// What a dry run of the system tar found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DryRun {
    /// Every name came back unchanged.
    Accepted,
    /// The tar failed or changed a name: why, without any name.
    Rejected(String),
}

/// How the choice is made (`lpk-bench run --tar`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TarPolicy {
    /// Per class: the system tar unless a name of the class is known to be rejected or a dry
    /// run rejects it.
    #[default]
    Auto,
    /// Always the system tar (a rejected name then fails the row as before).
    System,
    /// Always the runner's own writer.
    Harness,
}

impl TarPolicy {
    pub fn parse(s: &str) -> Option<TarPolicy> {
        match s {
            "auto" => Some(TarPolicy::Auto),
            "system" => Some(TarPolicy::System),
            "lpk-bench" => Some(TarPolicy::Harness),
            _ => None,
        }
    }
}

/// The non-ASCII relative paths of a class (a directory component counts), each once.
pub fn non_ascii_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let set: BTreeSet<String> = paths
        .into_iter()
        .filter(|p| !p.is_ascii())
        .map(str::to_string)
        .collect();
    set.into_iter().collect()
}

/// Choose the tar of a class. `paths` are the class's non-ASCII relative paths
/// ([`non_ascii_paths`]); `known_rejection` says the system tar is the Windows bsdtar, which
/// rejects every non-ASCII name (it crashes on a name outside the ANSI code page and rewrites
/// one inside it); otherwise `dry_run` runs the system tar over those paths. A class without
/// a non-ASCII path always gets the system tar. `Err`: the dry run could not be set up (the
/// caller aborts the run; the control is never changed silently). No text carries a name.
pub fn choose(
    policy: TarPolicy,
    paths: &[String],
    known_rejection: Option<&str>,
    dry_run: impl FnOnce(&[String]) -> std::result::Result<DryRun, String>,
) -> std::result::Result<TarChoice, String> {
    match policy {
        TarPolicy::System => return Ok(TarChoice::System),
        TarPolicy::Harness => {
            return Ok(TarChoice::Harness {
                reason: "--tar lpk-bench".to_string(),
                note: "--tar lpk-bench".to_string(),
            })
        }
        TarPolicy::Auto => {}
    }
    if paths.is_empty() {
        return Ok(TarChoice::System);
    }
    if let Some(tar) = known_rejection {
        return Ok(TarChoice::Harness {
            reason: format!(
                "the Windows bsdtar crashes on or rewrites non-ASCII names; the class has {} \
                 such path(s)",
                paths.len()
            ),
            note: format!("known rejection on {} ({tar})", std::env::consts::OS),
        });
    }
    match dry_run(paths)? {
        DryRun::Accepted => Ok(TarChoice::System),
        DryRun::Rejected(why) => Ok(TarChoice::Harness {
            reason: format!(
                "the system tar failed a dry run over the class's {} non-ASCII path(s): {why}",
                paths.len()
            ),
            note: format!("dry run: {why}"),
        }),
    }
}

/// The dry run: the system tar over a temporary tree holding one empty file per path (the
/// directories included), read back with the `tar` crate; every path must come back
/// byte-equal. `Err` when the run could not be set up (no name in any text).
pub fn dry_run_system_tar(
    exe: &Path,
    paths: &[String],
    tmp: &Path,
) -> std::result::Result<DryRun, String> {
    let dir = tmp.join(format!("tar-probe-{}", std::process::id()));
    let tree = dir.join("t");
    let _ = fs::remove_dir_all(&dir);
    let result = (|| {
        fs::create_dir_all(&tree).map_err(|e| format!("the probe tree: {e}"))?;
        for (i, p) in paths.iter().enumerate() {
            let full = p.split('/').fold(tree.clone(), |a, c| a.join(c));
            let parent = full.parent().unwrap_or(&tree);
            fs::create_dir_all(parent)
                .and_then(|()| fs::write(&full, b""))
                .map_err(|e| format!("probe path {i} could not be written: {e}"))?;
        }
        let archive = dir.join("probe.tar");
        let out = std::process::Command::new(exe)
            .arg("-cf")
            .arg(&archive)
            .arg("-C")
            .arg(&tree)
            .arg(".")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("could not run the tar: {e}"))?;
        if !out.status.success() {
            return Ok(DryRun::Rejected(match out.status.code() {
                Some(c) => format!("tar exited {c}"),
                None => "tar ended without an exit code".to_string(),
            }));
        }
        let found = match entry_names(&archive) {
            Ok(f) => f,
            Err(e) => {
                return Ok(DryRun::Rejected(format!(
                    "the probe archive could not be read: {e}"
                )))
            }
        };
        let changed = paths
            .iter()
            .filter(|p| {
                let want = format!("./{p}");
                !found.iter().any(|f| f == want.as_bytes())
            })
            .count();
        Ok(if changed == 0 {
            DryRun::Accepted
        } else {
            DryRun::Rejected(format!(
                "{changed} of {} path(s) did not come back unchanged",
                paths.len()
            ))
        })
    })();
    let _ = fs::remove_dir_all(&dir);
    result
}

// ---------------------------------------------------------------------------------------------
// Writer

/// One entry of the stream, in order.
struct Item {
    /// The name as the tar records it (`./a/b`, a directory with a trailing `/`).
    name: String,
    path: PathBuf,
    meta: fs::Metadata,
}

/// The entries of `-cf A -C root .`: `./`, then the tree in file-system order, a directory's
/// header before its contents.
fn walk_tree(root: &Path) -> io::Result<Vec<Item>> {
    fn visit(dir: &Path, prefix: &str, out: &mut Vec<Item>) -> io::Result<()> {
        for e in fs::read_dir(dir)? {
            let e = e?;
            let path = e.path();
            let name = e.file_name();
            let Some(name) = name.to_str() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("the name {name:?} is not UTF-8"),
                ));
            };
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                let name = format!("{prefix}{name}/");
                out.push(Item {
                    name: name.clone(),
                    path: path.clone(),
                    meta,
                });
                visit(&path, &name, out)?;
            } else {
                out.push(Item {
                    name: format!("{prefix}{name}"),
                    path,
                    meta,
                });
            }
        }
        Ok(())
    }
    let mut out = vec![Item {
        name: "./".to_string(),
        path: root.to_path_buf(),
        meta: fs::metadata(root)?,
    }];
    visit(root, "./", &mut out)?;
    Ok(out)
}

/// The entries of `-cf A -T list`: the listed relative paths, in list order, under `cwd`.
fn listed(list: &Path, cwd: &Path) -> io::Result<Vec<Item>> {
    let text = fs::read_to_string(list)?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let path = line.split('/').fold(cwd.to_path_buf(), |p, c| p.join(c));
        let meta = fs::symlink_metadata(&path)?;
        let name = if meta.is_dir() && !line.ends_with('/') {
            format!("{line}/")
        } else {
            line.to_string()
        };
        out.push(Item { name, path, meta });
    }
    Ok(out)
}

fn mode_of(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        if meta.is_dir() {
            0o755
        } else {
            0o644
        }
    }
}

fn mtime_of(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Truncate at a character boundary to at most `max` bytes.
fn truncated(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Put `name` in the ustar fields as it is (`./` kept): the `name` field when it fits, else
/// split at a `/` between `prefix` and `name`. `false` when it cannot be placed.
fn set_ustar_name(h: &mut Header, name: &str) -> bool {
    let bytes = name.as_bytes();
    let raw = h.as_mut_bytes();
    if bytes.len() <= USTAR_NAME {
        raw[..bytes.len()].copy_from_slice(bytes);
        return true;
    }
    // The split keeps `prefix` without its trailing `/`; `name` is what follows it.
    for (i, b) in bytes.iter().enumerate().rev() {
        if *b == b'/' && i > 0 && i <= USTAR_PREFIX && bytes.len() - i - 1 <= USTAR_NAME {
            let rest = &bytes[i + 1..];
            if rest.is_empty() {
                continue;
            }
            raw[..rest.len()].copy_from_slice(rest);
            raw[PREFIX_AT..PREFIX_AT + i].copy_from_slice(&bytes[..i]);
            return true;
        }
    }
    false
}

/// Whether `name` needs a PAX `path` record: not ASCII, or not placeable in the ustar fields.
fn needs_pax(name: &str, placeable: bool) -> bool {
    !name.is_ascii() || !placeable
}

/// Append one item: a PAX extended header first when the name or the link target needs it.
fn append_item<W: Write>(b: &mut Builder<W>, item: &Item) -> io::Result<()> {
    let mut h = Header::new_ustar();
    let placeable = set_ustar_name(&mut h, &item.name);
    if !placeable {
        // A readable stand-in; the PAX record carries the name.
        let short = truncated(&item.name, USTAR_NAME);
        let raw = h.as_mut_bytes();
        raw[..short.len()].copy_from_slice(short.as_bytes());
    }
    h.set_mode(mode_of(&item.meta));
    h.set_uid(0);
    h.set_gid(0);
    h.set_mtime(mtime_of(&item.meta));
    let mut pax: Vec<(&str, Vec<u8>)> = Vec::new();
    if needs_pax(&item.name, placeable) {
        pax.push(("path", item.name.as_bytes().to_vec()));
    }
    let ft = item.meta.file_type();
    if ft.is_dir() {
        h.set_entry_type(EntryType::Directory);
        h.set_size(0);
    } else if ft.is_symlink() {
        let target = fs::read_link(&item.path)?;
        let target = target.to_string_lossy().replace('\\', "/");
        h.set_entry_type(EntryType::Symlink);
        h.set_size(0);
        if target.len() <= USTAR_NAME && target.is_ascii() {
            h.set_link_name_literal(target.as_bytes())?;
        } else {
            h.set_link_name_literal(truncated(&target, USTAR_NAME).as_bytes())?;
            pax.push(("linkpath", target.into_bytes()));
        }
    } else {
        h.set_entry_type(EntryType::Regular);
        h.set_size(item.meta.len());
    }
    h.set_cksum();
    if !pax.is_empty() {
        b.append_pax_extensions(pax.iter().map(|(k, v)| (*k, v.as_slice())))?;
    }
    if ft.is_file() {
        let f = File::open(&item.path)?;
        let mut r = BufReader::with_capacity(1 << 20, f);
        let mut limited = (&mut r).take(item.meta.len());
        b.append(&h, &mut limited)?;
    } else {
        b.append(&h, io::empty())?;
    }
    Ok(())
}

fn write_items(archive: &Path, items: &[Item]) -> io::Result<()> {
    let f = File::create(archive)?;
    let mut b = Builder::new(BufWriter::with_capacity(1 << 20, f));
    for item in items {
        append_item(&mut b, item)?;
    }
    // Flushed to the OS, not fsynced: the system tar does not fsync either, and the runner
    // flushes between steps outside the timing.
    let w = b.into_inner()?;
    w.into_inner().map_err(|e| e.into_error())?;
    Ok(())
}

/// `-cf archive -C root .`
pub fn create_tree(archive: &Path, root: &Path) -> io::Result<()> {
    write_items(archive, &walk_tree(root)?)
}

/// `-cf archive -T list`, the paths relative to `cwd`.
pub fn create_list(archive: &Path, list: &Path, cwd: &Path) -> io::Result<()> {
    write_items(archive, &listed(list, cwd)?)
}

/// `-xf archive -C outdir`, with the `tar` crate's reader.
pub fn extract(archive: &Path, outdir: &Path) -> io::Result<()> {
    let f = File::open(archive)?;
    let mut a = tar::Archive::new(BufReader::with_capacity(1 << 20, f));
    a.set_preserve_permissions(false);
    a.set_preserve_mtime(true);
    a.unpack(outdir)
}

/// The entry names of a tar, as the `tar` crate reads them (PAX `path` applied).
pub fn entry_names(archive: &Path) -> io::Result<Vec<Vec<u8>>> {
    let f = File::open(archive)?;
    let mut a = tar::Archive::new(BufReader::new(f));
    let mut out = Vec::new();
    for e in a.entries()? {
        let e = e?;
        out.push(e.path_bytes().into_owned());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// The child process

/// `lpk-bench tar <args>`: the store row's argument shapes, nothing else.
pub fn command(args: &[String]) -> Result<()> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["-cf", archive, "-C", input, "."] => create_tree(Path::new(archive), Path::new(input))
            .with_context(|| format!("writing {archive} from {input}")),
        ["-cf", archive, "-T", list] => {
            let cwd = std::env::current_dir().context("the working directory")?;
            create_list(Path::new(archive), Path::new(list), &cwd)
                .with_context(|| format!("writing {archive} from the list {list}"))
        }
        ["-xf", archive, "-C", outdir] => extract(Path::new(archive), Path::new(outdir))
            .with_context(|| format!("extracting {archive} into {outdir}")),
        other => bail!(
            "lpk-bench tar takes `-cf ARCHIVE -C DIR .`, `-cf ARCHIVE -T LIST` or \
             `-xf ARCHIVE -C DIR`; got {other:?}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn tree(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
        fn walk(dir: &Path, base: &str, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
            for e in fs::read_dir(dir).expect("read_dir") {
                let e = e.expect("entry");
                let name = e.file_name().to_str().expect("utf-8").to_string();
                let rel = format!("{base}{name}");
                if e.path().is_dir() {
                    out.insert(format!("{rel}/"), None);
                    walk(&e.path(), &format!("{rel}/"), out);
                } else {
                    out.insert(rel, Some(fs::read(e.path()).expect("read")));
                }
            }
        }
        let mut m = BTreeMap::new();
        walk(root, "", &mut m);
        m
    }

    fn long_name(n: usize) -> String {
        (0..n).map(|i| (b'a' + (i % 26) as u8) as char).collect()
    }

    /// A tree with emoji, CJK, Cyrillic and combining-mark names, a 200-character name, a path
    /// above 260 characters (a split that no ustar prefix/name pair takes), an empty directory
    /// and an empty file.
    fn make_tree(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
        let deep: String = (0..14)
            .map(|i| format!("level-{i:02}-of-the-tree/"))
            .collect();
        assert!(deep.len() > 260);
        let files: Vec<(String, Vec<u8>)> = vec![
            ("😀 emoji/snapshot 🚀.txt".into(), b"rocket".to_vec()),
            ("日本語/ファイル.bin".into(), vec![7u8; 3000]),
            ("marks/cafe\u{301}.txt".into(), b"nfd".to_vec()),
            ("marks/café.txt".into(), b"nfc".to_vec()),
            ("привет/мир.txt".into(), b"cyrillic".to_vec()),
            (format!("long/{}", long_name(200)), b"long name".to_vec()),
            (format!("{deep}end.txt"), b"deep".to_vec()),
            ("plain.txt".into(), b"ascii".to_vec()),
            ("zero".into(), Vec::new()),
        ];
        for (name, data) in &files {
            let p = name.split('/').fold(root.to_path_buf(), |p, c| p.join(c));
            fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            fs::write(&p, data).expect("write");
        }
        fs::create_dir_all(root.join("empty 🗂")).expect("mkdir");
        tree(root)
    }

    #[test]
    fn the_tree_round_trips_through_the_writer_and_the_reader_with_pax_names() {
        let tmp = tempfile::tempdir().expect("tmp");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        let want = make_tree(&src);
        let archive = tmp.path().join("a.tar");
        create_tree(&archive, &src).expect("create");
        // Every name as the system tar names it: `./` first, directories with a trailing `/`.
        let names = entry_names(&archive).expect("names");
        assert_eq!(names[0], b"./");
        let mut got: Vec<String> = names
            .iter()
            .map(|n| String::from_utf8(n.clone()).expect("utf-8"))
            .collect();
        got.sort();
        let mut expected: Vec<String> = want.keys().map(|k| format!("./{k}")).collect();
        expected.push("./".into());
        expected.sort();
        assert_eq!(got, expected);
        // A directory's header precedes its contents.
        let order: Vec<String> = names
            .iter()
            .map(|n| String::from_utf8(n.clone()).expect("utf-8"))
            .collect();
        for (i, n) in order.iter().enumerate() {
            if let Some(parent) = n.trim_end_matches('/').rsplit_once('/') {
                let dir = format!("{}/", parent.0);
                let at = order.iter().position(|x| *x == dir).expect("parent entry");
                assert!(at < i, "{dir} after {n}");
            }
        }
        let out = tmp.path().join("out");
        fs::create_dir_all(&out).expect("mkdir");
        extract(&archive, &out).expect("extract");
        assert_eq!(tree(&out), want);
    }

    #[test]
    fn pax_records_are_written_for_non_ascii_and_unplaceable_names_only() {
        let tmp = tempfile::tempdir().expect("tmp");
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("d")).expect("mkdir");
        fs::write(src.join("d/plain.txt"), b"x").expect("write");
        fs::write(src.join("d/ünï.txt"), b"y").expect("write");
        // 123 bytes with `./`: too long for the `name` field, split at the `/` into a 62-byte
        // prefix and a 60-byte name.
        let long = long_name(60);
        fs::create_dir_all(src.join(&long)).expect("mkdir");
        fs::write(src.join(&long).join(&long), b"z").expect("write");
        let archive = tmp.path().join("a.tar");
        create_tree(&archive, &src).expect("create");
        // Count the `x` (PAX) headers and check which entries they precede.
        let data = fs::read(&archive).expect("read");
        let mut pax_before: Vec<String> = Vec::new();
        let mut i = 0;
        let mut pending_pax = false;
        while i + 512 <= data.len() {
            let h = &data[i..i + 512];
            if h.iter().all(|b| *b == 0) {
                break;
            }
            let size = usize::from_str_radix(
                std::str::from_utf8(&h[124..135])
                    .expect("size")
                    .trim_end_matches('\0')
                    .trim(),
                8,
            )
            .expect("octal");
            let kind = h[156];
            if kind == b'x' {
                pending_pax = true;
            } else {
                if pending_pax {
                    let name = String::from_utf8_lossy(&h[..100])
                        .trim_end_matches('\0')
                        .to_string();
                    pax_before.push(name);
                }
                pending_pax = false;
            }
            i += 512 + size.div_ceil(512) * 512;
        }
        // The split name needs no PAX record; the non-ASCII name gets one; the ASCII ones none.
        assert_eq!(pax_before, vec!["./d/ünï.txt".to_string()]);
        let names = entry_names(&archive).expect("names");
        assert!(names
            .iter()
            .any(|n| n == format!("./{long}/{long}").as_bytes()));
    }

    #[test]
    fn the_list_form_writes_the_listed_paths_in_order() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path().join("root");
        fs::create_dir_all(root.join("дом")).expect("mkdir");
        fs::write(root.join("дом/файл.txt"), b"1").expect("write");
        fs::write(root.join("b.txt"), b"2").expect("write");
        let list = tmp.path().join("list.txt");
        fs::write(&list, "дом/файл.txt\nb.txt\n").expect("write");
        let archive = tmp.path().join("a.tar");
        create_list(&archive, &list, &root).expect("create");
        let names = entry_names(&archive).expect("names");
        assert_eq!(
            names,
            vec![
                b"\xd0\xb4\xd0\xbe\xd0\xbc/\xd1\x84\xd0\xb0\xd0\xb9\xd0\xbb.txt".to_vec(),
                b"b.txt".to_vec()
            ]
        );
        let out = tmp.path().join("out");
        fs::create_dir_all(&out).expect("mkdir");
        extract(&archive, &out).expect("extract");
        assert_eq!(tree(&out), tree(&root));
    }

    #[test]
    fn the_choice_is_system_without_non_ascii_names_and_follows_the_rejection_otherwise() {
        let none: Vec<String> = Vec::new();
        let some = vec!["日本語/plain.txt".to_string()];
        let never = |_: &[String]| -> std::result::Result<DryRun, String> {
            panic!("the dry run must not be called")
        };
        let bsd = Some("bsdtar 3.8.9");
        assert_eq!(
            choose(TarPolicy::Auto, &none, bsd, never).expect("ok"),
            TarChoice::System
        );
        assert_eq!(
            choose(TarPolicy::Auto, &none, None, never).expect("ok"),
            TarChoice::System
        );
        // The Windows bsdtar: a known rejection, no dry run; the note names the platform.
        match choose(TarPolicy::Auto, &some, bsd, never).expect("ok") {
            TarChoice::Harness { reason, note } => {
                assert!(
                    reason.contains("bsdtar") && reason.contains("1 such"),
                    "{reason}"
                );
                assert!(
                    note.starts_with("known rejection on ") && note.ends_with("(bsdtar 3.8.9)"),
                    "{note}"
                );
            }
            other => panic!("{other:?}"),
        }
        // Any other tar: the dry run decides.
        assert_eq!(
            choose(TarPolicy::Auto, &some, None, |_| Ok(DryRun::Accepted)).expect("ok"),
            TarChoice::System
        );
        match choose(TarPolicy::Auto, &some, None, |_| {
            Ok(DryRun::Rejected("tar exited 139".into()))
        })
        .expect("ok")
        {
            TarChoice::Harness { reason, note } => {
                assert!(
                    reason.contains("dry run") && reason.contains("139"),
                    "{reason}"
                );
                assert_eq!(note, "dry run: tar exited 139");
            }
            other => panic!("{other:?}"),
        }
        // A dry run that could not be set up is an error, never a silent change of the control.
        let err = choose(TarPolicy::Auto, &some, None, |_| {
            Err("could not run the tar: not found".into())
        })
        .expect_err("setup failure");
        assert!(err.contains("could not run the tar"), "{err}");
        // The overrides.
        assert_eq!(
            choose(TarPolicy::System, &some, bsd, never).expect("ok"),
            TarChoice::System
        );
        assert!(matches!(
            choose(TarPolicy::Harness, &none, None, never).expect("ok"),
            TarChoice::Harness { .. }
        ));
        assert_eq!(TarPolicy::parse("lpk-bench"), Some(TarPolicy::Harness));
        assert_eq!(TarPolicy::parse("nope"), None);
    }

    #[test]
    fn non_ascii_paths_keep_the_directory_component_and_are_unique_and_sorted() {
        let got = non_ascii_paths([
            "a/ü.txt",
            "a/ü.txt",
            "c/plain.txt",
            "日本語/plain.txt",
            "日/本.txt",
        ]);
        assert_eq!(
            got,
            vec![
                "a/ü.txt".to_string(),
                "日/本.txt".to_string(),
                "日本語/plain.txt".to_string()
            ]
        );
    }

    /// The dry run over nested paths: a missing tar is a setup error (no name in the text); the
    /// harness's own `tar` subcommand, used as the tar under test, accepts every path.
    #[test]
    fn the_dry_run_distinguishes_a_missing_tar_from_a_rejection_and_probes_full_paths() {
        let tmp = tempfile::tempdir().expect("tmp");
        let paths = vec!["日本語/plain.txt".to_string(), "marks/café.txt".to_string()];
        let err = dry_run_system_tar(Path::new("no-such-tar-binary-xyz"), &paths, tmp.path())
            .expect_err("missing tar");
        assert!(err.starts_with("could not run the tar"), "{err}");
        assert!(!err.contains("日本語") && !err.contains("café"), "{err}");
        // This executable (the test binary) is not a tar: it exits non-zero, a rejection.
        let me = std::env::current_exe().expect("exe");
        match dry_run_system_tar(&me, &paths, tmp.path()).expect("ran") {
            DryRun::Rejected(why) => {
                assert!(
                    why.starts_with("tar exited") || why.starts_with("tar ended"),
                    "{why}"
                );
                assert!(!why.contains("日本語") && !why.contains("café"), "{why}");
            }
            DryRun::Accepted => panic!("a test binary is not a tar"),
        }
        assert!(!tmp
            .path()
            .join(format!("tar-probe-{}", std::process::id()))
            .exists());
    }

    #[test]
    fn the_command_accepts_the_store_shapes_only() {
        let tmp = tempfile::tempdir().expect("tmp");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("f"), b"1").expect("write");
        let archive = tmp.path().join("a.tar").display().to_string();
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        command(&args(&[
            "-cf",
            &archive,
            "-C",
            &src.display().to_string(),
            ".",
        ]))
        .expect("c");
        let out = tmp.path().join("out");
        fs::create_dir_all(&out).expect("mkdir");
        command(&args(&["-xf", &archive, "-C", &out.display().to_string()])).expect("x");
        assert_eq!(fs::read(out.join("f")).expect("read"), b"1");
        assert!(command(&args(&["-cvf", &archive])).is_err());
    }

    /// The system tar reads the harness stream back (GNU tar and bsdtar both read PAX).
    #[test]
    fn the_system_tar_reads_the_harness_stream() {
        let tar = if cfg!(windows) {
            std::env::var_os("SystemRoot")
                .map(|r| Path::new(&r).join("System32").join("tar.exe"))
                .filter(|p| p.exists())
        } else {
            Some(PathBuf::from("tar"))
        };
        let Some(tar) = tar else {
            eprintln!("skipped: no system tar");
            return;
        };
        if std::process::Command::new(&tar)
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipped: the system tar does not run");
            return;
        }
        let tmp = tempfile::tempdir().expect("tmp");
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("d")).expect("mkdir");
        fs::write(src.join("d/plain.txt"), b"x").expect("write");
        let long = long_name(200);
        fs::write(src.join("d").join(&long), b"z").expect("write");
        // ASCII names only here: the Windows bsdtar rewrites non-ASCII names on output, which
        // is the very reason for this writer.
        let archive = tmp.path().join("a.tar");
        create_tree(&archive, &src).expect("create");
        let out = tmp.path().join("out");
        fs::create_dir_all(&out).expect("mkdir");
        let st = std::process::Command::new(&tar)
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&out)
            .status()
            .expect("run");
        assert!(st.success(), "{st}");
        assert_eq!(tree(&out), tree(&src));
    }
}
