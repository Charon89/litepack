//! BLAKE3 of a frame payload given in two parts (the block header, then the encoded
//! bytes), on several threads: the standard BLAKE3 tree split into subtrees with
//! `blake3::hazmat`, each hashed on its own scoped thread, the result equal to the
//! one-thread hash of the concatenation (the tests compare the two).

use blake3::hazmat::{
    left_subtree_len, merge_subtrees_non_root, merge_subtrees_root, ChainingValue, HasherExt, Mode,
};

/// Subtrees shorter than twice this are hashed on one thread.
const SPLIT_MIN: u64 = 1 << 22;

/// The payload `head` then `body`, as one byte range.
#[derive(Clone, Copy)]
struct Parts<'a> {
    head: &'a [u8],
    body: &'a [u8],
}

impl Parts<'_> {
    /// Feed bytes `[at, at + len)` of the payload to `h`.
    fn feed(&self, h: &mut blake3::Hasher, at: u64, len: u64) {
        let hl = self.head.len() as u64;
        let end = at + len;
        if at < hl {
            h.update(&self.head[at as usize..end.min(hl) as usize]);
        }
        if end > hl {
            let from = at.saturating_sub(hl) as usize;
            h.update(&self.body[from..(end - hl) as usize]);
        }
    }
}

/// Most threads one hash fans out to (the calling thread included): the
/// subtrees are split at most `log2(MAX_FAN_OUT)` levels deep, whatever
/// `threads` asks for.
const MAX_FAN_OUT: usize = 16;

#[cfg(test)]
thread_local! {
    /// Tests only: every spawn on this thread fails, to reach the fallback.
    static FAIL_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `a` on a scoped thread and `b` here; both results. When the thread
/// cannot be created (a resource failure), `a` runs here too, after `b`:
/// the hash is the same, only slower.
fn join<A, B>(a: impl FnOnce() -> A + Send, b: impl FnOnce() -> B) -> (A, B)
where
    A: Send,
{
    #[cfg(test)]
    if FAIL_SPAWN.with(std::cell::Cell::get) {
        let right = b();
        return (a(), right);
    }
    // Taken by the new thread, or here when the spawn fails.
    let slot = std::sync::Mutex::new(Some(a));
    let take = || slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new().spawn_scoped(s, || take().map(|f| f()));
        let right = b();
        match spawned {
            Ok(left) => match left.join() {
                Ok(Some(l)) => (l, right),
                Ok(None) => unreachable_none(),
                Err(panic) => std::panic::resume_unwind(panic),
            },
            Err(_) => match take() {
                Some(f) => (f(), right),
                None => unreachable_none(),
            },
        }
    })
}

/// The closure is taken exactly once: by the thread, or here when the spawn failed.
#[cold]
fn unreachable_none() -> ! {
    panic!("parhash: the left subtree's closure was taken twice")
}

/// The chaining value of the non-root subtree `[at, at + len)`, split `depth` more times.
fn subtree(p: Parts<'_>, at: u64, len: u64, depth: u32) -> ChainingValue {
    if depth == 0 || len < 2 * SPLIT_MIN {
        let mut h = blake3::Hasher::new();
        h.set_input_offset(at);
        p.feed(&mut h, at, len);
        return h.finalize_non_root();
    }
    let left = left_subtree_len(len);
    let (l, r) = join(
        || subtree(p, at, left, depth - 1),
        || subtree(p, at + left, len - left, depth - 1),
    );
    merge_subtrees_non_root(&l, &r, Mode::Hash)
}

/// BLAKE3 of `head` followed by `body` on up to `threads` threads (1: this thread only;
/// at most [`MAX_FAN_OUT`]).
pub(crate) fn frame_payload_hash(head: &[u8], body: &[u8], threads: usize) -> [u8; 32] {
    let total = (head.len() + body.len()) as u64;
    if threads <= 1 || total < 2 * SPLIT_MIN {
        let mut h = blake3::Hasher::new();
        h.update(head);
        h.update(body);
        return *h.finalize().as_bytes();
    }
    let p = Parts { head, body };
    let fan_out = threads
        .min(MAX_FAN_OUT)
        .checked_next_power_of_two()
        .unwrap_or(MAX_FAN_OUT);
    let depth = fan_out.trailing_zeros();
    let left = left_subtree_len(total);
    let (l, r) = join(
        || subtree(p, 0, left, depth - 1),
        || subtree(p, left, total - left, depth - 1),
    );
    *merge_subtrees_root(&l, &r, Mode::Hash).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(len: usize) -> Vec<u8> {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn the_split_hash_equals_the_one_thread_hash() {
        let data = noise((3 << 23) + 12_345);
        for total in [
            0usize,
            1,
            1024,
            1025,
            2 * SPLIT_MIN as usize - 1,
            2 * SPLIT_MIN as usize,
            2 * SPLIT_MIN as usize + 1,
            (1 << 24) + 1,
            data.len(),
        ] {
            for head_len in [0usize, 1, 37, 1024, 5000] {
                let head_len = head_len.min(total);
                let (head, body) = data[..total].split_at(head_len);
                let want = *blake3::hash(&data[..total]).as_bytes();
                for threads in [1usize, 2, 3, 4, 8, 24, usize::MAX] {
                    assert_eq!(
                        frame_payload_hash(head, body, threads),
                        want,
                        "total {total} head {head_len} threads {threads}"
                    );
                }
            }
        }
    }

    /// A thread that cannot be created: the subtree is hashed on the calling thread and
    /// the hash is the same.
    #[test]
    fn a_failed_spawn_falls_back_to_the_calling_thread() {
        let data = noise((5 << 22) + 777);
        let want = *blake3::hash(&data).as_bytes();
        FAIL_SPAWN.with(|f| f.set(true));
        let got = frame_payload_hash(&data[..40], &data[40..], 8);
        FAIL_SPAWN.with(|f| f.set(false));
        assert_eq!(got, want);
    }
}
