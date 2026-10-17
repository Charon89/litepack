//! A counting global allocator for tests that measure a computation's peak heap use in-process
//! (the `deflate-reconstruct` memory term, E2-10). It lives here because this is the one crate
//! allowed `unsafe` (CLAUDE.md: `*-sys` crates); a test binary installs it with
//! `#[global_allocator] static A: CountingAlloc = CountingAlloc::new();`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Wraps the system allocator, counting the bytes currently allocated and their peak.
#[derive(Debug)]
pub struct CountingAlloc {
    current: AtomicUsize,
    peak: AtomicUsize,
}

impl CountingAlloc {
    /// A counter at zero.
    pub const fn new() -> Self {
        CountingAlloc {
            current: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    /// Bytes allocated now.
    pub fn current(&self) -> usize {
        self.current.load(Ordering::SeqCst)
    }

    /// The peak since the last [`CountingAlloc::reset_peak`].
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    /// Set the peak to the current allocation.
    pub fn reset_peak(&self) {
        self.peak.store(self.current(), Ordering::SeqCst);
    }

    fn add(&self, n: usize) {
        let now = self.current.fetch_add(n, Ordering::SeqCst) + n;
        self.peak.fetch_max(now, Ordering::SeqCst);
    }

    fn sub(&self, n: usize) {
        self.current.fetch_sub(n, Ordering::SeqCst);
    }
}

impl Default for CountingAlloc {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: every method forwards to `System` with the caller's arguments unchanged, so `System`'s
// guarantees hold; the counters are only updated after a successful allocation and before a
// deallocation, and never affect the memory handed out.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `GlobalAlloc::alloc`'s contract.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            self.add(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            self.add(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.sub(layout.size());
        // SAFETY: forwarded unchanged; `ptr` was allocated by `System` with `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `GlobalAlloc::realloc`'s contract.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            // The old and the new block exist together only inside `System.realloc`; count the
            // new size first so the peak includes both, as a copying realloc holds both.
            self.add(new_size);
            self.sub(layout.size());
        }
        p
    }
}
