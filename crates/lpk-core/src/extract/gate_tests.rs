//! Admission by need (R115): the pool's gate and the floor sizing.

#![allow(clippy::unwrap_used)]

use super::*;
use lpk_format::{DecodeGate, Denied, Grant};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const MIB: u64 = 1 << 20;

fn sizing(threads: usize, declared: u64, floor: u64, plain: u64) -> Sizing {
    Sizing {
        threads,
        writers: (threads / 4).clamp(1, 4),
        needed: 3054,
        fixed_shared: 2 * MIB,
        // encoded + plain + step input + allowance beside the floor.
        per_block: 3 * plain + BLOCK_ALLOWANCE + floor,
        per_worker: 256 << 10,
        held: plain + BLOCK_ALLOWANCE,
        decoder: floor,
        declared,
        budget_given: false,
    }
}

fn pool_of(sizing: Sizing, budget: u64) -> Pool {
    let (workers, in_flight) = sizing.size(budget);
    Pool {
        threads: sizing.threads,
        workers,
        in_flight,
        writers: sizing.writers,
        memory: ExtractMemory {
            budget: Budget {
                bytes: budget,
                source: BudgetSource::Given,
            },
            ..ExtractMemory::default()
        },
        sizing,
    }
}

/// A gate with a 100 MiB pool over a 10 MiB floor, 300 MiB for a lone decode, a declared term of
/// 300 MiB and a 1 GiB budget.
fn gate() -> Arc<AloneGate> {
    let s = sizing(8, 300 * MIB, 10 * MIB, MIB);
    AloneGate::with_terms(
        &pool_of(s, 1 << 30),
        GateTerms {
            shared: 100 * MIB,
            floor: 10 * MIB,
            lone: 300 * MIB,
            declared: 300 * MIB,
        },
    )
}

/// A one-entry archive whose single block of `plain` bytes is stored and declares `declared`
/// bytes of decoder memory.
fn archive_declaring(plain: usize, declared: u64) -> lpk_format::Archive<std::io::Cursor<Vec<u8>>> {
    use lpk_format::{Encoded, EntryFlags, Graph, GraphResources, Writer, WriterOptions};
    let data: Vec<u8> = (0..plain).map(|i| (i * 31 % 251) as u8).collect();
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, WriterOptions::default(), 1).unwrap();
    w.begin_entry("p.bin", EntryFlags::EMPTY, 0).unwrap();
    let encoded = Encoded {
        graph: Graph {
            steps: vec![lpk_format::Step {
                primitive: lpk_format::PrimitiveId::Store,
                params: Vec::new(),
            }],
        },
        bytes: data.clone(),
        resources: GraphResources::default(),
        memory: 0,
    };
    w.add_part_encoded(0, &data, encoded, declared).unwrap();
    w.end_entry().unwrap();
    w.finish().unwrap();
    lpk_format::Archive::open(
        std::io::Cursor::new(out),
        &lpk_format::Resources {
            memory: u64::MAX / 4,
            ..lpk_format::Resources::default()
        },
    )
    .unwrap()
}

/// `pool_size` on an archive declaring `decode_memory` with blocks of `plain` bytes, at 24
/// threads, 3054 needed blocks and `budget`.
fn pool_for(plain: usize, decode_memory: u64, budget: u64) -> Pool {
    let a = archive_declaring(plain, decode_memory - plain as u64);
    let needed = vec![0usize; 3054];
    let shape = Shape {
        needed: &needed,
        targets: 3054,
        with_content: 3054,
        placements: 3054,
        name_bytes: 3054 * 16,
        max_writers: usize::MAX,
    };
    let opts = ExtractOptions {
        threads: Some(24),
        memory: Some(budget),
        ..ExtractOptions::default()
    };
    pool_size(&a, &opts, &shape)
}

/// The envelope of `full/photo-jpeg` at 853acea declares 2,005,814,109 bytes: under a 2 GiB
/// budget `pool_size`, sized from the floor, keeps at least e9790b8's 4 workers (it chose 1 when
/// every block was counted with the declaration), a lone decode still has the declared term, and
/// the counted terms and the gate's pool stay within the budget.
#[test]
fn the_floor_keeps_the_workers_under_a_large_declaration() {
    let budget = 2u64 << 30;
    for plain in [1usize << 20, 4 << 20] {
        let pool = pool_for(plain, 2_005_814_109, budget);
        let m = pool.memory;
        assert!(m.declared >= 2_005_814_109 - plain as u64, "{m:?}");
        assert!(m.decoder < m.declared);
        assert!(pool.workers >= 4, "plain {plain}: workers {}", pool.workers);
        let terms = pool.sizing.gate_terms(m.budget);
        assert!(terms.lone >= m.declared, "plain {plain}: {terms:?}");
        let counted = m.total_in_flight(pool.workers, pool.in_flight) + m.pool;
        assert!(counted <= budget, "{counted} > {budget}");
    }
}

/// R117: the counted terms at a lone grant stay within the budget. Every worker holds its
/// block's terms other than its decoder (the others wait to run alone with their frames and step
/// inputs), the lone decode uses its whole room, and the permits beyond the workers are held
/// blocks. R115's formula, which counted the waiting decodes as held blocks, exceeds the budget;
/// the shared [`lone_room`] meets it exactly, for extraction's `Sizing` and the export's
/// `alone_memory_for` alike.
#[test]
fn the_counted_terms_at_a_lone_grant_stay_within_the_budget() {
    let budget = 2u64 << 30;
    let pool = pool_for(4 << 20, 1 << 30, budget);
    let s = pool.sizing;
    let (w, p) = s.size(budget);
    assert!(w > 1, "several workers");
    let (wu, pu) = (w as u64, p as u64);
    let other = s.per_block - s.decoder;
    assert!(other > s.held);
    let at_grant = |fixed: u64, lone: u64| fixed + wu * other + lone + (pu - wu) * s.held;
    // Extraction.
    let fixed = s.fixed_shared + wu * s.per_worker;
    let r115 = budget - fixed - other - (pu - 1) * s.held;
    let now = s.alone_memory(budget);
    eprintln!(
        "extraction: workers {w}, permits {p}; at the grant R115 {} and now {} against {budget}",
        at_grant(fixed, r115),
        at_grant(fixed, now)
    );
    assert!(at_grant(fixed, r115) > budget);
    assert!(at_grant(fixed, now) <= budget);
    // The export, with the same terms counted at the declared decoder term and more permits.
    let m = ExtractMemory {
        fixed,
        ..pool.memory.with_declared()
    };
    let permits = p + 4;
    let other = m.per_block - m.decoder;
    let at_grant = |lone: u64| m.fixed + wu * other + lone + (permits - w) as u64 * m.held;
    let r115 = budget - m.fixed - other - (permits as u64 - 1) * m.held;
    let now = alone_memory_for(&m, w, permits);
    eprintln!(
        "export: workers {w}, permits {permits}; at the grant R115 {} and now {} against {budget}",
        at_grant(r115),
        at_grant(now)
    );
    assert!(at_grant(r115) > budget);
    assert!(at_grant(now) <= budget);
}

fn envelope() -> lpk_format::Envelope {
    lpk_format::Envelope {
        max_window: 0,
        max_bwt_block: 0,
        max_block_plain: 0,
        max_frame_payload: 0,
        decode_memory: 0,
        threads_hint: 0,
    }
}

/// The floor at the edges of `max_window`: 0 and 1 (the smallest window), a window that is not a
/// power of two (rounded up), and windows above 2^62 (zstd's log is capped); never above the
/// declared term, never below the window within the block.
#[test]
fn the_floor_at_the_edges_of_max_window() {
    let plain = 8 * MIB;
    let big = |max_window: u64| {
        decoder_floor(&lpk_format::Envelope {
            max_window,
            max_block_plain: plain,
            decode_memory: u64::MAX,
            ..envelope()
        })
    };
    let zstd = crate::memory::zstd_decode_memory;
    assert_eq!(big(0), zstd(0, plain));
    assert_eq!(big(1), zstd(0, plain));
    assert_eq!(big(3 * MIB), zstd(22, plain));
    assert!(big(3 * MIB) >= 3 * MIB);
    assert_eq!(big((1 << 62) + 1), zstd(62, plain));
    assert_eq!(big(u64::MAX), zstd(62, plain));
    assert!(big(u64::MAX) >= plain);
    // Capped by the declared term.
    let small = decoder_floor(&lpk_format::Envelope {
        max_window: 1 << 30,
        max_block_plain: plain,
        decode_memory: plain + 1000,
        ..envelope()
    });
    assert_eq!(small, 1000);
}

/// Many small needs share the pool and never hold more than it at once; one outlier larger than
/// the pool waits for it to empty and runs with nothing else reserved; every grant comes back.
#[test]
fn small_needs_share_the_pool_and_an_outlier_runs_alone() {
    let g = gate();
    let peak = Arc::new(AtomicU64::new(0));
    let alone_seen_with_others = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    for i in 0..16u64 {
        let g = g.clone();
        let peak = peak.clone();
        let bad = alone_seen_with_others.clone();
        handles.push(std::thread::spawn(move || {
            for round in 0..20u64 {
                assert!(g.begin());
                // Needs from 15 to 45 MiB, beyond the 10 MiB floor; thread 0 asks once for an
                // outlier of 250 MiB (240 beyond the floor, more than the pool).
                let need = if i == 0 && round == 5 {
                    250 * MIB
                } else {
                    (15 + (i * 7 + round * 3) % 31) * MIB
                };
                let grant = g.acquire(need).unwrap();
                let r = g.reserved();
                if grant.alone {
                    assert_eq!(r, u64::MAX);
                    let s = g.lock();
                    if s.reserved != 0 || s.decoding != 1 {
                        bad.store(true, Ordering::SeqCst);
                    }
                } else {
                    assert!(r <= g.terms().shared, "reserved {r}");
                    peak.fetch_max(r, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_micros(200));
                g.release(grant);
                g.end();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert!(!alone_seen_with_others.load(Ordering::SeqCst));
    assert_eq!(g.alone_runs(), 1);
    assert_eq!(g.concurrent_while_alone(), 1);
    assert!(
        peak.load(Ordering::SeqCst) > 45 * MIB,
        "the pool was shared"
    );
    assert_eq!(g.reserved(), 0);
}

/// A need within the floor reserves nothing and never waits, even while a decode runs alone.
#[test]
fn a_need_within_the_floor_reserves_nothing() {
    let g = gate();
    let alone = g.acquire(250 * MIB).unwrap();
    assert!(alone.alone);
    let small = g.acquire(10 * MIB).unwrap();
    assert_eq!(
        small,
        Grant {
            bytes: 0,
            alone: false
        }
    );
    g.release(small);
    g.release(alone);
    assert_eq!(g.reserved(), 0);
}

/// A hostile need beyond what a lone decode may use is refused with a `--memory` figure that
/// would let it run, and the refusal reports the extraction's budget.
#[test]
fn a_hostile_need_is_refused_with_the_budget() {
    let g = gate();
    match g.acquire(u64::MAX / 2) {
        Err(Denied::TooLarge { needed }) => assert!(needed > g.budget()),
        other => panic!("{other:?}"),
    }
    struct Ctx(Arc<AloneGate>);
    impl lpk_format::DecodeContext for Ctx {
        fn block(&self) -> usize {
            0
        }
        fn record(&mut self, id: u64) -> Result<lpk_format::Record, FormatError> {
            Err(FormatError::RecordOutOfRange {
                record: id,
                count: 0,
            })
        }
        fn chunk(&mut self, _: u64, index: u64) -> Result<Vec<u8>, FormatError> {
            Err(FormatError::ChunkIndexOutOfRange {
                chunk: index,
                len: 0,
            })
        }
        fn memory_budget(&self) -> Option<u64> {
            // The reader's resource, which may exceed the extraction's budget.
            Some(4 << 30)
        }
        fn gate(&self) -> Option<Arc<dyn lpk_format::DecodeGate>> {
            Some(self.0.clone())
        }
    }
    let e = lpk_format::reserve(&Ctx(g.clone()), 400 * MIB).unwrap_err();
    let FormatError::Refused(r) = e else {
        panic!("{e:?}")
    };
    assert_eq!(r.field, lpk_format::envelope::MEMORY_OPTION);
    assert_eq!(r.allowed, 1 << 30);
    assert!(r.needed > r.allowed);
    assert_eq!(g.reserved(), 0);
}

/// A need above the archive's declared term (an archive written under revision 1.1) runs alone
/// even when it would fit the pool (D-116 (4)).
#[test]
fn an_over_declared_need_runs_alone() {
    let s = sizing(8, 300 * MIB, 10 * MIB, MIB);
    let g = AloneGate::with_terms(
        &pool_of(s, 1 << 30),
        GateTerms {
            shared: 100 * MIB,
            floor: 10 * MIB,
            lone: 300 * MIB,
            declared: 20 * MIB,
        },
    );
    let grant = g.acquire(30 * MIB).unwrap();
    assert!(grant.alone);
    g.release(grant);
    let grant = g.acquire(20 * MIB).unwrap();
    assert!(!grant.alone);
    assert_eq!(grant.bytes, 10 * MIB);
    g.release(grant);
    assert_eq!(g.alone_runs(), 1);
}

/// Cancelling the extraction wakes a decode waiting for shared bytes and one waiting to run
/// alone; both return `Cancelled` and leave nothing reserved or waiting.
#[test]
fn cancellation_wakes_the_waiting_decodes() {
    let g = gate();
    assert!(g.begin());
    let held = g.acquire(100 * MIB).unwrap();
    let spawn = |need: u64| {
        let g = g.clone();
        std::thread::spawn(move || {
            assert!(g.begin());
            let r = g.acquire(need);
            g.end();
            r
        })
    };
    let wait_for = |what: &str, done: &dyn Fn(&GateState) -> bool| {
        for _ in 0..10_000 {
            if done(&g.lock()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("{what} never happened");
    };
    // The shared waiter queues first: a decode waiting to run alone holds new decodes back at
    // `begin`, so the other order would leave the shared waiter outside the gate.
    let shared = spawn(50 * MIB);
    wait_for("the shared waiter queues", &|s| s.queue.len() == 1);
    let lone = spawn(250 * MIB);
    wait_for("the lone waiter waits", &|s| s.alone_waiting == 1);
    let waiters = [shared, lone];
    g.cancel();
    for w in waiters {
        assert_eq!(w.join().unwrap(), Err(Denied::Cancelled));
    }
    g.release(held);
    g.end();
    assert_eq!(g.reserved(), 0);
    let s = g.lock();
    assert_eq!((s.alone_waiting, s.queue.len(), s.decoding), (0, 0, 0));
    drop(s);
    // The reserve helper maps the cancellation to the error the pool reads as one.
    assert!(is_cancelled_decode(&lpk_format::cancelled()));
}

/// A reservation is returned when the decode fails or panics: nothing leaks.
#[test]
fn no_reservation_leaks_after_an_error_or_a_panic() {
    #[derive(Debug)]
    struct Ctx(Arc<AloneGate>);
    impl lpk_format::DecodeContext for Ctx {
        fn block(&self) -> usize {
            0
        }
        fn record(&mut self, id: u64) -> Result<lpk_format::Record, FormatError> {
            Err(FormatError::RecordOutOfRange {
                record: id,
                count: 0,
            })
        }
        fn chunk(&mut self, _: u64, index: u64) -> Result<Vec<u8>, FormatError> {
            Err(FormatError::ChunkIndexOutOfRange {
                chunk: index,
                len: 0,
            })
        }
        fn gate(&self) -> Option<Arc<dyn lpk_format::DecodeGate>> {
            Some(self.0.clone())
        }
    }
    let g = gate();
    let ctx = Ctx(g.clone());
    let failing = || -> Result<(), FormatError> {
        let _r = lpk_format::reserve(&ctx, 60 * MIB)?;
        assert!(g.reserved() > 0);
        Err(FormatError::RecordOutOfRange {
            record: 0,
            count: 0,
        })
    };
    assert!(failing().is_err());
    assert_eq!(g.reserved(), 0);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _r = lpk_format::reserve(&ctx, 250 * MIB).unwrap();
        panic!("decoder panic");
    }));
    assert!(r.is_err());
    assert_eq!(g.reserved(), 0);
    // The gate still admits after both.
    let r = lpk_format::reserve(&ctx, 250 * MIB).unwrap().unwrap();
    assert!(r.grant().alone);
}
