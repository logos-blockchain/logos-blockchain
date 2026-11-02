#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// Turn heap profiling on by default for `heap-profiling` builds, sampling one
// allocation per ~512 KiB allocated. `MALLOC_CONF` in the environment
// overrides it.
#[cfg(feature = "heap-profiling")]
#[allow(
    non_upper_case_globals,
    reason = "jemalloc looks this symbol up by name"
)]
#[unsafe(export_name = "malloc_conf")]
pub static malloc_conf: &[u8] = b"prof:true,prof_active:true,lg_prof_sample:19\0";
