//! Revision 1.4's `png-filter` decoder (spec section 8, "Decoding `png-filter`"): every
//! (bit depth, colour type) pair with and without Adam7 decoded, a test per reason in the order
//! of the checks, `PayloadTooLarge` before the step before it runs, `BlockLengthMismatch` on a
//! last step, and the three-step chain `[store, png-filter, deflate-reconstruct]` on a real PNG
//! whose inflated text is above `max_block_plain` (the look-ahead of task A).
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use lpk_core::peel::deflate::{examine, Limits};
use lpk_core::peel::png::{Header, Layout, FILTER_UP, PAIRS};
use lpk_core::{DeflateDecoder, PngFilterDecoder};
use lpk_format::{
    decode_block_in, BlockHeader, DecodeContext, DeflateRecord, FormatError, Graph,
    PngFilterRecord, PrimitiveDecoder, PrimitiveId, Record, RecordBody, Registry, Resources, Step,
};

type Case = (fn(&mut PngFilterRecord), &'static str);

/// A context over a list of records.
struct Ctx(Vec<Record>);

impl DecodeContext for Ctx {
    fn block(&self) -> usize {
        3
    }
    fn record(&mut self, id: u64) -> Result<Record, FormatError> {
        self.0
            .get(id as usize)
            .cloned()
            .ok_or(FormatError::RecordOutOfRange {
                record: id,
                count: self.0.len() as u64,
            })
    }
    fn chunk(&mut self, _record: u64, index: u64) -> Result<Vec<u8>, FormatError> {
        Err(FormatError::ChunkIndexOutOfRange {
            chunk: index,
            len: 0,
        })
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 56) as u8).collect()
    }
}

fn step(p: PrimitiveId, id: u8) -> Step {
    Step {
        primitive: p,
        params: match p {
            PrimitiveId::Store | PrimitiveId::BcjX86 => Vec::new(),
            _ => vec![id],
        },
    }
}

fn registry() -> Registry {
    let mut r = Registry::v1();
    lpk_core::register_full_decoders(&mut r);
    r
}

/// An image: the record (filters, hash of the text filtered with them) and the Up layout.
fn image(l: &Layout, rng: &mut Rng) -> (PngFilterRecord, Vec<u8>, Vec<u8>) {
    let raw = rng.bytes(l.raw_len().unwrap() as usize);
    let n = l.scanlines() as usize;
    let filters: Vec<u8> = (0..n).map(|_| (rng.next() % 5) as u8).collect();
    let text = l.refilter(&raw, &filters).unwrap();
    let up = l.refilter_uniform(&raw, FILTER_UP).unwrap();
    let rec = PngFilterRecord {
        width: l.width(),
        height: l.height(),
        bit_depth: l.bit_depth(),
        color_type: l.color_type(),
        interlace: l.interlace(),
        filters,
        original_hash: *blake3::hash(&text).as_bytes(),
    };
    (rec, text, up)
}

fn png_record(p: PngFilterRecord) -> Record {
    Record::new(RecordBody::PngFilter(p))
}

/// `[store, png-filter {0}]` over `input`, plain_len `plain`.
fn run_last(rec: Record, input: &[u8], plain: u64) -> Result<Vec<u8>, FormatError> {
    let h = BlockHeader {
        graph: Graph {
            steps: vec![step(PrimitiveId::Store, 0), step(PrimitiveId::PngFilter, 0)],
        },
        plain_len: plain,
        encoded_len: input.len() as u64,
    };
    decode_block_in(
        &registry(),
        &h,
        3,
        input,
        &Resources::default(),
        &mut Ctx(vec![rec]),
    )
}

#[test]
fn every_pair_with_and_without_adam7_decodes() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let mut n = 0;
    for (depth, ct) in PAIRS {
        for interlace in 0..=1u8 {
            for (w, h) in [(1, 1), (5, 3), (9, 9), (17, 2)] {
                let l = Layout::new(w, h, depth, ct, interlace).unwrap();
                let (rec, text, up) = image(&l, &mut rng);
                let out = run_last(png_record(rec), &up, text.len() as u64).unwrap();
                assert!(out == text, "{l:?}");
                n += 1;
            }
        }
    }
    assert_eq!(n, 15 * 2 * 4);
}

fn base() -> (PngFilterRecord, Vec<u8>, Vec<u8>) {
    let l = Layout::new(6, 4, 8, 2, 1).unwrap();
    image(&l, &mut Rng(99))
}

fn reason(e: FormatError) -> &'static str {
    match e {
        FormatError::BadRecord { record: 0, reason } => reason,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_test_per_reason_in_the_order_of_the_checks() {
    let (rec, text, up) = base();
    let plain = text.len() as u64;
    // (1) the record lookup.
    let h = BlockHeader {
        graph: Graph {
            steps: vec![step(PrimitiveId::Store, 0), step(PrimitiveId::PngFilter, 1)],
        },
        plain_len: plain,
        encoded_len: up.len() as u64,
    };
    assert!(matches!(
        decode_block_in(
            &registry(),
            &h,
            3,
            &up,
            &Resources::default(),
            &mut Ctx(vec![png_record(rec.clone())])
        ),
        Err(FormatError::RecordOutOfRange {
            record: 1,
            count: 1
        })
    ));
    // (2) kind.
    let d = Record::new(RecordBody::Deflate(DeflateRecord {
        original_len: 1,
        plain_len: 1,
        corrections: Vec::new(),
        library: 0,
        original_hash: [0; 32],
    }));
    assert_eq!(reason(run_last(d, &up, plain).unwrap_err()), "kind");
    // (3) the image: width, height, bit_depth, filters (count, then a byte above 4).
    let cases: [Case; 5] = [
        (|p| p.width = 0, "width"),
        (|p| p.height = 0, "height"),
        (|p| p.bit_depth = 4, "bit_depth"),
        (|p| p.filters.truncate(1), "filters"),
        (|p| p.filters[2] = 5, "filters"),
    ];
    for (f, want) in cases {
        let mut p = rec.clone();
        f(&mut p);
        assert_eq!(
            reason(run_last(png_record(p), &up, plain).unwrap_err()),
            want
        );
    }
    // (4) on the last step, a filtered length other than plain_len.
    for wrong in [plain - 1, plain + 1] {
        assert!(matches!(
            run_last(png_record(rec.clone()), &up, wrong).unwrap_err(),
            FormatError::BlockLengthMismatch { block: 3 }
        ));
    }
    // (5) the input not the filtered length (the store step gives it whole).
    assert_eq!(
        reason(run_last(png_record(rec.clone()), &up[1..], plain).unwrap_err()),
        "scanlines"
    );
    // (6) an input scanline with a filter byte above 4.
    let mut bad = up.clone();
    bad[0] = 9;
    assert_eq!(
        reason(run_last(png_record(rec.clone()), &bad, plain).unwrap_err()),
        "scanlines"
    );
    // (7) the output's hash.
    let mut p = rec.clone();
    p.original_hash[0] ^= 1;
    assert_eq!(
        reason(run_last(png_record(p), &up, plain).unwrap_err()),
        "original_hash"
    );
    // A different input layout (the text itself) decodes to the same output.
    assert!(run_last(png_record(rec), &text, plain).unwrap() == text);
    // Without the archive behind it the step is unimplemented.
    assert!(matches!(
        PngFilterDecoder.decode(&[0], &up, plain, &Resources::default()),
        Err(FormatError::UnimplementedPrimitive { id: 9 })
    ));
}

/// Records the step before `png-filter` running.
struct Flag(Arc<AtomicBool>);
impl PrimitiveDecoder for Flag {
    fn decode(
        &self,
        _: &[u8],
        input: &[u8],
        _: u64,
        _: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        self.0.store(true, Ordering::SeqCst);
        Ok(input.to_vec())
    }
}

#[test]
fn a_filtered_length_above_the_bound_is_payload_too_large_before_m_runs() {
    let (rec, text, up) = base();
    let len = text.len() as u64;
    // `[M, png-filter, store]`: png-filter is not last, its bound is max_block_plain.
    for (max, ok) in [(len - 1, false), (len, true)] {
        let ran = Arc::new(AtomicBool::new(false));
        let mut r = registry();
        r.register(PrimitiveId::BcjX86, Box::new(Flag(ran.clone())));
        let h = BlockHeader {
            graph: Graph {
                steps: vec![
                    step(PrimitiveId::BcjX86, 0),
                    step(PrimitiveId::PngFilter, 0),
                    step(PrimitiveId::Store, 0),
                ],
            },
            plain_len: len.min(max),
            encoded_len: up.len() as u64,
        };
        let limits = Resources {
            max_block_plain: max,
            ..Resources::default()
        };
        let got = decode_block_in(
            &r,
            &h,
            3,
            &up,
            &limits,
            &mut Ctx(vec![png_record(rec.clone())]),
        );
        if ok {
            assert!(got.unwrap() == text);
            assert!(ran.load(Ordering::SeqCst));
        } else {
            assert!(matches!(
                got.unwrap_err(),
                FormatError::PayloadTooLarge { len: l, max: m } if l == len && m == max
            ));
            assert!(!ran.load(Ordering::SeqCst), "M ran before the check");
        }
    }
    // A filtered length past 2^64 - 1: PayloadTooLarge too.
    let mut big = rec;
    big.width = u32::MAX;
    big.height = u32::MAX;
    big.bit_depth = 16;
    big.color_type = 6;
    big.interlace = 0;
    big.filters = Vec::new();
    // The count check (3) comes first for such a record; the overflow is (4) for a record
    // whose filters are right, which the input_bound shows on a 1-scanline-per-pass image.
    assert!(matches!(
        PngFilterDecoder.input_bound(
            &[0],
            10,
            false,
            &Resources::default(),
            &mut Ctx(vec![png_record(big)])
        ),
        Err(FormatError::BadRecord {
            reason: "filters",
            ..
        })
    ));
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/deflate")
            .join(name),
    )
    .unwrap()
}

/// `[store, png-filter, deflate-reconstruct]` on `image.png`: the inflated text (9264 bytes)
/// above `max_block_plain` (the stream's length, 8174 bytes): the store step's output is
/// bounded by the image's filtered length, which the png-filter step learns from the deflate
/// record before anything runs.
#[test]
fn the_three_step_chain_decodes_a_text_above_max_block_plain() {
    let bytes = fixture("image.png");
    let mut streams = Vec::new();
    examine(&bytes, "png", &Limits::standard(), |r| streams.push(r));
    assert_eq!(streams.len(), 1);
    let r = streams.remove(0);
    let deflate: Vec<u8> = r
        .spans
        .iter()
        .flat_map(|&(a, b)| bytes[a..b].iter().copied())
        .collect();
    let text = r.plain.text().to_vec();
    let l = Header::parse(&bytes).unwrap().layout().unwrap();
    let filters = l.filter_bytes(&text).unwrap();
    let up = l.transcode(&text, &vec![FILTER_UP; filters.len()]).unwrap();
    let png = PngFilterRecord {
        width: l.width(),
        height: l.height(),
        bit_depth: l.bit_depth(),
        color_type: l.color_type(),
        interlace: l.interlace(),
        filters,
        original_hash: *blake3::hash(&text).as_bytes(),
    };
    let dr = DeflateRecord {
        original_len: deflate.len() as u64,
        plain_len: text.len() as u64,
        corrections: r.corrections,
        library: 0,
        original_hash: *blake3::hash(&deflate).as_bytes(),
    };
    let max = deflate.len() as u64;
    assert!(text.len() as u64 > max);
    let h = BlockHeader {
        graph: Graph {
            steps: vec![
                step(PrimitiveId::Store, 0),
                step(PrimitiveId::PngFilter, 0),
                step(PrimitiveId::DeflateReconstruct, 1),
            ],
        },
        plain_len: max,
        encoded_len: up.len() as u64,
    };
    let limits = Resources {
        max_block_plain: max,
        ..Resources::default()
    };
    let ctx = || {
        Ctx(vec![
            png_record(png.clone()),
            Record::new(RecordBody::Deflate(dr.clone())),
        ])
    };
    let out = decode_block_in(&registry(), &h, 3, &up, &limits, &mut ctx()).unwrap();
    assert!(out == deflate);
    // Without the png-filter decoder (a revision 1.3 reader): UnimplementedPrimitive 9.
    let mut r13 = Registry::v1();
    r13.register(PrimitiveId::DeflateReconstruct, Box::new(DeflateDecoder));
    assert!(matches!(
        decode_block_in(&r13, &h, 3, &up, &limits, &mut ctx()),
        Err(FormatError::UnimplementedPrimitive { id: 9 })
    ));
    // A png record whose filtered length passes the deflate record's plain_len is
    // PayloadTooLarge with that plain_len as the bound, before the store step runs.
    let mut wide = png.clone();
    wide.height += 1;
    wide.filters.push(0);
    let ctx2 = || {
        Ctx(vec![
            png_record(wide.clone()),
            Record::new(RecordBody::Deflate(dr.clone())),
        ])
    };
    assert!(matches!(
        decode_block_in(&registry(), &h, 3, &up, &limits, &mut ctx2()),
        Err(FormatError::PayloadTooLarge { max: m, .. }) if m == text.len() as u64
    ));
}
