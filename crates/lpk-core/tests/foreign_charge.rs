//! E4-2: the listing charge (`foreign::entry_charge`) is not below what the host really holds
//! per entry during an extraction, measured with a counting allocator over the broker's
//! replay of a recorded worker (the decoder, the checks, the policy pass, the targets).
#![allow(clippy::unwrap_used)]

use lpk_core::foreign::{self, entry_charge, Limits};
use lpk_procstat_sys::alloc::CountingAlloc;
use lpk_proto::{Record, RecordKind, Response};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

/// What the broker holds besides the listing: the frame buffer and one decoded page (each
/// frame at most 1 MiB; a page's records at most three times its bytes), and the summary.
const FRAME_ALLOWANCE: u64 = 5 << 20;

fn script(n: usize, name_len: usize) -> Vec<u8> {
    let hello = Response::Hello {
        proto: lpk_proto::PROTO,
        build: lpk_proto::BUILD.into(),
    };
    let mut out = Vec::new();
    hello.write_to(&mut out).unwrap();
    Response::Opened { format: "t".into() }
        .write_to(&mut out)
        .unwrap();
    let recs: Vec<Record> = (0..n)
        .map(|i| {
            let mut name = format!("dir{}/f{i:08}", i % 100).into_bytes();
            name.resize(name_len.max(name.len()), b'x');
            Record {
                kind: RecordKind::File,
                flags: 0,
                mtime_ns: i64::MIN,
                size: 0,
                packed_offset: 0,
                packed_len: 0,
                block: lpk_proto::frame::NO_BLOCK,
                name,
                target: Vec::new(),
            }
        })
        .collect();
    let chunks: Vec<&[Record]> = recs.chunks(foreign::PAGE as usize).collect();
    for (k, c) in chunks.iter().enumerate() {
        Response::Entries {
            records: c.to_vec(),
            more: k + 1 < chunks.len(),
        }
        .write_to(&mut out)
        .unwrap();
    }
    for i in 0..n as u64 {
        Response::MemberStart { index: i }
            .write_to(&mut out)
            .unwrap();
        Response::MemberEnd {
            index: i,
            checked: false,
        }
        .write_to(&mut out)
        .unwrap();
    }
    out
}

#[test]
fn the_listing_charge_covers_what_the_host_holds() {
    for (n, name_len) in [(40_000usize, 16usize), (20_000, 200)] {
        let bytes = script(n, name_len);
        let limits = Limits {
            memory: Some(1 << 40),
            ..Limits::default()
        };
        ALLOC.reset_peak();
        let before = ALLOC.current();
        let (r, _) = foreign::replay(&bytes, 1 << 40, None, &lpk_core::DefaultPolicy, &limits);
        let peak = (ALLOC.peak() - before) as u64;
        let s = r.unwrap();
        assert_eq!(s.files, n as u64);
        let name = format!("dir0/f{:08}", 0).len().max(name_len);
        let charged = n as u64 * entry_charge(name, 0);
        eprintln!(
            "n {n}, name {name}: peak {peak} bytes ({} per entry), charged {charged} ({} per entry)",
            peak / n as u64,
            charged / n as u64
        );
        assert!(
            peak <= charged + FRAME_ALLOWANCE,
            "peak {peak} above the charge {charged} + {FRAME_ALLOWANCE}"
        );
    }
}
