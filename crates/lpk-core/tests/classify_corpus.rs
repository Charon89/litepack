//! Classifier against the corpus, run by hand:
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test classify_corpus -- --ignored --nocapture`
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use lpk_core::{classify, walk, Class, IngestOptions, Source};
use lpk_format::EntryKind;

/// The classes an extension may take; `None` means not asserted.
fn expected(class_dir: &str, rel: &str) -> Option<Vec<Class>> {
    if rel.contains(".git/objects/") {
        return Some(vec![
            Class::HighEntropy,
            Class::Other,
            Class::DeflateContainer,
        ]);
    }
    if class_dir == "encrypted-random" {
        return Some(vec![Class::HighEntropy]);
    }
    let ext = rel.rsplit_once('.')?.1.to_ascii_lowercase();
    let c = match ext.as_str() {
        "jpg" | "jpeg" => Class::Jpeg,
        "png" => Class::Png,
        "zip" | "jar" | "apk" | "epub" | "docx" | "xlsx" | "pptx" | "gz" | "pdf" => {
            Class::DeflateContainer
        }
        "exe" | "dll" | "pyd" | "so" => Class::Executable,
        "wav" | "flac" | "mp3" | "ogg" => Class::Audio,
        "mp4" | "mkv" | "webm" => Class::Video,
        "bmp" | "tif" | "tiff" | "ppm" | "pgm" => Class::ImageRaw,
        "7z" | "xz" | "zst" | "bz2" | "rar" => Class::Compressed,
        "txt" | "md" | "c" | "h" | "cpp" | "py" | "html" | "htm" | "xml" | "csv" | "log"
        | "json" | "rs" | "sh" | "yml" | "yaml" | "toml" | "obj" | "mtl" | "js" | "css" => {
            Class::Text
        }
        _ => return None,
    };
    Some(vec![c])
}

/// A file that legitimately disagrees with the table: it is accepted only while the stated
/// reason can be checked on the bytes read.
///
/// `small-files/**/*.log`: a few synthetic log files embed raw exploit bytes (0xF7, 0xFF, ...)
/// after a `gethostbyname error for` line, so the head is not valid UTF-8; the classifier is
/// right to say `Other` for them and the extension is wrong to promise text.
fn excepted(class_dir: &str, rel: &str, got: Class, head: &[u8]) -> bool {
    class_dir == "small-files"
        && rel.ends_with(".log")
        && got == Class::Other
        && std::str::from_utf8(head).is_err_and(|e| e.error_len().is_some())
}

#[derive(Default)]
struct Row {
    files: u64,
    asserted: u64,
    agreed: u64,
    disagreed: u64,
    excepted: u64,
}

#[test]
#[ignore]
fn classifier_agrees_with_the_extension_table() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty());
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut shown: Vec<String> = Vec::new();
    let mut total_bad = 0u64;
    for dir in dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let inputs = walk(&dir, &IngestOptions::default()).unwrap();
        let row = rows.entry(name.clone()).or_default();
        for i in inputs.iter().filter(|i| i.kind == EntryKind::File) {
            row.files += 1;
            let mut buf = Vec::new();
            Source::new()
                .open(i)
                .unwrap()
                .take(1 << 20)
                .read_to_end(&mut buf)
                .unwrap();
            let got = classify(&buf);
            let Some(want) = expected(&name, &i.path) else {
                continue;
            };
            row.asserted += 1;
            if want.contains(&got) {
                row.agreed += 1;
            } else if excepted(&name, &i.path, got, &buf) {
                row.agreed += 1;
                row.excepted += 1;
            } else {
                row.disagreed += 1;
                total_bad += 1;
                if shown.len() < 20 {
                    let w: Vec<&str> = want.iter().map(|c| c.as_str()).collect();
                    shown.push(format!(
                        "{name}/{}: got {}, expected {}",
                        i.path,
                        got.as_str(),
                        w.join("|")
                    ));
                }
            }
        }
    }
    println!(
        "{:<22} {:>7} {:>9} {:>7} {:>10}",
        "class", "files", "asserted", "agreed", "disagreed (+excepted, counted as agreed)"
    );
    for (n, r) in &rows {
        println!(
            "{n:<22} {:>7} {:>9} {:>7} {:>10} (+{})",
            r.files, r.asserted, r.agreed, r.disagreed, r.excepted
        );
    }
    for s in &shown {
        println!("DISAGREE {s}");
    }
    assert_eq!(total_bad, 0, "disagreements with the expectation table");
}
