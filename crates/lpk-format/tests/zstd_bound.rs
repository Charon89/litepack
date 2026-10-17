//! Spec section 8, zstd rule 7 (task C fix round, R14): when a frame both passes its bound and is
//! damaged, the class is decided at the granularity of a block: the output of the complete blocks
//! before the damaged one past the bound is `PayloadTooLarge`; otherwise `ZstdError` (the damaged
//! block's partial output is never counted).
#![allow(clippy::unwrap_used)]

mod common;

use common::{compress_stream, pattern, zstd_params};
use lpk_format::{FormatError, PrimitiveDecoder, Resources, ZstdDecoder};

const KIB: u64 = 1024;

/// Three full 128 KiB blocks of text (window_log 17, a checksum), cut 10 bytes into the third
/// block's end: the first two blocks are complete (256 KiB), the third is damaged.
fn damaged_frame() -> (Vec<u8>, Vec<u8>) {
    let data = pattern(5, 384 * 1024);
    let frame = compress_stream(3, 17, None, &data);
    (data, frame[..frame.len() - 10].to_vec())
}

fn step(input: &[u8], bound: u64) -> Result<Vec<u8>, FormatError> {
    ZstdDecoder::default().decode_step(
        &zstd_params(17, None),
        input,
        bound,
        false,
        &Resources::default(),
    )
}

#[test]
fn complete_blocks_past_the_bound_then_a_damaged_block_is_payload_too_large() {
    let (data, cut) = damaged_frame();
    // The whole frame decodes: the cut is the only damage.
    let whole = compress_stream(3, 17, None, &data);
    assert!(step(&whole, data.len() as u64).unwrap() == data);
    match step(&cut, 200 * KIB) {
        Err(FormatError::PayloadTooLarge { len, max }) => {
            assert_eq!(max, 200 * KIB);
            assert_eq!(len, 256 * KIB);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn damage_in_the_block_that_would_cross_is_zstd_error() {
    let (_, cut) = damaged_frame();
    assert!(matches!(
        step(&cut, 300 * KIB),
        Err(FormatError::ZstdError { .. })
    ));
}

#[test]
fn damage_before_any_crossing_is_zstd_error() {
    let (_, cut) = damaged_frame();
    assert!(matches!(
        step(&cut, 1024 * KIB),
        Err(FormatError::ZstdError { .. })
    ));
}
