#![no_main]
//! libsais's forward transform of arbitrary text in arbitrary block sizes, the revision 1.3
//! layout, then the `u32` inverse: the text comes back. Then one index or one transformed byte
//! damaged: an error or other bytes, never a panic (spec section 8, "Decoding `bwt`").
//!
//! Layout: bytes 0..2 = the block size (u16 LE, 0 taken as 1); bytes 2..4 = the position to
//! damage (u16 LE, modulo the layout's length after `out_len`); the rest is the text.
use libfuzzer_sys::fuzz_target;
use lpk_codecs_bwt::layout::{decode_blocks, encode, input_len, Reason};

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let block_size = u32::from(u16::from_le_bytes([data[0], data[1]])).max(1);
    let at = usize::from(u16::from_le_bytes([data[2], data[3]]));
    let text = &data[4..];
    let enc = encode(text, block_size).unwrap();
    assert_eq!(
        enc.len() as u64,
        input_len(text.len() as u64, u64::from(block_size))
    );
    let mut out = vec![0u8; text.len()];
    decode_blocks(
        &enc[8..],
        text.len() as u64,
        u64::from(block_size),
        &mut out,
    )
    .unwrap();
    assert_eq!(out, text);
    if enc.len() > 8 {
        let mut bad = enc.clone();
        let i = 8 + at % (enc.len() - 8);
        bad[i] ^= 1 | (at as u8 & 0xFE);
        match decode_blocks(
            &bad[8..],
            text.len() as u64,
            u64::from(block_size),
            &mut out,
        ) {
            Ok(()) => {
                // A damaged index is never accepted; a damaged byte may still give another text.
                let _ = out;
            }
            Err(Reason::Index | Reason::Inverse) => {}
            Err(Reason::Length) => panic!("the length did not change"),
        }
        // Shorter and longer by one: `length`.
        assert_eq!(
            decode_blocks(
                &enc[8..enc.len() - 1],
                text.len() as u64,
                u64::from(block_size),
                &mut out
            ),
            Err(Reason::Length)
        );
        let mut longer = enc.clone();
        longer.push(0);
        assert_eq!(
            decode_blocks(
                &longer[8..],
                text.len() as u64,
                u64::from(block_size),
                &mut out
            ),
            Err(Reason::Length)
        );
    }
});
