use std::{sync::Mutex, thread::panicking};

static DHAT_PROFILER: Mutex<Option<dhat::Profiler>> = Mutex::new(None);

/// Set up the profiling infrastructure, including the required global allocator
/// and the profiler itself.
///
/// Returns a guard that will drop the profiler (and thus write the profile to
/// disk) when it goes out of scope.
///
/// # Panics
///
/// If called more than once, this function will panic since the profiler is
/// already initialized.
pub fn setup() -> DhatDropGuard {
    #[global_allocator]
    static ALLOC: dhat::Alloc = dhat::Alloc;

    {
        let mut guard = DHAT_PROFILER.lock().expect("dhat mutex poisoned");
        assert!(guard.is_none(), "dhat profiler already initialized");

        *guard = Some(dhat::Profiler::new_heap());
    };
    println!("\n\nDHAT: Profiling enabled.\n\n");

    DhatDropGuard
}

pub struct DhatDropGuard;

impl Drop for DhatDropGuard {
    fn drop(&mut self) {
        drop_dhat_profiler();
    }
}

/// Drops the dhat profiler (if present), causing it to write `dhat-heap.json`.
pub(crate) fn drop_dhat_profiler() {
    let Ok(mut guard_lock) = DHAT_PROFILER.lock() else {
        return;
    };
    // Guard will be dropped at the end of this function, after we print the
    // message.
    let Some(_guard) = guard_lock.take() else {
        return;
    };

    if panicking() {
        println!(
            "\nDHAT: Dumping heap profile after panic (may be incomplete). \
            Output should be in 'dhat-heap.json' - run \
            https://nnethercote.github.io/dh_view/dh_view.html to view the results.\n"
        );
    } else {
        println!(
            "\nDHAT: Heap output capturing, should be in 'dhat-heap.json' - run \
            https://nnethercote.github.io/dh_view/dh_view.html to view the results.\n"
        );
    }
}
