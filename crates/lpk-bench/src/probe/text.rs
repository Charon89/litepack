//! `probe text` (PLAN P0-4): general-purpose compressors on text-like classes.
//!
//! Classes `text-prose`, `logs-text`, `small-files` and `backup-versions`, each as one solid
//! stream (the deterministic in-process tar of [`super::tarball`]). Per class and compressor the
//! probe records the stream's content bytes and tar bytes, the compressed bytes, compress and
//! decompress seconds and whether the round trip reproduced the stream:
//!
//! * `xz` preset 9 and `zstd` level 19 through the libraries, one thread, each step timed alone
//!   (`timing` is `in-process`);
//! * the same two as command-line programs (`xz-cli`, `zstd-cli`, `-9 -T1` and `-19 -T1`; xz decompresses with `-d -T1 -c`), and
//!   `bsc` and `kanzi`, all as external programs through `lpk-procstat-sys` with the stream in
//!   the scratch directory (`timing` is `process-wall`: process wall time, with CPU time and peak
//!   memory of each step), when found, otherwise a skip with the reason;
//! * an XWRT-style word-replacement pre-pass is recorded as skipped: XWRT is GPL and no
//!   permissively licensed implementation exists, so none is written here.
//!
//! The CLI rows find `xz` and `zstd` through `PATH` and `bench/tools.local.toml` only, while the
//! baseline runner also uses the install-location hints of `bench/tools.toml`, so the two can
//! run different binaries; the version banner of each program is recorded.
//!
//! The two timing bases are not interchangeable: compare `in-process` rows with each other and
//! `process-wall` rows with each other; the CLI rows exist to compare the two bases on the same
//! data. The recorded quantities are raw bytes and seconds; ratios and speeds appear only in the
//! table.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use lpk_procstat_sys as ps;
use serde::{Deserialize, Serialize};

use super::codec::{xz_measure, zstd_measure, XzSettings, ZstdSettings};
use super::tarball::{write_tar, TarStats};
use super::tool::{
    flush_file, run_tool, ToolSpec, LOCAL_PATH_NOT_FOUND, LOCAL_UNPARSEABLE, NOT_INSTALLED,
    STRIPPED_ENV,
};
use super::{mbps, md_header, md_table, pct, Ctx, Envelope, Output};

pub const NAME: &str = "text";

/// The classes, in table order.
pub const CLASSES: [&str; 4] = ["text-prose", "logs-text", "small-files", "backup-versions"];

/// `timing` of a row measured through the libraries.
pub const IN_PROCESS: &str = "in-process";
/// `timing` of a row measured as an external program's wall time.
pub const PROCESS_WALL: &str = "process-wall";

/// Appended to the setting of an external row that did not produce a measurement: its flags have
/// not run on this machine.
pub const NOT_RUN: &str = " (flags from documentation, not run)";

/// The compressors, in table order: name, the setting recorded with every row and the timing.
/// External settings are the literal compress arguments (`IN` and `OUT` stand for the stream
/// and the output file in the scratch directory).
pub const COMPRESSORS: [(&str, &str, &str); 6] = [
    ("xz", "preset 9, 1 thread", IN_PROCESS),
    ("zstd", "level 19, library defaults, 1 thread", IN_PROCESS),
    ("xz-cli", "-9 -T1 -c (stdin IN, stdout OUT)", PROCESS_WALL),
    ("zstd-cli", "-19 -T1 -q -f -o OUT IN", PROCESS_WALL),
    ("bsc", "e IN OUT -b256 -t -T", PROCESS_WALL),
    ("kanzi", "-c -f -i IN -o OUT -j 1 -l 9 -b 64m", PROCESS_WALL),
];

/// Why the XWRT-style pre-pass was not run.
pub const XWRT_SKIP: &str = "XWRT is GPL-2.0 and no permissively licensed implementation of a \
                             word-replacement pre-pass was found; not written here";

/// The `find_tool` skip reason for a tool whose override file cannot be read.
const LOCAL_UNREADABLE: &str = "tools.local.toml cannot be read";

/// CPU time and memory of one external step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Process {
    pub user_cpu_seconds: f64,
    pub kernel_cpu_seconds: f64,
    pub peak_rss_bytes: u64,
}

/// What one compressor did with one class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measured {
    pub compressed_bytes: u64,
    pub compress_seconds: f64,
    pub decompress_seconds: f64,
    /// The decompressed bytes equal the stream.
    pub verified: bool,
    /// External rows only.
    pub compress_process: Option<Process>,
    pub decompress_process: Option<Process>,
}

/// One row: exactly one of `measured`, `skipped`, `failed` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub compressor: String,
    pub setting: String,
    /// [`IN_PROCESS`] or [`PROCESS_WALL`]: what the seconds mean.
    pub timing: String,
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

fn is_external(name: &str) -> bool {
    COMPRESSORS
        .iter()
        .any(|c| c.0 == name && c.2 == PROCESS_WALL)
}

/// The program a compressor row runs.
fn tool_of(name: &str) -> &str {
    match name {
        "xz-cli" => "xz",
        "zstd-cli" => "zstd",
        other => other,
    }
}

/// The setting a row of `name` must carry.
fn expected_setting(name: &str, skipped: bool) -> Option<String> {
    let (_, setting, _) = COMPRESSORS.iter().find(|c| c.0 == name)?;
    Some(if is_external(name) && skipped {
        format!("{setting}{NOT_RUN}")
    } else {
        (*setting).to_string()
    })
}

// ---------------------------------------------------------------------------------------------
// External programs

/// An invocation: arguments, and whether the program reads the input from stdin and writes the
/// output to stdout (the files are then attached by the runner, not named in `args`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub args: Vec<String>,
    pub redirect: bool,
}

fn strings(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

pub fn xz_call(compress: bool, _input: &str, _output: &str) -> ToolCall {
    ToolCall {
        args: if compress {
            strings(&["-9", "-T1", "-c"])
        } else {
            strings(&["-d", "-T1", "-c"])
        },
        redirect: true,
    }
}

pub fn zstd_call(compress: bool, input: &str, output: &str) -> ToolCall {
    let mut args = if compress {
        strings(&["-19", "-T1", "-q", "-f", "-o"])
    } else {
        strings(&["-d", "-q", "-f", "-o"])
    };
    args.push(output.to_string());
    args.push(input.to_string());
    ToolCall {
        args,
        redirect: false,
    }
}

/// `bsc`: `-b256` is a 256 MB block, `-t` and `-T` switch off its parallelism so the timing is
/// single-threaded. The documentation in `docs/notes/p0-4-preflight.md` names these flags and
/// not `-e2`, so `-e2` is not used.
pub fn bsc_call(compress: bool, input: &str, output: &str) -> ToolCall {
    let mut args = strings(&[if compress { "e" } else { "d" }, input, output]);
    if compress {
        args.extend(strings(&["-b256", "-t", "-T"]));
    } else {
        args.extend(strings(&["-t", "-T"]));
    }
    ToolCall {
        args,
        redirect: false,
    }
}

/// `kanzi`: level 9, 64 MB blocks, one job, overwrite.
pub fn kanzi_call(compress: bool, input: &str, output: &str) -> ToolCall {
    let mut args = strings(&[
        if compress { "-c" } else { "-d" },
        "-f",
        "-i",
        input,
        "-o",
        output,
        "-j",
        "1",
    ]);
    if compress {
        args.extend(strings(&["-l", "9", "-b", "64m"]));
    }
    ToolCall {
        args,
        redirect: false,
    }
}

type Builder = fn(bool, &str, &str) -> ToolCall;

/// The argument builder of an external compressor; `None` for any other name.
pub fn builder_of(name: &str) -> Option<Builder> {
    match name {
        "xz-cli" => Some(xz_call),
        "zstd-cli" => Some(zstd_call),
        "bsc" => Some(bsc_call),
        "kanzi" => Some(kanzi_call),
        _ => None,
    }
}

fn process_of(m: &ps::Measurement) -> Process {
    Process {
        user_cpu_seconds: m.user_cpu.as_secs_f64(),
        kernel_cpu_seconds: m.kernel_cpu.as_secs_f64(),
        peak_rss_bytes: m.peak_rss,
    }
}

const SRC: &str = "stream.tar";
const PACKED: &str = "stream.packed";
const BACK: &str = "stream.back";

/// Compress and decompress the stream in `dir/stream.tar` with an external program; the result
/// is verified by comparing bytes. Files of an earlier run are removed first, so a program that
/// exits 0 without writing cannot pass on another program's output. `Err` is a failure reason
/// that starts with `name`.
pub fn measure_tool(
    timeout: Duration,
    exe: &Path,
    name: &str,
    dir: &Path,
    stream: &[u8],
    build: &dyn Fn(bool, &str, &str) -> ToolCall,
) -> std::result::Result<Measured, String> {
    let _ = std::fs::remove_file(dir.join(PACKED));
    let _ = std::fs::remove_file(dir.join(BACK));
    let result = measure_inner(timeout, exe, name, dir, stream, build);
    let _ = std::fs::remove_file(dir.join(PACKED));
    let _ = std::fs::remove_file(dir.join(BACK));
    result
}

fn measure_inner(
    timeout: Duration,
    exe: &Path,
    name: &str,
    dir: &Path,
    stream: &[u8],
    build: &dyn Fn(bool, &str, &str) -> ToolCall,
) -> std::result::Result<Measured, String> {
    let err_file = dir.join("stderr.txt");
    let step = |compress: bool, from: &str, to: &str, what: &str| {
        let call = build(compress, from, to);
        let (stdin, stdout) = if call.redirect {
            (
                ps::Input::File(dir.join(from)),
                ps::Output::File(dir.join(to)),
            )
        } else {
            (ps::Input::Null, ps::Output::Discard)
        };
        run_tool(
            &ToolSpec {
                exe,
                args: &call.args,
                cwd: dir,
                stdin,
                stdout,
                stderr_file: &err_file,
                timeout,
            },
            what,
        )
        .map_err(|f| {
            if f.reason.starts_with(name) {
                f.reason
            } else {
                format!("{name}: {}", f.reason)
            }
        })
    };
    flush_file(&dir.join(SRC));
    let c = step(true, SRC, PACKED, &format!("{name} compress"))?;
    let compressed_bytes = std::fs::metadata(dir.join(PACKED))
        .map_err(|_| format!("{name} compress: no output file"))?
        .len();
    flush_file(&dir.join(PACKED));
    let d = step(false, PACKED, BACK, &format!("{name} decompress"))?;
    let restored =
        std::fs::read(dir.join(BACK)).map_err(|_| format!("{name} decompress: no output file"))?;
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
        compress_process: Some(process_of(&c)),
        decompress_process: Some(process_of(&d)),
    })
}

/// The first line of `text` that holds a version-like number (a digit, a dot and a digit) and no
/// path separator (a line that shows a path is skipped).
pub fn extract_banner(text: &str) -> Option<String> {
    text.lines().map(str::trim).find_map(|l| {
        let b = l.as_bytes();
        let hit = b
            .windows(3)
            .any(|w| w[0].is_ascii_digit() && w[1] == b'.' && w[2].is_ascii_digit());
        (hit && !l.contains(['/', '\\'])).then(|| l.chars().take(200).collect())
    })
}

/// What is recorded for a program that prints no version: not a file name, the program's hash.
pub fn unknown_version(exe_bytes: &[u8]) -> String {
    format!("unknown (blake3 {})", blake3::hash(exe_bytes).to_hex())
}

/// The banner a program prints (stdout and stderr, whatever the exit code, for `--version`,
/// `-V` and no arguments), else `unknown` and its BLAKE3.
fn version_banner(exe: &Path, work: &Path) -> String {
    for args in [vec!["--version"], vec!["-V"], vec!["-h"], vec!["--help"]] {
        let out = work.join("banner-out.txt");
        let err = work.join("banner-err.txt");
        let mut spec = ps::Spec::new(exe)
            .args(args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .cwd(work)
            .stdin(ps::Input::Null)
            .stdout(ps::Output::File(out.clone()))
            .stderr(ps::Output::File(err.clone()))
            .timeout(Duration::from_secs(15));
        for name in STRIPPED_ENV {
            spec = spec.env_remove(*name);
        }
        if ps::run(&spec).is_err() {
            continue;
        }
        let mut text = std::fs::read_to_string(&out).unwrap_or_default();
        text.push('\n');
        text.push_str(&std::fs::read_to_string(&err).unwrap_or_default());
        if let Some(b) = extract_banner(&text) {
            return b;
        }
    }
    match std::fs::read(exe) {
        Ok(bytes) => unknown_version(&bytes),
        Err(_) => "unknown".to_string(),
    }
}

fn run_row(name: &str, timing: &str, r: std::result::Result<Measured, String>) -> Run {
    let (m, failed) = match r {
        Ok(m) => (Some(m), None),
        Err(e) => {
            let e = if e.starts_with(name) {
                e
            } else {
                format!("{name}: {e}")
            };
            (None, Some(e))
        }
    };
    Run {
        compressor: name.to_string(),
        setting: expected_setting(name, false).unwrap_or_default(),
        timing: timing.to_string(),
        measured: m,
        skipped: None,
        failed,
    }
}

fn skip_row(name: &str, timing: &str, reason: &str) -> Run {
    Run {
        compressor: name.to_string(),
        setting: expected_setting(name, true).unwrap_or_default(),
        timing: timing.to_string(),
        measured: None,
        skipped: Some(reason.to_string()),
        failed: None,
    }
}

fn in_process(name: &str, r: Result<super::codec::Measured>) -> Run {
    let r = r
        .map(|m| Measured {
            compressed_bytes: m.compressed_bytes,
            compress_seconds: m.compress_seconds,
            decompress_seconds: m.decompress_seconds,
            verified: true,
            compress_process: None,
            decompress_process: None,
        })
        .map_err(|e| e.to_string());
    run_row(name, IN_PROCESS, r)
}

pub fn run(ctx: &Ctx<'_>) -> Result<Output<Data>> {
    let mut notes = Vec::new();
    let mut versions: Vec<(String, String)> = Vec::new();
    let scratch = ctx.scratch_dir("text")?;
    let mut tools: Vec<(&str, std::result::Result<PathBuf, String>)> = Vec::new();
    for (name, _, timing) in &COMPRESSORS {
        if *timing == PROCESS_WALL {
            let found = ctx.find_tool(tool_of(name));
            if let Ok(exe) = &found {
                versions.push((name.to_string(), version_banner(exe, &scratch)));
            }
            tools.push((name, found));
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
            "xz",
            xz_measure(&stream, &XzSettings::preset9()),
        ));
        runs.push(in_process(
            "zstd",
            zstd_measure(&stream, &ZstdSettings::level19()),
        ));
        let dir = ctx.scratch_dir(&format!("text-{class}"))?;
        let mut written: Option<std::result::Result<(), String>> = None;
        for (name, found) in &tools {
            let exe = match found {
                Err(reason) => {
                    runs.push(skip_row(name, PROCESS_WALL, reason));
                    continue;
                }
                Ok(exe) => exe,
            };
            let ok = written.get_or_insert_with(|| {
                std::fs::write(dir.join(SRC), &stream)
                    .map_err(|_| "could not write the stream to the scratch directory".to_string())
            });
            let r = match (ok, builder_of(name)) {
                (Err(why), _) => Err(why.clone()),
                (Ok(()), None) => Err("no argument builder".to_string()),
                (Ok(()), Some(b)) => measure_tool(ctx.tool_timeout, exe, name, &dir, &stream, &b),
            };
            runs.push(run_row(name, PROCESS_WALL, r));
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
    for (k, v) in versions {
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

fn seconds_ok(v: f64) -> bool {
    v.is_finite() && v >= 0.0
}

fn check_process(p: &mut Vec<String>, at: &str, want: bool, v: &Option<Process>) {
    match (want, v) {
        (true, None) => p.push(format!("{at}: an external row needs CPU time and memory")),
        (false, Some(_)) => p.push(format!("{at}: an in-process row has no process record")),
        (true, Some(x)) if !seconds_ok(x.user_cpu_seconds) || !seconds_ok(x.kernel_cpu_seconds) => {
            p.push(format!("{at}: CPU seconds must be non-negative numbers"));
        }
        _ => {}
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
        let least = s
            .content_bytes
            .saturating_add(512u64.saturating_mul(s.files))
            .saturating_add(1024);
        if !s.tar_bytes.is_multiple_of(512) || s.tar_bytes < least {
            p.push(format!(
                "{at}/tar_bytes: {} is not a multiple of 512 of at least {least} (content, one \
                 header per file, end marker)",
                s.tar_bytes
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
            let external = is_external(&r.compressor);
            let set = u8::from(r.measured.is_some())
                + u8::from(r.skipped.is_some())
                + u8::from(r.failed.is_some());
            if set != 1 {
                p.push(format!(
                    "{at}: exactly one of measured, skipped and failed must be set (found {set})"
                ));
            }
            match expected_setting(&r.compressor, r.skipped.is_some()) {
                Some(s) if s == r.setting => {}
                Some(s) => p.push(format!("{at}/setting: `{}` (expected `{s}`)", r.setting)),
                None => p.push(format!("{at}/compressor: unknown `{}`", r.compressor)),
            }
            let timing = if external { PROCESS_WALL } else { IN_PROCESS };
            if r.timing != timing {
                p.push(format!("{at}/timing: `{}` (expected `{timing}`)", r.timing));
            }
            if let Some(why) = &r.skipped {
                if !external {
                    p.push(format!(
                        "{at}/skipped: in-process compressors are never skipped"
                    ));
                }
                if ![
                    NOT_INSTALLED,
                    LOCAL_PATH_NOT_FOUND,
                    LOCAL_UNPARSEABLE,
                    LOCAL_UNREADABLE,
                ]
                .contains(&why.as_str())
                {
                    p.push(format!("{at}/skipped: `{why}` is not a tool lookup reason"));
                }
            }
            if let Some(why) = &r.failed {
                if why.is_empty() || !why.starts_with(&r.compressor) {
                    p.push(format!(
                        "{at}/failed: the reason must start with the compressor name"
                    ));
                }
            }
            if external
                && (r.measured.is_some() || r.failed.is_some())
                && !e.libraries.contains_key(&r.compressor)
            {
                p.push(format!(
                    "/libraries/{}: an external program that ran needs a version entry",
                    r.compressor
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
                    if !seconds_ok(v) {
                        p.push(format!("{at}/measured/{k}: must be a non-negative number"));
                    }
                }
                check_process(
                    &mut p,
                    &format!("{at}/measured/compress_process"),
                    external,
                    &m.compress_process,
                );
                check_process(
                    &mut p,
                    &format!("{at}/measured/decompress_process"),
                    external,
                    &m.decompress_process,
                );
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
        "Each class is one solid stream (a deterministic tar of its files). Sizes are compressed \
         bytes as a percentage of the class's content bytes (the baseline runner's basis) and of \
         the tar bytes; speeds are MB/s of the tar (10^6 bytes per second), compress and \
         decompress each timed alone. Timing `in-process`: the library call on a stream in memory. \
         Timing `process-wall`: the external program's wall time on files in the scratch \
         directory, which includes its start-up and file I/O; the `xz-cli` and `zstd-cli` rows \
         run the installed command-line tools on the same stream so the two bases can be \
         compared. Comparisons are like-for-like only within this probe: the baseline runner \
         runs at the machine's thread count on bsdtar's tar, these rows on one thread on this \
         probe's tar. Every result is verified by decompressing and comparing bytes.\n\n",
    );
    let dash = || "-".to_string();
    let mut rows = Vec::new();
    for c in &d.classes {
        if !c.present {
            let mut r = vec![c.class.clone()];
            r.extend((0..9).map(|_| dash()));
            r.push("class not in this corpus".into());
            rows.push(r);
            continue;
        }
        for r in &c.runs {
            let (of_content, of_tar, cs, ds, status) = match (&r.measured, &r.skipped, &r.failed) {
                (Some(m), _, _) => (
                    pct(m.compressed_bytes, c.content_bytes),
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
                (_, Some(why), _) => (dash(), dash(), dash(), dash(), format!("skipped: {why}")),
                (_, _, Some(why)) => (dash(), dash(), dash(), dash(), format!("failed: {why}")),
                _ => (dash(), dash(), dash(), dash(), "no result".to_string()),
            };
            rows.push(vec![
                c.class.clone(),
                r.compressor.clone(),
                r.setting.clone(),
                r.timing.clone(),
                c.content_bytes.to_string(),
                c.tar_bytes.to_string(),
                of_content,
                of_tar,
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
            "timing",
            "content bytes",
            "tar bytes",
            "% of content bytes",
            "% of tar bytes",
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
    use std::collections::BTreeMap;

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

    fn run_probe(tmp: &Path, skip: Option<&str>, local: &str) -> Output<Data> {
        let dir = corpus(tmp, skip);
        let corpus = load_corpus(&dir).expect("corpus");
        let scratch = tmp.join("scratch");
        std::fs::create_dir_all(&scratch).expect("scratch");
        let ctx = Ctx {
            corpus: &corpus,
            threads: 2,
            scratch,
            local_tools: PathBuf::from(local),
            tool_timeout: Duration::from_secs(120),
        };
        run(&ctx).expect("run")
    }

    fn envelope(data: Data, libraries: BTreeMap<String, String>) -> Envelope<Data> {
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
            library_threads: 1,
            libraries,
            elapsed_seconds: 0.0,
            notes: Vec::new(),
            data,
        }
    }

    fn envelope_of(out: Output<Data>) -> Envelope<Data> {
        envelope(out.data, out.libraries)
    }

    fn outcomes(r: &Run) -> u8 {
        u8::from(r.measured.is_some())
            + u8::from(r.skipped.is_some())
            + u8::from(r.failed.is_some())
    }

    #[test]
    fn in_process_rows_are_verified_and_every_external_row_has_one_outcome() {
        let tmp = tempfile::tempdir().expect("tmp");
        // PATH may hold xz, zstd, bsc or kanzi: external rows are then measured for real.
        let out = run_probe(tmp.path(), None, "none.toml");
        let d = &out.data;
        assert_eq!(d.classes.len(), 4);
        for c in &d.classes {
            assert!(c.present);
            assert_eq!(c.files, 3);
            assert!(c.tar_bytes > c.content_bytes);
            assert_eq!(c.runs.len(), COMPRESSORS.len());
            for r in &c.runs[..2] {
                let m = r.measured.as_ref().expect("measured");
                assert!(m.verified && m.compressed_bytes > 0);
                assert!(m.compressed_bytes < c.tar_bytes);
                assert_eq!(r.timing, IN_PROCESS);
            }
            for r in &c.runs[2..] {
                assert_eq!(outcomes(r), 1);
                assert_eq!(r.timing, PROCESS_WALL);
                if let Some(m) = &r.measured {
                    assert!(m.compress_process.is_some() && m.decompress_process.is_some());
                }
            }
        }
        assert!(d.skipped[0].reason.contains("GPL"));
        let e = envelope_of(out);
        assert_eq!(check(&e), Vec::<String>::new());
        let md = render(&e);
        assert!(
            md.contains("text-prose") && md.contains("xz") && md.contains("skipped: xwrt-style")
        );
        assert!(md.contains("% of content bytes") && md.contains("process-wall"));
    }

    #[test]
    fn a_missing_class_is_recorded_and_tools_named_in_the_override_file_but_absent_are_skipped() {
        let tmp = tempfile::tempdir().expect("tmp");
        let local = tmp.path().join("local.toml");
        let mut text = String::new();
        for id in ["xz", "zstd", "bsc", "kanzi"] {
            text.push_str(&format!("[[tool]]\nid = \"{id}\"\npath = 'no/such/{id}'\n"));
        }
        std::fs::write(&local, text).expect("toml");
        let out = run_probe(tmp.path(), Some("logs-text"), local.to_str().expect("utf8"));
        let logs = &out.data.classes[1];
        assert!(!logs.present && logs.runs.is_empty());
        for c in out.data.classes.iter().filter(|c| c.present) {
            for r in &c.runs[2..] {
                assert_eq!(r.skipped.as_deref(), Some(LOCAL_PATH_NOT_FOUND));
                assert!(r.setting.ends_with(NOT_RUN));
            }
        }
        let e = envelope_of(out);
        assert_eq!(check(&e), Vec::<String>::new());
        assert!(render(&e).contains("class not in this corpus"));
    }

    // The test binary is the program: `--list` exits 0 and prints the same text every time.

    fn list_call(_c: bool, _i: &str, _o: &str) -> ToolCall {
        ToolCall {
            args: vec!["--list".to_string()],
            redirect: true,
        }
    }

    fn list_output(me: &Path) -> Vec<u8> {
        std::process::Command::new(me)
            .arg("--list")
            .output()
            .expect("run")
            .stdout
    }

    fn workdir(tmp: &Path, stream: &[u8]) -> PathBuf {
        let dir = tmp.join("work");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join(SRC), stream).expect("write");
        dir
    }

    #[test]
    fn a_program_that_writes_nothing_is_a_failure_and_stale_output_is_not_reused() {
        let tmp = tempfile::tempdir().expect("tmp");
        let me = std::env::current_exe().expect("exe");
        let stream = list_output(&me);
        let dir = workdir(tmp.path(), &stream);
        let t = Duration::from_secs(120);
        // A first program leaves a good packed file and a good restored file behind ...
        std::fs::write(dir.join(PACKED), b"left over").expect("write");
        std::fs::write(dir.join(BACK), &stream).expect("write");
        // ... a second one exits 0 without writing: that must not be recorded as verified.
        let none = |_c: bool, _i: &str, _o: &str| ToolCall {
            args: vec!["--list".to_string()],
            redirect: false,
        };
        let why = measure_tool(t, &me, "bsc", &dir, &stream, &none).expect_err("fails");
        assert_eq!(why, "bsc compress: no output file");
        assert!(!dir.join(PACKED).exists() && !dir.join(BACK).exists());
    }

    #[test]
    fn a_restored_stream_that_differs_is_a_round_trip_failure_without_paths() {
        let tmp = tempfile::tempdir().expect("tmp");
        let me = std::env::current_exe().expect("exe");
        let dir = workdir(tmp.path(), b"abc");
        let why = measure_tool(
            Duration::from_secs(120),
            &me,
            "bsc",
            &dir,
            b"abc",
            &list_call,
        )
        .expect_err("fails");
        assert!(why.starts_with("bsc: round trip failed"), "{why}");
        assert!(!why.contains(tmp.path().to_string_lossy().as_ref()));
        assert!(!dir.join(PACKED).exists() && !dir.join(BACK).exists());
    }

    #[test]
    fn a_verified_external_run_records_size_seconds_and_process_figures_and_cleans_up() {
        let tmp = tempfile::tempdir().expect("tmp");
        let me = std::env::current_exe().expect("exe");
        let stream = list_output(&me);
        let dir = workdir(tmp.path(), &stream);
        let m = measure_tool(
            Duration::from_secs(120),
            &me,
            "xz-cli",
            &dir,
            &stream,
            &list_call,
        )
        .expect("verified");
        assert!(m.verified);
        assert_eq!(m.compressed_bytes, stream.len() as u64);
        assert!(m.compress_seconds.is_finite() && m.compress_seconds >= 0.0);
        assert!(m.decompress_seconds.is_finite() && m.decompress_seconds >= 0.0);
        let p = m.compress_process.expect("process");
        assert!(p.peak_rss_bytes > 0);
        assert!(m.decompress_process.is_some());
        assert!(!dir.join(PACKED).exists() && !dir.join(BACK).exists());
        assert!(dir.join(SRC).exists());
    }

    #[test]
    fn a_program_that_exits_non_zero_names_the_compressor() {
        let tmp = tempfile::tempdir().expect("tmp");
        let me = std::env::current_exe().expect("exe");
        let dir = workdir(tmp.path(), b"abc");
        let bad = |_c: bool, _i: &str, _o: &str| ToolCall {
            args: vec!["--definitely-not-a-flag".to_string()],
            redirect: false,
        };
        let why = measure_tool(Duration::from_secs(120), &me, "kanzi", &dir, b"abc", &bad)
            .expect_err("fails");
        assert!(why.starts_with("kanzi compress: exit code"), "{why}");
    }

    #[test]
    fn tool_arguments_are_built_as_documented_and_unknown_names_have_no_builder() {
        let c = bsc_call(true, "in", "out");
        assert_eq!(c.args, ["e", "in", "out", "-b256", "-t", "-T"]);
        assert!(!c.redirect);
        assert_eq!(bsc_call(false, "a", "b").args, ["d", "a", "b", "-t", "-T"]);
        assert_eq!(
            kanzi_call(true, "in", "out").args,
            ["-c", "-f", "-i", "in", "-o", "out", "-j", "1", "-l", "9", "-b", "64m"]
        );
        assert_eq!(
            kanzi_call(false, "a", "b").args,
            ["-d", "-f", "-i", "a", "-o", "b", "-j", "1"]
        );
        let x = xz_call(true, "ignored", "ignored");
        assert_eq!(x.args, ["-9", "-T1", "-c"]);
        assert!(x.redirect);
        assert_eq!(xz_call(false, "a", "b").args, ["-d", "-T1", "-c"]);
        assert_eq!(
            zstd_call(true, "in", "out").args,
            ["-19", "-T1", "-q", "-f", "-o", "out", "in"]
        );
        assert_eq!(
            zstd_call(false, "in", "out").args,
            ["-d", "-q", "-f", "-o", "out", "in"]
        );
        for n in ["xz-cli", "zstd-cli", "bsc", "kanzi"] {
            assert!(builder_of(n).is_some(), "{n}");
        }
        assert!(builder_of("xz").is_none() && builder_of("other").is_none());
    }

    #[test]
    fn banners_come_from_a_version_like_line_else_the_hash_never_a_name() {
        assert_eq!(
            extract_banner("\nUsage: tool\n tool 3.3.12 (2025)\nmore 1.2\n"),
            Some("tool 3.3.12 (2025)".to_string())
        );
        assert_eq!(extract_banner("no numbers here\n5 only\n"), None);
        assert_eq!(
            extract_banner("usage: /home/me/tool 1.2 [opts]\nC:\\tools\\x 3.4\ntool 5.6\n"),
            Some("tool 5.6".to_string()),
            "lines with a path separator are never recorded"
        );
        let u = unknown_version(b"program bytes");
        assert!(u.starts_with("unknown (blake3 ") && u.ends_with(')'));
        assert_eq!(u, unknown_version(b"program bytes"));
        assert!(!u.contains("exe"));
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
        assert_eq!(stats.tar_bytes % 512, 0);
    }

    // ----- check rules, on a hand-built valid result -----

    fn proc_record() -> Option<Process> {
        Some(Process {
            user_cpu_seconds: 0.5,
            kernel_cpu_seconds: 0.1,
            peak_rss_bytes: 1 << 20,
        })
    }

    fn valid_class(class: &str) -> ClassRun {
        let mut runs = Vec::new();
        for (name, _, timing) in COMPRESSORS {
            let external = timing == PROCESS_WALL;
            let measure = name != "bsc" && name != "kanzi";
            let mut r = Run {
                compressor: name.to_string(),
                setting: expected_setting(name, !measure).expect("known"),
                timing: timing.to_string(),
                measured: None,
                skipped: None,
                failed: None,
            };
            if measure {
                r.measured = Some(Measured {
                    compressed_bytes: 500,
                    compress_seconds: 1.0,
                    decompress_seconds: 0.5,
                    verified: true,
                    compress_process: external.then(proc_record).flatten(),
                    decompress_process: external.then(proc_record).flatten(),
                });
            } else {
                r.skipped = Some(NOT_INSTALLED.to_string());
            }
            runs.push(r);
        }
        ClassRun {
            class: class.to_string(),
            present: true,
            files: 1,
            content_bytes: 10,
            tar_bytes: 2048,
            runs,
        }
    }

    fn valid() -> Envelope<Data> {
        let mut libs = BTreeMap::new();
        libs.insert("xz-cli".to_string(), "xz 5.8".to_string());
        libs.insert("zstd-cli".to_string(), "zstd 1.5".to_string());
        envelope(
            Data {
                classes: CLASSES.iter().map(|c| valid_class(c)).collect(),
                skipped: vec![Skip {
                    what: "xwrt-style pre-pass".into(),
                    reason: XWRT_SKIP.into(),
                }],
            },
            libs,
        )
    }

    #[test]
    fn the_hand_built_result_is_valid() {
        assert_eq!(check(&valid()), Vec::<String>::new());
    }

    #[test]
    fn check_reports_each_kind_of_inconsistency() {
        type Mutation = Box<dyn Fn(&mut Envelope<Data>)>;
        let cases: Vec<(&str, Mutation)> = vec![
            (
                "/data/classes/0:",
                Box::new(|e| e.data.classes[0].present = false),
            ),
            (
                "/data/classes/0/files",
                Box::new(|e| e.data.classes[0].files = 0),
            ),
            (
                "/data/classes/0/tar_bytes",
                Box::new(|e| e.data.classes[0].tar_bytes = 2049),
            ),
            (
                "/data/classes/0/tar_bytes",
                Box::new(|e| e.data.classes[0].tar_bytes = 1024),
            ),
            ("/data/classes:", Box::new(|e| e.data.classes.swap(0, 1))),
            (
                "/data/classes/0/runs:",
                Box::new(|e| {
                    e.data.classes[0].runs.pop();
                }),
            ),
            (
                "/data/classes/0/runs/0:",
                Box::new(|e| e.data.classes[0].runs[0].skipped = Some(NOT_INSTALLED.into())),
            ),
            (
                "/data/classes/0/runs/0:",
                Box::new(|e| e.data.classes[0].runs[0].measured = None),
            ),
            (
                "/data/classes/0/runs/4/skipped",
                Box::new(|e| e.data.classes[0].runs[4].skipped = Some(String::new())),
            ),
            (
                "/data/classes/0/runs/4/skipped",
                Box::new(|e| e.data.classes[0].runs[4].skipped = Some("because".into())),
            ),
            (
                "/data/classes/0/runs/0/skipped",
                Box::new(|e| {
                    let r = &mut e.data.classes[0].runs[0];
                    r.measured = None;
                    r.skipped = Some(NOT_INSTALLED.into());
                }),
            ),
            (
                "/data/classes/0/runs/4/failed",
                Box::new(|e| {
                    let r = &mut e.data.classes[0].runs[4];
                    r.skipped = None;
                    r.failed = Some("kanzi: wrong name".into());
                }),
            ),
            (
                "/data/classes/0/runs/4/failed",
                Box::new(|e| {
                    let r = &mut e.data.classes[0].runs[4];
                    r.skipped = None;
                    r.failed = Some(String::new());
                }),
            ),
            (
                "/data/classes/0/runs/0/setting",
                Box::new(|e| e.data.classes[0].runs[0].setting = "preset 6".into()),
            ),
            (
                "/data/classes/0/runs/2/setting",
                Box::new(|e| e.data.classes[0].runs[2].setting.push_str(NOT_RUN)),
            ),
            (
                "/data/classes/0/runs/0/timing",
                Box::new(|e| e.data.classes[0].runs[0].timing = PROCESS_WALL.into()),
            ),
            (
                "/data/classes/0/runs/2/timing",
                Box::new(|e| e.data.classes[0].runs[2].timing = IN_PROCESS.into()),
            ),
            (
                "/data/classes/0/runs/0/measured/verified",
                Box::new(|e| {
                    e.data.classes[0].runs[0]
                        .measured
                        .as_mut()
                        .expect("m")
                        .verified = false
                }),
            ),
            (
                "/data/classes/0/runs/0/measured/compressed_bytes",
                Box::new(|e| {
                    e.data.classes[0].runs[0]
                        .measured
                        .as_mut()
                        .expect("m")
                        .compressed_bytes = 0
                }),
            ),
            (
                "/data/classes/0/runs/1/measured/compress_seconds",
                Box::new(|e| {
                    e.data.classes[0].runs[1]
                        .measured
                        .as_mut()
                        .expect("m")
                        .compress_seconds = -1.0
                }),
            ),
            (
                "/data/classes/0/runs/2/measured/compress_process",
                Box::new(|e| {
                    e.data.classes[0].runs[2]
                        .measured
                        .as_mut()
                        .expect("m")
                        .compress_process = None
                }),
            ),
            (
                "/data/classes/0/runs/0/measured/decompress_process",
                Box::new(|e| {
                    e.data.classes[0].runs[0]
                        .measured
                        .as_mut()
                        .expect("m")
                        .decompress_process = proc_record()
                }),
            ),
            (
                "/libraries/xz-cli",
                Box::new(|e| {
                    e.libraries.remove("xz-cli");
                }),
            ),
            ("/library_threads", Box::new(|e| e.library_threads = 2)),
            ("/data/skipped", Box::new(|e| e.data.skipped.clear())),
        ];
        for (needle, mutate) in cases {
            let mut e = valid();
            mutate(&mut e);
            let p = check(&e);
            assert!(
                p.iter().any(|m| m.starts_with(needle)),
                "{needle} not reported: {p:?}"
            );
        }
    }
}
