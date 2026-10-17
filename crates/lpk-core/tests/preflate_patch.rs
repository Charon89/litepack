//! The patched `preflate-rs` 0.7.6 (D-66, `vendor/preflate-rs-0.7.6/LITEPACK-CHANGES.md`):
//! `recreate_whole_deflate_stream_limited` returns upstream's output for every valid record
//! (compared with `recreate_whole_deflate_stream`, whose code path the patch leaves as upstream
//! wrote it), and refuses records whose recreation would exceed the limit.
#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};

use flate2::{Compress, Compression, FlushCompress};
use lpk_core::peel::deflate::{analyse_stream, examine, preflate_config, Limits, StreamOutcome};
use preflate_rs::{
    recreate_whole_deflate_stream, recreate_whole_deflate_stream_limited, ExitCode, PreflateConfig,
};

fn cfg() -> PreflateConfig {
    preflate_config(lpk_core::peel::deflate::PLAIN_TEXT_LIMIT)
}

/// Upstream and the patched function agree on `stream`, recreated from `plain` and `corr`; the
/// patched one with the stream's own length as the limit, and refusing one byte less.
fn assert_equal(name: &str, stream: &[u8], plain: &[u8], corr: &[u8]) {
    let up = recreate_whole_deflate_stream(plain, corr).unwrap();
    assert_eq!(up, stream, "{name}: upstream recreation");
    let lim = recreate_whole_deflate_stream_limited(plain, corr, stream.len()).unwrap();
    assert_eq!(lim, up, "{name}: limited recreation");
    let roomy = recreate_whole_deflate_stream_limited(plain, corr, stream.len() * 2 + 64).unwrap();
    assert_eq!(roomy, up, "{name}: limited recreation, larger limit");
    assert!(
        recreate_whole_deflate_stream_limited(plain, corr, stream.len() - 1).is_err(),
        "{name}: one byte under the limit"
    );
}

/// The walker's golden fixtures: every reconstructed stream.
#[test]
fn every_fixture_stream_recreates_as_upstream() {
    let fixtures: [(&str, &str, &[u8]); 7] = [
        ("zip", "zip", include_bytes!("fixtures/deflate/zip.zip")),
        ("png", "png", include_bytes!("fixtures/deflate/png.png")),
        ("gzip", "gz", include_bytes!("fixtures/deflate/gzip.gz")),
        ("pdf", "pdf", include_bytes!("fixtures/deflate/pdf.pdf")),
        ("bin", "bin", include_bytes!("fixtures/deflate/bin.bin")),
        (
            "troubled",
            "zip",
            include_bytes!("fixtures/deflate/troubled.zip"),
        ),
        (
            "troubled-scan",
            "bin",
            include_bytes!("fixtures/deflate/troubled-scan.bin"),
        ),
    ];
    let mut streams = 0;
    for (name, ext, bytes) in fixtures {
        let mut done = Vec::new();
        examine(bytes, ext, &Limits::standard(), |r| done.push(r));
        for r in done {
            let stream: Vec<u8> = r
                .spans
                .iter()
                .flat_map(|&(a, b)| bytes[a..b].iter().copied())
                .collect();
            assert_eq!(stream.len(), r.deflate_len, "{name}");
            assert_equal(name, &stream, r.plain.text(), &r.corrections);
            streams += 1;
        }
    }
    assert!(streams >= 7, "{streams} streams");
}

fn text(n: usize, seed: u32) -> Vec<u8> {
    let words = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf",
    ];
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(12345);
    let mut out = Vec::new();
    while out.len() < n {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.extend_from_slice(words[(x >> 24) as usize % words.len()].as_bytes());
        out.push(b' ');
    }
    out.truncate(n);
    out
}

fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

fn raw(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), Compression::new(level));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// Raw Deflate with a sync flush (an empty stored block) after every `step` bytes: a stream of
/// many blocks of mixed types.
fn raw_flushed(data: &[u8], level: u32, step: usize) -> Vec<u8> {
    let mut c = Compress::new(Compression::new(level), false);
    let mut out = Vec::with_capacity(data.len() * 2 + 1024);
    for piece in data.chunks(step) {
        let before = c.total_in();
        while c.total_in() - before < piece.len() as u64 {
            let at = (c.total_in() - before) as usize;
            out.reserve(piece.len() + 1024);
            c.compress_vec(&piece[at..], &mut out, FlushCompress::Sync)
                .unwrap();
        }
    }
    loop {
        out.reserve(1024);
        if c.compress_vec(&[], &mut out, FlushCompress::Finish)
            .unwrap()
            == flate2::Status::StreamEnd
        {
            break;
        }
    }
    out
}

/// An empty stored block (not final) and a final one, by hand.
const EMPTY_STORED: [u8; 5] = [0x00, 0x00, 0x00, 0xFF, 0xFF];
const EMPTY_STORED_FINAL: [u8; 5] = [0x01, 0x00, 0x00, 0xFF, 0xFF];
/// A final fixed-Huffman block with no tokens (3 header bits and the end-of-block code).
const EMPTY_FIXED_FINAL: [u8; 2] = [0x03, 0x00];

/// Stored blocks (level 0 and by hand), fixed-Huffman blocks (short inputs), dynamic blocks
/// (long inputs), multi-block streams (sync flushes), at every zlib level.
#[test]
fn synthetic_streams_of_every_block_type_and_level_recreate_as_upstream() {
    let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();
    let sources: [(&str, Vec<u8>); 5] = [
        ("empty", Vec::new()),
        ("short", b"hello hello hello world".to_vec()),
        ("text", text(70_000, 3)),
        ("noise", noise(20_000, 5)),
        ("runs", vec![b'a'; 40_000]),
    ];
    for level in 0..=9 {
        for (name, data) in &sources {
            inputs.push((format!("{name} level {level}"), raw(data, level)));
        }
        let mixed = [text(9000, 7), noise(3000, 8), vec![0u8; 5000]].concat();
        inputs.push((
            format!("flushed level {level}"),
            raw_flushed(&mixed, level, 1500),
        ));
    }
    let mut by_hand = EMPTY_STORED.repeat(3);
    by_hand.extend_from_slice(&EMPTY_FIXED_FINAL);
    inputs.push(("empty stored then fixed".into(), by_hand));
    inputs.push(("one empty stored".into(), EMPTY_STORED_FINAL.to_vec()));

    let mut checked = 0;
    let mut not_analysed = Vec::new();
    for (name, stream) in &inputs {
        match analyse_stream(stream, None, &cfg()) {
            StreamOutcome::Done {
                plain,
                corrections,
                deflate_len,
                ..
            } => {
                assert_eq!(deflate_len, stream.len(), "{name}");
                assert_equal(name, stream, plain.text(), &corrections);
                checked += 1;
            }
            other => not_analysed.push(format!("{name}: {other:?}")),
        }
    }
    // Every stream zlib writes is analysable; the list says which are not if that changes.
    assert!(not_analysed.is_empty(), "{not_analysed:#?}");
    assert_eq!(checked, inputs.len());
}

/// A valid record of `blocks` empty stored blocks: tiny corrections, a stream five bytes per
/// block, an empty plain text.
fn many_empty_blocks(blocks: usize) -> (Vec<u8>, Vec<u8>) {
    let mut stream = EMPTY_STORED.repeat(blocks - 1);
    stream.extend_from_slice(&EMPTY_STORED_FINAL);
    match analyse_stream(&stream, None, &cfg()) {
        StreamOutcome::Done { corrections, .. } => (stream, corrections),
        other => panic!("{other:?}"),
    }
}

/// A crafted record: the corrections of a stream of many empty blocks, declared with a small
/// `original_len`. Upstream recreates the whole stream from an empty plain text (its output
/// grows far past the declared length, with no limit of its own); the patched copy stops at the
/// limit.
#[test]
fn a_record_that_makes_upstream_grow_is_refused_within_the_limit() {
    let blocks = 200_000;
    let (stream, corr) = many_empty_blocks(blocks);
    assert!(
        corr.len() * 100 < 5 * blocks,
        "{} correction bytes",
        corr.len()
    );
    let up = recreate_whole_deflate_stream(&[], &corr).unwrap();
    assert_eq!(up.len(), 5 * blocks);
    assert_eq!(up, stream);
    for limit in [0, 1, 64, 4096, 5 * blocks - 1] {
        let e = recreate_whole_deflate_stream_limited(&[], &corr, limit).unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::OutputLimit, "limit {limit}");
    }
    assert_eq!(
        recreate_whole_deflate_stream_limited(&[], &corr, 5 * blocks).unwrap(),
        stream
    );
}

/// Every one-byte change of a valid record's leading bytes (where the predictor parameters
/// lie): the patched function never returns an output upstream would not, never exceeds its
/// limit, and refuses some changes as out-of-range parameters.
#[test]
fn mutated_records_never_escape_the_limit_and_some_parameters_are_refused() {
    let data = text(6000, 11);
    let stream = raw(&data, 6);
    let corr = match analyse_stream(&stream, None, &cfg()) {
        StreamOutcome::Done { corrections, .. } => corrections,
        other => panic!("{other:?}"),
    };
    let limit = stream.len() + 100;
    let mut refused_parameters = 0;
    for at in 0..corr.len().min(24) {
        for v in 0..=255u8 {
            if corr[at] == v {
                continue;
            }
            let mut m = corr.clone();
            m[at] = v;
            let lim = catch_unwind(AssertUnwindSafe(|| {
                recreate_whole_deflate_stream_limited(&data, &m, limit)
            }));
            match lim {
                Ok(Ok(out)) => {
                    assert!(out.len() <= limit);
                    let up = catch_unwind(AssertUnwindSafe(|| {
                        recreate_whole_deflate_stream(&data, &m)
                    }));
                    assert!(
                        matches!(&up, Ok(Ok(u)) if *u == out),
                        "byte {at} = {v}: upstream differs"
                    );
                }
                Ok(Err(e)) if e.exit_code() == ExitCode::InvalidParameterHeader => {
                    refused_parameters += 1;
                }
                Ok(Err(_)) | Err(_) => {}
            }
        }
    }
    assert!(refused_parameters > 0);
}

/// On real files (a directory named by `LPK_PREFLATE_CORPUS`, walked recursively; run with
/// `--ignored` in a release build): every stream the analysis reconstructs recreates to the same
/// bytes through the limited function with its own length as the limit. Prints the counts.
#[test]
#[ignore = "needs a corpus directory in LPK_PREFLATE_CORPUS"]
fn corpus_streams_recreate_as_upstream() {
    let root = std::path::PathBuf::from(std::env::var("LPK_PREFLATE_CORPUS").unwrap());
    let mut stack = vec![root];
    let (mut files, mut streams, mut differ) = (0u64, 0u64, Vec::new());
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            files += 1;
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let mut done = Vec::new();
            examine(&bytes, &ext, &Limits::standard(), |r| done.push(r));
            for r in done {
                let stream: Vec<u8> = r
                    .spans
                    .iter()
                    .flat_map(|&(a, b)| bytes[a..b].iter().copied())
                    .collect();
                streams += 1;
                let lim = catch_unwind(AssertUnwindSafe(|| {
                    recreate_whole_deflate_stream_limited(
                        r.plain.text(),
                        &r.corrections,
                        stream.len(),
                    )
                }));
                if !matches!(&lim, Ok(Ok(out)) if *out == stream) {
                    differ.push(format!("{}: {:?}", path.display(), lim.map(|r| r.err())));
                }
            }
        }
    }
    println!(
        "files {files}, streams {streams}, differing {}",
        differ.len()
    );
    assert!(differ.is_empty(), "{differ:#?}");
}
