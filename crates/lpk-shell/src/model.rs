//! The safe part of the handler (`docs/notes/e6-design.md` sections 1 and 2): the menu's verbs,
//! their texts and the `lpk-gui.exe` flags they send, the selection kinds, each verb's state per
//! kind, the computed titles and the request a verb makes (E6-2). Nothing here touches the shell,
//! the registry or a process, so it is tested on every OS.

/// The title of the one top-level command, which holds the seven verbs as subcommands.
pub const ROOT_TITLE: &str = "LitePack";

/// The seven verbs of the "LitePack" submenu (decision (b)), in menu order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// `Add to archive…`: the create wizard with the selection as input.
    AddToArchive,
    /// `Add to "<name>.lpk"`: creates at once with the defaults.
    AddToNamed,
    /// `Extract here`.
    ExtractHere,
    /// `Extract to "<name>\"`.
    ExtractTo,
    /// `Open`.
    Open,
    /// `Test`.
    Test,
    /// `Convert to…`: a foreign archive re-created as `.lpk`.
    Convert,
}

impl Verb {
    /// Every verb, in menu order.
    pub const ALL: [Verb; 7] = [
        Verb::AddToArchive,
        Verb::AddToNamed,
        Verb::ExtractHere,
        Verb::ExtractTo,
        Verb::Open,
        Verb::Test,
        Verb::Convert,
    ];

    /// The menu text before `<name>` is computed (E6-2 substitutes it from the selection).
    pub fn text(self) -> &'static str {
        match self {
            Verb::AddToArchive => "Add to archive\u{2026}",
            Verb::AddToNamed => "Add to \"<name>.lpk\"",
            Verb::ExtractHere => "Extract here",
            Verb::ExtractTo => "Extract to \"<name>\\\"",
            Verb::Open => "Open",
            Verb::Test => "Test",
            Verb::Convert => "Convert to\u{2026}",
        }
    }

    /// The `lpk-gui.exe` flag of E5-7 (`docs/API.md`, "Activation verbs"). `Add to "<name>.lpk"`
    /// sends `--create` followed by `--name <name>.lpk`.
    pub fn flag(self) -> &'static str {
        match self {
            Verb::AddToArchive | Verb::AddToNamed => "--create",
            Verb::ExtractHere => "--extract-here",
            Verb::ExtractTo => "--extract-to",
            Verb::Open => "--open",
            Verb::Test => "--test",
            Verb::Convert => "--convert",
        }
    }
}

/// The most items the menu scans; past it only `Add to archive…` is shown (design section 2).
pub const MAX_ITEMS: usize = 16_384;

/// The longest `<name>`, in characters, a title shows; longer ones are shortened in the middle.
pub const MAX_TITLE_NAME: usize = 40;

/// The name used when an item has none to give (a drive's root).
pub const FALLBACK_NAME: &str = "Archive";

/// The archive extensions, one list kept in `archive_extensions.txt`.
const ARCHIVE_EXTENSIONS: &str = include_str!("archive_extensions.txt");

/// The extensions Explorer's menu treats as archives (lower case, no dot).
pub fn archive_extensions() -> impl Iterator<Item = &'static str> {
    ARCHIVE_EXTENSIONS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

fn is_archive_extension(ext: &str) -> bool {
    archive_extensions().any(|e| e.eq_ignore_ascii_case(ext))
}

/// `s` less `suffix`, matched without regard to ASCII case.
fn strip_suffix_ci<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    let at = s.len().checked_sub(suffix.len())?;
    let tail = s.get(at..)?;
    tail.eq_ignore_ascii_case(suffix).then(|| s.get(..at))?
}

/// `name` split at its last dot: `(stem, extension)`; a leading dot alone is no extension.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (
            name.get(..i).unwrap_or(name),
            name.get(i + 1..).unwrap_or(""),
        ),
        _ => (name, ""),
    }
}

/// The stem of an archive's file name, or `None` when the name is no archive's. An archive's
/// `.tar.gz`-style pair is dropped whole, and a volume of a multi-volume set (`x.7z.001`,
/// `x.part2.rar`) gives the set's stem, so every volume of a set has the same one.
pub fn archive_stem(name: &str) -> Option<String> {
    let (before, ext) = split_ext(name);
    // `x.7z.001` and onward: three digits or more after a listed extension.
    if ext.len() >= 3 && ext.bytes().all(|b| b.is_ascii_digit()) {
        let (stem, inner) = split_ext(before);
        if !inner.is_empty() && is_archive_extension(inner) {
            return Some(drop_tar(stem).to_owned());
        }
        return None;
    }
    if ext.is_empty() || !is_archive_extension(ext) {
        return None;
    }
    // `x.part1.rar`: a `.part<digits>` before a listed extension.
    let (stem, part) = split_ext(before);
    if part.len() > 4
        && part
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case("part"))
        && part
            .get(4..)
            .is_some_and(|d| d.bytes().all(|b| b.is_ascii_digit()))
    {
        return Some(stem.to_owned());
    }
    Some(drop_tar(before).to_owned())
}

fn drop_tar(stem: &str) -> &str {
    match strip_suffix_ci(stem, ".tar") {
        Some(s) if !s.is_empty() => s,
        _ => stem,
    }
}

/// The last component of a path written with either separator (no trailing separators).
pub fn leaf(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['\\', '/']);
    trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed)
}

/// What the menu reads of one selected item: names and attributes only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The item's file name.
    pub name: String,
    /// A real folder: `SFGAO_FOLDER` without `SFGAO_STREAM`, so a ZIP (folder and stream to
    /// Explorer) is a file.
    pub is_dir: bool,
    /// The full path of the folder holding the item.
    pub parent: String,
}

/// The selection kinds of design section 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Files, none an archive.
    F,
    /// Folders.
    D,
    /// The folder background (no items).
    B,
    /// One `.lpk`.
    L,
    /// One foreign archive (one multi-volume set included).
    A,
    /// Several archives.
    S,
    /// Anything else.
    M,
    /// More than [`MAX_ITEMS`] items, of any kind.
    Over,
}

/// Whether `verb` is shown for `kind` (the design's table; a blank is `ECS_HIDDEN`).
pub fn visible(kind: Kind, verb: Verb) -> bool {
    use Kind::*;
    match verb {
        Verb::AddToArchive => true,
        Verb::AddToNamed => matches!(kind, F | D | M),
        Verb::ExtractHere | Verb::ExtractTo | Verb::Test => matches!(kind, L | A | S),
        Verb::Open => matches!(kind, L | A),
        Verb::Convert => matches!(kind, A),
    }
}

/// A classified selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The kind.
    pub kind: Kind,
    /// `<name>` in full, already safe as a file name (empty for the background).
    pub name: String,
    /// The archives the archive verbs take: per set, the first volume's index in the items.
    pub archives: Vec<usize>,
    /// How many items the selection holds.
    pub count: usize,
}

/// `name` made safe as the stem of a file name: Windows' forbidden characters replaced, no
/// trailing dots or spaces, never empty.
fn file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        FALLBACK_NAME.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Classify a selection of `count` items read lazily from `items` (`None`: an item without a
/// file-system path). `None` hides the menu. Past [`MAX_ITEMS`] nothing is read.
pub fn classify(count: usize, items: impl IntoIterator<Item = Option<Entry>>) -> Option<Selection> {
    if count > MAX_ITEMS {
        return Some(Selection {
            kind: Kind::Over,
            name: String::new(),
            archives: Vec::new(),
            count,
        });
    }
    let mut entries = Vec::with_capacity(count);
    for item in items.into_iter().take(count) {
        entries.push(item?);
    }
    if entries.is_empty() {
        return Some(Selection {
            kind: Kind::B,
            name: String::new(),
            archives: Vec::new(),
            count: 0,
        });
    }
    // Per entry: the archive stem (never for a real folder).
    let stems: Vec<Option<String>> = entries
        .iter()
        .map(|e| {
            if e.is_dir {
                None
            } else {
                archive_stem(&e.name)
            }
        })
        .collect();
    let n_archives = stems.iter().flatten().count();
    let n_dirs = entries.iter().filter(|e| e.is_dir).count();
    // One representative per set (same folder, same stem): the first name in order.
    let mut sets: Vec<(String, String, usize)> = Vec::new();
    for (i, (e, stem)) in entries.iter().zip(&stems).enumerate() {
        let Some(stem) = stem else { continue };
        let key = (e.parent.to_lowercase(), stem.to_lowercase());
        match sets.iter_mut().find(|s| (&s.0, &s.1) == (&key.0, &key.1)) {
            Some(set) => {
                let better = entries
                    .get(set.2)
                    .is_none_or(|cur| e.name.to_lowercase() < cur.name.to_lowercase());
                if better {
                    set.2 = i;
                }
            }
            None => sets.push((key.0, key.1, i)),
        }
    }
    let archives: Vec<usize> = sets.iter().map(|s| s.2).collect();
    let count = entries.len();
    let first = entries.first()?;
    let kind = if n_archives == count {
        match (
            archives.as_slice(),
            archives.first().and_then(|&i| entries.get(i)),
        ) {
            ([_], Some(rep)) if split_ext(&rep.name).1.eq_ignore_ascii_case("lpk") => Kind::L,
            ([_], _) => Kind::A,
            _ => Kind::S,
        }
    } else if n_archives > 0 {
        Kind::M
    } else if n_dirs == count {
        Kind::D
    } else if n_dirs == 0 {
        Kind::F
    } else {
        Kind::M
    };
    let name = if matches!(kind, Kind::L | Kind::A) {
        archives
            .first()
            .and_then(|&i| stems.get(i))
            .cloned()
            .flatten()
            .unwrap_or_default()
    } else if count == 1 {
        if first.is_dir {
            first.name.clone()
        } else {
            split_ext(&first.name).0.to_owned()
        }
    } else {
        leaf(&first.parent).to_owned()
    };
    Some(Selection {
        kind,
        name: file_stem(&name),
        archives,
        count,
    })
}

/// The characters a title shows escaped: controls and Unicode's bidirectional formatting
/// characters (UTS 36), as `\u{XXXX}`.
fn is_bidi(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// `name` for a menu title: shortened in the middle past [`MAX_TITLE_NAME`] characters, control
/// and bidirectional characters escaped, `&` doubled (it marks an access key in a menu).
pub fn title_name(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let shown: Vec<char> = if chars.len() > MAX_TITLE_NAME {
        let head = (MAX_TITLE_NAME - 1) / 2;
        let tail = MAX_TITLE_NAME - 1 - head;
        let mut v: Vec<char> = chars.iter().take(head).copied().collect();
        v.push('\u{2026}');
        v.extend(chars.iter().skip(chars.len() - tail));
        v
    } else {
        chars
    };
    let mut out = String::new();
    for c in shown {
        if c == '&' {
            out.push_str("&&");
        } else if c.is_control() || is_bidi(c) {
            out.push_str(&format!("\\u{{{:04X}}}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

/// The longest title, in characters, [`title`] can return: the widest text around a name of
/// [`MAX_TITLE_NAME`] characters, each escaped to ten characters (`\u{XXXXXX}` at the most).
pub const MAX_TITLE_CHARS: usize = 32 + MAX_TITLE_NAME * 10;

/// The menu text of `verb` for `selection`.
pub fn title(verb: Verb, selection: &Selection) -> String {
    let name = title_name(&selection.name);
    match verb {
        Verb::AddToNamed => format!("Add to \"{name}.lpk\""),
        Verb::ExtractTo if selection.kind == Kind::S => "Extract to \"*\\\"".to_owned(),
        Verb::ExtractTo => format!("Extract to \"{name}\\\""),
        other => other.text().to_owned(),
    }
}

/// What a verb sends the GUI for a selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The GUI's verb flag.
    pub flag: &'static str,
    /// `--name`'s value, `<name>.lpk` in full, for `Add to "<name>.lpk"`.
    pub name: Option<String>,
    /// The selected items to send, by index.
    pub items: Vec<usize>,
    /// Send the background folder instead (`items` is empty).
    pub background: bool,
}

/// The request `verb` makes for `selection`, or `None` when the verb is hidden for it.
pub fn request(verb: Verb, selection: &Selection) -> Option<Request> {
    if !visible(selection.kind, verb) {
        return None;
    }
    let all: Vec<usize> = (0..selection.count).collect();
    let (items, name) = match verb {
        Verb::AddToArchive => (all, None),
        Verb::AddToNamed => (all, Some(format!("{}.lpk", selection.name))),
        _ => (selection.archives.clone(), None),
    };
    let background = selection.kind == Kind::B;
    Some(Request {
        flag: verb.flag(),
        name,
        items,
        background,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use std::path::Path;

    /// Every verb's text and flag appear together in a row of `docs/API.md`'s table, so the
    /// menu and the GUI's parser cannot drift apart silently.
    #[test]
    fn verbs_match_the_api_table() {
        let api = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/API.md"),
        )
        .expect("docs/API.md");
        let rows = api.lines().filter(|l| l.starts_with("| `--")).count();
        assert_eq!(
            rows,
            Verb::ALL.len(),
            "docs/API.md's verb table and Verb::ALL differ in length"
        );
        for verb in Verb::ALL {
            let text = verb.text().replace('<', "&lt;").replace('>', "&gt;");
            let row = api
                .lines()
                .filter(|l| l.starts_with("| `--"))
                .find(|l| l.contains(&format!("| {text} |")))
                .unwrap_or_else(|| unreachable!("no API row for {text}"));
            assert!(row.contains(&format!("`{}", verb.flag())), "{row}");
            if verb == Verb::AddToNamed {
                assert!(row.contains("--name"), "{row}");
            }
        }
    }
}
