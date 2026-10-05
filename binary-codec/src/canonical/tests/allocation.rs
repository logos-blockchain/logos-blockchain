//! A counting global allocator for the test binary, so decoders can be checked
//! for how much they allocate from a declared length.
//!
//! A binary can install only one global allocator, so every allocation test in
//! the crate measures through this module.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

/// Runs `f` and reports how many bytes it allocated on this thread.
pub fn bytes_allocated_by<F, R>(f: F) -> (R, usize)
where
    F: FnOnce() -> R,
{
    let before = ALLOCATED_BYTES.get();
    let result = f();
    (result, ALLOCATED_BYTES.get() - before)
}

/// Forwards to the system allocator, tallying every byte handed out. Growth
/// is counted too, so a `Vec` that reallocates as it fills is not free.
///
/// Installed for the whole test binary — every test in this crate allocates
/// through it — but it only adds a counter bump on top of `System`.
struct CountingAllocator;

// SAFETY: every method forwards its arguments unchanged to `System`, which
// upholds the `GlobalAlloc` contract. The only added work is a thread-local
// counter bump, which allocates nothing and so cannot re-enter the
// allocator.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        // SAFETY: `layout` is forwarded untouched from our caller.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        // SAFETY: `layout` is forwarded untouched from our caller.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was handed out by `System` under `layout`, since
        // every allocating method here delegates to it.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_allocation(new_size.saturating_sub(layout.size()));
        // SAFETY: as `dealloc`; `new_size` is forwarded untouched.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

// The tally is thread-local, not a global counter: the harness runs the
// tests in this binary in parallel, each on its own thread, so a global one
// would attribute their allocations to whoever happens to be measuring.
//
// `const`-initialised so reading it neither allocates nor registers a
// destructor — either would re-enter the allocator below.
thread_local! {
    static ALLOCATED_BYTES: Cell<usize> = const { Cell::new(0) };
}

fn record_allocation(bytes: usize) {
    // `try_with` because TLS is gone while a thread is being torn down, and
    // an allocation at that point is not part of any measurement anyway.
    let _ = ALLOCATED_BYTES.try_with(|counter| counter.set(counter.get() + bytes));
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;
