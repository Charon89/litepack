//! `lpk_core::peel::deflate::examine` reports, for the synthetic fixtures, the counts Phase 0's
//! `probe deflate` reported for the same files before its walker moved here (recorded from the
//! unmodified probe; the probe's own test `golden_counts_of_synthetic_files_are_unchanged`
//! generates the same bytes and compares against the same values).
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use lpk_core::peel::deflate::{examine, Counts, Limits, Plain};

/// (name, extension, bytes, kind, streams, deflate bytes, plain bytes, correction bytes,
/// replaced length, replaced BLAKE3).
type Golden = (
    &'static str,
    &'static str,
    &'static [u8],
    &'static str,
    u64,
    u64,
    u64,
    u64,
    usize,
    &'static str,
);

const GOLDEN: [Golden; 5] = [
    (
        "zip",
        "zip",
        include_bytes!("fixtures/deflate/zip.zip"),
        "zip",
        2,
        1525,
        12000,
        48,
        12296,
        "527a2dfd1ed0c0c06e5e621c242a286d857fa3de02096d5abe8826de6b014007",
    ),
    (
        "png",
        "png",
        include_bytes!("fixtures/deflate/png.png"),
        "png",
        1,
        463,
        4640,
        49,
        4754,
        "28fbea47ecff8ce408dd920ef2908dd44fe947d8644a490d107766efd00dac7e",
    ),
    (
        "gzip",
        "gz",
        include_bytes!("fixtures/deflate/gzip.gz"),
        "gzip",
        1,
        3236,
        30000,
        24,
        30018,
        "53b01ce9e19b7fdf6cad6bec488dd5d05077c464a587f800f893da34e75457c8",
    ),
    (
        "pdf",
        "pdf",
        include_bytes!("fixtures/deflate/pdf.pdf"),
        "pdf",
        1,
        2205,
        20000,
        38,
        20117,
        "c659bda7d83ea7b9e5cfc91c629d58a24b54b8b3539a7b72b3ca4c1c8d202d16",
    ),
    (
        "bin",
        "bin",
        include_bytes!("fixtures/deflate/bin.bin"),
        "other",
        1,
        3223,
        30000,
        32,
        34032,
        "a48039e84157720e0c6a2908ae3baba5640896f977d367f25d5c5c51ce812491",
    ),
];

/// The counts without the timings (they are not reproducible).
fn untimed(c: &Counts) -> Counts {
    Counts {
        analyse_seconds: 0.0,
        recreate_seconds: 0.0,
        unreconstructed_seconds: 0.0,
        ..c.clone()
    }
}

/// The probe's replaced file: every reconstructed stream's spans replaced by its plain text
/// (written at the first span; later spans are cut out).
fn replaced(bytes: &[u8], edits: &[(Vec<(usize, usize)>, Plain)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0;
    for (spans, plain) in edits {
        for (i, &(a, b)) in spans.iter().enumerate() {
            out.extend_from_slice(&bytes[pos..a]);
            if i == 0 {
                out.extend_from_slice(plain.text());
            }
            pos = b;
        }
    }
    out.extend_from_slice(&bytes[pos..]);
    out
}

#[test]
fn examine_reports_the_counts_the_probe_reported() {
    for (name, ext, bytes, kind, n, defl, plain, corr, rep_len, rep_hash) in GOLDEN {
        let mut edits = Vec::new();
        let e = examine(bytes, ext, &Limits::standard(), |r| {
            edits.push((r.spans, r.plain));
        });
        assert_eq!(e.kind, kind, "{name}");
        let want = Counts {
            found: n,
            reconstructed: n,
            skipped: 0,
            failed: BTreeMap::new(),
            structural_refusals: 0,
            scan_false_hits: 0,
            walker_panics: 0,
            work_budget_stopped: 0,
            unexamined: 0,
            zip_deflate64_members: 0,
            zip_other_method_members: 0,
            found_deflate_bytes: defl,
            reconstructed_deflate_bytes: defl,
            skipped_deflate_bytes: 0,
            failed_deflate_bytes: 0,
            reconstructed_plain_bytes: plain,
            correction_bytes: corr,
            analyse_seconds: 0.0,
            recreate_seconds: 0.0,
            unreconstructed_seconds: 0.0,
        };
        assert_eq!(untimed(&e.counts), want, "{name}");
        let r = replaced(bytes, &edits);
        assert_eq!(
            (r.len(), blake3::hash(&r).to_hex().to_string()),
            (rep_len, rep_hash.to_string()),
            "{name}: the replaced file"
        );
    }
}

/// The non-success paths (failed by name, skipped by the plain-text limit and by the per-file
/// budget, a structural refusal, scan false hits), recorded by running the probe's golden test
/// code on a scratch checkout of 8423a14 (before the move).
#[test]
fn examine_reports_the_probes_non_success_counts() {
    /// (name, bytes, limits, kind, counts, replaced length, replaced BLAKE3).
    type Troubled = (
        &'static str,
        &'static [u8],
        Limits,
        &'static str,
        Counts,
        usize,
        &'static str,
    );
    let cases: [Troubled; 2] = [
        (
            "troubled",
            include_bytes!("fixtures/deflate/troubled.zip"),
            Limits {
                plain_text_limit: 10_000,
                file_plain_budget: 5000,
            },
            "zip",
            Counts {
                found: 4,
                reconstructed: 1,
                skipped: 2,
                failed: BTreeMap::from([("InvalidDeflate".to_string(), 1)]),
                structural_refusals: 1,
                scan_false_hits: 0,
                walker_panics: 0,
                found_deflate_bytes: 3380,
                reconstructed_deflate_bytes: 652,
                skipped_deflate_bytes: 2664,
                failed_deflate_bytes: 64,
                reconstructed_plain_bytes: 5000,
                correction_bytes: 24,
                ..Counts::default()
            },
            8742,
            "fdf04005632224e8a8de4e5cbc59018f7271688007368f57541ff83401e008f4",
        ),
        (
            "troubled-scan",
            include_bytes!("fixtures/deflate/troubled-scan.bin"),
            Limits::standard(),
            "other",
            Counts {
                found: 1,
                reconstructed: 1,
                scan_false_hits: 2,
                found_deflate_bytes: 762,
                reconstructed_deflate_bytes: 762,
                reconstructed_plain_bytes: 6000,
                correction_bytes: 24,
                ..Counts::default()
            },
            7106,
            "50bcf0f8bfb965abc33116b0e77d559bddcc238ddd279787e046dece6fa9d30c",
        ),
    ];
    for (name, bytes, limits, kind, want, rep_len, rep_hash) in cases {
        let mut edits = Vec::new();
        let ext = if kind == "zip" { "zip" } else { "bin" };
        let e = examine(bytes, ext, &limits, |r| edits.push((r.spans, r.plain)));
        assert_eq!(e.kind, kind, "{name}");
        assert_eq!(untimed(&e.counts), want, "{name}");
        let r = replaced(bytes, &edits);
        assert_eq!(
            (r.len(), blake3::hash(&r).to_hex().to_string()),
            (rep_len, rep_hash.to_string()),
            "{name}: the replaced file"
        );
    }
}
