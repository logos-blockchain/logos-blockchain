#[cfg(feature = "dhat-heap")]
pub mod dhat_heap;

#[cfg(all(
    feature = "jemalloc",
    not(feature = "dhat-heap"),
    not(target_env = "msvc")
))]
pub mod jemalloc;
