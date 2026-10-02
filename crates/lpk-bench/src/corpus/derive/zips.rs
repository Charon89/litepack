//! `built-zips`: ZIP archives written in-process from fixed subsets of other classes, each subset
//! once per Deflate encoder.
//!
//! Why: D-15 item 6 replaces "ZIPs made by Info-ZIP / 7-Zip / Explorer" with in-process ZIPs so
//! that recompression tests see Deflate streams from several encoder families:
//! * `zlib-<1..9>`: genuine zlib (C zlib through `flate2`'s `zlib` feature), raw Deflate at that
//!   level;
//! * `miniz-<1..10>`: `miniz_oxide`, a different encoder with its own match finder.
//!
//! The ZIP writer is in-tree and canonical: entries sorted by name bytes, UTF-8 names (flag bit
//! 11), DOS time 1980-01-01 00:00:00, no extra fields, no comments, empty files stored. A bundle
//! picks every `every`-th file of its class in sorted path order, skipping files that would
//! exceed `max_bytes`, until `max_files`. Entry names are `<source>/<path in source>`.
//! Output: `<bundle>-<encoder>.zip`.

use std::io::Write;

use anyhow::{bail, Context, Result};

use super::{input_files, Output, Skip};
use crate::corpus::build::Ctx;
use crate::corpus::registry::{BuiltZipsSpec, Source};

/// A Deflate encoder choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    Zlib(u32),
    Miniz(u8),
}

impl Encoder {
    /// Parse `zlib-N` (1..=9) or `miniz-N` (1..=10).
    pub fn parse(s: &str) -> Result<Encoder> {
        let (family, level) = s
            .split_once('-')
            .with_context(|| format!("encoder `{s}`: expected `zlib-N` or `miniz-N`"))?;
        let n: u32 = level
            .parse()
            .with_context(|| format!("encoder `{s}`: bad level"))?;
        match family {
            "zlib" if (1..=9).contains(&n) => Ok(Encoder::Zlib(n)),
            "miniz" if (1..=10).contains(&n) => Ok(Encoder::Miniz(n as u8)),
            _ => bail!("encoder `{s}`: expected `zlib-1..9` or `miniz-1..10`"),
        }
    }

    pub fn name(self) -> String {
        match self {
            Encoder::Zlib(n) => format!("zlib-{n}"),
            Encoder::Miniz(n) => format!("miniz-{n}"),
        }
    }

    /// Raw Deflate stream of `data`.
    pub fn deflate(self, data: &[u8]) -> Result<Vec<u8>> {
        match self {
            Encoder::Zlib(level) => {
                let mut e =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
                e.write_all(data)?;
                Ok(e.finish()?)
            }
            Encoder::Miniz(level) => Ok(miniz_oxide::deflate::compress_to_vec(data, level)),
        }
    }
}

const DOS_TIME: u16 = 0;
/// 1980-01-01.
const DOS_DATE: u16 = (1 << 5) | 1;
const UTF8_FLAG: u16 = 0x0800;

/// One ZIP entry to write.
pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
}

/// Write a canonical ZIP of `entries` (sorted by name here) with `enc`.
pub fn write_zip(entries: &[Entry], enc: Encoder) -> Result<Vec<u8>> {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    if sorted.len() >= 0xFFFF {
        bail!(
            "too many ZIP entries ({}): 65,535 and more would need ZIP64",
            sorted.len()
        );
    }
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for e in sorted {
        let (method, body) = if e.data.is_empty() {
            (0u16, Vec::new())
        } else {
            (8u16, enc.deflate(&e.data)?)
        };
        let mut crc = flate2::Crc::new();
        crc.update(&e.data);
        let (crc, usize_, csize) = (
            crc.sum(),
            u32::try_from(e.data.len()).context("ZIP entry over 4 GB")?,
            u32::try_from(body.len()).context("ZIP entry over 4 GB")?,
        );
        let name = e.name.as_bytes();
        let name_len = u16::try_from(name.len()).context("ZIP entry name too long")?;
        let offset = u32::try_from(out.len()).context("ZIP over 4 GB")?;
        out.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&UTF8_FLAG.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&DOS_TIME.to_le_bytes());
        out.extend_from_slice(&DOS_DATE.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&csize.to_le_bytes());
        out.extend_from_slice(&usize_.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&body);

        central.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // made by: MS-DOS, 2.0
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&UTF8_FLAG.to_le_bytes());
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&DOS_TIME.to_le_bytes());
        central.extend_from_slice(&DOS_DATE.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&csize.to_le_bytes());
        central.extend_from_slice(&usize_.to_le_bytes());
        central.extend_from_slice(&name_len.to_le_bytes());
        central.extend_from_slice(&[0; 2 + 2 + 2 + 2 + 4]); // extra, comment, disk, int. attr, ext. attr
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let cd_offset = u32::try_from(out.len()).context("ZIP over 4 GB")?;
    let cd_size = u32::try_from(central.len()).context("ZIP over 4 GB")?;
    let count = entries.len() as u16;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    Ok(out)
}

pub fn build(
    ctx: &Ctx<'_>,
    source: &Source,
    spec: &BuiltZipsSpec,
    out: &mut Output<'_>,
) -> Result<()> {
    let encoders = spec
        .encoders
        .iter()
        .map(|e| Encoder::parse(e))
        .collect::<Result<Vec<_>>>()?;
    let all = input_files(ctx, source, &[])?;
    for bundle in &spec.bundles {
        let mut entries = Vec::new();
        let mut total = 0u64;
        for f in all
            .iter()
            .filter(|f| f.class == bundle.class)
            .step_by(bundle.every)
        {
            if entries.len() >= bundle.max_files {
                break;
            }
            let len = std::fs::metadata(&f.path)?.len();
            if total + len > bundle.max_bytes {
                continue;
            }
            total += len;
            entries.push(Entry {
                name: format!("{}/{}", f.source, f.rel),
                data: std::fs::read(&f.path).with_context(|| format!("reading {}", f.full()))?,
            });
        }
        if entries.is_empty() {
            return Err(Skip(format!(
                "bundle `{}`: no files of class `{}` fit its limits",
                bundle.name, bundle.class
            ))
            .into());
        }
        for enc in &encoders {
            let zip = write_zip(&entries, *enc)?;
            out.write(&format!("{}-{}.zip", bundle.name, enc.name()), &zip)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use super::super::testutil::{put, read_all, run, source};
    use super::*;
    use crate::corpus::registry::{SourceSpec, ZipBundle};

    fn text(n: usize, salt: usize) -> Vec<u8> {
        (0..n)
            .flat_map(|i| {
                format!(
                    "line {} of file {salt}: the quick brown fox {}\n",
                    i,
                    i * 31 % 17
                )
                .into_bytes()
            })
            .collect()
    }

    fn src(encoders: &[&str]) -> Source {
        source(
            "zips",
            "archives-nested",
            &["office-pdf", "logs-text"],
            SourceSpec::BuiltZips(BuiltZipsSpec {
                bundles: vec![
                    ZipBundle {
                        name: "docs".into(),
                        class: "office-pdf".into(),
                        every: 2,
                        max_files: 4,
                        max_bytes: 1 << 20,
                    },
                    ZipBundle {
                        name: "logs".into(),
                        class: "logs-text".into(),
                        every: 1,
                        max_files: 100,
                        max_bytes: 1 << 20,
                    },
                ],
                encoders: encoders.iter().map(|s| s.to_string()).collect(),
            }),
        )
    }

    fn fixture(root: &std::path::Path) {
        for i in 0..10 {
            put(
                root,
                "office-pdf",
                "govdocs",
                &format!("d{i:02}.txt"),
                &text(40 + i, i),
            );
        }
        put(root, "office-pdf", "govdocs", "empty.bin", b"");
        put(root, "logs-text", "apache", "a.log", &text(300, 99));
        put(root, "logs-text", "apache", "sub/b.log", &text(200, 98));
    }

    const BUILT: [(&str, &str); 2] = [("office-pdf", "govdocs"), ("logs-text", "apache")];

    #[test]
    fn encoder_names_parse() {
        assert_eq!(Encoder::parse("zlib-9").expect("p"), Encoder::Zlib(9));
        assert_eq!(Encoder::parse("miniz-10").expect("p"), Encoder::Miniz(10));
        for bad in ["zlib-0", "zlib-10", "miniz-11", "gzip-1", "zlib", "zlib-x"] {
            assert!(Encoder::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_zip_lists_the_same_entries_round_trips_and_repeats() {
        let encoders = ["zlib-1", "zlib-9", "miniz-6"];
        let a = tempfile::tempdir().expect("tmp");
        let b = tempfile::tempdir().expect("tmp");
        fixture(a.path());
        fixture(b.path());
        run(a.path(), &src(&encoders), &BUILT).expect("run a");
        run(b.path(), &src(&encoders), &BUILT).expect("run b");
        let files = read_all(a.path(), "archives-nested", "zips");
        assert_eq!(files, read_all(b.path(), "archives-nested", "zips"));
        assert_eq!(files.len(), 6, "two bundles x three encoders");

        let mut by_bundle: std::collections::BTreeMap<&str, Vec<Vec<String>>> = Default::default();
        for (name, data) in &files {
            // Fixed timestamp in every local header (time 0, date 1980-01-01).
            assert_eq!(&data[10..14], &[0, 0, 0x21, 0]);
            let mut z = zip::ZipArchive::new(Cursor::new(data.clone())).expect("zip");
            let mut names = Vec::new();
            for i in 0..z.len() {
                let mut f = z.by_index(i).expect("entry");
                let mut got = Vec::new();
                f.read_to_end(&mut got).expect("read");
                let want = std::fs::read(
                    a.path()
                        .join("out")
                        .join(if f.name().starts_with("govdocs/") {
                            "office-pdf"
                        } else {
                            "logs-text"
                        })
                        .join(if f.name().starts_with("govdocs/") {
                            "govdocs"
                        } else {
                            "apache"
                        })
                        .join(f.name().split_once('/').expect("slash").1),
                )
                .expect("source file");
                assert_eq!(got, want, "{name}: {}", f.name());
                names.push(f.name().to_string());
            }
            let mut sorted = names.clone();
            sorted.sort();
            assert_eq!(names, sorted, "entries are sorted");
            by_bundle
                .entry(name.split('-').next().expect("stem"))
                .or_default()
                .push(names);
        }
        for (bundle, lists) in &by_bundle {
            assert_eq!(lists.len(), 3);
            assert!(
                lists.windows(2).all(|w| w[0] == w[1]),
                "{bundle}: same entries"
            );
        }
        // every=2 over 11 docs (10 text + empty), max 4: d00 d02 d04 d06 (sorted: d00..d09, empty.bin).
        assert_eq!(
            by_bundle["docs"][0],
            [
                "govdocs/d00.txt",
                "govdocs/d02.txt",
                "govdocs/d04.txt",
                "govdocs/d06.txt"
            ]
        );
        assert_eq!(by_bundle["logs"][0], ["apache/a.log", "apache/sub/b.log"]);
        // The encoders really differ.
        let bytes = |n: &str| files.iter().find(|f| f.0 == n).expect("file").1.clone();
        assert_ne!(bytes("docs-zlib-1.zip"), bytes("docs-zlib-9.zip"));
        assert_ne!(bytes("docs-zlib-9.zip"), bytes("docs-miniz-6.zip"));
    }

    #[test]
    fn write_zip_stores_empty_files_and_sorts() {
        let z = write_zip(
            &[
                Entry {
                    name: "b".into(),
                    data: b"hello hello hello hello".to_vec(),
                },
                Entry {
                    name: "a".into(),
                    data: Vec::new(),
                },
            ],
            Encoder::Zlib(6),
        )
        .expect("zip");
        let mut ar = zip::ZipArchive::new(Cursor::new(z)).expect("open");
        assert_eq!(ar.len(), 2);
        let f = ar.by_index(0).expect("a");
        assert_eq!(
            (f.name(), f.size(), f.compression()),
            ("a", 0, zip::CompressionMethod::Stored)
        );
        drop(f);
        let f = ar.by_index(1).expect("b");
        assert_eq!(
            (f.name(), f.compression()),
            ("b", zip::CompressionMethod::Deflated)
        );
    }

    #[test]
    fn zip64_sized_entry_counts_are_refused() {
        let many = |n: usize| -> Vec<Entry> {
            (0..n)
                .map(|i| Entry {
                    name: format!("f{i}"),
                    data: Vec::new(),
                })
                .collect()
        };
        assert!(write_zip(&many(0xFFFE), Encoder::Zlib(1)).is_ok());
        let err = write_zip(&many(0xFFFF), Encoder::Zlib(1)).expect_err("0xFFFF signals ZIP64");
        assert!(format!("{err:#}").contains("ZIP64"));
    }

    #[test]
    fn absent_input_class_is_a_skip() {
        let d = tempfile::tempdir().expect("tmp");
        let err = run(d.path(), &src(&["zlib-6"]), &[]).expect_err("no inputs");
        assert!(err.downcast_ref::<Skip>().is_some(), "{err:#}");
    }
}
