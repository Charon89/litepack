#![allow(clippy::unwrap_used)]

mod common;

use common::{compress, pattern, trained_dictionary, write_archive, zstd_params, ZstdTestEncoder};
use lpk_format::{
    decode_block, prior_id, Archive, BlockHeader, FormatError, Frame, FrameFlags, FrameKind, Graph,
    MemoryPriors, PrimitiveDecoder, PrimitiveId, Registry, Resources, Step, Trailer, WriterOptions,
    ZstdDecoder,
};
use std::io::Cursor;

fn dec(
    params: &[u8],
    input: &[u8],
    expected: u64,
    limits: &Resources,
) -> Result<Vec<u8>, FormatError> {
    ZstdDecoder::default().decode(params, input, expected, limits)
}

fn decode_plain(input: &[u8], expected: u64) -> Result<Vec<u8>, FormatError> {
    dec(
        &zstd_params(24, None),
        input,
        expected,
        &Resources::default(),
    )
}

#[test]
fn decodes_frames_of_the_zstd_crate() {
    for level in [1, 3, 19] {
        for len in [0usize, 1, 65535, 65536, (1 << 20) + 1] {
            let data = pattern(level as u64 * 31 + len as u64, len);
            let frame = compress(level, None, None, &data);
            let out = decode_plain(&frame, len as u64).unwrap();
            assert!(out == data, "level {level} len {len}");
        }
    }
}

#[test]
fn a_sequence_of_frames_and_skippable_frames() {
    let (a, b) = (pattern(1, 70_000), pattern(2, 5));
    let mut input = compress(3, None, None, &a);
    // A skippable frame: magic 0x184D2A50, a length, that many bytes.
    input.extend_from_slice(&[0x50, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 9, 9, 9]);
    input.extend_from_slice(&compress(3, None, None, &b));
    let mut want = a;
    want.extend_from_slice(&b);
    assert_eq!(decode_plain(&input, want.len() as u64).unwrap(), want);
    assert!(decode_plain(b"", 0).unwrap().is_empty());
}

#[test]
fn damaged_input_is_a_zstd_error() {
    let data = pattern(5, 300_000);
    let frame = compress(3, None, None, &data);
    let is_zstd = |r: Result<Vec<u8>, FormatError>| {
        assert!(matches!(r, Err(FormatError::ZstdError { .. })), "{r:?}");
    };
    // Truncated in the header, in the middle and in the checksum.
    for cut in [3, 20, frame.len() / 2, frame.len() - 2] {
        is_zstd(decode_plain(&frame[..cut], data.len() as u64));
    }
    // A flipped checksum byte.
    let mut bad = frame.clone();
    *bad.last_mut().unwrap() ^= 1;
    is_zstd(decode_plain(&bad, data.len() as u64));
    is_zstd(decode_plain(b"definitely not a frame", 100));
}

#[test]
fn a_frame_window_above_the_declared_one_is_bad_params() {
    // 200 KB of content compressed with a 2^20 window: the frame declares a
    // window above 2^15.
    let data = pattern(9, 200_000);
    let frame = compress(3, Some(20), None, &data);
    assert!(matches!(
        dec(
            &zstd_params(15, None),
            &frame,
            data.len() as u64,
            &Resources::default()
        ),
        Err(FormatError::BadParams {
            id: 1,
            reason: "frame window exceeds declared"
        })
    ));
    // With the window declared it decodes.
    let out = dec(
        &zstd_params(20, None),
        &frame,
        data.len() as u64,
        &Resources::default(),
    )
    .unwrap();
    assert!(out == data);
}

#[test]
fn declared_window_above_the_limit_is_refused_before_reading() {
    let limits = Resources {
        max_window: 1 << 20,
        ..Resources::default()
    };
    // The input is garbage: the refusal comes first.
    let r = dec(&zstd_params(24, None), b"garbage", 10, &limits);
    assert!(matches!(
        r,
        Err(FormatError::WindowTooLarge {
            needed: 16_777_216,
            allowed: 1_048_576
        })
    ));
    // At the limit it is not refused.
    let data = pattern(3, 1000);
    let frame = compress(3, None, None, &data);
    let r = dec(&zstd_params(20, None), &frame, 1000, &limits).unwrap();
    assert_eq!(r, data);
}

#[test]
fn output_beyond_expected_len_is_payload_too_large() {
    let data = pattern(4, 1 << 20);
    let frame = compress(3, None, None, &data);
    for expected in [0u64, 1, 65_535, (1 << 20) - 1] {
        assert!(
            matches!(
                decode_plain(&frame, expected),
                Err(FormatError::PayloadTooLarge { max, .. }) if max == expected
            ),
            "{expected}"
        );
    }
    assert!(decode_plain(&frame, 1 << 20).is_ok());

    // Through `decode_block` the last step reports a length mismatch.
    let header = BlockHeader {
        graph: Graph {
            steps: vec![Step {
                primitive: PrimitiveId::Zstd,
                params: zstd_params(24, None),
            }],
        },
        plain_len: 1000,
        encoded_len: frame.len() as u64,
    };
    assert!(matches!(
        decode_block(&Registry::v1(), &header, 4, &frame, &Resources::default()),
        Err(FormatError::BlockLengthMismatch { block: 4 })
    ));
}

#[test]
fn dictionary_needs_the_right_prior() {
    let dict = trained_dictionary(1);
    let other = trained_dictionary(2);
    let id = prior_id(&dict);
    assert_eq!(id, *blake3::hash(&dict).as_bytes());
    let data = pattern(77, 40_000);
    let frame = compress(3, None, Some(&dict), &data);
    let params = zstd_params(20, Some(id));
    let limits = Resources::default();
    let with = |bytes: &[u8]| {
        let mut m = MemoryPriors::new();
        // Whatever the bytes are, file them under the id the step names.
        let real = m.insert(bytes.to_vec());
        (m, real)
    };

    // The right prior.
    let (store, real) = with(&dict);
    assert_eq!(real, id);
    let d = ZstdDecoder::new(std::sync::Arc::new(store));
    assert!(d.decode(&params, &frame, 40_000, &limits).unwrap() == data);

    // No prior at all: MissingPrior, before anything is decoded.
    let r = ZstdDecoder::default().decode(&params, b"garbage", 40_000, &limits);
    assert!(matches!(r, Err(FormatError::MissingPrior { id: m }) if m == id));

    // A store that has another prior only.
    let (store, _) = with(&other);
    let d = ZstdDecoder::new(std::sync::Arc::new(store));
    assert!(matches!(
        d.decode(&params, &frame, 40_000, &limits),
        Err(FormatError::MissingPrior { .. })
    ));

    // The frame was made with `other` but the step names `dict`.
    let wrong_frame = compress(3, None, Some(&other), &data);
    let (store, _) = with(&dict);
    let d = ZstdDecoder::new(std::sync::Arc::new(store));
    assert!(matches!(
        d.decode(&params, &wrong_frame, 40_000, &limits),
        Err(FormatError::ZstdError { .. })
    ));

    // A prior that is not a zstd dictionary at all.
    let junk = vec![7u8; 100];
    let junk_id = prior_id(&junk);
    let (store, _) = with(&junk);
    let d = ZstdDecoder::new(std::sync::Arc::new(store));
    assert!(matches!(
        d.decode(&zstd_params(20, Some(junk_id)), &frame, 40_000, &limits),
        Err(FormatError::ZstdError { .. })
    ));
}

#[test]
fn registry_with_priors_serves_the_zstd_decoder() {
    let dict = trained_dictionary(3);
    let id = prior_id(&dict);
    let data = pattern(8, 10_000);
    let frame = compress(3, None, Some(&dict), &data);
    let header = BlockHeader {
        graph: Graph {
            steps: vec![Step {
                primitive: PrimitiveId::Zstd,
                params: zstd_params(20, Some(id)),
            }],
        },
        plain_len: 10_000,
        encoded_len: frame.len() as u64,
    };
    let limits = Resources::default();
    assert!(matches!(
        decode_block(&Registry::v1(), &header, 0, &frame, &limits),
        Err(FormatError::MissingPrior { .. })
    ));
    let mut store = MemoryPriors::new();
    store.insert(dict.clone());
    let reg = Registry::v1().with_priors(Box::new(store));
    assert_eq!(reg.priors().get(&id), Some(dict.as_slice()));
    assert!(decode_block(&reg, &header, 0, &frame, &limits).unwrap() == data);
}

fn tree() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a", pattern(1, 0)),
        ("b", pattern(2, 1)),
        ("c", pattern(3, 4095)),
        ("d", pattern(4, 4096)),
        ("e", pattern(5, 50_000)),
        ("f", pattern(6, 123_457)),
    ]
}

fn options(encoder: ZstdTestEncoder) -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 32 * 1024,
        archive_id: [3; 16],
        encoder: Box::new(encoder),
        records: Vec::new(),
        recovery: Default::default(),
        seal: None,
    }
}

fn extract_all(a: &mut Archive<Cursor<Vec<u8>>>) -> Vec<(String, Vec<u8>)> {
    let table = a.entry_table().unwrap();
    let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
    entries
        .iter()
        .map(|e| {
            let mut buf = Vec::new();
            a.extract(e, &mut buf).unwrap();
            (e.path.clone(), buf)
        })
        .collect()
}

#[test]
fn writer_with_the_zstd_encoder_round_trips() {
    let files = tree();
    let bytes = write_archive(options(ZstdTestEncoder::new(3, 20, None)), &files);
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    assert!(a.priors().is_empty());
    assert!(a.index().blocks.len() > 1);
    assert_eq!(a.index().envelope.max_window, 1 << 20);
    let s = a.verify().unwrap();
    assert_eq!(s.entries, files.len() as u64);
    let got = extract_all(&mut a);
    for ((path, data), (gp, gd)) in files.iter().zip(&got) {
        assert_eq!(path, gp);
        assert!(data == gd, "{path}");
    }
}

type Files = Vec<(&'static str, Vec<u8>)>;

fn dict_archive() -> (Vec<u8>, Vec<u8>, Files) {
    let dict = trained_dictionary(4);
    let files = tree();
    let bytes = write_archive(
        options(ZstdTestEncoder::new(3, 20, Some(dict.clone()))),
        &files,
    );
    (bytes, dict, files)
}

#[test]
fn the_index_lists_the_dictionary_and_the_reader_asks_for_it() {
    let (bytes, dict, files) = dict_archive();
    let id = prior_id(&dict);
    let mut a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    assert_eq!(a.priors(), &[id]);
    // Without the prior nothing decodes.
    assert!(matches!(
        a.verify(),
        Err(FormatError::MissingPrior { id: m }) if m == id
    ));
    let mut store = MemoryPriors::new();
    store.insert(dict);
    a.set_priors(Box::new(store));
    a.verify().unwrap();
    let got = extract_all(&mut a);
    assert!(files.iter().zip(&got).all(|((_, d), (_, g))| d == g));
}

/// Replace the index of `archive` by `index`, keeping everything before it.
fn with_index(archive: &[u8], index: lpk_format::Index) -> Vec<u8> {
    let a = Archive::open(Cursor::new(archive.to_vec()), &Resources::default()).unwrap();
    let at = a.trailer().index_offset as usize;
    let header = *a.header();
    let payload = index.encode().unwrap();
    let hash = *blake3::hash(&payload).as_bytes();
    let mut out = archive[..at].to_vec();
    let frame = Frame {
        kind: FrameKind::Index,
        flags: FrameFlags::EMPTY,
        payload,
    };
    let len = frame.encoded_len();
    frame.write(&mut out).unwrap();
    Trailer {
        index_offset: at as u64,
        index_len: len,
        index_hash: hash,
        generation: 0,
        archive_id: header.archive_id,
    }
    .write(&mut out)
    .unwrap();
    out
}

#[test]
fn a_block_naming_an_unlisted_prior_is_refused_at_its_header() {
    let (bytes, dict, _) = dict_archive();
    let id = prior_id(&dict);
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let mut index = a.index().clone();
    index.priors.clear();
    let tampered = with_index(&bytes, index);
    let mut a = Archive::open(Cursor::new(tampered), &Resources::default()).unwrap();
    assert!(a.priors().is_empty());
    let mut store = MemoryPriors::new();
    store.insert(dict);
    a.set_priors(Box::new(store));
    // Even with the prior at hand the list is checked first.
    assert!(matches!(
        a.verify(),
        Err(FormatError::UnlistedPrior { id: m }) if m == id
    ));
}

#[test]
fn a_block_needing_more_than_the_envelope_is_a_mismatch() {
    let bytes = write_archive(
        options(ZstdTestEncoder::new(3, 24, None)),
        &[("f", pattern(1, 10_000))],
    );
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    assert_eq!(a.index().envelope.max_window, 1 << 24);
    let mut index = a.index().clone();
    index.envelope.max_window = 1 << 20;
    let mut a = Archive::open(
        Cursor::new(with_index(&bytes, index)),
        &Resources::default(),
    )
    .unwrap();
    assert!(matches!(
        a.verify(),
        Err(FormatError::EnvelopeMismatch {
            field: "max_window"
        })
    ));
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
        fn random_bytes_never_panic(input in proptest::collection::vec(any::<u8>(), 0..2000)) {
            let _ = dec(&zstd_params(16, None), &input, 10_000, &tight());
        }

        #[test]
        fn mutated_frames_never_panic(
            seed in any::<u64>(),
            len in 0usize..6000,
            muts in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..6),
            cut in proptest::option::of(any::<usize>()),
        ) {
            let data = pattern(seed, len);
            let mut frame = compress(3, Some(16), None, &data);
            for (at, v) in muts {
                let n = frame.len();
                frame[at % n] ^= v | 1;
            }
            if let Some(c) = cut {
                frame.truncate(c % (frame.len() + 1));
            }
            if let Ok(out) = dec(&zstd_params(16, None), &frame, 10_000, &tight()) {
                prop_assert!(out.len() <= 10_000);
            }
        }
    }
}
