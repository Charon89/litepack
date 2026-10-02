//! `ffmpeg-encode`: short HEVC and AV1 encodes of the video inputs, through the external `ffmpeg`
//! program (D-15 item 8: no official HEVC/AV1 encodes of the test movie exist).
//!
//! The program name comes from `BuildOptions::ffmpeg_program` (default `ffmpeg` on `PATH`) and is
//! run without a shell: every argument is a separate item. Without the program the source is
//! skipped with a reason (it must be `optional`, as in the registry); a codec whose encoder this
//! ffmpeg build lacks is left out with a note on stderr, and if none is available the whole
//! source is skipped. The first line of `ffmpeg -version` goes to `tools` in `build-info.json`.
//!
//! Reproducibility: the arguments strip everything that varies (metadata, chapters, encoder tags,
//! container timestamps via `bitexact`), use one thread (`-threads 1`, x265 `pools=none` and
//! `frame-threads=1`, libaom `row-mt` off) and the video track only, and the clip starts at the
//! start of the input. The same ffmpeg binary gives the same bytes; a different ffmpeg or encoder
//! version may not, which is why the version is recorded.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use super::{input_files_with, Output, Skip};
use crate::corpus::build::Ctx;
use crate::corpus::registry::{FfmpegEncodeSpec, Source};

const VIDEO_EXTS: [&str; 6] = [".mp4", ".m4v", ".mkv", ".webm", ".avi", ".mov"];

/// `(codec key, ffmpeg encoder name, output suffix)`.
const CODECS: [(&str, &str, &str); 2] = [
    ("hevc", "libx265", "-hevc.mp4"),
    ("av1", "libaom-av1", "-av1.mp4"),
];

/// The arguments of one encode (never joined into a shell line).
pub fn encode_args(input: &Path, output: &Path, codec: &str, clip_seconds: u32) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let mut push = |s: &str| a.push(OsString::from(s));
    for s in [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-threads",
        "1",
        "-i",
    ] {
        push(s);
    }
    a.push(input.as_os_str().to_os_string());
    let mut push = |s: &str| a.push(OsString::from(s));
    push("-t");
    push(&clip_seconds.to_string());
    for s in [
        "-map",
        "0:v:0",
        "-an",
        "-sn",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-fflags",
        "+bitexact",
        "-flags:v",
        "+bitexact",
        "-pix_fmt",
        "yuv420p",
        "-threads",
        "1",
    ] {
        push(s);
    }
    match codec {
        "hevc" => {
            for s in [
                "-c:v",
                "libx265",
                "-preset",
                "medium",
                "-crf",
                "26",
                "-tag:v",
                "hvc1",
                "-x265-params",
                "pools=none:frame-threads=1:no-wpp=1:info=0:log-level=error",
            ] {
                push(s);
            }
        }
        _ => {
            for s in [
                "-c:v",
                "libaom-av1",
                "-crf",
                "32",
                "-b:v",
                "0",
                "-cpu-used",
                "6",
                "-row-mt",
                "0",
                "-lag-in-frames",
                "19",
            ] {
                push(s);
            }
        }
    }
    for s in ["-f", "mp4"] {
        push(s);
    }
    a.push(output.as_os_str().to_os_string());
    a
}

/// Run `program args...`, mapping "not found" to a [`Skip`].
fn run_program(program: &str, args: &[OsString]) -> Result<std::process::Output> {
    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) => Ok(o),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Skip(format!(
            "ffmpeg not installed (program `{program}` not found); HEVC and AV1 encodes skipped"
        ))
        .into()),
        Err(e) => Err(e).with_context(|| format!("running `{program}`")),
    }
}

/// The version line an encoder library prints when ffmpeg runs a one-frame encode at verbose
/// log level: x265's `HEVC encoder version ...` line, libaom's bare version line.
pub fn parse_encoder_version(encoder: &str, log: &str) -> Option<String> {
    for line in log.lines() {
        let line = line.trim();
        if encoder == "libx265" {
            if let Some(i) = line.find("HEVC encoder version ") {
                return Some(line[i..].trim().to_string());
            }
        } else if line.starts_with("[libaom-av1 @ ") {
            if let Some((_, rest)) = line.split_once("] ") {
                if rest.starts_with(|c: char| c.is_ascii_digit()) {
                    return Some(format!("libaom {}", rest.trim()));
                }
            }
        }
    }
    None
}

type Wanted = Vec<(&'static str, &'static str, &'static str)>;

/// Split the wanted codecs by whether the `ffmpeg -encoders` listing has their encoder:
/// `(key, encoder, suffix)` for those available, a reason for each one that is not.
fn select_codecs(codecs: &[String], listing: &str, version: &str) -> (Wanted, Vec<String>) {
    let has = |name: &str| {
        listing
            .lines()
            .any(|l| l.split_whitespace().nth(1) == Some(name))
    };
    let (mut wanted, mut dropped) = (Vec::new(), Vec::new());
    for (key, encoder, suffix) in CODECS {
        if !codecs.iter().any(|c| c == key) {
            continue;
        }
        if has(encoder) {
            wanted.push((key, encoder, suffix));
        } else {
            dropped.push(format!(
                "{key} encodes skipped: this ffmpeg ({version}) has no {encoder} encoder"
            ));
        }
    }
    (wanted, dropped)
}

fn probe_version(program: &str, encoder: &str) -> Option<String> {
    let args: Vec<OsString> = [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "verbose",
        "-f",
        "lavfi",
        "-i",
        "color=size=64x64:rate=1:duration=1",
        "-frames:v",
        "1",
        "-c:v",
        encoder,
        "-f",
        "null",
        "-",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    let o = run_program(program, &args).ok()?;
    parse_encoder_version(encoder, &String::from_utf8_lossy(&o.stderr))
}

fn stem(rel: &str) -> &str {
    let name_start = rel.rfind('/').map_or(0, |i| i + 1);
    match rel[name_start..].rfind('.') {
        Some(i) if i > 0 => &rel[..name_start + i],
        _ => rel,
    }
}

pub fn build(
    ctx: &mut Ctx<'_>,
    source: &Source,
    spec: &FfmpegEncodeSpec,
    out: &mut Output<'_>,
) -> Result<()> {
    let program = ctx.ffmpeg_program().to_string();
    let inputs = input_files_with(ctx, source, &spec.from, &VIDEO_EXTS)?;
    let version = run_program(&program, &[OsString::from("-version")])?;
    if !version.status.success() {
        bail!("`{program} -version` failed");
    }
    let version_text = String::from_utf8_lossy(&version.stdout);
    let first = version_text.lines().next().unwrap_or("").trim().to_string();
    ctx.note_tool("ffmpeg", &first);
    let encoders = run_program(
        &program,
        &[OsString::from("-hide_banner"), OsString::from("-encoders")],
    )?;
    let listing = String::from_utf8_lossy(&encoders.stdout);
    let (wanted, dropped) = select_codecs(&spec.codecs, &listing, &first);
    if wanted.is_empty() {
        return Err(Skip(format!(
            "this ffmpeg ({first}) has none of the wanted encoders ({})",
            spec.codecs.join(", ")
        ))
        .into());
    }
    for d in dropped {
        eprintln!("  {d}");
        ctx.note_skip(&source.id, d);
    }
    // The encoder libraries decide the bytes, and one ffmpeg version string can hide different
    // builds: record their versions too.
    for (key, encoder, _) in &wanted {
        let tool = if *key == "hevc" { "x265" } else { "libaom" };
        let v = probe_version(&program, encoder).unwrap_or_else(|| "unknown".to_string());
        ctx.note_tool(tool, &v);
    }
    let wanted: Vec<(&str, &str)> = wanted.into_iter().map(|(k, _, s)| (k, s)).collect();
    for f in &inputs {
        for (key, suffix) in &wanted {
            let rel = format!("{}{suffix}", stem(&f.rel));
            let dest = out.path_for(&rel)?;
            let args = encode_args(&f.path, &dest, key, spec.clip_seconds);
            let o = run_program(&program, &args)?;
            if !o.status.success() {
                let _ = std::fs::remove_file(&dest);
                let err = String::from_utf8_lossy(&o.stderr);
                let tail: String = err.lines().rev().take(5).collect::<Vec<_>>().join(" | ");
                bail!("ffmpeg failed for `{}` ({key}): {tail}", f.full());
            }
            let data =
                std::fs::read(&dest).with_context(|| format!("reading {}", dest.display()))?;
            out.record(
                &rel,
                data.len() as u64,
                blake3::hash(&data).to_hex().to_string(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::testutil::{put, read_all, source};
    use super::*;
    use crate::corpus::fetch::fake::FakeFetcher;
    use crate::corpus::registry::SourceSpec;

    fn src(codecs: &[&str], from: &[&str]) -> Source {
        let mut s = source(
            "video-encodes",
            "video",
            &["video"],
            SourceSpec::FfmpegEncode(FfmpegEncodeSpec {
                from: from.iter().map(|s| s.to_string()).collect(),
                clip_seconds: 1,
                codecs: codecs.iter().map(|s| s.to_string()).collect(),
            }),
        );
        s.optional = true;
        s
    }

    fn run_with(
        root: &Path,
        s: &Source,
        program: Option<&str>,
    ) -> (
        Result<Vec<crate::corpus::manifest::ManifestFile>>,
        Option<String>,
    ) {
        let fetcher = FakeFetcher::default();
        let mut ctx = Ctx::for_tests(&fetcher, root, false);
        ctx.derive.built = vec![("video".into(), "bbb".into())];
        ctx.derive.run_classes = ["video".to_string()].into();
        ctx.derive.ffmpeg = program.map(String::from);
        let dir = root.join("out/video/video-encodes");
        std::fs::create_dir_all(&dir).expect("dir");
        let r = super::super::build(&mut ctx, s, &dir);
        (r, ctx.take_tool("ffmpeg"))
    }

    #[test]
    fn arguments_are_discrete_single_threaded_and_bit_exact() {
        for codec in ["hevc", "av1"] {
            let args = encode_args(
                &PathBuf::from("in put with spaces.mp4"),
                &PathBuf::from("out dir/o.mp4"),
                codec,
                7,
            );
            let s: Vec<String> = args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert!(
                s.contains(&"in put with spaces.mp4".to_string()),
                "no splitting or quoting"
            );
            assert_eq!(s.last().map(String::as_str), Some("out dir/o.mp4"));
            let pos = |k: &str| {
                s.iter()
                    .position(|x| x == k)
                    .unwrap_or_else(|| panic!("{k}"))
            };
            assert_eq!(s[pos("-threads") + 1], "1");
            // Once as an input option and once as an output option (after -i), which is what
            // limits the encoder's threads.
            let threads: Vec<usize> = (0..s.len()).filter(|i| s[*i] == "-threads").collect();
            assert_eq!(threads.len(), 2);
            assert!(threads[0] < pos("-i") && threads[1] > pos("-i"));
            assert!(threads[1] < pos("-c:v"), "before the codec options");
            assert_eq!(s[threads[1] + 1], "1");
            assert_eq!(s[pos("-t") + 1], "7");
            assert_eq!(s[pos("-map_metadata") + 1], "-1");
            assert_eq!(s[pos("-fflags") + 1], "+bitexact");
            assert!(s.contains(&"-an".to_string()));
            assert!(s
                .iter()
                .all(|x| !x.contains(" && ") && !x.starts_with("sh")));
        }
    }

    #[test]
    fn a_missing_encoder_is_dropped_with_a_reason_and_versions_are_parsed() {
        let listing =
            " V....D libx265              libx265 H.265 / HEVC (codec hevc)\n V....D librav1e  x\n";
        let both = ["hevc".to_string(), "av1".to_string()];
        let (wanted, dropped) = select_codecs(&both, listing, "ffmpeg version 9");
        assert_eq!(wanted.len(), 1);
        assert_eq!(wanted[0].0, "hevc");
        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].contains("av1") && dropped[0].contains("libaom-av1"));

        let x265 = "x265 [info]: HEVC encoder version 4.2+3-3f4120d\nx265 [info]: build info";
        assert_eq!(
            parse_encoder_version("libx265", x265).as_deref(),
            Some("HEVC encoder version 4.2+3-3f4120d")
        );
        let aom =
            "[libaom-av1 @ 0000016c] 3.13.3-432-g8224199539\n[libaom-av1 @ 0000016c] cmake ../";
        assert_eq!(
            parse_encoder_version("libaom-av1", aom).as_deref(),
            Some("libaom 3.13.3-432-g8224199539")
        );
        assert!(parse_encoder_version("libaom-av1", "nothing").is_none());
    }

    #[test]
    fn a_missing_ffmpeg_is_a_skip_with_a_reason() {
        let d = tempfile::tempdir().expect("tmp");
        put(d.path(), "video", "bbb", "bunny.mp4", b"not a video");
        let (r, tool) = run_with(
            d.path(),
            &src(&["hevc", "av1"], &[]),
            Some("no-such-ffmpeg-program-xyz"),
        );
        let err = r.expect_err("must skip");
        let skip = err.downcast_ref::<Skip>().expect("a Skip");
        assert!(skip.0.contains("not installed") && skip.0.contains("no-such-ffmpeg-program-xyz"));
        assert!(tool.is_none());
    }

    #[test]
    fn no_video_input_is_a_skip() {
        let d = tempfile::tempdir().expect("tmp");
        put(d.path(), "video", "bbb", "readme.txt", b"x");
        let (r, _) = run_with(
            d.path(),
            &src(&["hevc"], &[]),
            Some("no-such-ffmpeg-program-xyz"),
        );
        assert!(r.expect_err("skip").downcast_ref::<Skip>().is_some());
    }

    /// With a real ffmpeg: encode a tiny generated clip twice and compare the bytes.
    #[test]
    fn real_ffmpeg_encodes_repeat_when_installed() {
        let probe = Command::new("ffmpeg").arg("-version").output();
        if probe.is_err() {
            eprintln!("ffmpeg not installed: skipping");
            return;
        }
        let mut results = Vec::new();
        for _ in 0..2 {
            let d = tempfile::tempdir().expect("tmp");
            let clip = d.path().join("out/video/bbb");
            std::fs::create_dir_all(&clip).expect("dir");
            let made = Command::new("ffmpeg")
                .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
                .arg("testsrc=size=64x64:rate=10:duration=1")
                .args(["-pix_fmt", "yuv420p"])
                .arg(clip.join("tiny.mp4"))
                .output()
                .expect("run ffmpeg");
            assert!(
                made.status.success(),
                "{}",
                String::from_utf8_lossy(&made.stderr)
            );
            let (r, tool) = run_with(d.path(), &src(&["hevc", "av1"], &["bbb"]), None);
            match r {
                Ok(files) => {
                    assert!(tool
                        .expect("version recorded")
                        .starts_with("ffmpeg version"));
                    assert!(!files.is_empty());
                    results.push(read_all(d.path(), "video", "video-encodes"));
                }
                Err(e) if e.downcast_ref::<Skip>().is_some() => {
                    eprintln!("no HEVC/AV1 encoder in this ffmpeg: skipping ({e:#})");
                    return;
                }
                Err(e) => panic!("{e:#}"),
            }
        }
        assert_eq!(results[0], results[1], "encodes must be byte-identical");
        assert!(results[0].iter().all(|f| !f.1.is_empty()));
    }
}
