//! Test-only instrumentation: how many bytes did that call ask for?
//!
//! Several of this crate's guarantees are about the *order* of two things —
//! "validate before you allocate", "refuse before you reserve" — and an
//! ordinary assertion cannot see the difference. `PsdLayer::rgba8` on a
//! 30 000 × 30 000 layer with no channels returns `None` either way; what
//! separates the two versions is that one of them memsets 3.6 GB first.
//!
//! Timing would show it and must not be used: a wall-clock threshold measures
//! the machine, not the code, and goes red on a loaded CI box. So this module
//! installs a global allocator that counts the bytes requested **on the
//! calling thread**, and the tests assert on that count instead. The counter is
//! thread-local because `libtest` runs tests in parallel, and a process-wide
//! counter would be measuring every other test at the same time.
//!
//! Only compiled under `cfg(test)`, so nothing here reaches a real build.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// Bytes requested on this thread since it started. `const`-initialised and
    /// holding a type with no destructor, so touching it from inside the
    /// allocator cannot itself allocate or register a TLS destructor.
    static REQUESTED: Cell<u64> = const { Cell::new(0) };
}

fn record(bytes: usize) {
    // `try_with` rather than `with`: during thread teardown the slot may be
    // gone, and a panic from inside the allocator would abort.
    let _ = REQUESTED.try_with(|c| c.set(c.get().saturating_add(bytes as u64)));
}

/// The system allocator, plus a tally.
pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        System.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        System.alloc_zeroed(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(new_size.saturating_sub(layout.size()));
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `f`, and report the bytes it asked the allocator for on this thread.
pub fn bytes_allocated_by<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let before = REQUESTED.with(Cell::get);
    let value = f();
    let after = REQUESTED.with(Cell::get);
    (value, after.saturating_sub(before))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counter_sees_an_allocation_and_ignores_a_call_that_makes_none() {
        let (v, bytes) = bytes_allocated_by(|| vec![0u8; 1 << 20]);
        assert_eq!(v.len(), 1 << 20);
        assert!(bytes >= 1 << 20, "a 1 MiB vector counted {bytes} bytes");

        let (sum, bytes) = bytes_allocated_by(|| (0u64..100).sum::<u64>());
        assert_eq!(sum, 4950);
        assert_eq!(bytes, 0, "arithmetic must not register as an allocation");
    }
}
