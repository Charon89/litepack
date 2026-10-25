#![no_main]
//! Revision 1.4's `png-filter` step over arbitrary records and input (spec section 8, "Decoding
//! `png-filter`"): no panic; an output is exactly the record's filtered length, within the
//! bound, and re-filters back to the input's image; nothing is allocated past the output, two
//! rows and the record's copy (a counting allocator).
//!
//! Layout: bytes 0..2 = width (u16 LE), 2..4 = height (u16 LE), 4 = bit depth, 5 = colour
//! type, 6 = interlace, 7 = flags (bit 0: the step is the last one, the bound being the record's
//! filtered length when bit 1 is set too; bit 2: the record's hash is the output's, computed
//! from the input so that a well-formed case passes check (7)); 8..10 = the bound (u16 LE); then
//! one filter byte per scanline the record claims (as many as are left, capped), then the input.
use libfuzzer_sys::fuzz_target;
use lpk_core::peel::png::Layout;
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

fuzz_target!(|data: &[u8]| {
    if data.len() < 10 {
        return;
    }
    let width = u32::from(u16::from_le_bytes([data[0], data[1]]));
    let height = u32::from(u16::from_le_bytes([data[2], data[3]]));
    let (bit_depth, color_type, interlace, flags) = (data[4], data[5], data[6] & 1, data[7]);
    let mut bound = u64::from(u16::from_le_bytes([data[8], data[9]]));
    let rest = &data[10..];
    let mut rec = PngFilterRecord {
        width,
        height,
        bit_depth,
        color_type,
        interlace,
        filters: Vec::new(),
        original_hash: [0; 32],
    };
    let claimed = usize::try_from(rec.scanlines()).unwrap_or(usize::MAX);
    let n = claimed.min(rest.len()).min(1 << 16);
    rec.filters = rest[..n].to_vec();
    let input = &rest[n..];
    let last = flags & 1 != 0;
    if last && flags & 2 != 0 {
        bound = rec.filtered_len().unwrap_or(bound);
    }
    if flags & 4 != 0 {
        // The output a correct decoder gives for this input, when the image is well formed.
        if let Ok(l) = Layout::new(width, height, bit_depth, color_type, interlace) {
            if let Ok(out) = l.transcode(input, &rec.filters) {
                rec.original_hash = *blake3::hash(&out).as_bytes();
            }
        }
    }
    let filters_len = rec.filters.len() as u64;
    let mut ctx = Ctx(Record::new(RecordBody::PngFilter(rec.clone())));
    ALLOC.reset_peak();
    let before = ALLOC.current();
    let r = PngFilterDecoder.decode_in(&[0], input, bound, last, &Resources::default(), &mut ctx);
    let peak = (ALLOC.peak() - before) as u64;
    // The output (at most the bound), two rows (each at most the output), the record's copy.
    assert!(
        peak <= 3 * bound + filters_len + 4096,
        "peak {peak} over bound {bound}"
    );
    if let Ok(out) = r {
        assert_eq!(Some(out.len() as u64), rec.filtered_len());
        assert!(out.len() as u64 <= bound);
        assert_eq!(out.len(), input.len());
        assert!(rec.verify_hash(&out));
    }
});
