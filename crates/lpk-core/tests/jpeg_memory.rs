//! The `jpeg-reconstruct` step's decode memory (spec section 8, "Resources"): a Lepton stream's
//! header is checked before the library decodes it, so no length in it makes `lepton_jpeg`
//! allocate past what the primary image explains, and the image's term (`image_term`) bounds
//! what the library allocates. Measured with a counting global allocator; its own test binary,
//! its tests one at a time, so no other test allocates meanwhile.
//!
//! The fuzzing finding (E4-6, CI run 40269058613, `oom-32b7…`): a stream whose `GRB` section
//! declares 4282667855 bytes of garbage made the library allocate them at once. It is kept as
//! `tests/fixtures/fuzz-jpeg-reconstruct-oom-32b7.bin`, in the fuzz target's layout (bytes 0..4
//! the record's `primary_len`, byte 4 flags, then the stream).
//!
//! The large images are written by a minimal encoder below (one grey level, so every block is
//! a zero DC difference and an end of block): a legitimate JPEG of any size, baseline or
//! progressive, without a third-party image.
#![allow(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use lpk_core::peel::jpeg::{
    image_term, memory_bound, read_stream_header, with_decode_threads, DecodeThreads,
};
use lpk_core::peel::{LEPTON_VERSION, MODEL_ALLOWANCE};
use lpk_core::{JpegDecoder, JpegPeel, PeelStage};
use lpk_format::{DecodeContext, FormatError, PrimitiveDecoder, Record, Refusal, Resources};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

/// The tests of this binary measure one at a time (the allocator counts every thread).
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// MSB-first bit writer with JPEG byte stuffing.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, code: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = self.acc << 1 | (code >> i & 1);
            self.n += 1;
            if self.n == 8 {
                let b = self.acc as u8;
                self.out.push(b);
                if b == 0xFF {
                    self.out.push(0);
                }
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        while self.n != 0 {
            self.put(1, 1);
        }
        self.out
    }
}

fn segment(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(body);
}

/// A one-grey-level YCbCr 4:2:0 JPEG of `w` x `h` (multiples of 16): baseline (SOF0, one
/// interleaved scan) or progressive (SOF2, an interleaved DC scan and one AC scan per
/// component, end-of-block runs as libjpeg writes them).
fn grey_jpeg(w: u16, h: u16, progressive: bool) -> Vec<u8> {
    assert!(w % 16 == 0 && h % 16 == 0);
    let mut out = vec![0xFF, 0xD8];
    let mut dqt = vec![0u8];
    dqt.extend_from_slice(&[1; 64]);
    segment(&mut out, 0xDB, &dqt);
    let mut sof = vec![8];
    sof.extend_from_slice(&h.to_be_bytes());
    sof.extend_from_slice(&w.to_be_bytes());
    sof.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 0, 3, 0x11, 0]);
    segment(&mut out, if progressive { 0xC2 } else { 0xC0 }, &sof);
    // DC table 0: the one symbol 0, code `0`. AC table 0: EOB0..EOB14 (0x00, 0x10, .., 0xE0),
    // 4-bit codes 0000..1110 in that order.
    let mut dht = vec![0x00, 1];
    dht.extend_from_slice(&[0; 15]);
    dht.push(0);
    dht.push(0x10);
    dht.extend_from_slice(&[0, 0, 0, 15]);
    dht.extend_from_slice(&[0; 12]);
    dht.extend((0..15u8).map(|n| n << 4));
    segment(&mut out, 0xC4, &dht);
    let mcus = u32::from(w / 16) * u32::from(h / 16);
    if !progressive {
        segment(&mut out, 0xDA, &[3, 1, 0, 2, 0, 3, 0, 0, 63, 0]);
        let mut b = Bits::default();
        for _ in 0..mcus * 6 {
            b.put(0, 1); // DC difference 0
            b.put(0, 4); // EOB0
        }
        out.extend_from_slice(&b.finish());
    } else {
        segment(&mut out, 0xDA, &[3, 1, 0, 2, 0, 3, 0, 0, 0, 0]);
        let mut b = Bits::default();
        for _ in 0..mcus * 6 {
            b.put(0, 1);
        }
        out.extend_from_slice(&b.finish());
        for (id, blocks) in [(1u8, mcus * 4), (2, mcus), (3, mcus)] {
            segment(&mut out, 0xDA, &[1, id, 0, 1, 63, 0]);
            let mut b = Bits::default();
            let mut left = blocks;
            while left > 0 {
                let run = left.min(0x7FFF);
                let s = 31 - run.leading_zeros();
                b.put(s, 4); // EOBs: code s
                b.put(run - (1 << s), s);
                left -= run;
            }
            out.extend_from_slice(&b.finish());
        }
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// Limits whose decode memory is `memory` with a block of `block` bytes.
fn limits(block: u64, memory: u64) -> Resources {
    Resources {
        max_window: 1 << 24,
        max_bwt_block: 1 << 20,
        max_block_plain: block,
        max_frame_payload: 1 << 30,
        memory,
    }
}

/// Decodes `stream` for `record` and returns the result with the peak above the start.
fn measured(
    stream: &[u8],
    record: Record,
    bound: u64,
    limits: &Resources,
    threads: Option<usize>,
) -> (Result<Vec<u8>, FormatError>, u64, Duration) {
    let mut ctx = Ctx(record);
    let budget = threads.map(DecodeThreads::new);
    let _token = budget.as_ref().map(|b| b.hold());
    let t = Instant::now();
    ALLOC.reset_peak();
    let before = ALLOC.current();
    let r = with_decode_threads(budget.clone(), || {
        JpegDecoder.decode_in(&[0], stream, bound, true, limits, &mut ctx)
    });
    let peak = (ALLOC.peak() - before) as u64;
    (r, peak, t.elapsed())
}

/// A JPEG record for a primary of `primary_len` bytes, nothing after it.
fn record_for(primary: &[u8]) -> Record {
    Record::new(lpk_format::RecordBody::Jpeg(lpk_format::JpegRecord {
        original_len: primary.len() as u64,
        primary_len: primary.len() as u64,
        trailing: Vec::new(),
        nested_trailing_chunks: Vec::new(),
        gainmaps: Vec::new(),
        lepton_version: LEPTON_VERSION,
        original_hash: *blake3::hash(primary).as_bytes(),
    }))
}

#[test]
fn the_fuzz_input_is_refused_before_the_library_with_a_small_peak() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let data = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fuzz-jpeg-reconstruct-oom-32b7.bin"),
    )
    .unwrap();
    let primary_len = u64::from(u32::from_le_bytes(data[..4].try_into().unwrap()));
    assert_eq!(data[4], 0, "flags of the finding");
    let stream = &data[5..];
    assert!(read_stream_header(stream, primary_len).is_none());
    let record = Record::new(lpk_format::RecordBody::Jpeg(lpk_format::JpegRecord {
        original_len: primary_len,
        primary_len,
        trailing: Vec::new(),
        nested_trailing_chunks: Vec::new(),
        gainmaps: Vec::new(),
        lepton_version: LEPTON_VERSION,
        original_hash: [0; 32],
    }));
    // The fuzz target's limits: a 16 MiB block, decode memory 112 MiB.
    let block = 1u64 << 24;
    let l = limits(block, block + MODEL_ALLOWANCE + (32 << 20));
    let (r, peak, took) = measured(stream, record, primary_len, &l, Some(1));
    assert!(
        matches!(
            r,
            Err(FormatError::BadRecord {
                reason: "lepton stream",
                ..
            })
        ),
        "{r:?}"
    );
    assert!(peak < 1 << 20, "peak {peak}");
    assert!(took < Duration::from_secs(5), "{took:?}");
}

/// The inflated header of `stream`, changed by `f`, compressed back in place.
fn rewrite_header(stream: &[u8], f: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let clen = u32::from_le_bytes(stream[24..28].try_into().unwrap()) as usize;
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&stream[28..28 + clen])
        .read_to_end(&mut raw)
        .unwrap();
    f(&mut raw);
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(9));
    z.write_all(&raw).unwrap();
    let z = z.finish().unwrap();
    let mut out = stream[..24].to_vec();
    out.extend_from_slice(&(z.len() as u32).to_le_bytes());
    out.extend_from_slice(&z);
    out.extend_from_slice(&stream[28 + clen..]);
    out
}

/// The offset of the `HH` section in an inflated header.
fn hh_at(raw: &[u8]) -> usize {
    let n = u32::from_le_bytes(raw[3..7].try_into().unwrap()) as usize;
    let mut p = 7 + n;
    loop {
        match &raw[p..p + 3] {
            b"P0D" => p += 4,
            [b'H', b'H', _] => return p,
            m => panic!("unexpected section {m:?}"),
        }
    }
}

#[test]
fn hostile_header_lengths_are_lepton_stream_before_the_library() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let jpeg = grey_jpeg(256, 256, false);
    let plan = JpegPeel::default().peel(&jpeg, 1 << 24).unwrap();
    let primary_len = plan.primary_len;
    let header = read_stream_header(&plan.stream, primary_len).unwrap();
    assert_eq!(header.frame.width, 256);
    let big = |m: &[u8], len: u32| {
        let mut s = m.to_vec();
        s.extend_from_slice(&len.to_le_bytes());
        s
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "GRB",
            rewrite_header(&plan.stream, |r| {
                r.extend_from_slice(&big(b"GRB", 0xFF44_534F));
            }),
        ),
        (
            "FRS",
            rewrite_header(&plan.stream, |r| {
                r.extend_from_slice(&big(b"FRS", primary_len as u32 + 1));
                r.extend(std::iter::repeat_n(0, primary_len as usize + 1));
            }),
        ),
        (
            "CRS",
            rewrite_header(&plan.stream, |r| {
                r.extend_from_slice(&big(b"CRS", u32::MAX));
            }),
        ),
        (
            "HDR",
            rewrite_header(&plan.stream, |r| {
                r[3..7].copy_from_slice(&(primary_len as u32 + 1).to_le_bytes());
            }),
        ),
        (
            "second HH",
            rewrite_header(&plan.stream, |r| {
                let at = hh_at(r);
                let n = usize::from(r[at + 2]);
                let hh = r[at..at + 3 + 16 * n].to_vec();
                r.extend_from_slice(&hh);
            }),
        ),
        (
            "rows going down",
            rewrite_header(&plan.stream, |r| {
                let at = hh_at(r);
                // One partition more, starting at row 0 after the others.
                r[at + 2] += 1;
                let n = usize::from(r[at + 2]);
                let end = at + 3 + 16 * (n - 1);
                let mut p = r[at + 3..at + 19].to_vec();
                p[0..2].copy_from_slice(&0u16.to_le_bytes());
                p[2..6].copy_from_slice(&0u32.to_le_bytes());
                if n > 1 {
                    r[at + 3..at + 5].copy_from_slice(&1u16.to_le_bytes());
                }
                r.splice(end..end, p);
            }),
        ),
        (
            "segments past the primary",
            rewrite_header(&plan.stream, |r| {
                let at = hh_at(r);
                r[at + 5..at + 9].copy_from_slice(&u32::MAX.to_le_bytes());
            }),
        ),
        (
            "inflated header past its cap",
            rewrite_header(&plan.stream, |r| {
                for _ in 0..(primary_len as usize * 2 / 4 + (64 << 10) / 4 + 1) {
                    r.extend_from_slice(b"P0D\0");
                }
            }),
        ),
        (
            "unknown section",
            rewrite_header(&plan.stream, |r| r.extend_from_slice(b"PGR\0\0\0\0")),
        ),
        (
            "data ending inside a section",
            rewrite_header(&plan.stream, |r| r.extend_from_slice(b"EEE\0\0")),
        ),
    ];
    let block = 1u64 << 24;
    let l = limits(block, block + (512 << 20));
    for (name, stream) in cases {
        assert!(
            read_stream_header(&stream, primary_len).is_none(),
            "{name} passed the check"
        );
        let (r, peak, _) = measured(&stream, record_for(&jpeg), primary_len, &l, Some(1));
        assert!(
            matches!(
                r,
                Err(FormatError::BadRecord {
                    reason: "lepton stream",
                    ..
                })
            ),
            "{name}: {r:?}"
        );
        assert!(peak < 1 << 20, "{name}: peak {peak}");
    }
    // The unchanged stream still decodes.
    let (r, _, _) = measured(&plan.stream, record_for(&jpeg), primary_len, &l, Some(1));
    assert_eq!(r.unwrap(), jpeg);
}

#[test]
fn small_images_declare_revision_1_1s_term() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for name in ["baseline.jpg", "progressive.jpg", "secondary.jpg"] {
        let jpeg = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .unwrap();
        let plan = JpegPeel::default().peel(&jpeg, 1 << 24).unwrap();
        let h = read_stream_header(&plan.stream, plan.primary_len).unwrap();
        assert_eq!(plan.memory, memory_bound(&h.frame), "{name}");
        assert_eq!(
            plan.memory,
            image_term(&h, plan.primary_len, plan.stream.len() as u64)
        );
    }
}

/// A large image round-trips, and the decode's peak (one thread and the library's eight) stays
/// within the record's term plus the block (the output).
fn round_trip_within_the_term(w: u16, h: u16, progressive: bool) -> (u64, u64) {
    let jpeg = grey_jpeg(w, h, progressive);
    let block = 1u64 << 26;
    let plan = JpegPeel::default().peel(&jpeg, block).unwrap();
    let header = read_stream_header(&plan.stream, plan.primary_len).unwrap();
    assert_eq!(
        plan.memory,
        image_term(&header, plan.primary_len, plan.stream.len() as u64)
    );
    let l = limits(block, block + plan.memory);
    for threads in [Some(1), None] {
        let (r, peak, _) = measured(
            &plan.stream,
            record_for(&jpeg),
            plan.primary_len,
            &l,
            threads,
        );
        assert!(r.unwrap() == jpeg);
        let allowed = plan.primary_len + plan.memory;
        eprintln!(
            "{w}x{h} progressive={progressive} threads={threads:?}: peak {peak}, term {}, revision 1.1 term {}",
            plan.memory,
            memory_bound(&header.frame)
        );
        assert!(peak <= allowed, "peak {peak} > {allowed}");
    }
    // One byte less decode memory than the term is refused, before decoding.
    let l = limits(block, block + plan.memory - 1);
    let (r, peak, _) = measured(
        &plan.stream,
        record_for(&jpeg),
        plan.primary_len,
        &l,
        Some(1),
    );
    assert!(
        matches!(r, Err(FormatError::Refused(Refusal { field: "decode_memory", needed, .. })) if needed == block + plan.memory),
        "{r:?}"
    );
    assert!(peak < 1 << 20, "peak {peak}");
    (plan.memory, memory_bound(&header.frame))
}

#[test]
fn a_large_baseline_jpeg_round_trips_within_its_term() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let (term, old) = round_trip_within_the_term(2048, 2048, false);
    assert_eq!(
        term, old,
        "a baseline image of this size declares what it did"
    );
}

#[test]
fn a_large_progressive_jpeg_round_trips_within_its_term() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let (term, old) = round_trip_within_the_term(2048, 2048, true);
    assert!(
        term > old,
        "a large progressive image declares more: {term} vs {old}"
    );
}
