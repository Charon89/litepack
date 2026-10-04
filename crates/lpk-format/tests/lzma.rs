#![allow(clippy::unwrap_used)]

mod common;

use common::{lzma_options, lzma_params, lzma_raw, pattern, write_archive, LzmaTestEncoder};
use lpk_format::{
    Archive, FormatError, LzmaDecoder, PrimitiveDecoder, PrimitiveId, Registry, Resources,
    WriterOptions,
};
use std::io::Cursor;

fn dec(
    params: &[u8],
    input: &[u8],
    expected: u64,
    limits: &Resources,
) -> Result<Vec<u8>, FormatError> {
    LzmaDecoder.decode(params, input, expected, limits)
}

fn reason(r: Result<Vec<u8>, FormatError>) -> String {
    match r.unwrap_err() {
        FormatError::LzmaError { reason } => reason,
        e => panic!("unexpected {e:?}"),
    }
}

/// A stream without the end-of-payload marker: the `.lzma` writer of
/// `lzma-rs` with a known size, header cut off (lc 3, lp 0, pb 2, 8 MiB).
fn markerless(data: &[u8]) -> Vec<u8> {
    let opts = lzma_rs::compress::Options {
        unpacked_size: lzma_rs::compress::UnpackedSize::WriteToHeader(Some(data.len() as u64)),
    };
    let mut out = Vec::new();
    lzma_rs::lzma_compress_with_options(&mut &data[..], &mut out, &opts).unwrap();
    out.split_off(13)
}

const PRESET_DICTS: [(u32, u32); 3] = [(0, 1 << 18), (6, 1 << 23), (9, 1 << 26)];
const SIZES: [usize; 5] = [0, 1, 65535, 65536, (1 << 20) + 1];

#[test]
fn decodes_raw_streams_of_liblzma_at_presets_0_6_9() {
    for (preset, dict) in PRESET_DICTS {
        for len in SIZES {
            let data = pattern(preset as u64 * 7 + len as u64, len);
            let raw = lzma_raw(&lzma_options(preset, dict, 3, 0, 2), &data);
            let out = dec(
                &lzma_params(dict, 3, 0, 2),
                &raw,
                len as u64,
                &Resources::default(),
            )
            .unwrap();
            assert!(out == data, "preset {preset} len {len}");
        }
    }
}

#[test]
fn decodes_every_property_set() {
    for (lc, lp, pb) in [(3, 0, 2), (0, 2, 0), (4, 0, 4), (0, 4, 4), (2, 2, 1)] {
        for len in [0usize, 1, 65536, 200_001] {
            let data = pattern(lc as u64 * 3 + len as u64, len);
            let raw = lzma_raw(&lzma_options(6, 1 << 20, lc, lp, pb), &data);
            let out = dec(
                &lzma_params(1 << 20, lc, lp, pb),
                &raw,
                len as u64,
                &Resources::default(),
            )
            .unwrap();
            assert!(out == data, "({lc},{lp},{pb}) len {len}");
        }
    }
}

#[test]
fn liblzma_writes_the_end_marker_and_a_markerless_stream_is_accepted_too() {
    // liblzma's raw LZMA1 encoder always ends with the end-of-payload marker:
    // an empty input still yields more than the 5 bytes of range coder start.
    let raw = lzma_raw(&lzma_options(6, 1 << 20, 3, 0, 2), b"");
    assert!(raw.len() > 5);
    let p = lzma_params(1 << 23, 3, 0, 2);
    for len in [0usize, 1, 100, 70_000] {
        let data = pattern(len as u64 + 1, len);
        let stream = markerless(&data);
        let out = dec(&p, &stream, len as u64, &Resources::default()).unwrap();
        assert!(out == data, "markerless len {len}");
        // One byte more is trailing input.
        let mut more = stream.clone();
        more.push(0);
        assert_eq!(
            reason(dec(&p, &more, len as u64, &Resources::default())),
            "trailing input"
        );
    }
}

#[test]
fn window_above_the_limit_is_refused_before_reading() {
    let limits = Resources {
        max_window: 1 << 20,
        ..Resources::default()
    };
    // The input is garbage: the window check comes first.
    let e = dec(&lzma_params((1 << 20) + 1, 3, 0, 2), b"junk", 10, &limits).unwrap_err();
    assert!(matches!(
        e,
        FormatError::WindowTooLarge {
            needed: 0x100001,
            allowed: 0x100000
        }
    ));
    let data = pattern(1, 1000);
    let raw = lzma_raw(&lzma_options(6, 1 << 20, 3, 0, 2), &data);
    assert!(dec(&lzma_params(1 << 20, 3, 0, 2), &raw, 1000, &limits).is_ok());
}

#[test]
fn bad_params_are_refused() {
    let bad = |p: &[u8]| match dec(p, b"", 0, &Resources::default()).unwrap_err() {
        FormatError::BadParams { id: 2, reason } => reason,
        e => panic!("unexpected {e:?}"),
    };
    assert_eq!(bad(&lzma_params(4096, 4, 1, 0)), "lc + lp");
    assert_eq!(bad(&lzma_params(4096, 9, 0, 0)), "lc");
    assert_eq!(bad(&lzma_params(4096, 0, 5, 0)), "lp");
    assert_eq!(bad(&lzma_params(4096, 0, 0, 5)), "pb");
    assert_eq!(bad(&[0; 6]), "length");
}

#[test]
fn truncated_and_trailing_input() {
    let data = pattern(9, 50_000);
    let p = lzma_params(1 << 20, 3, 0, 2);
    let raw = lzma_raw(&lzma_options(6, 1 << 20, 3, 0, 2), &data);
    let full = Resources::default();
    for cut in [0, 1, 4, 5, 6, raw.len() / 2, raw.len() - 12] {
        assert_eq!(
            reason(dec(&p, &raw[..cut], 50_000, &full)),
            "truncated",
            "{cut}"
        );
    }
    // Marker-less stream cut short.
    let m = markerless(&data);
    assert_eq!(
        reason(dec(&p, &m[..m.len() - 8], 50_000, &full)),
        "truncated"
    );
    // Input after a complete stream.
    let mut more = raw.clone();
    more.extend_from_slice(&[1, 2, 3]);
    assert_eq!(reason(dec(&p, &more, 50_000, &full)), "trailing input");
    // A stream that produces fewer bytes than expected ends early.
    assert_eq!(reason(dec(&p, &raw, 50_001, &full)), "truncated");
}

#[test]
fn output_never_passes_expected_len() {
    let data = pattern(3, 50_000);
    let p = lzma_params(1 << 20, 3, 0, 2);
    let raw = lzma_raw(&lzma_options(6, 1 << 20, 3, 0, 2), &data);
    for want in [0u64, 1, 49_999, 25_000] {
        let r = dec(&p, &raw, want, &Resources::default());
        match r {
            Err(FormatError::PayloadTooLarge { len, max }) => {
                assert_eq!(max, want);
                assert!(len > max);
            }
            // A cut that lands on the stream's end of input reads as trailing.
            Err(FormatError::LzmaError { reason }) => assert_eq!(reason, "trailing input"),
            other => panic!("want {want}: {other:?}"),
        }
    }
}

#[test]
fn a_distance_beyond_dict_size_is_refused_whatever_max_window_is() {
    let mut data = pattern(1, 2000);
    data.extend_from_slice(&data.clone());
    let raw = lzma_raw(&lzma_options(6, 1 << 20, 3, 0, 2), &data);
    let n = data.len() as u64;
    // Declared dictionary 1000, but the stream reaches back 2000.
    let p = lzma_params(1000, 3, 0, 2);
    let tight = Resources {
        max_window: 1000,
        ..Resources::default()
    };
    let a = reason(dec(&p, &raw, n, &tight));
    let b = reason(dec(&p, &raw, n, &Resources::default()));
    assert!(a.contains("beyond"), "{a}");
    assert_eq!(a, b);
}

#[test]
fn first_range_coder_byte_must_be_zero() {
    let data = pattern(2, 1000);
    let p = lzma_params(1 << 20, 3, 0, 2);
    let mut raw = lzma_raw(&lzma_options(6, 1 << 20, 3, 0, 2), &data);
    assert_eq!(raw[0], 0);
    raw[0] = 1;
    assert_eq!(
        reason(dec(&p, &raw, 1000, &Resources::default())),
        "range coder"
    );
    let mut m = markerless(&data);
    m[0] = 0x80;
    assert_eq!(
        reason(dec(
            &lzma_params(1 << 23, 3, 0, 2),
            &m,
            1000,
            &Resources::default()
        )),
        "range coder"
    );
}

#[test]
fn a_match_crossing_the_bound_is_payload_too_large_and_a_symbol_boundary_is_trailing() {
    // Zeros: one literal, then long matches. Every bound is either inside a
    // match (PayloadTooLarge) or between two symbols (trailing input).
    let data = vec![0u8; 10_000];
    let marker = lzma_raw(&lzma_options(6, 1 << 20, 3, 0, 2), &data);
    let free = markerless(&data);
    let (pm, pf) = (lzma_params(1 << 20, 3, 0, 2), lzma_params(1 << 23, 3, 0, 2));
    let (mut over, mut trailing) = (0, 0);
    for want in 1..700u64 {
        let class = |p: &[u8], s: &[u8]| match dec(p, s, want, &Resources::default()) {
            Err(FormatError::PayloadTooLarge { len, max }) => {
                assert_eq!(max, want);
                assert!(len > want);
                0
            }
            Err(FormatError::LzmaError { reason }) if reason == "trailing input" => 1,
            other => panic!("want {want}: {other:?}"),
        };
        // The two encoders cut their symbols differently, so each stream is
        // classified on its own.
        for c in [class(&pm, &marker), class(&pf, &free)] {
            if c == 0 {
                over += 1;
            } else {
                trailing += 1;
            }
        }
    }
    assert!(over > 600, "{over}");
    assert!(trailing >= 2, "{trailing}");
}

#[test]
fn registry_has_it_and_the_writer_round_trips() {
    assert!(Registry::v1().is_implemented(PrimitiveId::Lzma));
    let files = vec![
        ("a", pattern(1, 0)),
        ("b", pattern(2, 1)),
        ("c", pattern(3, 4096)),
        ("d", pattern(4, 90_001)),
    ];
    let bytes = write_archive(
        WriterOptions {
            chunk_size: 4096,
            block_size: 32 * 1024,
            archive_id: [4; 16],
            encoder: Box::new(LzmaTestEncoder::new(6, 1 << 20, 3, 0, 2)),
            records: Vec::new(),
            recovery: Default::default(),
        },
        &files,
    );
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    assert!(a.index().blocks.len() > 1);
    assert_eq!(a.index().envelope.max_window, 1 << 20);
    a.verify().unwrap();
    let table = a.entry_table().unwrap();
    let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
    for (e, (path, data)) in entries.iter().zip(&files) {
        let mut got = Vec::new();
        a.extract(e, &mut got).unwrap();
        assert_eq!(&e.path, path);
        assert!(&got == data, "{path}");
    }
}

mod hostile {
    use super::*;
    use proptest::prelude::*;

    fn tight() -> Resources {
        Resources {
            max_window: 1 << 16,
            max_block_plain: 20_000,
            ..Resources::default()
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn random_bytes_never_panic(
            input in proptest::collection::vec(any::<u8>(), 0..2000),
            lc in 0u8..=4,
            lp in 0u8..=4,
            pb in 0u8..=4,
        ) {
            let lp = lp.min(4 - lc);
            let _ = dec(&lzma_params(1 << 16, lc, lp, pb), &input, 10_000, &tight());
        }

        #[test]
        fn mutated_streams_never_panic(
            seed in any::<u64>(),
            len in 0usize..6000,
            muts in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..6),
            cut in proptest::option::of(any::<usize>()),
            marker in any::<bool>(),
            lp in 0u8..=1,
        ) {
            let data = pattern(seed, len);
            let mut stream = if marker {
                lzma_raw(&lzma_options(6, 1 << 16, 3, lp, 2), &data)
            } else {
                markerless(&data)
            };
            let lp = if marker { lp } else { 0 };
            for (at, v) in muts {
                let n = stream.len();
                stream[at % n] ^= v | 1;
            }
            if let Some(c) = cut {
                stream.truncate(c % (stream.len() + 1));
            }
            let p = lzma_params(1 << 16, 3, lp, 2);
            if let Ok(out) = dec(&p, &stream, data.len() as u64, &tight()) {
                prop_assert_eq!(out.len(), data.len());
            }
        }
    }
}
