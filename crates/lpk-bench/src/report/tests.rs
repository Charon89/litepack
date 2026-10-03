//! Tests of the report with small synthetic inputs (no corpus, no measurement).

use std::collections::BTreeMap;
use std::path::Path;

use regex::Regex;

use super::model::*;
use super::traced::Traced;
use super::*;
use crate::probe::codec::{XzSettings, ZstdSettings};
use crate::probe::{
    dedup, deflate, entropy_gate, jpeg, text, weights, BuildProfile, CorpusId, Envelope,
    FORMAT_VERSION,
};
use crate::run::result::samples;
use crate::run::result::{render as render_json, Sample, ToolResult};

const MB: u64 = 1_000_000;

// ---------------------------------------------------------------------------------------------
// Fixtures

/// A measured result of `tool/setting` on `class`: three equal repeats.
fn result(
    tool: &str,
    setting: &str,
    class: &str,
    cb: u64,
    archive: u64,
    cw: f64,
    ew: f64,
) -> ToolResult {
    let mut r = samples::measured();
    r.private = false;
    r.tool.id = tool.into();
    r.setting.id = setting.into();
    r.class = class.into();
    r.corpus.class_bytes = cb;
    let mut s = samples::sample(cw);
    s.extract = samples::measure(ew);
    s.archive_bytes = archive;
    let reps: Vec<Sample> = vec![s; 3];
    r.median = Some(Sample::median_of(&reps));
    r.repeats = Some(reps);
    r
}

/// (class, class bytes, [store, 7z/ultra, 7z/mx5, zstd/3] archive bytes).
const CLASSES: [(&str, u64, [u64; 4]); 7] = [
    (
        "photo-jpeg",
        10 * MB,
        [10_000_100, 9_000_000, 9_100_000, 9_500_000],
    ),
    (
        "photo-jpeg-edited",
        4 * MB,
        [4_000_100, 3_600_000, 3_700_000, 3_800_000],
    ),
    (
        "office-pdf",
        6 * MB,
        [6_000_100, 5_000_000, 5_200_000, 5_400_000],
    ),
    (
        "backup-versions",
        30 * MB,
        [30_000_100, 8_000_000, 9_000_000, 12_000_000],
    ),
    (
        "video",
        50 * MB,
        [50_000_100, 49_900_000, 49_950_000, 49_990_000],
    ),
    (
        "audio",
        5 * MB,
        [5_000_100, 4_900_000, 4_950_000, 4_990_000],
    ),
    (
        "encrypted-random",
        8 * MB,
        [8_000_100, 8_000_900, 8_000_800, 8_000_700],
    ),
];

/// Seconds of (compress, extract) of `store`, `7z/ultra`, `7z/mx5`, `zstd/3`.
fn walls(zstd_extract: f64, store_compress: f64) -> [(f64, f64); 4] {
    [
        (store_compress, 1.0),
        (10.0, 2.0),
        (5.0, 2.0),
        (1.0, zstd_extract),
    ]
}

fn baseline_results(zstd_extract: f64, store_compress: f64) -> Vec<ToolResult> {
    let tools = [
        ("store", "store"),
        ("7z", "ultra"),
        ("7z", "mx5"),
        ("zstd", "3"),
    ];
    let w = walls(zstd_extract, store_compress);
    let mut out = Vec::new();
    for (class, cb, sizes) in CLASSES {
        for (i, (t, s)) in tools.iter().enumerate() {
            out.push(result(t, s, class, cb, sizes[i], w[i].0, w[i].1));
        }
    }
    out
}

fn sample_env<D>(probe: &str, data: D) -> Envelope<D> {
    Envelope {
        probe: probe.into(),
        format_version: FORMAT_VERSION,
        corpus: CorpusId {
            profile: "small".into(),
            manifest_blake3: "ab".repeat(32),
            private: false,
        },
        build: "0123456789ab".into(),
        build_profile: BuildProfile {
            profile: "release".into(),
            opt_level: "3".into(),
            debug_assertions: false,
            allow_debug_build: false,
        },
        host: "testbox".into(),
        date: "2026-10-02T10:00:00Z".into(),
        threads: 2,
        library_threads: 1,
        libraries: BTreeMap::from([("libzstd".to_string(), "1.5.7".to_string())]),
        elapsed_seconds: 1.0,
        notes: vec![],
        data,
    }
}

fn probe_file<D: serde::Serialize>(name: &str, data: D, src: usize) -> ProbeFile<D> {
    let env = sample_env(name, data);
    ProbeFile {
        json: render_json(&env),
        env,
        src,
    }
}

fn scan() -> jpeg::Scan {
    jpeg::Scan {
        frame: None,
        restart_interval: false,
        mpf: false,
        gain_map_marker: false,
        adobe: false,
        scans: 1,
        eoi_found: true,
        trailing_bytes: 0,
        problem: None,
    }
}

/// A JPEG class of two files: the first recompressed to `lepton`, the second failed (stored).
fn jpeg_class(class: &str, bytes: u64, lepton: u64) -> jpeg::ClassData {
    let rec = |index: u32, bytes: u64, lepton: Option<u64>| jpeg::FileRecord {
        index,
        group: 0,
        path: None,
        bytes,
        scan: scan(),
        lepton_bytes: lepton,
        failure: None,
        default_threads: None,
        one_thread: None,
        one_thread_output_identical: None,
    };
    let first = bytes - bytes / 10;
    let second = bytes / 10;
    jpeg::ClassData {
        class: class.into(),
        manifest_files: 2,
        group_labels: vec![String::new()],
        not_jpeg: vec![],
        files: vec![rec(0, first, Some(lepton - second)), rec(1, second, None)],
    }
}

fn jpeg_data(photo_after: u64, edited_after: u64) -> jpeg::Data {
    jpeg::Data {
        features: jpeg::Features::write_preset(),
        one_thread_processor_threads: 1,
        one_thread_pool: "pool".into(),
        trailing_limit_bytes: 4 * MB,
        classes: vec![
            jpeg_class("photo-jpeg", 10 * MB, photo_after),
            jpeg_class("photo-jpeg-edited", 4 * MB, edited_after),
        ],
    }
}

fn deflate_data(office_b_plus_corr: u64) -> deflate::Data {
    let corr = 1_000;
    deflate::Data {
        settings: deflate::Settings {
            recognition: "by estimate".into(),
            max_chain_length: 4096,
            plain_text_limit: 1 << 28,
            file_plain_budget: 1 << 30,
            min_scanned_plain: 64,
            library_verification: false,
            zstd: ZstdSettings::level(19),
            xz: XzSettings::preset9(),
        },
        classes: vec![deflate::ClassRecord {
            class: "office-pdf".into(),
            manifest_files: 1,
            files: vec![deflate::FileRecord {
                index: 0,
                path: None,
                kind: "pdf".into(),
                bytes: 6 * MB,
                streams: deflate::StreamStats {
                    correction_bytes: corr,
                    ..Default::default()
                },
                a: deflate::Sizes {
                    zstd_bytes: 5_500_000,
                    xz_bytes: 5_000_000,
                },
                b_input_bytes: 9 * MB,
                b: deflate::Sizes {
                    zstd_bytes: 1,
                    xz_bytes: office_b_plus_corr - corr,
                },
            }],
        }],
    }
}

fn patch(bytes: Option<u64>) -> dedup::ToolRun {
    dedup::ToolRun {
        tool: "zstd --patch-from".into(),
        settings: "level 19".into(),
        skipped: bytes.is_none().then(|| "not run".to_string()),
        failed: None,
        patch_bytes: bytes,
        create_seconds: bytes.map(|_| 1.0),
        apply_seconds: bytes.map(|_| 1.0),
        verified: bytes.is_some(),
    }
}

/// Three versions of 10 MB; the new unique chunks compress to `3 MB`, `500 kB` and `last`; the
/// patches (when given) are the two numbers.
fn dedup_data(last_new: u64, patches: Option<(u64, u64)>) -> dedup::Data {
    let row = |class: &str| dedup::Row {
        class: class.into(),
        files: 3,
        bytes: 30 * MB,
        dup_files: 0,
        dup_bytes: 0,
        chunks: 10,
        unique_chunks: 9,
        unique_chunk_bytes: 25 * MB,
        cdc_seconds: 1.0,
        cdc_hash_seconds: 2.0,
    };
    let version = |label: &str, new19: u64, p: Option<Option<u64>>| dedup::Version {
        label: label.into(),
        files: 1,
        bytes: 10 * MB,
        cumulative_unique_chunks: 3,
        cumulative_unique_chunk_bytes: 10 * MB,
        new_unique_chunk_bytes: 5 * MB,
        new_unique_chunks_zstd19_bytes: new19,
        delta: p.map(|p| dedup::Delta {
            old_tar_bytes: 10 * MB,
            new_tar_bytes: 10 * MB,
            new_alone_zstd19_bytes: 4 * MB,
            new_alone_zstd19_long_bytes: 4 * MB,
            window_log: 27,
            window_covers_input: true,
            tools: vec![patch(p)],
        }),
    };
    let (p2, p3) = match patches {
        Some((a, b)) => (Some(a), Some(b)),
        None => (None, None),
    };
    dedup::Data {
        chunking: dedup::Chunking {
            algorithm: "fastcdc".into(),
            min_bytes: 4096,
            avg_bytes: 65536,
            max_bytes: 524_288,
            chunk_hash: "blake3".into(),
        },
        classes: vec![row("backup-versions")],
        corpus: row("(corpus)"),
        versions: vec![
            version("v1", 3 * MB, None),
            version("v2", 500_000, Some(p2)),
            version("v3", last_new, Some(p3)),
        ],
    }
}

/// A raw-read pass over the video class: `seconds` of reading, none of opening.
fn raw_pass(seconds: f64) -> entropy_gate::RawPass {
    entropy_gate::RawPass {
        files: 1,
        bytes: 50 * MB,
        seconds,
        open_seconds: 0.0,
        classes: vec![entropy_gate::RawClass {
            class: "video".into(),
            files: 1,
            buffered_files: 0,
            bytes: 50 * MB,
            seconds,
            open_seconds: 0.0,
        }],
    }
}

fn entropy_data(passes: Vec<entropy_gate::RawPass>) -> entropy_gate::Data {
    entropy_gate::Data {
        block_bytes: 1 << 20,
        min_tail_bytes: 1,
        sample_bytes: 1,
        sample_period_bytes: 1,
        entropy_thresholds: vec![],
        zstd_percents: vec![],
        truth_percents: vec![],
        zstd: ZstdSettings::level(1),
        xz: XzSettings::preset9(),
        classes: vec![],
        gate_cost: vec![entropy_gate::GateCost {
            gate: "entropy".into(),
            blocks: 10,
            bytes: 10 * MB,
            seconds: 0.5,
        }],
        raw_read: entropy_gate::RawRead {
            mode: "unbuffered".into(),
            cache_bypassed: true,
            buffer_bytes: 1 << 20,
            alignment: 4096,
            fallback_note: None,
            passes,
        },
    }
}

fn text_data() -> text::Data {
    let run = |c: &str, bytes: u64, verified: bool| text::Run {
        compressor: c.into(),
        setting: "-9".into(),
        timing: "in-process".into(),
        measured: Some(text::Measured {
            compressed_bytes: bytes,
            compress_seconds: 1.0,
            decompress_seconds: 1.0,
            verified,
            compress_process: None,
            decompress_process: None,
        }),
        skipped: None,
        failed: None,
    };
    text::Data {
        classes: vec![text::ClassRun {
            class: "text-prose".into(),
            present: true,
            files: 2,
            content_bytes: 2 * MB,
            tar_bytes: 2_100_000,
            runs: vec![
                run("xz", 500_000, true),
                run("zstd", 600_000, true),
                run("kanzi", 100, false),
            ],
        }],
        skipped: vec![],
    }
}

fn weights_data(files: Vec<weights::FileRecord>) -> weights::Data {
    weights::Data {
        class: "model-weights".into(),
        manifest_files: files.len() as u32,
        zstd: ZstdSettings::level(19),
        files,
        not_parsed: vec![],
    }
}

fn weights_file() -> weights::FileRecord {
    let planes = |total: u64| weights::Planes {
        compressed_bytes: total,
        plane_compressed_bytes: vec![total / 2, total - total / 2],
        compress_seconds: 1.0,
        decompress_seconds: 1.0,
    };
    weights::FileRecord {
        index: 0,
        path: None,
        bytes: 1000,
        header_bytes: 100,
        tensors: 1,
        whole_file_compressed_bytes: 800,
        other_tensors: 0,
        other_bytes: 0,
        dtypes: vec![weights::DtypeRecord {
            dtype: "BF16".into(),
            element_bytes: 2,
            tensors: 1,
            original_bytes: 900,
            plain: weights::Whole {
                compressed_bytes: 700,
                compress_seconds: 1.0,
                decompress_seconds: 1.0,
            },
            byte_planes: planes(600),
            rotated_planes: planes(500),
        }],
    }
}

const MIXES: &str = "[[mix]]\nname = \"docs\"\nweights = { photo-jpeg = 50, office-pdf = 50 }\n\
                     [[mix]]\nname = \"media\"\nweights = { video = 60, audio = 40 }\n\
                     [[mix]]\nname = \"absent\"\nweights = { model-weights = 100 }\n";

struct Knobs {
    zstd_extract: f64,
    photo_after: u64,
    edited_after: u64,
    office: u64,
    last_new: u64,
    patches: Option<(u64, u64)>,
    raw_seconds: f64,
    store_compress: f64,
}

impl Default for Knobs {
    fn default() -> Self {
        Knobs {
            zstd_extract: 0.5,
            photo_after: 7 * MB,
            edited_after: 2_800_000,
            office: 3_800_000,
            last_new: 500_000,
            patches: None,
            raw_seconds: 1.0,
            store_compress: 1.0,
        }
    }
}

/// Inputs with all probes (or none). Source ids: 1-3 baseline files, then the 24 results, then
/// the probes, then the mixes file.
fn inputs(k: &Knobs, with_probes: bool) -> Inputs {
    let results = baseline_results(k.zstd_extract, k.store_compress);
    let mut sources: Vec<String> = vec![
        "d/host.json".into(),
        "d/tools.json".into(),
        "d/run.json".into(),
    ];
    let mut res = Vec::new();
    for r in results.iter().cloned() {
        sources.push(format!("d/{}", r.file_name()));
        res.push((r, sources.len()));
    }
    let mut add = |name: &str| {
        sources.push(format!("d/probe-{name}.json"));
        sources.len()
    };
    let probes = if with_probes {
        Probes {
            jpeg: Some(probe_file(
                "jpeg",
                jpeg_data(k.photo_after, k.edited_after),
                add("jpeg"),
            )),
            deflate: Some(probe_file(
                "deflate",
                deflate_data(k.office),
                add("deflate"),
            )),
            dedup: Some(probe_file(
                "dedup",
                dedup_data(k.last_new, k.patches),
                add("dedup"),
            )),
            text: Some(probe_file("text", text_data(), add("text"))),
            weights: Some(probe_file("weights", weights_data(vec![]), add("weights"))),
            entropy_gate: Some(probe_file(
                "entropy-gate",
                entropy_data(vec![raw_pass(k.raw_seconds); 3]),
                add("entropy-gate"),
            )),
        }
    } else {
        Probes::default()
    };
    sources.push("bench/report-mixes.toml".into());
    let mixes_src = sources.len();
    let run = samples::run_file(&results);
    let mut host = samples::host();
    host.dirty_build_allowed = false;
    Inputs {
        baseline: Baseline {
            dir: "2026-10-02-testbox".into(),
            host,
            host_src: 1,
            tools: samples::tools(),
            tools_src: 2,
            run,
            run_src: 3,
            results: res,
        },
        probes,
        mixes: mixes::Mixes::parse(MIXES).expect("mixes"),
        mixes_src,
        mixes_text: MIXES.to_string(),
        sources,
        outside: vec![],
        mixes_outside: false,
    }
}

fn gate(inputs: &Inputs, id: &str) -> GateRow {
    let m = Model::build(inputs);
    gates(&m, inputs)
        .into_iter()
        .find(|g| g.id == id)
        .expect("gate")
}

fn verdict_of(inputs: &Inputs, id: &str) -> Verdict {
    gate(inputs, id).verdict
}

// ---------------------------------------------------------------------------------------------
// Baseline tables

#[test]
fn per_class_figures_are_the_hand_computed_ones() {
    let i = inputs(&Knobs::default(), true);
    let text = report_text(&i);
    let jpeg_section = text
        .split("### photo-jpeg:")
        .nth(1)
        .and_then(|t| t.split("###").next())
        .expect("section");
    // 7z/ultra on photo-jpeg: 9 MB of 10 MB; 10 MB in 10 s compress and 2 s extract; 1 MiB peak.
    let line = jpeg_section
        .lines()
        .find(|l| l.starts_with("| 7z/ultra"))
        .expect("row");
    let cells: Vec<&str> = line.split(" | ").collect();
    assert!(cells[1].starts_with("90.0% ["), "{line}");
    assert!(cells[2].starts_with("1.0 ["), "{line}");
    assert!(cells[3].starts_with("5.0 ["), "{line}");
    assert!(cells[4].starts_with("1.0 ["), "{line}");
    assert!(cells[5].starts_with("3 ["), "{line}");
}

#[test]
fn blended_figures_are_the_hand_computed_ones() {
    let i = inputs(&Knobs::default(), false);
    let rows = rows(&i.baseline);
    let mix = &i.mixes.mix[1]; // media: video 60, audio 40
                               // zstd/3: video 49.99 MB of 50 MB, audio 4.99 of 5; compress 1 s each; extract 0.5 s.
    let b = blend(&rows, ("zstd", "3"), mix, i.mixes_src).expect("blend");
    let expect_size =
        (60.0 * (49_990_000.0 / 50e6 * 100.0) + 40.0 * (4_990_000.0 / 5e6 * 100.0)) / 100.0;
    assert!((b.size_pct.value - expect_size).abs() < 1e-9);
    // speeds: video 50 MB/s, audio 5 MB/s: 100 / (60/50 + 40/5) = 100 / 9.2
    assert!((b.compress_mbps.value - 100.0 / (60.0 / 50.0 + 40.0 / 5.0)).abs() < 1e-9);
    assert!((b.extract_mbps.value - 100.0 / (60.0 / 100.0 + 40.0 / 10.0)).abs() < 1e-9);
    assert!(
        b.size_pct.sources.contains(&i.mixes_src),
        "the weights' source is listed"
    );
    // The mix whose class is absent from the corpus has no blend.
    let absent = &i.mixes.mix[2];
    let cb = class_bytes(&rows);
    assert_eq!(
        missing_classes(absent, &cb),
        vec!["model-weights".to_string()]
    );
    assert!(blend(&rows, ("zstd", "3"), absent, i.mixes_src).is_err());
    assert!(report_text(&i).contains("n/a: the corpus has no class model-weights"));
}

#[test]
fn failed_skipped_and_single_measurement_rows_are_marked() {
    let mut i = inputs(&Knobs::default(), false);
    let (fail, src) = {
        let (r, s) = &i.baseline.results[1];
        let mut f = samples::failed();
        f.tool = r.tool.clone();
        f.setting = r.setting.clone();
        f.class = r.class.clone();
        f.corpus = r.corpus.clone();
        (f, *s)
    };
    i.baseline.results[1] = (fail, src);
    let (r, _) = &mut i.baseline.results[2];
    r.repeats = r.repeats.take().map(|v| v[..1].to_vec());
    r.repeats_short = Some("measured once: long run".into());
    let mut skipped = samples::skipped();
    skipped.class = "photo-jpeg".into();
    skipped.setting.id = "best".into();
    let n = i.sources.len();
    i.baseline.results.push((skipped, n));
    i.baseline.run = samples::run_file(
        &i.baseline
            .results
            .iter()
            .map(|(r, _)| r.clone())
            .collect::<Vec<_>>(),
    );
    let text = report_text(&i);
    assert!(
        text.contains("FAILED (extract): extract: exit code 2 ["),
        "{text}"
    );
    assert!(text.contains("skipped: not installed ["));
    assert!(text.contains("| 7z/mx5 * |"), "one measurement is marked");
    assert!(
        text.contains("measured once: long run"),
        "the reason is in the caveats"
    );
    assert!(text.contains("combinations that failed: 7z/ultra on photo-jpeg"));
}

// ---------------------------------------------------------------------------------------------
// Estimates

#[test]
fn estimates_follow_the_rules() {
    let i = inputs(&Knobs::default(), true);
    let m = Model::build(&i);
    let est = |c: &str| {
        m.class(c)
            .and_then(|v| v.est.as_ref().ok())
            .map(|e| e.bytes.value)
    };
    // Lepton: the first file (9 MB) becomes 6.6 MB (7 MB minus the stored 0.4 MB tenth), the stored
    // tenth stays: the figure is the class's "after fallback".
    assert_eq!(est("photo-jpeg"), Some(7_000_000.0));
    assert_eq!(est("photo-jpeg-edited"), Some(2_800_000.0));
    // Deflate: B with xz plus the corrections.
    assert_eq!(est("office-pdf"), Some(3_800_000.0));
    // Backup: (a) 3.0 + 0.5 + 0.5 MB; (b) unavailable without patches.
    assert_eq!(est("backup-versions"), Some(4_000_000.0));
    // Stored.
    assert_eq!(est("video"), Some(50_000_000.0));
    // No rule: the best incumbent.
    assert_eq!(est("audio"), Some(4_900_000.0));
    // Text-prose is not in this corpus fixture, so nothing is derived for it.
    assert!(m.class("text-prose").is_none());
}

#[test]
fn estimates_for_text_and_weights_and_backup_patches() {
    let i = inputs(&Knobs::default(), true);
    let text = text_est(&i.probes, "text-prose", &Traced::new(2_000_000.0, 1)).expect("text");
    assert_eq!(text.bytes.value, 500_000.0, "the unverified row is ignored");
    assert!(text.basis.contains("xz -9"));
    let cb = Traced::new(1000.0, 1);
    // No parsed file: not available.
    assert!(weights_est(&i.probes, "model-weights", &cb).is_err());
    let p = Probes {
        weights: Some(probe_file("weights", weights_data(vec![weights_file()]), 9)),
        ..Probes::default()
    };
    let w = weights_est(&p, "model-weights", &cb).expect("weights");
    // header 100 + the smaller plane total 500 (rotated)
    assert_eq!(w.bytes.value, 600.0);
    assert!(w.basis.contains("rotated planes"));
    // Backup with patches: (b) = 3.0 MB + 100 kB + 100 kB beats (a).
    let k = Knobs {
        patches: Some((100_000, 100_000)),
        ..Knobs::default()
    };
    let i = inputs(&k, true);
    let m = Model::build(&i);
    let e = m
        .class("backup-versions")
        .and_then(|v| v.est.as_ref().ok())
        .expect("est");
    assert_eq!(e.bytes.value, 3_200_000.0);
    assert!(e.basis.contains("(b)"));
}

#[test]
fn a_missing_probe_makes_estimates_not_available_and_gates_not_evaluable() {
    let i = inputs(&Knobs::default(), false);
    let m = Model::build(&i);
    for c in ["photo-jpeg", "office-pdf", "backup-versions"] {
        assert!(m.class(c).is_some_and(|v| v.est.is_err()), "{c}");
    }
    // Classes that need no probe keep their estimate.
    assert!(m.class("video").is_some_and(|v| v.est.is_ok()));
    let g = gates(&m, &i);
    for id in ["G1", "G2", "G3"] {
        let row = g.iter().find(|r| r.id == id).expect("gate");
        assert!(
            matches!(row.verdict, Verdict::NotEvaluable(_)),
            "{id}: {:?}",
            row.verdict
        );
    }
    let line = verdict_line(&g);
    assert!(line.starts_with("NO-GO proposal"), "{line}");
    assert!(line.contains("not evaluable"), "{line}");
    let text = report_text(&i);
    assert!(text.contains("not available: probe jpeg is not available"));
    assert!(
        text.contains("probes not available: jpeg, deflate, dedup, text, weights, entropy-gate")
    );
    assert!(
        !text.contains("| PASS |"),
        "no gate may pass on missing data"
    );
}

// ---------------------------------------------------------------------------------------------
// Gates, both sides of every threshold

#[test]
fn threshold_helpers_are_inclusive_and_exact() {
    assert!(within(85.0, 100.0, 85.0));
    assert!(!within(85.000001, 100.0, 85.0));
    assert!(at_least_pct(80.0, 100.0, 80.0));
    assert!(!at_least_pct(79.999999, 100.0, 80.0));
}

#[test]
fn g1_passes_and_fails_around_both_limits() {
    // Best incumbents per class are the 7z/ultra rows: 9.0 + 3.6 + 5.0 = 17.6 MB.
    // 85% of 17.6 MB is 14.96 MB, the binding limit (90% is 15.84 MB).
    let at = |office: u64| {
        let k = Knobs {
            office,
            ..Knobs::default()
        };
        verdict_of(&inputs(&k, true), "G1")
    };
    // photo 7.0 + edited 2.8 + office: the boundary office figure is 14.96 - 9.8 = 5.16 MB.
    assert_eq!(at(5_160_000), Verdict::Pass);
    assert_eq!(at(5_160_001), Verdict::Fail);
    assert_eq!(at(3_800_000), Verdict::Pass);
    // The 90% limit alone: make 7-Zip Ultra larger than the best incumbent is impossible (it is
    // the best), so the check is that both numbers are printed with sources.
    let g = gate(&inputs(&Knobs::default(), true), "G1");
    assert!(g
        .numbers
        .iter()
        .any(|n| n.starts_with("combined estimate / best single tool: ")));
    assert!(g
        .numbers
        .iter()
        .any(|n| n.starts_with("combined estimate / 7-Zip Ultra: ")));
    assert!(g.numbers.iter().any(|n| n.starts_with("photo-jpeg alone")));
    assert!(g
        .title
        .contains("WinZip and PowerArchiver were not measured"));
}

#[test]
fn g1_uses_the_best_incumbent_when_it_is_not_7_zip_ultra() {
    // Make mx5 the best on every G1 class by shrinking it below ultra: the 90% limit binds then.
    let mut i = inputs(&Knobs::default(), true);
    for (r, _) in &mut i.baseline.results {
        if r.tool.id == "7z"
            && r.setting.id == "mx5"
            && ["photo-jpeg", "photo-jpeg-edited", "office-pdf"].contains(&r.class.as_str())
        {
            let mut m = r.median.expect("median");
            m.archive_bytes = m.archive_bytes * 8 / 10; // best = 80% of ultra
            r.median = Some(m);
        }
    }
    // best sum = 0.8 * (9.1 + 3.7 + 5.2) = 14.4 MB; 90% of it is 12.96 MB; estimate 13.6 MB fails
    // the 90% rule although it passes 85% of 7-Zip Ultra's 17.6 MB (14.96 MB).
    assert_eq!(verdict_of(&i, "G1"), Verdict::Fail);
}

#[test]
fn g2_passes_and_fails_around_half_of_the_best_incumbent() {
    // Best incumbent 8.0 MB; the estimate (a) is 3.0 + 0.5 + last.
    let at = |last_new: u64| {
        let k = Knobs {
            last_new,
            ..Knobs::default()
        };
        verdict_of(&inputs(&k, true), "G2")
    };
    assert_eq!(at(500_000), Verdict::Pass, "exactly half");
    assert_eq!(at(500_001), Verdict::Fail);
}

#[test]
fn g3_compares_the_store_tool_with_the_raw_read_rate() {
    // store compress on video: 50 MB in 1 s = 50 MB/s.
    let at = |raw_seconds: f64| {
        let k = Knobs {
            raw_seconds,
            ..Knobs::default()
        };
        verdict_of(&inputs(&k, true), "G3")
    };
    assert_eq!(at(1.0), Verdict::Pass, "100% of the raw rate");
    assert_eq!(at(0.5), Verdict::Fail, "50% of the raw rate");
    // The gate costs are printed but do not decide.
    let g = gate(&inputs(&Knobs::default(), true), "G3");
    assert!(g
        .numbers
        .iter()
        .any(|n| n.starts_with("gate cost, entropy")));
}

#[test]
fn the_raw_rate_is_the_median_of_the_passes_including_opens() {
    let mut p = Probes::default();
    let pass = |s: f64, o: f64| {
        let mut x = raw_pass(s);
        x.open_seconds = o;
        x.classes[0].open_seconds = o;
        x
    };
    // 50 MB in 0.5 + 0.5, 1.5 + 0.5 and 0.25 + 0.25 seconds: 50, 25 and 100 MB/s.
    p.entropy_gate = Some(probe_file(
        "entropy-gate",
        entropy_data(vec![pass(0.5, 0.5), pass(1.5, 0.5), pass(0.25, 0.25)]),
        4,
    ));
    let r = raw_video_rate(&p, "video").expect("rate");
    assert_eq!(r.value, 50.0);
    assert!(r.sources.contains(&4));
    assert!(raw_video_rate(&p, "audio").is_none());
}

#[test]
fn g4_compares_zstd_3_with_7z_mx5_on_every_mix() {
    // zstd/3 extracts in 0.5 s, 7z/mx5 in 2 s, on every class: faster on every mix. The mix with
    // a class missing from the corpus is not evaluable, which does not hide a pass elsewhere.
    let pass = verdict_of(&inputs(&Knobs::default(), true), "G4");
    assert!(
        matches!(pass, Verdict::NotEvaluable(_)),
        "{pass:?} (the absent mix)"
    );
    let mut i = inputs(&Knobs::default(), true);
    i.mixes = mixes::Mixes::parse(
        MIXES
            .split("[[mix]]\nname = \"absent\"")
            .next()
            .unwrap_or(""),
    )
    .expect("mixes");
    assert_eq!(verdict_of(&i, "G4"), Verdict::Pass);
    // Slower than 7z/mx5 (3 s against 2 s): FAIL.
    let mut j = inputs(
        &Knobs {
            zstd_extract: 3.0,
            ..Knobs::default()
        },
        true,
    );
    j.mixes = i.mixes.clone();
    assert_eq!(verdict_of(&j, "G4"), Verdict::Fail);
    // Equal speed passes (at least as fast).
    let mut k = inputs(
        &Knobs {
            zstd_extract: 2.0,
            ..Knobs::default()
        },
        true,
    );
    k.mixes = i.mixes.clone();
    assert_eq!(verdict_of(&k, "G4"), Verdict::Pass);
}

#[test]
fn the_verdict_line_names_failures_and_goes_only_when_all_pass() {
    let row = |id: &'static str, v: Verdict| GateRow {
        id,
        title: String::new(),
        numbers: vec![],
        notes: vec![],
        verdict: v,
    };
    let all = [row("G1", Verdict::Pass), row("G2", Verdict::Pass)];
    assert_eq!(verdict_line(&all), "Verdict proposal: GO");
    let some = [
        row("G1", Verdict::Pass),
        row("G2", Verdict::Fail),
        row("G3", Verdict::NotEvaluable("x".into())),
    ];
    let line = verdict_line(&some);
    assert!(
        line.starts_with("NO-GO proposal: failing gates: G2"),
        "{line}"
    );
    assert!(
        line.contains("not evaluable") && line.contains("G3"),
        "{line}"
    );
}

// ---------------------------------------------------------------------------------------------
// Traceability

/// Every number inside a table cell of sections 2, 3, 5 and 6 is followed by a source bracket.
#[test]
fn every_number_in_a_table_cell_has_a_source_bracket() {
    let i = inputs(
        &Knobs {
            patches: Some((100_000, 100_000)),
            ..Knobs::default()
        },
        true,
    );
    let text = report_text(&i);
    // A value segment is `[label: ]value[ unit] [ids]`: nothing but one value and its source.
    let value = Regex::new(
        r"^(?:[^\[\]]*: )?-?[\d.]+(?:% of class|%| bytes| MB/s| s)? \[\d+(?:[,-]\d+)*\]$",
    )
    .expect("regex");
    let bracket = Regex::new(r"\[\d+(?:[,-]\d+)*\]$").expect("regex");
    let digit = Regex::new(r"\d").expect("regex");
    let number = Regex::new(r"\d+(?:\.\d+)?").expect("regex");
    let tools = Regex::new(r"(?:\b(?:7z|zstd|zpaqfranz|rar|xz|store|tsaur)/[\w-]+|7-Zip|zstd1)")
        .expect("regex");
    let tags = Regex::new(r"\[[\d,-]+\]").expect("regex");
    // Label columns (names, rules, results) carry no measured value.
    let label = [
        "class",
        "tool/setting",
        "best measured incumbent",
        "best measured incumbent (single tool x setting)",
        "gate",
        "result",
        "mix",
    ];
    let mut checked = 0usize;
    for (from, to) in [("## 2.", "## 4."), ("## 5.", "## 7.")] {
        let part = text
            .split(from)
            .nth(1)
            .and_then(|t| t.split(to).next())
            .expect("section");
        let lines: Vec<&str> = part.lines().collect();
        let mut head: Vec<String> = Vec::new();
        for (n, line) in lines.iter().enumerate() {
            if !line.starts_with('|') || line.starts_with("|---") {
                continue;
            }
            let cells: Vec<String> = line
                .trim_start_matches("| ")
                .trim_end_matches(" |")
                .split(" | ")
                .map(String::from)
                .collect();
            if lines.get(n + 1).is_some_and(|l| l.starts_with("|---")) {
                head = cells;
                continue;
            }
            for (c, cell) in cells.iter().enumerate() {
                if label.contains(&head.get(c).map_or("", String::as_str)) {
                    continue;
                }
                for seg in cell.split("<br>") {
                    let outside = tags.replace_all(seg, "");
                    if !digit.is_match(&outside) && seg.contains('[') {
                        assert!(bracket.is_match(seg), "no source in `{seg}`");
                        checked += 1;
                    } else if digit.is_match(&outside) {
                        let free_text = ["FAILED", "skipped:", "n/a:"]
                            .iter()
                            .any(|p| seg.starts_with(p));
                        // Exactly one numeric token outside the brackets once tool names
                        // (`7z/mx5`, `zstd/3`, `zstd1`, `7-Zip`) are removed.
                        let bare = tools.replace_all(&outside, "");
                        let numbers = number.find_iter(&bare).count();
                        let ok = if free_text {
                            bracket.is_match(seg)
                        } else {
                            numbers == 1 && value.is_match(seg)
                        };
                        assert!(ok, "not `value [source]`: `{seg}` (row: {line})");
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked > 100, "only {checked} numbers were checked");
    // Every id used is in the list.
    let ids = Regex::new(r"\[(\d+)(?:[,-](\d+))*\]").expect("regex");
    for c in ids.captures_iter(&text) {
        let id: usize = c[1].parse().expect("id");
        assert!(id >= 1 && id <= i.sources.len(), "id {id} has no source");
    }
    assert!(text.contains(&format!("{}. `d/7z-ultra-photo-jpeg.json`", 5)));
}

#[test]
fn derived_numbers_carry_every_source_they_used() {
    let i = inputs(&Knobs::default(), true);
    let m = Model::build(&i);
    let v = m.class("photo-jpeg").expect("class");
    let best = v.best.as_ref().expect("best");
    // The best incumbent was chosen among the four tools' files of the class.
    assert_eq!(best.bytes.sources.len(), 4);
    let est = v.est.as_ref().expect("est");
    assert!(
        est.bytes.sources.len() >= 2,
        "probe and baseline class bytes"
    );
    let blended = m
        .mixes
        .iter()
        .find(|x| x.name == "docs")
        .and_then(|x| x.est.as_ref().ok())
        .expect("mix");
    assert!(blended.sources.contains(&i.mixes_src));
}

#[test]
fn estimates_are_labelled_and_the_report_has_all_sections_in_order() {
    let i = inputs(&Knobs::default(), true);
    let text = report_text(&i);
    let mut at = 0usize;
    for h in [
        "## 1. Header",
        "## 2. Baseline, per class",
        "## 3. Baseline, blended",
        "## 4. Probes",
        "## 5. Estimates",
        "## 6. Gates (D-07)",
        "## 7. Sources",
        "## 8. Caveats",
    ] {
        let p = text.find(h).unwrap_or_else(|| panic!("missing {h}"));
        assert!(p >= at, "{h} out of order");
        at = p;
    }
    assert!(text.contains("LitePack Balanced, estimate"));
    assert!(text.contains("Verdict proposal") || text.contains("NO-GO proposal"));
    assert!(text.contains("assumes nothing about the container"));
    // Probe tables come from the probes' own renderer.
    assert!(text.contains("#### Probe `entropy-gate`"));
}

// ---------------------------------------------------------------------------------------------
// Loading and validation

fn write_baseline(root: &Path, results: &[ToolResult]) -> std::path::PathBuf {
    let dir = root.join("2026-10-01-testbox");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("host.json"), render_json(&samples::host())).expect("host");
    std::fs::write(dir.join("tools.json"), render_json(&samples::tools())).expect("tools");
    std::fs::write(
        dir.join("run.json"),
        render_json(&samples::run_file(results)),
    )
    .expect("run");
    for r in results {
        std::fs::write(dir.join(r.file_name()), render_json(r)).expect("result");
    }
    dir
}

fn mixes_file(root: &Path) -> std::path::PathBuf {
    let p = root.join("mixes.toml");
    std::fs::write(&p, "[[mix]]\nname = \"only\"\nweights = { text = 100 }\n").expect("mixes");
    p
}

/// `text` is not a corpus class, so the committed class list refuses it: use a real class name.
fn real_class_results() -> Vec<ToolResult> {
    let mut a = samples::measured();
    a.class = "audio".into();
    let mut b = samples::tar_stream();
    b.class = "audio".into();
    let mut c = samples::skipped();
    c.class = "audio".into();
    vec![a, b, c]
}

#[test]
fn a_valid_directory_loads_and_the_command_writes_the_report() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dir = write_baseline(tmp.path(), &real_class_results());
    let mixes = tmp.path().join("m.toml");
    std::fs::write(
        &mixes,
        "[[mix]]\nname = \"only\"\nweights = { audio = 100 }\n",
    )
    .expect("mixes");
    let out = tmp.path().join("out").join("r.md");
    let args = ReportArgs {
        results: dir.clone(),
        probes: vec![],
        mixes,
        out: Some(out.clone()),
        allow_unclean: true,
    };
    assert_eq!(command(&args), ExitCode::SUCCESS);
    let text = std::fs::read_to_string(&out).expect("report");
    assert!(text.contains("## 6. Gates (D-07)"));
    assert!(text.contains("/2026-10-01-testbox/7z-mx5-audio.json`"));
    assert!(
        text.contains("`m.toml`"),
        "the mixes file is a source, by file name only"
    );
    assert!(
        !text.contains(&tmp.path().to_string_lossy().to_string()),
        "no absolute path in the report"
    );
    // The sample host's build is not clean: refused without the flag.
    let strict = ReportArgs {
        allow_unclean: false,
        out: None,
        ..args
    };
    let err = load(&strict).expect_err("unclean refused");
    assert!(format!("{err:#}").contains("--allow-unclean"), "{err:#}");
}

#[test]
fn a_directory_that_does_not_validate_is_refused() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dir = write_baseline(tmp.path(), &real_class_results());
    let mixes = mixes_file(tmp.path());
    let args = |d: &Path| ReportArgs {
        results: d.to_path_buf(),
        probes: vec![],
        mixes: mixes.clone(),
        out: Some(tmp.path().join("r.md")),
        allow_unclean: true,
    };
    std::fs::remove_file(dir.join("run.json")).expect("remove");
    let err = format!("{:#}", load(&args(&dir)).expect_err("no run.json"));
    assert!(err.contains("does not validate"), "{err}");
    // The parent directory is not a results directory.
    let err = format!("{:#}", load(&args(tmp.path())).expect_err("parent"));
    assert!(err.contains("no host.json"), "{err}");
    assert_eq!(command(&args(&dir)), ExitCode::FAILURE);
    assert!(
        !tmp.path().join("r.md").exists(),
        "nothing is written on refusal"
    );
}

/// A real probe directory (the weights probe on a tiny corpus) and baselines for it.
#[test]
fn probe_files_load_through_the_validator_and_must_match_host_and_corpus() {
    use crate::corpus::manifest::{Manifest, ManifestFile};
    let tmp = tempfile::tempdir().expect("tmp");
    let data =
        weights::build_safetensors(&[("a", "F32", vec![64, 16], weights::sample_floats(1024, 4))]);
    let corpus = tmp.path().join("corpus");
    std::fs::create_dir_all(corpus.join("model-weights")).expect("mkdir");
    std::fs::write(corpus.join("model-weights").join("m.safetensors"), &data).expect("write");
    let file = ManifestFile {
        blake3: blake3::hash(&data).to_hex().to_string(),
        bytes: data.len() as u64,
        licence: "CC0-1.0".into(),
        path: "model-weights/m.safetensors".into(),
        source: "test".into(),
    };
    let manifest = Manifest::with_profile_name("small", vec![("model-weights".to_string(), file)]);
    std::fs::write(corpus.join("manifest.json"), manifest.render()).expect("manifest");
    let cfg = crate::probe::Config {
        probes: vec!["weights".into()],
        corpus,
        into: None,
        results_root: tmp.path().join("probe-results"),
        tmp_root: tmp.path().join("probe-tmp"),
        threads: 2,
        allow_dirty: true,
        allow_debug_build: true,
        local_tools: std::path::PathBuf::from("none.toml"),
        tool_timeout: std::time::Duration::from_secs(60),
    };
    let probe_dir = crate::probe::execute(&cfg).expect("probe").results_dir;
    let hash = {
        let text = std::fs::read_to_string(probe_dir.join("probe-weights.json")).expect("json");
        let v: serde_json::Value = serde_json::from_str(&text).expect("value");
        v["corpus"]["manifest_blake3"]
            .as_str()
            .expect("hash")
            .to_string()
    };
    // A baseline of the same machine and corpus (the `audio` results only matter as inputs).
    let baseline = |name: &str, host_name: &str, manifest: &str| {
        let mut results = real_class_results();
        for r in &mut results {
            r.corpus.manifest_blake3 = manifest.to_string();
        }
        let mut host = crate::run::host::collect(true);
        host.host = host_name.to_string();
        let dir = tmp.path().join(name).join(format!(
            "2026-10-01-{}",
            crate::run::host::sanitize_host(host_name)
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("host.json"), render_json(&host)).expect("host");
        std::fs::write(dir.join("tools.json"), render_json(&samples::tools())).expect("tools");
        std::fs::write(
            dir.join("run.json"),
            render_json(&samples::run_file(&results)),
        )
        .expect("run");
        for r in &results {
            std::fs::write(dir.join(r.file_name()), render_json(r)).expect("result");
        }
        dir
    };
    let machine = crate::run::host::collect(true).host;
    let mixes = tmp.path().join("m.toml");
    std::fs::write(
        &mixes,
        "[[mix]]\nname = \"only\"\nweights = { audio = 100 }\n",
    )
    .expect("mixes");
    let args = |results: std::path::PathBuf, allow: bool| ReportArgs {
        results,
        probes: vec![probe_dir.clone()],
        mixes: mixes.clone(),
        out: None,
        allow_unclean: allow,
    };
    let good = baseline("good", &machine, &hash);
    let loaded = load(&args(good.clone(), true)).expect("loads");
    assert!(loaded.probes.weights.is_some());
    assert!(loaded
        .sources
        .iter()
        .any(|s| s.ends_with("/probe-weights.json")));
    let text = report_text(&loaded);
    assert!(
        text.contains("#### Probe `weights`"),
        "the probe's own table is embedded"
    );
    assert!(text.contains("probes not available: jpeg, deflate, dedup, text, entropy-gate"));
    // A debug build of the probe is refused without the flag.
    let err = format!("{:#}", load(&args(good, false)).expect_err("debug refused"));
    assert!(err.contains("--allow-unclean"), "{err}");
    // The same probe in two directories is refused and both directories are named.
    let mut twice = args(baseline("twice", &machine, &hash), true);
    twice.probes.push(probe_dir.clone());
    let err = format!("{:#}", load(&twice).expect_err("twice"));
    assert!(
        err.contains("given twice") && err.contains(" and in "),
        "{err}"
    );
    // Another corpus, another machine.
    let other_corpus = baseline("corpus", &machine, &"cd".repeat(32));
    let err = format!("{:#}", load(&args(other_corpus, true)).expect_err("corpus"));
    assert!(err.contains("another corpus"), "{err}");
    let other_host = baseline("host", "somebody-elses-box", &hash);
    let err = format!("{:#}", load(&args(other_host, true)).expect_err("host"));
    assert!(err.contains("one machine"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// The review round

fn set_row(
    i: &mut Inputs,
    tool: &str,
    setting: &str,
    class: &str,
    f: impl FnOnce(&mut ToolResult),
) {
    let (r, _) = i
        .baseline
        .results
        .iter_mut()
        .find(|(r, _)| r.tool.id == tool && r.setting.id == setting && r.class == class)
        .expect("row");
    f(r);
}

fn one_repeat(r: &mut ToolResult) {
    r.repeats = r.repeats.take().map(|v| v[..1].to_vec());
    r.repeats_short = Some("measured once: long run".into());
}

#[test]
fn settle_zero_and_directories_outside_bench_results_are_refused_unless_allowed() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut results = real_class_results();
    let dir = write_baseline(tmp.path(), &results);
    let mixes = tmp.path().join("m.toml");
    std::fs::write(
        &mixes,
        "[[mix]]\nname = \"only\"\nweights = { audio = 100 }\n",
    )
    .expect("mixes");
    let args = |allow: bool| ReportArgs {
        results: dir.clone(),
        probes: vec![],
        mixes: mixes.clone(),
        out: None,
        allow_unclean: allow,
    };
    // Outside bench/results (a temporary directory): refused, accepted and marked with the flag.
    let err = format!("{:#}", load(&args(false)).expect_err("outside"));
    assert!(err.contains("not under bench/results"), "{err}");
    let loaded = load(&args(true)).expect("allowed");
    assert_eq!(loaded.outside.len(), 1);
    let text = report_text(&loaded);
    assert!(text.starts_with("# UNCLEAN INPUTS: "), "{}", &text[..60]);
    assert!(
        text.contains("**UNCLEAN INPUTS: "),
        "the verdict line is marked"
    );
    assert!(text.contains("not all of them under bench/results"));
    assert!(!text.contains("from result files under bench/results"));
    // A baseline without the settle pause (field 0 or absent) is refused first.
    let mut run = samples::run_file(&results);
    run.settle_ms_per_1000_files = 0;
    std::fs::write(dir.join("run.json"), render_json(&run)).expect("run");
    let err = format!("{:#}", load(&args(false)).expect_err("settle"));
    assert!(err.contains("no settle pause"), "{err}");
    let text = report_text(&load(&args(true)).expect("allowed"));
    assert!(text.contains("the baseline has no settle pause (D-21)"));
    results.clear();
}

#[test]
fn a_clean_report_says_committed_and_is_not_marked() {
    let text = report_text(&inputs(&Knobs::default(), true));
    assert!(text.starts_with("# LitePack Phase 0 report"));
    assert!(text.contains("from result files under bench/results"));
    assert!(
        !text.contains("UNCLEAN"),
        "nothing is unclean in the fixture"
    );
    // The baseline's build profile cannot be checked: a caveat says so.
    assert!(text.contains("build profile is not recorded in its host file"));
}

#[test]
fn input_directories_are_labelled_from_the_repository_root() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).expect("git");
    let a = root.join("bench").join("results").join("2026-10-02-box");
    let b = root.join("scratch").join("2026-10-02-box");
    std::fs::create_dir_all(&a).expect("a");
    std::fs::create_dir_all(&b).expect("b");
    assert_eq!(
        dir_label(&a),
        ("bench/results/2026-10-02-box".to_string(), true)
    );
    assert_eq!(dir_label(&b), ("scratch/2026-10-02-box".to_string(), false));
    // A nested `bench/results` below the repository root is not the repository's.
    let n = root
        .join("crates")
        .join("x")
        .join("bench")
        .join("results")
        .join("d");
    std::fs::create_dir_all(&n).expect("n");
    assert_eq!(
        dir_label(&n),
        ("crates/x/bench/results/d".to_string(), false)
    );
    // `bench/results/d` with no repository above it is outside, and labelled by its last two parts.
    let lone = tempfile::tempdir().expect("lone");
    let l = lone.path().join("bench").join("results").join("d");
    std::fs::create_dir_all(&l).expect("l");
    assert_eq!(dir_label(&l), ("results/d".to_string(), false));
    // Files: relative to the root inside a repository, the bare name outside.
    let f = root.join("bench").join("report-mixes.toml");
    std::fs::write(&f, "x").expect("f");
    assert_eq!(
        file_label(&f),
        ("bench/report-mixes.toml".to_string(), true)
    );
    let g = lone.path().join("m.toml");
    std::fs::write(&g, "x").expect("g");
    assert_eq!(file_label(&g), ("m.toml".to_string(), false));
}

#[test]
fn a_mixes_file_outside_the_repository_marks_the_report_and_the_console_line() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dir = write_baseline(tmp.path(), &real_class_results());
    let mixes = tmp.path().join("m.toml");
    std::fs::write(
        &mixes,
        "[[mix]]\nname = \"only\"\nweights = { audio = 100 }\n",
    )
    .expect("mixes");
    let args = ReportArgs {
        results: dir,
        probes: vec![],
        mixes,
        out: None,
        allow_unclean: true,
    };
    let i = load(&args).expect("loads");
    assert!(i.mixes_outside);
    assert!(unclean_reasons(&i)
        .iter()
        .any(|r| r.contains("mixes file is outside")));
    // The console line is the report's verdict line, mark included.
    let m = Model::build(&i);
    let g = gates(&m, &i);
    let line = marked_verdict(&i, &g);
    assert!(line.starts_with("UNCLEAN INPUTS: "), "{line}");
    assert!(report_text(&i).contains(&format!("**{line}**")));
    // A fixture inside the repository is not marked.
    let clean = inputs(&Knobs::default(), true);
    assert!(!marked_verdict(&clean, &gates(&Model::build(&clean), &clean)).contains("UNCLEAN"));
}

#[test]
fn g1_uses_the_best_single_tool_and_prints_the_stricter_sum() {
    // mx5 at 95% of its own sizes on the G1 classes: best single = mx5, 17.1 MB combined, while
    // the per-class bests are also mx5 (each below ultra).
    let at = |office: u64| {
        let mut i = inputs(
            &Knobs {
                office,
                ..Knobs::default()
            },
            true,
        );
        for class in G1_CLASSES {
            set_row(&mut i, "7z", "mx5", class, |r| {
                let mut m = r.median.expect("median");
                m.archive_bytes = m.archive_bytes * 95 / 100;
                r.median = Some(m);
            });
        }
        gate(&i, "G1")
    };
    // 9.8 MB + office: 90% of 17.1 MB is 15.39 MB (passes), 85% of ultra's 17.6 MB is 14.96 MB.
    let pass = at(5_100_000);
    assert_eq!(pass.verdict, Verdict::Pass, "14.9 MB passes both limits");
    assert!(pass
        .numbers
        .iter()
        .any(|n| n.starts_with("combined best single tool (7z/mx5): ")));
    assert!(pass
        .numbers
        .iter()
        .any(|n| n.starts_with("stricter, sum of the per-class bests")));
    assert!(pass.title.contains("best single tool"));
    // 15.0 MB: inside the 90% limit of the best single tool, outside 85% of ultra.
    assert_eq!(at(5_200_000).verdict, Verdict::Fail);
}

#[test]
fn mixes_use_the_best_single_tool_and_keep_the_stars() {
    let mut i = inputs(&Knobs::default(), true);
    set_row(&mut i, "7z", "ultra", "photo-jpeg", one_repeat);
    let m = Model::build(&i);
    let docs = m.mixes.iter().find(|x| x.name == "docs").expect("mix");
    let best = docs.best.as_ref().expect("best");
    assert_eq!(
        best.name, "7z/ultra *",
        "the single-measurement row is starred"
    );
    let per_class = docs.best_per_class.as_ref().expect("per class");
    assert!(
        per_class.value <= best.bytes.value + 1e-9,
        "per-class bests are never larger"
    );
    let text = report_text(&i);
    assert!(
        text.contains("| 7z/ultra * |"),
        "blended and section 5 rows keep the star"
    );
    // The class table's incumbent name carries it too.
    assert!(m
        .class("photo-jpeg")
        .and_then(|c| c.best.as_ref())
        .is_some_and(|b| b.name == "7z/ultra *"));
}

#[test]
fn no_rule_and_stored_estimates_are_labelled() {
    let text = report_text(&inputs(&Knobs::default(), true));
    assert!(
        text.contains("= best incumbent, no rule ["),
        "audio has no rule"
    );
    assert!(text.contains("stored, no rule ["), "video is stored");
}

#[test]
fn a_mixes_file_without_a_final_newline_keeps_the_code_fence_intact() {
    let mut i = inputs(&Knobs::default(), false);
    i.mixes_text = i.mixes_text.trim_end().to_string();
    let text = report_text(&i);
    assert!(
        text.contains("= 100 }\n```\n"),
        "the fence closes on its own line"
    );
}

#[test]
fn g1_and_g2_name_failed_and_skipped_rows_of_their_classes() {
    let mut i = inputs(&Knobs::default(), true);
    set_row(&mut i, "zstd", "3", "backup-versions", |r| {
        r.median = None;
        r.failed = samples::failed().failed;
    });
    set_row(&mut i, "7z", "mx5", "photo-jpeg", |r| {
        r.median = None;
        r.repeats = None;
        r.skipped = Some("not installed".into());
    });
    let (g1, g2) = (gate(&i, "G1"), gate(&i, "G2"));
    assert!(
        g1.notes
            .iter()
            .any(|n| n.contains("skipped: 7z/mx5 (not installed)")),
        "{:?}",
        g1.notes
    );
    assert!(
        g2.notes
            .iter()
            .any(|n| n.contains("failed: zstd/3 on backup-versions")),
        "{:?}",
        g2.notes
    );
    assert!(report_text(&i).contains("- G2 inputs, failed: zstd/3 on backup-versions"));
}

#[test]
fn g3_exactly_at_eighty_percent_passes_and_just_below_fails() {
    // The raw rate is 100 MB/s (50 MB in 0.5 s); store at 0.625 s is exactly 80 MB/s.
    let at = |store_compress: f64| {
        let k = Knobs {
            raw_seconds: 0.5,
            store_compress,
            ..Knobs::default()
        };
        verdict_of(&inputs(&k, true), "G3")
    };
    assert_eq!(at(0.625), Verdict::Pass);
    assert_eq!(at(0.626), Verdict::Fail);
}

#[test]
fn g3_shows_its_context_and_marks_a_failed_or_single_measurement_store() {
    let g = gate(&inputs(&Knobs::default(), true), "G3");
    for want in [
        "video class bytes: ",
        "store median compress wall seconds on video: ",
        "store first repeat compress on video: ",
        "store compress on encrypted-random: ",
        "raw read of video, median of the passes, read only: ",
        "gate cost, entropy, gate rate as a share of the raw read rate: ",
    ] {
        assert!(
            g.numbers.iter().any(|n| n.starts_with(want)),
            "missing `{want}`"
        );
    }
    let mut one = inputs(&Knobs::default(), true);
    set_row(&mut one, "store", "store", "video", one_repeat);
    let g = gate(&one, "G3");
    assert!(g
        .numbers
        .iter()
        .any(|n| n.starts_with("store* compress on video: ")));
    assert!(g
        .notes
        .iter()
        .any(|n| n.contains("rests on one measurement")));
    let mut failed = inputs(&Knobs::default(), true);
    set_row(&mut failed, "store", "store", "video", |r| {
        r.median = None;
        r.failed = samples::failed().failed;
    });
    let g = gate(&failed, "G3");
    assert!(matches!(g.verdict, Verdict::NotEvaluable(_)));
    assert!(
        g.notes.iter().any(|n| n.contains("store on video failed")),
        "{:?}",
        g.notes
    );
    // The static note on how the two rates differ is in the report.
    assert!(report_text(&failed).contains("cache-warm"));
}

#[test]
fn g4_keeps_the_reason_of_a_mix_it_could_not_evaluate_when_it_fails() {
    let mut i = inputs(
        &Knobs {
            zstd_extract: 3.0,
            ..Knobs::default()
        },
        true,
    );
    // The default fixture's `absent` mix is not evaluable; zstd/3 is slower on the others: FAIL.
    let g = gate(&i, "G4");
    assert_eq!(g.verdict, Verdict::Fail);
    assert!(
        g.notes.iter().any(|n| n.contains("mix absent")),
        "{:?}",
        g.notes
    );
    i.mixes = mixes::Mixes::parse(MIXES).expect("mixes");
}

fn dedup_with(f: impl FnOnce(&mut dedup::Data)) -> Probes {
    let mut d = dedup_data(500_000, Some((100_000, 100_000)));
    f(&mut d);
    Probes {
        dedup: Some(probe_file("dedup", d, 7)),
        ..Probes::default()
    }
}

#[test]
fn backup_b_needs_every_patch_verified_and_present() {
    let cb = Traced::new(30_000_000.0, 1);
    let ok = backup_est_for(&dedup_with(|_| {}), &cb);
    assert_eq!(ok.bytes.value, 3_200_000.0);
    assert!(ok.basis.contains("(b)"));
    // An unverified patch: (b) cannot be formed, (a) is used and the basis says so.
    let unverified = backup_est_for(
        &dedup_with(|d| {
            if let Some(delta) = d.versions[2].delta.as_mut() {
                delta.tools[0].verified = false;
            }
        }),
        &cb,
    );
    assert_eq!(unverified.bytes.value, 4_000_000.0);
    assert!(unverified.basis.contains("(a)") && unverified.basis.contains("incomplete"));
    // A missing delta behaves the same.
    let missing = backup_est_for(&dedup_with(|d| d.versions[1].delta = None), &cb);
    assert_eq!(missing.bytes.value, 4_000_000.0);
}

fn backup_est_for(p: &Probes, cb: &Traced) -> Est {
    estimate("backup-versions", p, cb, None).expect("estimate")
}

#[test]
fn partial_coverage_counts_as_stored_and_over_coverage_is_refused() {
    let p = Probes {
        jpeg: Some(probe_file("jpeg", jpeg_data(7 * MB, 2_800_000), 5)),
        ..Probes::default()
    };
    // The probe measured 10 MB of a 12 MB class: the other 2 MB are counted as stored.
    let e = estimate("photo-jpeg", &p, &Traced::new(12e6, 1), None).expect("partial");
    assert_eq!(e.bytes.value, 9_000_000.0);
    assert!(e.basis.contains("did not measure"));
    assert!(e.bytes.sources.contains(&1) && e.bytes.sources.contains(&5));
    // The probe covering more bytes than the baseline's class is an error, not an estimate.
    let err = estimate("photo-jpeg", &p, &Traced::new(9e6, 1), None).expect_err("over");
    assert!(err.contains("more bytes"), "{err}");
}

#[test]
fn the_known_classes_come_from_the_corpus_registry() {
    let known = mixes::known_classes();
    assert_eq!(known.len(), 17);
    for c in ["video", "backup-versions", "photo-jpeg-edited"] {
        assert!(known.contains(c), "{c}");
    }
}
