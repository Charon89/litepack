//! `small-files`: thousands of small files cut at line boundaries from log and CSV inputs.
//!
//! The inputs (class `logs-text`) are split by extension: `.csv` files are CSV, `.log`, `.txt`
//! and `.out` files are logs. For each output file a fixed-seed generator picks the kind (JSON
//! lines made from CSV rows, a raw CSV segment, or a log segment, by the registry percentages),
//! the input file, a start offset and a target size (skewed towards small files, capped by
//! `max_file_bytes`). The segment starts at the first line start at or after the offset and takes
//! whole lines while they fit the target; JSON lines are the CSV rows rendered as objects keyed
//! by the file's header line. Names and directories are fixed: `dNN/fNNNNN.<ext>`, 500 files per
//! directory.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{bail, Context, Result};

use super::{input_files, InputFile, Output, Skip, SplitMix64};
use crate::corpus::build::Ctx;
use crate::corpus::registry::{SmallFilesSpec, Source};

const SEED: u64 = 0x4C50_4B5F_534D_414C; // "LPK_SMAL"
const FILES_PER_DIR: usize = 500;
const MIN_TARGET: u64 = 64;
/// Slack read beyond the target to find the first line start.
const SLACK: u64 = 8192;
const ATTEMPTS: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Json,
    Csv,
    Log,
}

struct Cand {
    path: std::path::PathBuf,
    len: u64,
}

fn classify(files: &[InputFile]) -> Result<(Vec<Cand>, Vec<Cand>)> {
    let (mut csv, mut logs) = (Vec::new(), Vec::new());
    for f in files {
        let lower = f.rel.to_ascii_lowercase();
        let len = std::fs::metadata(&f.path)?.len();
        if len == 0 {
            continue;
        }
        let cand = Cand {
            path: f.path.clone(),
            len,
        };
        if lower.ends_with(".csv") {
            csv.push(cand);
        } else if lower.ends_with(".log") || lower.ends_with(".txt") || lower.ends_with(".out") {
            logs.push(cand);
        }
    }
    Ok((csv, logs))
}

fn read_window(path: &Path, offset: u64, want: u64) -> Result<Vec<u8>> {
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    f.take(want).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Split `window` (which starts at `offset` of a file of `file_len` bytes) into complete lines
/// (each including its line feed; a last line without one counts only at the end of the file and
/// gets one). When `offset > 0` the partial first line is dropped. `skip_first` additionally drops
/// the first complete line (the CSV header at offset 0).
fn lines_of(window: &[u8], offset: u64, file_len: u64, skip_first: bool) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    let mut pos = 0usize;
    if offset > 0 {
        match window.iter().position(|b| *b == b'\n') {
            Some(i) => pos = i + 1,
            None => return lines,
        }
    }
    let at_eof = offset + window.len() as u64 >= file_len;
    while pos < window.len() {
        match window[pos..].iter().position(|b| *b == b'\n') {
            Some(i) => {
                lines.push(window[pos..=pos + i].to_vec());
                pos += i + 1;
            }
            None => {
                if at_eof {
                    let mut l = window[pos..].to_vec();
                    l.push(b'\n');
                    lines.push(l);
                }
                break;
            }
        }
    }
    if skip_first && offset == 0 && !lines.is_empty() {
        lines.remove(0);
    }
    lines
}

/// Split one CSV record into fields (RFC 4180 quoting on a single line).
pub fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.trim_end_matches(['\r', '\n']).chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') => {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            }
            (true, c) => cur.push(c),
            (false, '"') if cur.is_empty() => quoted = true,
            (false, ',') => fields.push(std::mem::take(&mut cur)),
            (false, c) => cur.push(c),
        }
    }
    fields.push(cur);
    fields
}

/// True for strings that are valid JSON numbers without an exponent.
fn is_json_number(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let int_ok = !int.is_empty()
        && int.bytes().all(|b| b.is_ascii_digit())
        && (int == "0" || !int.starts_with('0'));
    let frac_ok = frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()));
    int_ok && frac_ok
}

/// One CSV row as a JSON object line (keys in header order; numbers stay numbers, empty
/// fields are `null`). `None` when the field count differs from the header's.
pub fn row_to_json(header: &[String], row: &[String]) -> Option<String> {
    if header.len() != row.len() {
        return None;
    }
    let mut s = String::from("{");
    for (i, (k, v)) in header.iter().zip(row).enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&serde_json::to_string(k).ok()?);
        s.push(':');
        if v.is_empty() {
            s.push_str("null");
        } else if is_json_number(v) {
            s.push_str(v);
        } else {
            s.push_str(&serde_json::to_string(v).ok()?);
        }
    }
    s.push_str("}\n");
    Some(s)
}

/// The header line of a CSV file as fields.
fn header_of(path: &Path) -> Result<Option<Vec<String>>> {
    let head = read_window(path, 0, SLACK)?;
    let Some(i) = head.iter().position(|b| *b == b'\n') else {
        return Ok(None);
    };
    Ok(Some(parse_csv_line(&String::from_utf8_lossy(&head[..i]))))
}

/// Take lines while they fit `target`; `None` when not even one does.
fn take_fitting(lines: impl Iterator<Item = Vec<u8>>, target: u64) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for l in lines {
        if (out.len() + l.len()) as u64 > target {
            break;
        }
        out.extend_from_slice(&l);
    }
    (!out.is_empty()).then_some(out)
}

pub fn build(
    ctx: &Ctx<'_>,
    source: &Source,
    spec: &SmallFilesSpec,
    out: &mut Output<'_>,
) -> Result<()> {
    let inputs = input_files(ctx, source, &spec.from)?;
    let (csvs, logs) = classify(&inputs)?;
    if csvs.is_empty() && logs.is_empty() {
        return Err(Skip(format!(
            "no .csv or .log/.txt/.out files among the inputs ({})",
            source.inputs.join(", ")
        ))
        .into());
    }
    let mut rng = SplitMix64::new(SEED);
    for i in 0..spec.count {
        let r = rng.below(100);
        let mut kind = if r < u64::from(spec.json_percent) {
            Kind::Json
        } else if r < u64::from(spec.json_percent) + u64::from(spec.csv_percent) {
            Kind::Csv
        } else {
            Kind::Log
        };
        if kind != Kind::Log && csvs.is_empty() {
            kind = Kind::Log;
        } else if kind == Kind::Log && logs.is_empty() {
            kind = Kind::Json;
        }
        let u = rng.below(1000);
        let target = MIN_TARGET + u * u * (spec.max_file_bytes - MIN_TARGET) / 1_000_000;
        let pool = if kind == Kind::Log { &logs } else { &csvs };

        let mut data = None;
        for attempt in 0..ATTEMPTS {
            let cand = &pool[rng.below(pool.len() as u64) as usize];
            let offset = if attempt < ATTEMPTS / 2 {
                rng.below(cand.len)
            } else {
                0
            };
            // The first attempts honour the target; later ones allow up to the maximum so that
            // inputs with long lines still yield a file.
            let t = if attempt < ATTEMPTS / 4 {
                target
            } else {
                spec.max_file_bytes
            };
            let want = (t + SLACK).min(cand.len - offset);
            let window = read_window(&cand.path, offset, want)?;
            let lines = lines_of(&window, offset, cand.len, kind == Kind::Json);
            let found = if kind == Kind::Json {
                let Some(header) = header_of(&cand.path)? else {
                    continue;
                };
                take_fitting(
                    lines.iter().filter_map(|l| {
                        let row = parse_csv_line(&String::from_utf8_lossy(l));
                        row_to_json(&header, &row).map(String::into_bytes)
                    }),
                    t,
                )
            } else {
                take_fitting(lines.into_iter(), t)
            };
            if found.is_some() {
                data = found;
                break;
            }
        }
        let Some(data) = data else {
            bail!(
                "source `{}`: no line segment of at most {target} bytes found for file {i}; \
                 the inputs have lines that are too long",
                source.id
            );
        };
        let ext = match kind {
            Kind::Json => "jsonl",
            Kind::Csv => "csv",
            Kind::Log => "log",
        };
        out.write(&format!("d{:02}/f{i:05}.{ext}", i / FILES_PER_DIR), &data)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::testutil::{put, read_all, run, source};
    use super::*;
    use crate::corpus::registry::SourceSpec;

    fn src(count: usize, max: u64, json: u8, csv: u8) -> Source {
        source(
            "small",
            "small-files",
            &["logs-text"],
            SourceSpec::SmallFiles(SmallFilesSpec {
                count,
                max_file_bytes: max,
                json_percent: json,
                csv_percent: csv,
                from: vec![],
            }),
        )
    }

    fn fixture(root: &Path) -> (BTreeSet<Vec<u8>>, BTreeSet<Vec<u8>>) {
        let mut log = Vec::new();
        for n in 0..2000 {
            log.extend_from_slice(
                format!(
                    "2026-10-01 12:00:{:02} INFO worker-{n} handled request {}\n",
                    n % 60,
                    n * 7
                )
                .as_bytes(),
            );
        }
        // No trailing line feed on purpose.
        log.extend_from_slice(b"last line without newline");
        put(root, "logs-text", "logs", "a/app.log", &log);
        let mut csv = String::from("id,name,amount,note\n");
        for n in 0..1500 {
            csv.push_str(&format!(
                "{n},\"Doe, John {n}\",{}.{:02},\"said \"\"hi\"\"\"\n",
                n * 3,
                n % 100
            ));
        }
        put(root, "logs-text", "taxi", "t.csv", csv.as_bytes());
        let lines = |d: &[u8]| -> BTreeSet<Vec<u8>> {
            let mut v: Vec<Vec<u8>> = d.split(|b| *b == b'\n').map(<[u8]>::to_vec).collect();
            v.retain(|l| !l.is_empty());
            v.into_iter().collect()
        };
        (lines(&log), lines(csv.as_bytes()))
    }

    #[test]
    fn files_are_small_line_aligned_counted_and_repeatable() {
        let a = tempfile::tempdir().expect("tmp");
        let b = tempfile::tempdir().expect("tmp");
        let (log_lines, csv_lines) = fixture(a.path());
        fixture(b.path());
        let s = src(1200, 2000, 30, 20);
        let built = [("logs-text", "logs"), ("logs-text", "taxi")];
        let m1 = run(a.path(), &s, &built).expect("run");
        let m2 = run(b.path(), &s, &built).expect("run");
        assert_eq!(m1, m2);
        let files = read_all(a.path(), "small-files", "small");
        assert_eq!(files.len(), 1200);
        assert_eq!(files, read_all(b.path(), "small-files", "small"));
        let (mut json, mut csv, mut log) = (0, 0, 0);
        for (name, data) in &files {
            assert!(
                !data.is_empty() && data.len() <= 2000,
                "{name}: {}",
                data.len()
            );
            assert_eq!(
                data.last(),
                Some(&b'\n'),
                "{name} must end at a line boundary"
            );
            let lines: Vec<&[u8]> = data[..data.len() - 1].split(|b| *b == b'\n').collect();
            if name.ends_with(".jsonl") {
                json += 1;
                for l in lines {
                    let v: serde_json::Value = serde_json::from_slice(l).expect("json line");
                    let obj = v.as_object().expect("object");
                    assert_eq!(obj.len(), 4);
                    assert!(obj["amount"].is_number() && obj["name"].is_string());
                }
            } else if name.ends_with(".csv") {
                csv += 1;
                for l in lines {
                    assert!(csv_lines.contains(l), "{name}: foreign line");
                }
            } else {
                log += 1;
                for l in lines {
                    assert!(log_lines.contains(l), "{name}: foreign line");
                }
            }
            // Fixed layout.
            assert!(name.starts_with('d') && name.as_bytes()[3] == b'/');
        }
        assert!(json > 0 && csv > 0 && log > 0, "{json} {csv} {log}");
        assert_eq!(
            files
                .iter()
                .map(|f| f.0.split('/').next().expect("dir"))
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn missing_inputs_are_a_skip() {
        let dir = tempfile::tempdir().expect("tmp");
        let err = run(dir.path(), &src(10, 1000, 30, 20), &[]).expect_err("no inputs");
        assert!(err.downcast_ref::<Skip>().is_some(), "{err:#}");
    }

    #[test]
    fn csv_parsing_and_json_rendering() {
        assert_eq!(
            parse_csv_line("1,\"a, b\",\"x\"\"y\",\r\n"),
            ["1", "a, b", "x\"y", ""]
        );
        let h: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let row: Vec<String> = ["007", "-1.5", "", "t\"x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            row_to_json(&h, &row).as_deref(),
            Some("{\"a\":\"007\",\"b\":-1.5,\"c\":null,\"d\":\"t\\\"x\"}\n")
        );
        assert!(row_to_json(&h, &row[..3]).is_none());
    }
}
