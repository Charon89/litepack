#![no_main]
//! Revision 1.4's PNG filter undo on the compress side (`png_candidate` of the Deflate peel: the
//! IHDR parse, the layout, the filter bytes and the Up transcode checked by re-filtering back)
//! over arbitrary headers and inflated IDAT text: no panic; a candidate's record describes the
//! header, its filtered length is the text's, its hash is the text's, and its Up text re-filters
//! to the text; nothing is allocated past the filter bytes, the Up filter list, the Up text, one
//! re-filtered copy and its two rows (a counting allocator: five times the text plus a page). `png_filter_decode`
//! covers the decoder's side (`PngFilterDecoder`) and not this one.
//!
//! Layout: bytes 0..2 = width (u16 LE), 2..4 = height (u16 LE), 4 = bit depth, 5 = colour type,
//! 6 = interlace, 7 = flags (bit 0: the chunk before the text is not named IDAT; bit 1: a
//! compression method other than 0; bit 2: a filter method other than 0); the rest is the
//! inflated IDAT text. Seeds: the filtered text of the committed PNG fixtures with their headers.
use libfuzzer_sys::fuzz_target;
use lpk_core::peel::png::{Layout, SIGNATURE};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let width = u32::from(u16::from_le_bytes([data[0], data[1]]));
    let height = u32::from(u16::from_le_bytes([data[2], data[3]]));
    let (bit_depth, color_type, interlace, flags) = (data[4], data[5], data[6], data[7]);
    let plain = &data[8..];
    let mut file = Vec::with_capacity(64);
    file.extend_from_slice(&SIGNATURE);
    file.extend_from_slice(&[0, 0, 0, 13]);
    file.extend_from_slice(b"IHDR");
    file.extend_from_slice(&width.to_be_bytes());
    file.extend_from_slice(&height.to_be_bytes());
    file.extend_from_slice(&[
        bit_depth,
        color_type,
        u8::from(flags & 2 != 0),
        u8::from(flags & 4 != 0),
        interlace,
    ]);
    file.extend_from_slice(&[0; 4]); // the CRC, not checked
    file.extend_from_slice(&(plain.len() as u32).to_be_bytes());
    file.extend_from_slice(if flags & 1 != 0 { b"IDAX" } else { b"IDAT" });
    // The zlib header; the raw Deflate stream starts after it, two bytes after the chunk name.
    file.extend_from_slice(&[0x78, 0x01]);
    let start = file.len();
    ALLOC.reset_peak();
    let before = ALLOC.current();
    let r = lpk_core::peel::deflate::stage::png_candidate(&file, start, plain);
    let peak = (ALLOC.peak() - before) as u64;
    let n = plain.len() as u64;
    assert!(
        peak <= 5 * n + 4096,
        "peak {peak} over five times the text {n}"
    );
    if let Some((rec, up)) = r {
        assert!(flags & 7 == 0);
        assert_eq!(
            (
                rec.width,
                rec.height,
                rec.bit_depth,
                rec.color_type,
                rec.interlace
            ),
            (width, height, bit_depth, color_type, interlace)
        );
        assert_eq!(rec.filtered_len(), Some(n));
        assert!(rec.verify_hash(plain));
        assert_eq!(up.len(), plain.len());
        let l = Layout::new(width, height, bit_depth, color_type, interlace).expect("layout");
        assert_eq!(l.transcode(&up, &rec.filters).expect("transcode"), plain);
    }
});
