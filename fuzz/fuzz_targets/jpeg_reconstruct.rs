#![no_main]
//! Revision 1.1's `jpeg-reconstruct` step (`lpk_core::JpegDecoder::decode_in`, spec section 8,
//! "Decoding `jpeg-reconstruct`") over an arbitrary Lepton stream and record: no panic (the
//! library's panics are caught by the decoder, but libFuzzer's hook aborts on any panic, so a
//! caught one is a finding too); an output is exactly the record's primary length within the
//! step's bound; nothing is allocated past the decode memory (a counting allocator): the
//! decoder refuses an image whose memory term does not fit `memory - max_block_plain`, and its
//! output is at most `max_block_plain`.
//!
//! Layout: bytes 0..4 = the record's `primary_len` (u32 LE); byte 4 = flags (bit 0: a wrong
//! `lepton_version`; bit 1: the trailing data is nested, two chunk ids and one secondary image
//! read through the context, whose chunks are cut from the end of the input; bit 2: the step's
//! bound is one byte short of `primary_len`; bit 3: the decode memory is cut to the block plus
//! 1 MiB, so most images are `Refused`); then the Lepton stream. The seeds are the streams
//! `JpegPeel::peel` writes for the committed JPEG fixtures, with their primary lengths.
use libfuzzer_sys::fuzz_target;
use lpk_core::peel::jpeg::{with_decode_threads, DecodeThreads};
use lpk_core::peel::{JpegDecoder, LEPTON_VERSION, MODEL_ALLOWANCE};
use lpk_format::{
    DecodeContext, FormatError, JpegRecord, PrimitiveDecoder, Record, RecordBody, Resources,
    SecondaryImage,
};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

/// The largest primary image a run decodes.
const MAX_BLOCK_PLAIN: u64 = 1 << 24;

struct Ctx {
    record: Record,
    chunks: Vec<Vec<u8>>,
}

impl DecodeContext for Ctx {
    fn block(&self) -> usize {
        0
    }
    fn record(&mut self, _id: u64) -> Result<Record, FormatError> {
        Ok(self.record.clone())
    }
    fn chunk(&mut self, _record: u64, index: u64) -> Result<Vec<u8>, FormatError> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.chunks.get(i))
            .cloned()
            .ok_or(FormatError::ChunkIndexOutOfRange {
                chunk: index,
                len: self.chunks.len() as u64,
            })
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 5 {
        return;
    }
    let primary_len = u64::from(u32::from_le_bytes([data[0], data[1], data[2], data[3]]));
    let flags = data[4];
    let mut stream = &data[5..];
    let mut chunks = Vec::new();
    let mut nested = Vec::new();
    let mut gainmaps = Vec::new();
    if flags & 2 != 0 && stream.len() >= 24 {
        // Three 8-byte chunks from the end: trailing, secondary image, trailing.
        let (s, tail) = stream.split_at(stream.len() - 24);
        stream = s;
        chunks = tail.chunks(8).map(<[u8]>::to_vec).collect();
        nested = vec![0, 2];
        gainmaps.push(SecondaryImage {
            offset: primary_len + 8,
            len: 8,
            chunks: vec![1],
        });
    }
    let original_len = primary_len + chunks.iter().map(|c| c.len() as u64).sum::<u64>();
    let record = Record::new(RecordBody::Jpeg(JpegRecord {
        original_len,
        primary_len,
        trailing: Vec::new(),
        nested_trailing_chunks: nested,
        gainmaps,
        lepton_version: if flags & 1 != 0 {
            LEPTON_VERSION.wrapping_add(1)
        } else {
            LEPTON_VERSION
        },
        original_hash: [0; 32],
    }));
    let bound = if flags & 4 != 0 {
        primary_len.saturating_sub(1)
    } else {
        primary_len
    }
    .min(MAX_BLOCK_PLAIN);
    let memory = if flags & 8 != 0 {
        MAX_BLOCK_PLAIN + (1 << 20)
    } else {
        MAX_BLOCK_PLAIN + MODEL_ALLOWANCE + (32 << 20)
    };
    let limits = Resources {
        max_window: 1 << 24,
        max_bwt_block: 1 << 20,
        max_block_plain: MAX_BLOCK_PLAIN,
        max_frame_payload: 1 << 24,
        memory,
    };
    let mut ctx = Ctx { record, chunks };
    // One decode thread: the run is deterministic and every allocation is this thread's.
    let budget = DecodeThreads::new(1);
    let _token = budget.hold();
    ALLOC.reset_peak();
    let before = ALLOC.current();
    // params: the record id 0 as a varint.
    let r = with_decode_threads(Some(budget.clone()), || {
        JpegDecoder.decode_in(&[0], stream, bound, true, &limits, &mut ctx)
    });
    let peak = (ALLOC.peak() - before) as u64;
    assert!(
        peak <= memory,
        "peak {peak} over the decode memory {memory}"
    );
    match r {
        Ok(out) => {
            assert_eq!(out.len() as u64, primary_len);
            assert!(out.len() as u64 <= bound);
        }
        Err(FormatError::Refused(_)) => {}
        Err(_) => {}
    }
});
