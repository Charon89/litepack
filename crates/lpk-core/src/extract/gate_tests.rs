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

/// The envelope of `full/photo-jpeg` at 853acea declares 2,005,814,109 bytes: under a 2 GiB
/// budget the pool sized from the floor keeps at least e9790b8's 4 workers (it chose 1 when every
/// block was counted with the declaration), and a lone decode still has the declared term. The
/// terms follow `pool_size` for an archive with records: `encoded + plain + floor + step_input +
/// nested + BLOCK_ALLOWANCE`, the nested term a lower block's plain bytes and largest frame.
#[test]
fn the_floor_keeps_the_workers_under_a_large_declaration() {
    // Blocks of 1 and 4 MiB (a 16 MiB block leaves room for one worker beside the declared
    // term once every waiting decode keeps its frame and step input, R117).
    for plain in [MIB, 4 * MIB] {
        let env = lpk_format::Envelope {
            max_window: plain,
            max_block_plain: plain,
            decode_memory: 2_005_814_109,
            ..envelope()
        };
        let floor = decoder_floor(&env);
        let declared = env.decode_memory - plain;
        let frame = plain + plain / 64;
        let nested = plain + frame;
        let per_block = frame + plain + floor + plain + nested + BLOCK_ALLOWANCE;
        let s = Sizing {
            threads: 24,
            writers: 4,
            needed: 3054,
            fixed_shared: 2 * MIB,
            per_block,
            per_worker: 256 << 10,
            held: plain + BLOCK_ALLOWANCE,
            decoder: floor,
            declared,
            budget_given: true,
        };
        let budget = Budget {
            bytes: 2 << 30,
            source: BudgetSource::Given,
        };
        let (workers, in_flight) = s.size(budget.bytes);
        assert!(workers >= 4, "plain {plain}: workers {workers}");
        assert!(in_flight >= workers);
        let terms = s.gate_terms(budget);
        assert!(terms.lone >= declared, "plain {plain}: {terms:?}");
        let w = workers as u64;
        let counted = s.fixed_shared
            + w * (s.per_block + s.per_worker)
            + (in_flight - workers) as u64 * s.held
            + terms.shared;
        assert!(counted <= budget.bytes, "{counted} > {}", budget.bytes);
    }
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
    let waiters: Vec<_> = [50 * MIB, 250 * MIB]
        .into_iter()
        .map(|need| {
            let g = g.clone();
            std::thread::spawn(move || {
                assert!(g.begin());
                let r = g.acquire(need);
                g.end();
                r
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(100));
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
