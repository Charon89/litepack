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
    assert!(w.is_multiple_of(16) && h.is_multiple_of(16));
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
    assert!(lpk_check::jpeg::stream_header(0, stream, primary_len).is_err());
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
        assert_eq!(
            lpk_check::jpeg::stream_header(0, &stream, primary_len)
                .unwrap_err()
                .detail,
            "bad record 0: lepton stream",
            "{name} passed lpk-check"
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

/// The nightly fuzz finding (scheduled run 40444702572, `crash-b6df…`): a `GRB` section longer
/// than the JPEG length the fixed header declares made lepton_jpeg 0.5.8 subtract with overflow
/// (`lepton_file_reader.rs:299`). Both decoders refuse it before the library, with a small peak.
#[test]
fn garbage_past_the_jpeg_length_is_refused_before_the_library() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let data = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fuzz-jpeg-reconstruct-crash-b6df.bin"),
    )
    .unwrap();
    let primary_len = u64::from(u32::from_le_bytes(data[..4].try_into().unwrap()));
    assert_eq!(data[4], 0, "flags of the finding");
    let stream = &data[5..];
    assert!(read_stream_header(stream, primary_len).is_none());
    assert_eq!(
        lpk_check::jpeg::stream_header(0, stream, primary_len)
            .unwrap_err()
            .detail,
        "bad record 0: lepton stream"
    );
    let record = Record::new(lpk_format::RecordBody::Jpeg(lpk_format::JpegRecord {
        original_len: primary_len,
        primary_len,
        trailing: Vec::new(),
        nested_trailing_chunks: Vec::new(),
        gainmaps: Vec::new(),
        lepton_version: LEPTON_VERSION,
        original_hash: [0; 32],
    }));
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

/// `stream` with the fixed header's JPEG length set to `len`.
fn with_jpeg_len(stream: &[u8], len: u32) -> Vec<u8> {
    let mut s = stream.to_vec();
    s[20..24].copy_from_slice(&len.to_le_bytes());
    s
}

/// The inflated header's `HH` section with byte `at` of every partition set to `v`.
fn set_partition_byte(stream: &[u8], at: usize, v: u8) -> Vec<u8> {
    rewrite_header(stream, |r| {
        let hh = hh_at(r);
        for p in 0..usize::from(r[hh + 2]) {
            r[hh + 3 + 16 * p + at] = v;
        }
    })
}

/// An appended section `m`, a u32 LE `len` and `body`.
fn appended(stream: &[u8], m: &[u8], len: u32, body: &[u8]) -> Vec<u8> {
    rewrite_header(stream, |r| {
        r.extend_from_slice(m);
        r.extend_from_slice(&len.to_le_bytes());
        r.extend_from_slice(body);
    })
}

/// An `EEE` section whose `max_dpos` are all `dpos`.
fn early_eof(stream: &[u8], dpos: u32) -> Vec<u8> {
    rewrite_header(stream, |r| {
        r.extend_from_slice(b"EEE");
        r.extend_from_slice(&[0; 12]);
        for _ in 0..4 {
            r.extend_from_slice(&dpos.to_le_bytes());
        }
    })
}

/// Each header field lepton_jpeg 0.5.8 does unchecked arithmetic or indexing with, on both
/// sides of the bound both decoders give it: past it, `lepton stream` before the library with a
/// small peak; at it, the header walk passes.
#[test]
fn header_fields_the_library_trusts_are_bounded_by_both_decoders() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let jpeg = grey_jpeg(256, 256, false);
    let plan = JpegPeel::default().peel(&jpeg, 1 << 24).unwrap();
    let s = &plan.stream;
    let pjpeg = grey_jpeg(256, 256, true);
    let pplan = JpegPeel::default().peel(&pjpeg, 1 << 24).unwrap();
    let ps = &pplan.stream;
    let scans = pjpeg.windows(2).filter(|w| *w == [0xFF, 0xDA]).count() as u32;
    assert_eq!(scans, 4);
    let counts = |n: u32| -> Vec<u8> { (0..n).flat_map(|_| 1u32.to_le_bytes()).collect() };
    let garbage = vec![0u8; 100];
    // (name, refused, passing, the primary both belong to)
    type Case<'a> = (&'a str, Vec<u8>, Vec<u8>, &'a [u8]);
    let cases: Vec<Case> = vec![
        (
            "garbage past the JPEG length (lepton_file_reader.rs:299)",
            with_jpeg_len(&appended(s, b"GRB", 100, &garbage), 99),
            with_jpeg_len(&appended(s, b"GRB", 100, &garbage), 100),
            &jpeg,
        ),
        (
            "the assumed EOI past the JPEG length (lepton_file_reader.rs:299)",
            with_jpeg_len(&appended(s, b"GRB", 2, &[0xFF, 0xD9]), 1),
            with_jpeg_len(&appended(s, b"GRB", 1, &[0xD9]), 1),
            &jpeg,
        ),
        (
            "overhang bits past 7 (bit_writer.rs:143)",
            set_partition_byte(s, 7, 65),
            set_partition_byte(s, 7, 7),
            &jpeg,
        ),
        (
            "max_dpos of u32::MAX (truncate_components.rs:69)",
            early_eof(s, u32::MAX),
            early_eof(s, u32::MAX - 1),
            &jpeg,
        ),
        (
            "first restart error above 1 (lepton_file_reader.rs:393)",
            appended(s, b"FRS", 1, &[2]),
            appended(s, b"FRS", 2, &[1, 255]),
            &jpeg,
        ),
        (
            "restart counts fewer than the scans (jpeg_write.rs:323)",
            appended(ps, b"CRS", scans - 1, &counts(scans - 1)),
            appended(ps, b"CRS", scans, &counts(scans)),
            &pjpeg,
        ),
    ];
    let block = 1u64 << 24;
    let l = limits(block, block + (512 << 20));
    for (name, refused, passing, primary) in cases {
        let primary_len = primary.len() as u64;
        assert!(
            read_stream_header(&refused, primary_len).is_none(),
            "{name} passed the check"
        );
        assert_eq!(
            lpk_check::jpeg::stream_header(0, &refused, primary_len)
                .unwrap_err()
                .detail,
            "bad record 0: lepton stream",
            "{name} passed lpk-check"
        );
        let (r, peak, _) = measured(&refused, record_for(primary), primary_len, &l, Some(1));
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
        assert!(
            read_stream_header(&passing, primary_len).is_some(),
            "{name}: the bound itself was refused"
        );
        assert!(
            lpk_check::jpeg::stream_header(0, &passing, primary_len).is_ok(),
            "{name}: the bound itself was refused by lpk-check"
        );
    }
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
    // The independent decoder computes the same term.
    let checked = lpk_check::jpeg::stream_header(0, &plan.stream, plan.primary_len).unwrap();
    assert_eq!(
        checked.term(plan.primary_len, plan.stream.len() as u64),
        plan.memory
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

/// An archive with `jpeg` peeled as one entry whose block declares `declare(plan)` (the term an
/// earlier writer declared under revision 1.1, or the plan's).
fn archive_declaring(jpeg: &[u8], declare: &dyn Fn(&lpk_core::peel::PeelPlan) -> u64) -> Vec<u8> {
    use lpk_format::{Encoded, EntryFlags, Graph, GraphResources, Step, Writer, WriterOptions};
    let peel = JpegPeel::default();
    let plan = peel.peel(jpeg, 1 << 24).unwrap();
    assert!(plan.nested.is_empty());
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, WriterOptions::default(), 1).unwrap();
    w.begin_entry("p.jpg", EntryFlags::EMPTY, 0).unwrap();
    let id = w.add_record(peel.record(&plan, &[])).unwrap();
    let mut params = Vec::new();
    lpk_format::varint::write(&mut params, id).unwrap();
    let encoded = Encoded {
        graph: Graph {
            steps: vec![Step {
                primitive: plan.primitive,
                params,
            }],
        },
        bytes: plan.stream.clone(),
        resources: GraphResources::default(),
        memory: 0,
    };
    w.add_part_encoded(0, jpeg, encoded, declare(&plan))
        .unwrap();
    w.end_entry().unwrap();
    w.finish().unwrap();
    out
}

/// Extracts every file of `bytes` with the full reader under a memory resource of `budget`.
fn extract_with(bytes: &[u8], budget: u64) -> Result<Vec<u8>, FormatError> {
    let r = Resources {
        memory: budget,
        ..Resources::default()
    };
    let mut a = lpk_format::Archive::open(std::io::Cursor::new(bytes.to_vec()), &r)?;
    lpk_core::register_full_reader(&mut a);
    let t = a.entry_table()?;
    let entries: Vec<_> = t.table()?.iter().collect::<Result<_, _>>()?;
    let mut v = Vec::new();
    for e in &entries {
        a.extract(e, &mut v)?;
    }
    Ok(v)
}

/// Revision 1.1 is final: an archive whose large progressive JPEG declares exactly revision
/// 1.1's term (as an earlier writer did) still extracts, byte-exact, when the reader's own memory
/// covers the library's need; under less it is refused (`decode_memory`) before decoding. Both
/// decoders agree.
#[test]
fn an_archive_declaring_revision_1_1s_term_extracts_within_a_budget_covering_the_library() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let jpeg = grey_jpeg(2048, 2048, true);
    let old = archive_declaring(&jpeg, &|plan| {
        let h = read_stream_header(&plan.stream, plan.primary_len).unwrap();
        memory_bound(&h.frame)
    });
    let a = lpk_format::Archive::open(std::io::Cursor::new(old.clone()), &Resources::default())
        .unwrap();
    let env = a.index().envelope;
    let plan = JpegPeel::default().peel(&jpeg, 1 << 24).unwrap();
    let h = read_stream_header(&plan.stream, plan.primary_len).unwrap();
    let library = image_term(&h, plan.primary_len, plan.stream.len() as u64);
    assert_eq!(
        env.decode_memory,
        env.max_block_plain + memory_bound(&h.frame)
    );
    assert!(library > memory_bound(&h.frame));
    let enough = env.max_block_plain + library;
    assert_eq!(extract_with(&old, enough).unwrap(), jpeg);
    match extract_with(&old, enough - 1) {
        Err(FormatError::Refused(Refusal {
            field: "decode_memory",
            needed,
            allowed,
        })) => assert_eq!((needed, allowed), (enough, enough - 1)),
        r => panic!("{:?}", r.map(|v| v.len())),
    }
    // lpk-check: the same outcomes.
    let check = |memory: u64| {
        let mut o = lpk_check::archive::Options::default();
        o.resources.memory = memory;
        let mut a = lpk_check::archive::Archive::open(old.clone(), o)?;
        let e = a.entries()?;
        let file = e.iter().find(|e| e.path == "p.jpg").unwrap().clone();
        a.read_file(&file)
    };
    assert_eq!(check(enough).unwrap(), jpeg);
    let e = check(enough - 1).unwrap_err();
    assert_eq!(e.class, "Refused");
    assert!(e.detail.contains("decode_memory"), "{}", e.detail);
    // A new archive declares the library's need, and extracts within that.
    assert_eq!(plan.memory, library);
    let new = archive_declaring(&jpeg, &|plan| plan.memory);
    assert_eq!(extract_with(&new, enough).unwrap(), jpeg);
    // Through the pooled engine: a declaration that covers the need keeps the concurrency (no
    // decode runs alone).
    let dir = tempfile::tempdir().unwrap();
    let arch = dir.path().join("new.lpk");
    std::fs::write(&arch, &new).unwrap();
    let opts = lpk_core::ExtractOptions {
        threads: Some(8),
        memory: Some(enough + (64 << 20)),
        ..lpk_core::ExtractOptions::default()
    };
    let out = dir.path().join("out");
    let s =
        lpk_core::extract_file(&arch, &out, &[], None, &lpk_core::DefaultPolicy, &opts).unwrap();
    assert_eq!(s.alone_decodes, 0);
    assert_eq!(s.concurrent_while_alone, 0);
    assert!(std::fs::read(out.join("p.jpg")).unwrap() == jpeg);
}

/// An archive of `jpegs` (entries `p0.jpg`, `p1.jpg`, ..), each peeled into a block of its own
/// declaring `declare(plan)`.
fn archive_of(jpegs: &[Vec<u8>], declare: &dyn Fn(&lpk_core::peel::PeelPlan) -> u64) -> Vec<u8> {
    use lpk_format::{Encoded, EntryFlags, Graph, GraphResources, Step, Writer, WriterOptions};
    let peel = JpegPeel::default();
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, WriterOptions::default(), 1).unwrap();
    for (i, jpeg) in jpegs.iter().enumerate() {
        let plan = peel.peel(jpeg, 1 << 24).unwrap();
        w.begin_entry(&format!("p{i}.jpg"), EntryFlags::EMPTY, 0)
            .unwrap();
        let id = w.add_record(peel.record(&plan, &[])).unwrap();
        let mut params = Vec::new();
        lpk_format::varint::write(&mut params, id).unwrap();
        let encoded = Encoded {
            graph: Graph {
                steps: vec![Step {
                    primitive: plan.primitive,
                    params,
                }],
            },
            bytes: plan.stream.clone(),
            resources: GraphResources::default(),
            memory: 0,
        };
        w.add_part_encoded(0, jpeg, encoded, declare(&plan))
            .unwrap();
        w.end_entry().unwrap();
    }
    w.finish().unwrap();
    out
}

/// `--memory` holds across workers: an archive of several large progressive JPEGs, each
/// declaring only revision 1.1's term, extracted by the pooled engine at eight threads. Each
/// decode needs more than the archive declares, so each runs alone through the pool's gate; the
/// process peak stays within the budget and every file is byte-exact.
#[test]
fn over_declared_decodes_run_alone_and_the_peak_stays_within_the_budget() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let jpegs: Vec<Vec<u8>> = [2048u16, 2032, 2016, 2000]
        .iter()
        .map(|&w| grey_jpeg(w, 2048, true))
        .collect();
    let old = archive_of(&jpegs, &|plan| {
        let h = read_stream_header(&plan.stream, plan.primary_len).unwrap();
        memory_bound(&h.frame)
    });
    let dir = tempfile::tempdir().unwrap();
    let arch = dir.path().join("old.lpk");
    std::fs::write(&arch, &old).unwrap();
    let budget = 640u64 << 20;
    let opts = lpk_core::ExtractOptions {
        threads: Some(8),
        memory: Some(budget),
        ..lpk_core::ExtractOptions::default()
    };
    let out = dir.path().join("out");
    let before = ALLOC.current();
    ALLOC.reset_peak();
    let s =
        lpk_core::extract_file(&arch, &out, &[], None, &lpk_core::DefaultPolicy, &opts).unwrap();
    let peak = (ALLOC.peak() - before) as u64;
    eprintln!(
        "workers {}, in flight {}, decoder {}, alone {}, concurrent while alone {}, peak {peak}, budget {budget}",
        s.workers, s.in_flight, s.memory.decoder, s.alone_decodes, s.concurrent_while_alone
    );
    assert!(s.workers > 1, "the pool runs several workers");
    assert_eq!(s.alone_decodes, jpegs.len() as u64);
    // While a decode ran alone, no other block decoded.
    assert_eq!(s.concurrent_while_alone, 1);
    assert!(peak <= budget, "peak {peak} > budget {budget}");
    for (i, j) in jpegs.iter().enumerate() {
        assert!(std::fs::read(out.join(format!("p{i}.jpg"))).unwrap() == *j);
    }
}

/// D-100: a header ending in a marker cut short (two bytes) counts those bytes in both decoders,
/// which compute the same term.
#[test]
fn a_header_ending_in_two_stray_bytes_gives_both_decoders_the_same_term() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let jpeg = grey_jpeg(256, 256, false);
    let plan = JpegPeel::default().peel(&jpeg, 1 << 24).unwrap();
    let stream = rewrite_header(&plan.stream, |r| r.extend_from_slice(b"P0"));
    let mine = read_stream_header(&stream, plan.primary_len).unwrap();
    let theirs = lpk_check::jpeg::stream_header(0, &stream, plan.primary_len).unwrap();
    let plain = read_stream_header(&plan.stream, plan.primary_len).unwrap();
    assert_eq!(mine.header_len, plain.header_len + 2);
    assert_eq!(theirs.header_len, mine.header_len);
    let len = stream.len() as u64;
    assert_eq!(
        theirs.term(plan.primary_len, len),
        image_term(&mine, plan.primary_len, len)
    );
}

/// A truncated JPEG (no EOI: the library's early-EOF path, an `EEE` section, whose last segment
/// the library cuts) encoded by the library as another writer could, decoded through both
/// decoders' checks: it passes them and round-trips. (The peel itself never writes one: it peels
/// a primary only up to its EOI.)
#[test]
fn an_early_eof_jpeg_round_trips() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let full = grey_jpeg(512, 512, false);
    let sos = full.windows(2).rposition(|w| w == [0xFF, 0xDA]).unwrap();
    let data = sos + 2 + 12;
    let jpeg = full[..data + (full.len() - 2 - data) / 3].to_vec();
    let f = lpk_core::peel::jpeg::features(1 << 24);
    let mut w = std::io::Cursor::new(Vec::new());
    lepton_jpeg::encode_lepton(
        &mut std::io::Cursor::new(&jpeg[..]),
        &mut w,
        &f,
        &lepton_jpeg::DEFAULT_THREAD_POOL,
    )
    .unwrap();
    let stream = w.into_inner();
    let clen = u32::from_le_bytes(stream[24..28].try_into().unwrap()) as usize;
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&stream[28..28 + clen])
        .read_to_end(&mut raw)
        .unwrap();
    assert!(raw.windows(3).any(|w| w == b"EEE"), "an early-EOF stream");
    let primary_len = jpeg.len() as u64;
    let h = read_stream_header(&stream, primary_len).unwrap();
    let theirs = lpk_check::jpeg::stream_header(0, &stream, primary_len).unwrap();
    let term = image_term(&h, primary_len, stream.len() as u64);
    assert_eq!(theirs.term(primary_len, stream.len() as u64), term);
    let block = 1u64 << 24;
    let l = limits(block, block + term);
    let (r, _, _) = measured(&stream, record_for(&jpeg), primary_len, &l, Some(1));
    assert!(r.unwrap() == jpeg);
}

/// A decode the extraction's budget cannot run alone is refused with the `--memory` that would
/// let it run, in the message the CLI and the API show; extracting with exactly that `--memory`
/// succeeds.
#[test]
fn the_refusal_names_a_sufficient_memory_option() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let jpegs: Vec<Vec<u8>> = [2048u16, 2032, 2016]
        .iter()
        .map(|&w| grey_jpeg(w, 2048, true))
        .collect();
    let old = archive_of(&jpegs, &|plan| {
        let h = read_stream_header(&plan.stream, plan.primary_len).unwrap();
        memory_bound(&h.frame)
    });
    let env = lpk_format::Archive::open(std::io::Cursor::new(old.clone()), &Resources::default())
        .unwrap()
        .index()
        .envelope;
    let dir = tempfile::tempdir().unwrap();
    let arch = dir.path().join("old.lpk");
    std::fs::write(&arch, &old).unwrap();
    for threads in [1usize, 8] {
        let run = |memory: u64, out: &str| {
            let opts = lpk_core::ExtractOptions {
                threads: Some(threads),
                memory: Some(memory),
                ..lpk_core::ExtractOptions::default()
            };
            lpk_core::extract_file(
                &arch,
                &dir.path().join(out),
                &[],
                None,
                &lpk_core::DefaultPolicy,
                &opts,
            )
        };
        let small = env.decode_memory + (16 << 20);
        let e = run(small, &format!("refused-{threads}")).unwrap_err();
        let message = e.to_string();
        let lpk_core::CoreError::Decode {
            source: FormatError::Refused(r),
            ..
        } = e
        else {
            panic!("{message}")
        };
        assert_eq!(r.field, "--memory");
        assert_eq!(r.allowed, small);
        assert_eq!(r.needed % (1 << 20), 0, "whole MiB");
        assert!(r.needed > small);
        let mib = r.needed >> 20;
        assert!(
            message.contains(&format!("needs --memory {mib} MiB or more")),
            "{message}"
        );
        eprintln!("threads {threads}: {message}");
        let s = run(r.needed, &format!("ok-{threads}")).unwrap();
        assert_eq!(s.alone_decodes, jpegs.len() as u64);
        for (i, j) in jpegs.iter().enumerate() {
            let got = std::fs::read(dir.path().join(format!("ok-{threads}/p{i}.jpg"))).unwrap();
            assert!(got == *j);
        }
    }
}

/// The open compatibility question of review round 3 (segment sizes): could the writer have
/// produced an early-EOF stream from a file that still ends in EOI? A progressive JPEG whose
/// last scan is cut short but ends in EOI, through the writer's own path (the peel): the peel
/// refuses it and stores the file as it is (`Cause::Other`), because lepton_jpeg 0.5.8 refuses
/// to encode it ("invalid reset ff d9 code found in stream": the EOI arrives inside the scan).
/// The library's early-EOF path is reached only by a file without EOI, which the peel never
/// hands it (`Cause::NoEoi`), so no archive the reference writer wrote holds such a stream and
/// the segment rule cannot refuse one.
#[test]
fn a_progressive_jpeg_with_its_last_scan_cut_short_is_refused_by_the_peel() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let full = grey_jpeg(512, 512, true);
    let sos = full.windows(2).rposition(|w| w == [0xFF, 0xDA]).unwrap();
    let data = sos + 2 + 8;
    let end = full.len() - 2;
    let mut jpeg = full[..data + (end - data) / 2].to_vec();
    jpeg.extend_from_slice(&[0xFF, 0xD9]);
    let r = JpegPeel::default().peel(&jpeg, 1 << 24);
    assert!(
        matches!(r, Err(lpk_core::Fallback::Jpeg(lpk_core::Cause::Other))),
        "{:?}",
        r.map(|p| p.primary_len)
    );
    let f = lpk_core::peel::jpeg::features(1 << 24);
    let e = lepton_jpeg::encode_lepton(
        &mut std::io::Cursor::new(&jpeg[..]),
        &mut std::io::Cursor::new(Vec::new()),
        &f,
        &lepton_jpeg::DEFAULT_THREAD_POOL,
    )
    .unwrap_err();
    assert!(
        e.message().starts_with("invalid reset ff d9"),
        "{}",
        e.message()
    );
    // The whole file still round-trips through the Fast pipeline, stored as it is.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("in")).unwrap();
    std::fs::write(dir.path().join("in").join("cut.jpg"), &jpeg).unwrap();
    let mut bytes = Vec::new();
    lpk_core::archive_fast(
        &dir.path().join("in"),
        &mut bytes,
        lpk_core::FastOptions::default(),
    )
    .unwrap();
    let mut a =
        lpk_format::Archive::open(std::io::Cursor::new(bytes), &Resources::default()).unwrap();
    lpk_core::register_full_reader(&mut a);
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    let e = entries.iter().find(|e| e.path == "cut.jpg").unwrap();
    let mut got = Vec::new();
    a.extract(e, &mut got).unwrap();
    assert!(got == jpeg);
}
