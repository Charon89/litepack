//! Revision 1.4's claim "Resources: none" for `png-filter` (spec section 8): the decoder holds
//! its output (the image's filtered length) and two rows beyond its input, so a block
//! `[M, png-filter, deflate-reconstruct]` needs no more than `max_block_plain` plus the deflate
//! term, which already counts the png-filter step's output as the deflate step's input and
//! seven more bytes per plain byte. Measured with a counting global allocator on a 12 MiB
//! image, above any test block size; its own test binary, so no other test allocates meanwhile.
#![allow(clippy::unwrap_used)]

use lpk_core::peel::deflate::memory_term;
use lpk_core::peel::png::{Layout, FILTER_UP};
use lpk_core::PngFilterDecoder;
use lpk_format::{
    DecodeContext, FormatError, PngFilterRecord, PrimitiveDecoder, Record, RecordBody, Resources,
};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

struct Ctx(Record);

impl DecodeContext for Ctx {
    fn block(&self) -> usize {
        0
    }
    fn record(&mut self, _id: u64) -> Result<Record, FormatError> {
        Ok(self.0.clone())
    }
    fn chunk(&mut self, _record: u64, index: u64) -> Result<Vec<u8>, FormatError> {
        Err(FormatError::ChunkIndexOutOfRange {
            chunk: index,
            len: 0,
        })
    }
}

#[test]
fn the_decoder_holds_its_output_and_two_rows() {
    for (w, h, depth, ct, interlace) in [(2048u32, 2048u32, 8u8, 2u8, 0u8), (1500, 1100, 16, 6, 1)]
    {
        let l = Layout::new(w, h, depth, ct, interlace).unwrap();
        let mut x = 7u32;
        let raw: Vec<u8> = (0..l.raw_len().unwrap())
            .map(|i| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((x >> 28) as u8).wrapping_add((i % 251) as u8)
            })
            .collect();
        let n = l.scanlines() as usize;
        let filters: Vec<u8> = (0..n).map(|i| (i % 5) as u8).collect();
        let text = l.refilter(&raw, &filters).unwrap();
        let up = l.refilter_uniform(&raw, FILTER_UP).unwrap();
        drop(raw);
        let rec = Record::new(RecordBody::PngFilter(PngFilterRecord {
            width: w,
            height: h,
            bit_depth: depth,
            color_type: ct,
            interlace,
            filters,
            original_hash: *blake3::hash(&text).as_bytes(),
        }));
        let len = text.len() as u64;
        let mut ctx = Ctx(rec);
        ALLOC.reset_peak();
        let before = ALLOC.current();
        let out = PngFilterDecoder
            .decode_in(&[0], &up, len, true, &Resources::default(), &mut ctx)
            .unwrap();
        let peak = (ALLOC.peak() - before) as u64;
        assert!(out == text);
        let rows = 2 * l.row_bytes(w);
        // The output, two rows, the record's parsed copy (filters) and small change.
        let allowed = len + rows + n as u64 + (64 << 10);
        assert!(peak <= allowed, "{peak} > {allowed}");
        // Within the deflate term of a stream with this text (beyond the input, which the
        // term's first byte per plain byte counts).
        assert!(peak <= memory_term(len, 0), "{peak}");
        assert!(len > 8 << 20);
    }
}
