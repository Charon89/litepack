#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::{LzmaDecoder, PrimitiveDecoder, Resources};

// Layout: bytes 0..7 = params (dict_size u32 LE, lc, lp, pb), bytes 7..9 =
// expected_len / 16, rest = the raw LZMA1 stream.
fuzz_target!(|data: &[u8]| {
    if data.len() < 9 {
        return;
    }
    let limits = Resources {
        max_window: 1 << 24,
        max_bwt_block: 1 << 20,
        max_block_plain: 1 << 22,
        max_frame_payload: 1 << 22,
        memory: 1 << 26,
    };
    let expected = u64::from(u16::from_le_bytes([data[7], data[8]])) * 16;
    let _ = LzmaDecoder.decode(&data[..7], &data[9..], expected, &limits);
});
