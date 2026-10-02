//! `probe text` (PLAN P0-4): general-purpose compressors on text-like classes.
//!
//! Classes `text-prose`, `logs-text`, `small-files` and `backup-versions`, each as one solid
//! stream (the deterministic in-process tar of [`super::tarball`]). Per class and compressor the
//! probe records the stream's content bytes and tar bytes, the compressed bytes, compress and
//! decompress seconds and whether the round trip reproduced the stream:
//!
//! * `xz` preset 9 and `zstd` level 19 in process, one library thread, each step timed alone;
//! * `bsc` and `kanzi` as external programs (through `lpk-procstat-sys`, the stream in the
//!   scratch directory) when found, otherwise a skip with the reason;
//! * an XWRT-style word-replacement pre-pass is recorded as skipped: XWRT is GPL and no
//!   permissively licensed implementation exists, so none is written here.
//!
//! The recorded quantities are raw bytes and seconds; ratios and speeds appear only in the table.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::codec::{xz_measure, zstd_measure, XzSettings, ZstdSettings};
use super::tarball::{write_tar, TarStats};
use super::tool::{run_tool, tool_version, ToolSpec};
use super::{mbps, md_header, md_table, pct, Ctx, Envelope, Output};
use lpk_procstat_sys as ps;

pub const NAME: &str = "text";

/// The classes, in table order.
pub const CLASSES: [&str; 4] = ["text-prose", "logs-text", "small-files", "backup-versions"];

/// The compressors, in table order: name and the setting label recorded with every row.
pub const COMPRESSORS: [(&str, &str); 4] = [
    ("xz", "preset 9, 1 thread"),
    ("zstd", "level 19, library defaults, 1 thread"),
    ("bsc", BSC_SETTING),
    ("kanzi", KANZI_SETTING),
];

const BSC_SETTING: &str = "block 256 MB, default transform, no parallelism";
const KANZI_SETTING: &str = "level 9, block 64 MB, 1 job";

/// Why the XWRT-style pre-pass was not run.
pub const XWRT_SKIP: &str = "XWRT is GPL-2.0 and no permissively licensed implementation of a \
                             word-replacement pre-pass was found; not written here";

/// What one compressor did with one class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measured {
    pub compressed_bytes: u64,
    pub compress_seconds: f64,
    pub decompress_seconds: f64,
    /// The decompressed bytes equal the stream.
    pub verified: bool,
}

/// One row: exactly one of `measured`, `skipped`, `failed` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub compressor: String,
    pub setting: String,
    pub measured: Option<Measured>,
    /// Why the compressor was not run (for example `not installed`).
    pub skipped: Option<String>,
    /// Why a run that started did not produce a verified result (never contains a path).
    pub failed: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassRun {
    pub class: String,
    /// The corpus has files of this class.
    pub present: bool,
    pub files: u64,
    pub content_bytes: u64,
    pub tar_bytes: u64,
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skip {
    pub what: String,
    pub reason: String,
}

/// The probe's own part of the result file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub classes: Vec<ClassRun>,
    /// Things not run at all (the pre-pass).
    pub skipped: Vec<Skip>,
}

// ---------------------------------------------------------------------------------------------
// External programs

/// Arguments of `bsc` for compression and decompression (paths as given, relative to the
/// working directory). `-b256` is the largest practical block, `-t` and `-T` switch off its
/// block and multi-core parallelism so the timing is single-threaded.
pub fn bsc_args(compress: bool, input: &str, output: &str) -> Vec<String> {
    let mut a = vec![
        if compress { "e" } else { "d" }.to_string(),
        input.to_string(),
        output.to_string(),
    ];
    if compress {
        a.extend(["-b256", "-t", "-T"].map(str::to_string));
    } else {
        a.extend(["-t", "-T"].map(str::to_string));
    }
    a
}

/// Arguments of `kanzi`: level 9 is its strongest general level, `-j 1` one job, `-f` overwrite.
pub fn kanzi_args(compress: bool, input: &str, output: &str) -> Vec<String> {
    let mut a = vec![
        if compress { "-c" } else { "-d" }.to_string(),
        "-f".to_string(),
        "-i".to_string(),
        input.to_string(),
        "-o".to_string(),
        output.to_string(),
        "-j".to_string(),
        "1".to_string(),
    ];
    if compress {
        a.extend(["-l", "9", "-b", "64m"].map(str::to_string));
    }
    a
}

fn tool_args(name: &str, compress: bool, input: &str, output: &str) -> Vec<String> {
    match name {
        "bsc" => bsc_args(compress, input, output),
        _ => kanzi_args(compress, input, output),
    }
}

/// Compress and decompress the stream in `dir/stream.tar` with an external program; the result
/// is verified by comparing bytes. `Err` is a failure reason.
fn measure_tool(
    ctx: &Ctx<'_>,
    exe: &Path,
    name: &str,
    dir: &Path,
    stream: &[u8],
) -> std::result::Result<Measured, String> {
    let timeout = ctx.tool_timeout;
    let err_file = dir.join("stderr.txt");
    let (src, packed, back) = ("stream.tar", "stream.packed", "stream.back");
    let step = |args: Vec<String>, what: &str| {
        run_tool(
            &ToolSpec {
                exe,
                args: &args,
                cwd: dir,
                stdin: ps::Input::Null,
                stdout: ps::Output::Discard,
                stderr_file: &err_file,
                timeout,
            },
            what,
        )
        .map_err(|f| f.reason)
    };
    let c = step(
        tool_args(name, true, src, packed),
        &format!("{name} compress"),
    )?;
    let compressed_bytes = std::fs::metadata(dir.join(packed))
        .map_err(|_| format!("{name} compress: no output file"))?
        .len();
    let d = step(
        tool_args(name, false, packed, back),
        &format!("{name} decompress"),
    )?;
    let restored =
        std::fs::read(dir.join(back)).map_err(|_| format!("{name} decompress: no output file"))?;
    let _ = std::fs::remove_file(dir.join(packed));
    let _ = std::fs::remove_file(dir.join(back));
    if restored != stream {
        return Err(format!(
            "{name}: round trip failed, the decompressed bytes differ from the stream"
        ));
    }
    Ok(Measured {
        compressed_bytes,
        compress_seconds: c.wall.as_secs_f64(),
        decompress_seconds: d.wall.as_secs_f64(),
        verified: true,
    })
}

fn row(
    name: &str,
    setting: &str,
    r: std::result::Result<Measured, String>,
    skipped: Option<String>,
) -> Run {
    let (measured, failed) = match r {
        Ok(m) => (Some(m), None),
        Err(e) => (None, Some(e)),
    };
    Run {
        compressor: name.to_string(),
        setting: setting.to_string(),
        measured,
        skipped,
        failed,
    }
}

fn skip_row(name: &str, setting: &str, reason: &str) -> Run {
    Run {
        compressor: name.to_string(),
        setting: setting.to_string(),
        measured: None,
        skipped: Some(reason.to_string()),
        failed: None,
    }
}

fn in_process(name: &str, setting: &str, r: Result<super::codec::Measured>) -> Run {
    let r = r
        .map(|m| Measured {
            compressed_bytes: m.compressed_bytes,
            compress_seconds: m.compress_seconds,
            decompress_seconds: m.decompress_seconds,
            verified: true,
        })
        .map_err(|e| e.to_string());
    row(name, setting, r, None)
}

pub fn run(ctx: &Ctx<'_>) -> Result<Output<Data>> {
    let mut notes = Vec::new();
    let mut libraries = Vec::new();
    // Look the tools up once.
    let mut tools: Vec<(&str, &str, std::result::Result<std::path::PathBuf, String>)> = Vec::new();
    for (name, setting) in &COMPRESSORS[2..] {
        tools.push((name, setting, ctx.find_tool(name)));
    }
    let scratch = ctx.scratch_dir("text")?;
    for (name, _, found) in &tools {
        if let Ok(exe) = found {
            if let Some(v) = tool_version(exe, &["--version"], &scratch) {
                libraries.push((name.to_string(), v));
            }
        }
    }
    let mut classes = Vec::new();
    for class in CLASSES {
        if ctx.class_files(class).is_none_or(|f| f.is_empty()) {
            notes.push(format!(
                "the corpus has no class `{class}`: nothing was measured for it"
            ));
            classes.push(ClassRun {
                class: class.to_string(),
                present: false,
                files: 0,
                content_bytes: 0,
                tar_bytes: 0,
                runs: Vec::new(),
            });
            continue;
        }
        let mut stream = Vec::new();
        let stats = write_tar(ctx, class, None, &mut stream)?;
        let mut runs = Vec::new();
        // Timed sections run alone, one after the other, on the stream already in memory.
        runs.push(in_process(
            COMPRESSORS[0].0,
            COMPRESSORS[0].1,
            xz_measure(&stream, &XzSettings::preset9()),
        ));
        runs.push(in_process(
            COMPRESSORS[1].0,
            COMPRESSORS[1].1,
            zstd_measure(&stream, &ZstdSettings::level19()),
        ));
        let dir = ctx.scratch_dir(&format!("text-{class}"))?;
        let mut written = false;
        for (name, setting, found) in &tools {
            match found {
                Err(reason) => runs.push(skip_row(name, setting, reason)),
                Ok(exe) => {
                    if !written {
                        std::fs::write(dir.join("stream.tar"), &stream)
                            .context("writing the stream to the scratch directory")?;
                        written = true;
                    }
                    let r = measure_tool(ctx, exe, name, &dir, &stream);
                    runs.push(row(name, setting, r, None));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        classes.push(ClassRun {
            class: class.to_string(),
            present: true,
            files: stats.files,
            content_bytes: stats.content_bytes,
            tar_bytes: stats.tar_bytes,
            runs,
        });
    }
    let data = Data {
        classes,
        skipped: vec![Skip {
            what: "xwrt-style word-replacement pre-pass".to_string(),
            reason: XWRT_SKIP.to_string(),
        }],
    };
    let mut out = Output::new(data, 1).with_zstd().with_xz();
    for (k, v) in libraries {
        out.libraries.insert(k, v);
    }
    out.notes = notes;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Validation rules and the table

fn stats_of(c: &ClassRun) -> TarStats {
    TarStats {
        files: c.files,
        content_bytes: c.content_bytes,
        tar_bytes: c.tar_bytes,
    }
}

/// Consistency rules of `data`, each as `<json pointer>: <message>`.
pub fn check(e: &Envelope<Data>) -> Vec<String> {
    let d = &e.data;
    let mut p = Vec::new();
    if e.library_threads != 1 {
        p.push("/library_threads: this probe runs its in-process compressors on one thread".into());
    }
    let names: Vec<&str> = d.classes.iter().map(|c| c.class.as_str()).collect();
    if names != CLASSES {
        p.push(format!(
            "/data/classes: classes are {names:?}, expected {CLASSES:?}"
        ));
    }
    if !d
        .skipped
        .iter()
        .any(|s| s.what.starts_with("xwrt") && !s.reason.is_empty())
    {
        p.push("/data/skipped: the XWRT-style pre-pass needs a skip entry with a reason".into());
    }
    for (i, c) in d.classes.iter().enumerate() {
        let at = format!("/data/classes/{i}");
        let s = stats_of(c);
        if !c.present {
            if s != TarStats::default() || !c.runs.is_empty() {
                p.push(format!("{at}: class not present but it has counts or runs"));
            }
            continue;
        }
        if s.files == 0 {
            p.push(format!("{at}/files: a present class has no files"));
        }
        if s.tar_bytes < s.content_bytes {
            p.push(format!(
                "{at}/tar_bytes: {} is smaller than the content ({})",
                s.tar_bytes, s.content_bytes
            ));
        }
        let got: Vec<&str> = c.runs.iter().map(|r| r.compressor.as_str()).collect();
        let want: Vec<&str> = COMPRESSORS.iter().map(|c| c.0).collect();
        if got != want {
            p.push(format!(
                "{at}/runs: compressors are {got:?}, expected {want:?}"
            ));
        }
        for (j, r) in c.runs.iter().enumerate() {
            let at = format!("{at}/runs/{j}");
            let set = u8::from(r.measured.is_some())
                + u8::from(r.skipped.is_some())
                + u8::from(r.failed.is_some());
            if set != 1 {
                p.push(format!(
                    "{at}: exactly one of measured, skipped and failed must be set (found {set})"
                ));
            }
            if matches!(r.skipped.as_deref(), Some("")) || matches!(r.failed.as_deref(), Some("")) {
                p.push(format!("{at}: a skip or failure needs a reason"));
            }
            if (r.compressor == "xz" || r.compressor == "zstd") && (r.skipped.is_some()) {
                p.push(format!(
                    "{at}/skipped: in-process compressors are never skipped"
                ));
            }
            if let Some(m) = &r.measured {
                if !m.verified {
                    p.push(format!(
                        "{at}/measured/verified: a recorded result must be verified"
                    ));
                }
                if m.compressed_bytes == 0 && s.tar_bytes > 0 {
                    p.push(format!(
                        "{at}/measured/compressed_bytes: zero for a non-empty stream"
                    ));
                }
                for (k, v) in [
                    ("compress_seconds", m.compress_seconds),
                    ("decompress_seconds", m.decompress_seconds),
                ] {
                    if !(v.is_finite() && v >= 0.0) {
                        p.push(format!("{at}/measured/{k}: must be a non-negative number"));
                    }
                }
            }
        }
    }
    p
}

/// The table, as a pure function of the parsed JSON.
pub fn render(e: &Envelope<Data>) -> String {
    let d = &e.data;
    let mut s = md_header(e);
    s.push_str(
        "Each class is one solid stream (a deterministic tar of its files). Size is compressed \
         bytes as a percentage of the tar bytes; speeds are MB/s of the tar (10^6 bytes per \
         second), compress and decompress each timed alone. `xz` and `zstd` run in process on \
         one thread; `bsc` and `kanzi` run as external programs (wall time) when installed. \
         Every result is verified by decompressing and comparing bytes.\n\n",
    );
    let mut rows = Vec::new();
    for c in &d.classes {
        if !c.present {
            rows.push(vec![
                c.class.clone(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "class not in this corpus".into(),
            ]);
            continue;
        }
        for r in &c.runs {
            let (size, cs, ds, status) = match (&r.measured, &r.skipped, &r.failed) {
                (Some(m), _, _) => (
                    pct(m.compressed_bytes, c.tar_bytes),
                    mbps(c.tar_bytes, m.compress_seconds),
                    mbps(c.tar_bytes, m.decompress_seconds),
                    if m.verified {
                        "verified"
                    } else {
                        "NOT verified"
                    }
                    .to_string(),
                ),
                (_, Some(why), _) => (
                    "-".into(),
                    "-".into(),
                    "-".into(),
                    format!("skipped: {why}"),
                ),
                (_, _, Some(why)) => ("-".into(), "-".into(), "-".into(), format!("failed: {why}")),
                _ => ("-".into(), "-".into(), "-".into(), "no result".to_string()),
            };
            rows.push(vec![
                c.class.clone(),
                r.compressor.clone(),
                r.setting.clone(),
                c.content_bytes.to_string(),
                c.tar_bytes.to_string(),
                size,
                cs,
                ds,
                status,
            ]);
        }
    }
    s.push_str(&md_table(
        &[
            "class",
            "compressor",
            "setting",
            "content bytes",
            "tar bytes",
            "size",
            "compress MB/s",
            "decompress MB/s",
            "status",
        ],
        &rows,
    ));
    s.push('\n');
    for k in &d.skipped {
        s.push_str(&format!("- skipped: {}: {}\n", k.what, k.reason));
    }
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::super::tarball::tar_bytes;
    use super::super::Ctx;
    use super::*;
    use crate::corpus::manifest::{Manifest, ManifestFile};
    use crate::run::exec::load_corpus;

    fn mf(path: &str, data: &[u8]) -> ManifestFile {
        ManifestFile {
            blake3: blake3::hash(data).to_hex().to_string(),
            bytes: data.len() as u64,
            licence: "CC0-1.0".to_string(),
            path: path.to_string(),
            source: "test".to_string(),
        }
    }

    /// Synthetic text classes; `skip` leaves one class out.
    fn corpus(tmp: &Path, skip: Option<&str>) -> PathBuf {
        let dir = tmp.join("textcorpus");
        let mut files = Vec::new();
        for class in CLASSES {
            if Some(class) == skip {
                continue;
            }
            for i in 0..3 {
                let body: String = (0..400)
                    .map(|n| format!("{class} line {n} of file {i}: the quick brown fox\n"))
                    .collect();
                let rel = format!("{class}/f{i}.txt");
                let p = dir.join(&rel);
                std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
                std::fs::write(&p, body.as_bytes()).expect("write");
                files.push((class.to_string(), mf(&rel, body.as_bytes())));
            }
        }
        let m = Manifest::with_profile_name("small", files);
        std::fs::write(dir.join("manifest.json"), m.render()).expect("manifest");
        dir
    }

    fn run_probe(tmp: &Path, skip: Option<&str>, local: &str) -> Data {
        let dir = corpus(tmp, skip);
        let corpus = load_corpus(&dir).expect("corpus");
        let scratch = tmp.join("scratch");
        std::fs::create_dir_all(&scratch).expect("scratch");
        let ctx = Ctx {
            corpus: &corpus,
            threads: 2,
            scratch,
            local_tools: PathBuf::from(local),
            tool_timeout: Duration::from_secs(60),
        };
        run(&ctx).expect("run").data
    }

    fn envelope(data: Data) -> Envelope<Data> {
        let out = Output::new(data, 1);
        Envelope {
            probe: NAME.to_string(),
            format_version: super::super::FORMAT_VERSION,
            corpus: super::super::CorpusId {
                profile: "small".into(),
                manifest_blake3: "0".repeat(64),
                private: false,
            },
            build: "test".into(),
            build_profile: super::super::BuildProfile::current(true),
            host: "h".into(),
            date: "2026-01-01T00:00:00Z".into(),
            threads: 1,
            library_threads: out.library_threads,
            libraries: out.libraries,
            elapsed_seconds: 0.0,
            notes: out.notes,
            data: out.data,
        }
    }

    #[test]
    fn every_class_has_verified_in_process_rows_and_tool_skips() {
        let tmp = tempfile::tempdir().expect("tmp");
        // A local override that names no tool; PATH may or may not hold bsc or kanzi.
        let d = run_probe(tmp.path(), None, "none.toml");
        assert_eq!(d.classes.len(), 4);
        for c in &d.classes {
            assert!(c.present);
            assert_eq!(c.files, 3);
            assert!(c.tar_bytes > c.content_bytes);
            assert_eq!(c.runs.len(), 4);
            for r in &c.runs[..2] {
                let m = r.measured.as_ref().expect("measured");
                assert!(m.verified && m.compressed_bytes > 0);
                assert!(m.compressed_bytes < c.tar_bytes);
            }
            for r in &c.runs[2..] {
                assert_eq!(
                    r.measured.is_some() as u8
                        + r.skipped.is_some() as u8
                        + r.failed.is_some() as u8,
                    1
                );
            }
        }
        assert!(d.skipped[0].reason.contains("GPL"));
        let e = envelope(d);
        assert_eq!(check(&e), Vec::<String>::new());
        let md = render(&e);
        assert!(
            md.contains("text-prose") && md.contains("xz") && md.contains("skipped: xwrt-style")
        );
    }

    #[test]
    fn a_missing_class_is_recorded_and_a_missing_tool_names_its_reason() {
        let tmp = tempfile::tempdir().expect("tmp");
        let local = tmp.path().join("local.toml");
        std::fs::write(
            &local,
            "[[tool]]\nid = \"bsc\"\npath = 'no/such/bsc'\n[[tool]]\nid = \"kanzi\"\npath = 'no/such/kanzi'\n",
        )
        .expect("toml");
        let d = run_probe(tmp.path(), Some("logs-text"), local.to_str().expect("utf8"));
        let logs = &d.classes[1];
        assert!(!logs.present && logs.runs.is_empty());
        for c in d.classes.iter().filter(|c| c.present) {
            for r in &c.runs[2..] {
                assert_eq!(
                    r.skipped.as_deref(),
                    Some(super::super::tool::LOCAL_PATH_NOT_FOUND)
                );
            }
        }
        let e = envelope(d);
        assert_eq!(check(&e), Vec::<String>::new());
        assert!(render(&e).contains("class not in this corpus"));
    }

    #[test]
    fn a_tool_that_writes_a_wrong_output_is_a_failure_not_a_result() {
        // The test binary itself is "the tool": it exits non-zero on these arguments.
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("work");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("stream.tar"), b"abc").expect("write");
        let corpus_dir = corpus(tmp.path(), None);
        let corpus = load_corpus(&corpus_dir).expect("corpus");
        let ctx = Ctx {
            corpus: &corpus,
            threads: 1,
            scratch: tmp.path().to_path_buf(),
            local_tools: PathBuf::from("none.toml"),
            tool_timeout: Duration::from_secs(60),
        };
        let me = std::env::current_exe().expect("exe");
        let r = measure_tool(&ctx, &me, "bsc", &dir, b"abc");
        let why = r.expect_err("fails");
        assert!(why.starts_with("bsc compress:"), "{why}");
        assert!(!why.contains(tmp.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn tool_arguments_are_built_as_documented() {
        assert_eq!(
            bsc_args(true, "in", "out"),
            ["e", "in", "out", "-b256", "-t", "-T"]
        );
        assert_eq!(bsc_args(false, "a", "b"), ["d", "a", "b", "-t", "-T"]);
        assert_eq!(
            kanzi_args(true, "in", "out"),
            ["-c", "-f", "-i", "in", "-o", "out", "-j", "1", "-l", "9", "-b", "64m"]
        );
        assert_eq!(
            kanzi_args(false, "a", "b"),
            ["-d", "-f", "-i", "a", "-o", "b", "-j", "1"]
        );
    }

    #[test]
    fn check_catches_inconsistent_rows() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut d = run_probe(tmp.path(), None, "none.toml");
        d.classes[0].runs[0].measured.as_mut().expect("m").verified = false;
        d.classes[1].runs[1].skipped = Some("because".into());
        d.classes[2].runs.pop();
        d.classes[3].tar_bytes = 1;
        d.skipped.clear();
        let p = check(&envelope(d));
        for needle in [
            "/data/classes/0/runs/0/measured/verified",
            "/data/classes/1/runs/1:",
            "/data/classes/2/runs:",
            "/data/classes/3/tar_bytes",
            "/data/skipped",
        ] {
            assert!(p.iter().any(|m| m.starts_with(needle)), "{needle}: {p:?}");
        }
    }

    #[test]
    fn the_stream_is_the_class_tar() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = corpus(tmp.path(), None);
        let corpus = load_corpus(&dir).expect("corpus");
        let ctx = Ctx {
            corpus: &corpus,
            threads: 1,
            scratch: tmp.path().to_path_buf(),
            local_tools: PathBuf::from("none.toml"),
            tool_timeout: Duration::from_secs(60),
        };
        let a = tar_bytes(&ctx, "text-prose", None).expect("tar");
        let mut b = Vec::new();
        let stats = write_tar(&ctx, "text-prose", None, &mut b).expect("tar");
        assert_eq!(a, b);
        assert_eq!(stats.tar_bytes, a.len() as u64);
    }
}
