#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::{PrimitiveDecoder, Resources, ZstdDecoder};

// Layout: byte 0 = window_log, bytes 1..3 = expected_len / 16, rest = the frame.
fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let limits = Resources {
        max_window: 1 << 20,
        max_bwt_block: 1 << 20,
        max_block_plain: 1 << 22,
        max_frame_payload: 1 << 22,
        memory: 1 << 26,
    };
    let expected = u64::from(u16::from_le_bytes([data[1], data[2]])) * 16;
    let mut params = vec![data[0] % 32];
    params.extend_from_slice(&[0u8; 32]);
    let _ = ZstdDecoder::default().decode(&params, &data[3..], expected, &limits);
    // Also with the window byte alone and a bogus length of params.
    let _ = ZstdDecoder::default().decode(&params[..1], &data[3..], expected, &limits);
});
