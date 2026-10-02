//! Test helper for the `lpk-bench run` tests: a tiny archiver with switchable misbehaviour.
//! Not part of the product.
//!
//! ```text
//! lpk-fake-archiver create  [--mode=M] <archive> <input-dir | @list-file>
//! lpk-fake-archiver extract [--mode=M] [--nested] <archive> <outdir>
//! lpk-fake-archiver stream  compress|decompress [--mode=M]     (stdin to stdout)
//! ```
//!
//! Modes: `ok` (default); create: `noarchive` (exit 0, nothing written), `empty` (empty archive),
//! `exit1`, `sleep` (until killed), `envcheck` (exit 17 when a tool-configuration variable is set);
//! extract: `miss` (leaves out the first file), `alter` (changes a byte of the first non-empty
//! file), `extra` (adds a file), `exit3`; stream: `exit1`.
//! An input of `@file` archives the relative paths listed in the file (flat, like a tool given a
//! list). `--nested` extracts into `<outdir>/<name of the archived directory>`.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::exit;

const MAGIC: &[u8] = b"FAKE1\n";
const CONFIG_VARS: &[&str] = &[
    "XZ_OPT",
    "XZ_DEFAULTS",
    "ZSTD_CLEVEL",
    "ZSTD_NBTHREADS",
    "GZIP",
    "RAR",
    "TAR_OPTIONS",
];

fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let mut names: Vec<_> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    names.sort_by_key(|e| e.file_name());
    for e in names {
        let p = e.path();
        if p.is_dir() {
            walk(base, &p, out);
        } else if let Ok(rel) = p.strip_prefix(base) {
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.push(parts.join("/"));
        }
    }
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn take<'a>(buf: &'a [u8], at: &mut usize) -> &'a [u8] {
    let n = u64::from_le_bytes(buf[*at..*at + 8].try_into().unwrap_or([0; 8])) as usize;
    *at += 8;
    let s = &buf[*at..*at + n];
    *at += n;
    s
}

fn create(mode: &str, archive: &str, input: &str) {
    match mode {
        "noarchive" => return,
        "empty" => {
            let _ = fs::write(archive, b"");
            return;
        }
        "exit1" => exit(1),
        "sleep" => loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        },
        "envcheck" if CONFIG_VARS.iter().any(|v| std::env::var_os(v).is_some()) => exit(17),
        _ => {}
    }
    let mut buf = MAGIC.to_vec();
    let mut files = Vec::new();
    let (top, base) = if let Some(list) = input.strip_prefix('@') {
        let text = fs::read_to_string(list).unwrap_or_default();
        files.extend(text.lines().filter(|l| !l.is_empty()).map(String::from));
        (String::new(), PathBuf::from("."))
    } else {
        let base = PathBuf::from(input);
        walk(&base, &base, &mut files);
        (
            base.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            base,
        )
    };
    put(&mut buf, top.as_bytes());
    for rel in files {
        let data = fs::read(base.join(&rel)).unwrap_or_else(|e| {
            eprintln!("cannot read {rel}: {e}");
            exit(2)
        });
        put(&mut buf, rel.as_bytes());
        put(&mut buf, &data);
    }
    if fs::write(archive, buf).is_err() {
        exit(2);
    }
}

fn extract(mode: &str, nested: bool, archive: &str, outdir: &str) {
    if mode == "exit3" {
        exit(3);
    }
    let buf = fs::read(archive).unwrap_or_else(|_| exit(2));
    if !buf.starts_with(MAGIC) {
        eprintln!("not an archive");
        exit(2);
    }
    let mut at = MAGIC.len();
    let top = String::from_utf8_lossy(take(&buf, &mut at)).into_owned();
    let mut root = PathBuf::from(outdir);
    if nested && !top.is_empty() {
        root.push(&top);
    }
    let mut index = 0usize;
    let mut altered = false;
    while at < buf.len() {
        let name = String::from_utf8_lossy(take(&buf, &mut at)).into_owned();
        let mut data = take(&buf, &mut at).to_vec();
        index += 1;
        if mode == "miss" && index == 1 {
            continue;
        }
        if mode == "alter" && !altered && !data.is_empty() {
            data[0] ^= 0xFF;
            altered = true;
        }
        let path = root.join(&name);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::write(&path, &data).is_err() {
            exit(2);
        }
    }
    if mode == "extra" {
        let _ = fs::create_dir_all(&root);
        let _ = fs::write(root.join("__extra.txt"), b"not in the manifest");
    }
}

fn stream(direction: &str, mode: &str) {
    if mode == "exit1" {
        exit(1);
    }
    let mut data = Vec::new();
    if std::io::stdin().read_to_end(&mut data).is_err() {
        exit(2);
    }
    for b in &mut data {
        *b ^= 0x5A;
    }
    let _ = direction;
    let mut out = std::io::stdout();
    if out.write_all(&data).is_err() || out.flush().is_err() {
        exit(2);
    }
}

fn main() {
    let mut mode = "ok".to_string();
    let mut nested = false;
    let mut positional = Vec::new();
    for a in std::env::args().skip(1) {
        if let Some(m) = a.strip_prefix("--mode=") {
            mode = m.to_string();
        } else if a.starts_with("--threads=") {
            // accepted and ignored
        } else if a == "--nested" {
            nested = true;
        } else {
            positional.push(a);
        }
    }
    match positional
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["create", archive, input] => create(&mode, archive, input),
        ["extract", archive, outdir] => extract(&mode, nested, archive, outdir),
        ["stream", direction] => stream(direction, &mode),
        ["version"] => println!("lpk-fake-archiver 1.0"),
        other => {
            eprintln!("usage error: {other:?}");
            exit(64);
        }
    }
}
