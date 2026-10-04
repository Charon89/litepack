//! Clustering: files with the same class (and, for text, the same dictionary kind) are written
//! together so a block never mixes unlike data and the encoder knows which dictionary applies.

use std::io::Read;

use lpk_format::EntryKind;

use crate::classify::{classify, Class, SAMPLE_LEN};
use crate::error::CoreError;
use crate::ingest::Input;
use crate::source::Source;

/// Which caller-supplied dictionary suits a cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DictionaryKind {
    /// No dictionary.
    None,
    /// Natural-language text.
    Prose,
    /// Delimited or markup data (`csv json log jsonl ndjson xml yaml yml toml`).
    Structured,
    /// Source code.
    Source,
}

/// Extensions treated as source code.
const SOURCE_EXT: &[&str] = &[
    "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "rs", "go", "py", "js", "mjs", "ts", "tsx",
    "jsx", "java", "cs", "rb", "php", "sh", "bash", "swift", "kt", "scala", "lua", "pl", "cmake",
    "mk", "s", "asm", "m",
];

/// Extensions treated as structured data.
const STRUCTURED_EXT: &[&str] = &[
    "csv", "json", "log", "jsonl", "ndjson", "xml", "yaml", "yml", "toml",
];

/// The dictionary kind of a text file, from its extension (lower-cased).
pub fn text_kind(path: &str) -> DictionaryKind {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = match name.rsplit_once('.') {
        Some((stem, e)) if !stem.is_empty() => e.to_ascii_lowercase(),
        _ => String::new(),
    };
    if SOURCE_EXT.contains(&ext.as_str()) {
        DictionaryKind::Source
    } else if STRUCTURED_EXT.contains(&ext.as_str()) {
        DictionaryKind::Structured
    } else {
        DictionaryKind::Prose
    }
}

/// A group of file inputs written together.
#[derive(Debug, Clone)]
pub struct Cluster {
    /// The class of every file in the cluster.
    pub class: Class,
    /// The dictionary kind (`None` outside `Text`).
    pub dictionary: DictionaryKind,
    /// The files, in path order.
    pub inputs: Vec<Input>,
}

fn order(class: Class, dict: DictionaryKind) -> u8 {
    match (class, dict) {
        (Class::Text, DictionaryKind::Prose) => 0,
        (Class::Text, DictionaryKind::Structured) => 1,
        (Class::Text, _) => 2,
        (Class::Other, _) => 3,
        (Class::ImageRaw, _) => 4,
        (Class::Audio, _) => 5,
        (Class::Executable, _) => 6,
        (Class::DeflateContainer, _) => 7,
        (Class::Png, _) => 8,
        (Class::Jpeg, _) => 9,
        (Class::Compressed, _) => 10,
        (Class::Video, _) => 11,
        (Class::HighEntropy, _) => 12,
    }
}

/// Class of a file input from its first 64 KiB.
fn class_of(source: &Source, input: &Input) -> Result<Class, CoreError> {
    let mut r = source.open(input)?.take(SAMPLE_LEN as u64);
    let mut buf = Vec::new();
    r.read_to_end(&mut buf)
        .map_err(|e| CoreError::io(&input.source, e))?;
    Ok(classify(&buf))
}

/// Group the file inputs by `(class, dictionary kind)`, clusters in the fixed order of the Fast
/// tier, files in path order. Directories and symlinks are not part of any cluster.
pub fn cluster(inputs: &[Input]) -> Result<Vec<Cluster>, CoreError> {
    let source = Source::new();
    let mut files: Vec<&Input> = inputs
        .iter()
        .filter(|i| i.kind == EntryKind::File)
        .collect();
    files.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    let mut out: Vec<Cluster> = Vec::new();
    for input in files {
        let class = class_of(&source, input)?;
        let dictionary = if class == Class::Text {
            text_kind(&input.path)
        } else {
            DictionaryKind::None
        };
        match out
            .iter_mut()
            .find(|c| c.class == class && c.dictionary == dictionary)
        {
            Some(c) => c.inputs.push(input.clone()),
            None => out.push(Cluster {
                class,
                dictionary,
                inputs: vec![input.clone()],
            }),
        }
    }
    out.sort_by_key(|c| order(c.class, c.dictionary));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{walk, IngestOptions};

    #[test]
    fn extension_kinds() {
        assert_eq!(text_kind("a/b.RS"), DictionaryKind::Source);
        assert_eq!(text_kind("x.csv"), DictionaryKind::Structured);
        assert_eq!(text_kind("x.yml"), DictionaryKind::Structured);
        assert_eq!(text_kind("notes.txt"), DictionaryKind::Prose);
        assert_eq!(text_kind("README"), DictionaryKind::Prose);
        assert_eq!(text_kind(".hidden"), DictionaryKind::Prose);
    }

    #[test]
    fn a_file_that_vanished_after_the_walk_is_an_io_error_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gone.txt"), b"abc").unwrap();
        let inputs = walk(dir.path(), &IngestOptions::default()).unwrap();
        std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
        match cluster(&inputs) {
            Err(CoreError::Io { path, .. }) => assert!(path.ends_with("gone.txt")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn clusters_in_the_fixed_order_with_files_in_path_order() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::create_dir(p.join("d")).unwrap();
        let prose = "the quick brown fox jumps over the lazy dog\n".repeat(50);
        std::fs::write(p.join("z.txt"), &prose).unwrap();
        std::fs::write(p.join("a.txt"), &prose).unwrap();
        std::fs::write(p.join("m.c"), "int main(void) { return 0; }\n".repeat(30)).unwrap();
        std::fs::write(p.join("t.csv"), "1,2,3\n4,5,6\n".repeat(80)).unwrap();
        std::fs::write(
            p.join("img.jpg"),
            [0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F'],
        )
        .unwrap();
        std::fs::write(p.join("v.mp4"), b"\0\0\0\x18ftypmp42\0\0\0\0mp42isom").unwrap();
        let inputs = walk(p, &IngestOptions::default()).unwrap();
        let cs = cluster(&inputs).unwrap();
        let shape: Vec<_> = cs
            .iter()
            .map(|c| {
                (
                    c.class,
                    c.dictionary,
                    c.inputs.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (Class::Text, DictionaryKind::Prose, vec!["a.txt", "z.txt"]),
                (Class::Text, DictionaryKind::Structured, vec!["t.csv"]),
                (Class::Text, DictionaryKind::Source, vec!["m.c"]),
                (Class::Jpeg, DictionaryKind::None, vec!["img.jpg"]),
                (Class::Video, DictionaryKind::None, vec!["v.mp4"]),
            ]
        );
    }
}
