// Replace the current global allocator with the DHAT heap profiler,
// regardless of what it is.
#[cfg(feature = "dhat-heap")]
pub mod dhat_heap;

// Replace the global allocator with jemalloc.
// If `dhat-heap` is enabled, this must not be applied.
#[cfg(all(
    feature = "jemalloc",
    not(feature = "dhat-heap"),
    not(target_env = "msvc")
))]
pub mod jemalloc;

// With both features enabled, `dhat-heap` takes over and nothing references
// jemalloc, so mark the dependency as used.
#[cfg(all(feature = "jemalloc", feature = "dhat-heap", not(target_env = "msvc")))]
use tikv_jemallocator as _;
