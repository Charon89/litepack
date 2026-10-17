//! The BLAKE3 of the chunks of one cut segment, on a few scoped threads.
//!
//! The writer's store path cuts a segment, then needs every chunk's hash before
//! its in-order dedup lookups and appends. With two or more blocks in flight the
//! hashes are computed here on up to [`MAX_HASH_THREADS`] threads (the calling
//! thread included): the chunks are split into contiguous groups of about equal
//! bytes, each group hashed on its own scoped thread, and the hashes come back in
//! chunk order — the same values the one-thread loop computes, so the lookups and
//! every archive byte stay those of the sequential writer. The threads live for
//! one segment and are joined before the call returns, whatever happens: a panic
//! in one is returned to the caller (who marks the writer failed and re-raises
//! it), and a thread that cannot be created leaves its group to the calling
//! thread (the hashes are the same, only slower).

use std::any::Any;

/// Most threads one segment's hashes fan out to, the calling thread included.
pub const MAX_HASH_THREADS: usize = 4;

/// A group shorter than this (bytes) is not worth a thread of its own.
const GROUP_MIN: usize = 512 * 1024;

#[cfg(test)]
thread_local! {
    /// Tests only: every spawn on this thread fails, to reach the fallback.
    pub(crate) static FAIL_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Tests only: the writer's hashes panic on every `lpk-hash` thread.
    pub(crate) static HASH_FAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Tests only: [`hash_two`], except that it panics on a hashing thread.
#[cfg(test)]
pub(crate) fn faulty_hash(p: (&[u8], &[u8])) -> [u8; 32] {
    if std::thread::current().name() == Some("lpk-hash") {
        panic!("a hashing thread fails");
    }
    hash_two(p)
}

/// The bytes being cut: the held-back carry, then the segment, as one range.
#[derive(Clone, Copy)]
pub(crate) struct Two<'a> {
    pub(crate) carry: &'a [u8],
    pub(crate) seg: &'a [u8],
}

impl<'a> Two<'a> {
    /// The two pieces of `[a, b)`: in the carry, then in the segment.
    pub(crate) fn piece(&self, a: usize, b: usize) -> (&'a [u8], &'a [u8]) {
        let held = self.carry.len();
        let head = &self.carry[a.min(held)..b.min(held)];
        let tail = &self.seg[a.max(held) - held..b.max(held) - held];
        (head, tail)
    }
}

/// BLAKE3 of `head` followed by `tail`.
pub(crate) fn hash_two((head, tail): (&[u8], &[u8])) -> [u8; 32] {
    if tail.is_empty() {
        return *blake3::hash(head).as_bytes();
    }
    if head.is_empty() {
        return *blake3::hash(tail).as_bytes();
    }
    let mut h = blake3::Hasher::new();
    h.update(head);
    h.update(tail);
    *h.finalize().as_bytes()
}

/// A chunk hash function over the chunk's two pieces.
pub(crate) type HashFn<'f> = dyn Fn((&[u8], &[u8])) -> [u8; 32] + Sync + 'f;

/// A panic payload from a hashing thread.
pub(crate) type Panic = Box<dyn Any + Send + 'static>;

/// The hash of each chunk `ranges` names in `bytes`, in order, on up to
/// `threads` threads (at most [`MAX_HASH_THREADS`]; 1: this thread only).
/// `hash` is [`hash_two`] outside tests. Every thread is joined before this
/// returns; a thread's panic is returned as `Err`.
pub(crate) fn hash_ranges(
    bytes: Two<'_>,
    ranges: &[(usize, usize)],
    threads: usize,
    hash: &HashFn<'_>,
) -> Result<Vec<[u8; 32]>, Panic> {
    let total: usize = ranges.iter().map(|&(a, b)| b - a).sum();
    let groups = threads
        .min(MAX_HASH_THREADS)
        .min(total / GROUP_MIN)
        .min(ranges.len())
        .max(1);
    let one = |rs: &[(usize, usize)]| -> Vec<[u8; 32]> {
        rs.iter().map(|&(a, b)| hash(bytes.piece(a, b))).collect()
    };
    if groups == 1 {
        return Ok(one(ranges));
    }
    // Contiguous groups of about `total / groups` bytes each, at least one chunk.
    let target = total.div_ceil(groups);
    let mut bounds = Vec::with_capacity(groups + 1);
    bounds.push(0usize);
    let mut acc = 0usize;
    for (i, &(a, b)) in ranges.iter().enumerate() {
        acc += b - a;
        if bounds.len() < groups && acc >= target * bounds.len() && i + 1 < ranges.len() {
            bounds.push(i + 1);
        }
    }
    bounds.push(ranges.len());
    let parts: Vec<&[(usize, usize)]> = bounds.windows(2).map(|w| &ranges[w[0]..w[1]]).collect();
    std::thread::scope(|s| {
        // Groups 1.. on their own threads (or here when a spawn fails), group 0 here.
        let mut handles = Vec::with_capacity(parts.len() - 1);
        let mut here: Vec<(usize, Vec<[u8; 32]>)> = Vec::new();
        for (g, rs) in parts.iter().enumerate().skip(1) {
            match spawn(s, move || one(rs)) {
                Some(h) => handles.push((g, h)),
                None => here.push((g, Vec::new())),
            }
        }
        let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut out = vec![one(parts[0])];
            out.reserve(parts.len() - 1);
            for (g, v) in here.iter_mut() {
                *v = one(parts[*g]);
            }
            out
        }));
        // Join every thread, whatever happened here, before deciding.
        let mut results: Vec<(usize, std::thread::Result<Vec<[u8; 32]>>)> =
            handles.into_iter().map(|(g, h)| (g, h.join())).collect();
        let first = first?;
        results.extend(here.into_iter().map(|(g, v)| (g, Ok(v))));
        results.sort_by_key(|(g, _)| *g);
        let mut out = Vec::with_capacity(ranges.len());
        for v in first {
            out.extend(v);
        }
        for (_, r) in results {
            out.extend(r?);
        }
        Ok(out)
    })
}

/// A scoped thread running `f`, or `None` when it cannot be created.
fn spawn<'scope, 'env, T: Send + 'scope>(
    s: &'scope std::thread::Scope<'scope, 'env>,
    f: impl FnOnce() -> T + Send + 'scope,
) -> Option<std::thread::ScopedJoinHandle<'scope, T>> {
    #[cfg(test)]
    if FAIL_SPAWN.with(std::cell::Cell::get) {
        return None;
    }
    std::thread::Builder::new()
        .name("lpk-hash".to_string())
        .spawn_scoped(s, f)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    /// Ranges of the given sizes (cycled) covering `total` bytes.
    fn ranges(total: usize, sizes: &[usize]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let (mut at, mut i) = (0usize, 0usize);
        while at < total {
            let n = sizes[i % sizes.len()].min(total - at);
            out.push((at, at + n));
            at += n;
            i += 1;
        }
        out
    }

    fn sequential(bytes: Two<'_>, rs: &[(usize, usize)]) -> Vec<[u8; 32]> {
        rs.iter()
            .map(|&(a, b)| hash_two(bytes.piece(a, b)))
            .collect()
    }

    #[test]
    fn the_pool_hashes_equal_the_sequential_hashes() {
        let data = noise(3, 9 << 20);
        for held in [0usize, 1, 4096, 300_000, 524_288] {
            let (carry, seg) = data.split_at(held);
            let bytes = Two { carry, seg };
            for sizes in [
                vec![65_536usize],
                vec![4096, 524_288, 70_001],
                vec![524_288],
                vec![1, 2, 3, 100_000],
                vec![9 << 20],
            ] {
                let rs = ranges(data.len(), &sizes);
                let want = sequential(bytes, &rs);
                for threads in [1usize, 2, 3, 4, 8, 24] {
                    let got = hash_ranges(bytes, &rs, threads, &hash_two).ok();
                    assert_eq!(
                        got.as_ref(),
                        Some(&want),
                        "held {held} {sizes:?} t{threads}"
                    );
                }
            }
        }
        // Few chunks, few bytes, no chunks.
        let bytes = Two {
            carry: &data[..10],
            seg: &data[10..2000],
        };
        let rs = ranges(2000, &[700]);
        assert_eq!(
            hash_ranges(bytes, &rs, 4, &hash_two).ok(),
            Some(sequential(bytes, &rs))
        );
        assert_eq!(hash_ranges(bytes, &[], 4, &hash_two).ok(), Some(Vec::new()));
    }

    #[test]
    fn a_failed_spawn_hashes_on_the_calling_thread() {
        let data = noise(4, 8 << 20);
        let bytes = Two {
            carry: &[],
            seg: &data,
        };
        let rs = ranges(data.len(), &[65_536, 100_003]);
        let want = sequential(bytes, &rs);
        FAIL_SPAWN.with(|f| f.set(true));
        let got = hash_ranges(bytes, &rs, 4, &hash_two).ok();
        FAIL_SPAWN.with(|f| f.set(false));
        assert_eq!(got, Some(want));
    }

    /// A panic on a pool thread or on the calling thread comes back as `Err`
    /// after every thread is joined (the scope cannot return before that).
    #[test]
    fn a_panic_in_the_pool_is_returned_joined() {
        let data = noise(5, 8 << 20);
        let bytes = Two {
            carry: &[],
            seg: &data,
        };
        let rs = ranges(data.len(), &[65_536]);
        let last = data.len();
        let first = rs[0].1;
        // On a pool thread (the last group), then on the calling thread (group 0).
        for bad_end in [last, first] {
            let started = std::sync::atomic::AtomicUsize::new(0);
            let r = hash_ranges(bytes, &rs, 4, &|(h, t): (&[u8], &[u8])| {
                started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let p = t.as_ptr() as usize + t.len() - data.as_ptr() as usize;
                if p == bad_end && h.is_empty() {
                    panic!("injected");
                }
                hash_two((h, t))
            });
            let p = r.err().map(|p| p.downcast_ref::<&str>().copied());
            assert_eq!(p, Some(Some("injected")), "bad end {bad_end}");
            assert!(started.load(std::sync::atomic::Ordering::SeqCst) >= 2);
        }
    }
}
