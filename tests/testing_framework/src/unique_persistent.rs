use std::{
    hash::{Hash as _, Hasher as _},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use lb_utils::net::ReservedPortBlock;

/// One allocator slot per process.
///
/// Cross-process coordination is done by a kernel-owned socket lease.
/// We keep the allocator inside an Option so it can be explicitly released.
static TEST_PORT_ALLOCATOR: OnceLock<Mutex<Option<ReservedPortBlock>>> = OnceLock::new();

static PROCESS_START_NONCE: OnceLock<String> = OnceLock::new();

// A nonce that is unique to the current process id and start time.
fn process_start_nonce() -> &'static str {
    PROCESS_START_NONCE.get_or_init(|| {
        let started_at_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();

        format!("{started_at_ns:016x}-{:08x}", std::process::id())
    })
}

/// Returns a unique string keyed to the currently-running process start nonce
/// optional test context and optional nextest control parameters.
#[must_use]
pub fn unique_test_context(test_context: Option<&str>) -> String {
    let current_thread = std::thread::current();
    let thread_name = current_thread.name().unwrap_or("genesis");

    let workspace_root = std::env::var("NEXTEST_WORKSPACE_ROOT")
        .or_else(|_| std::env::var("GITHUB_WORKSPACE"))
        .unwrap_or_else(|_| "none".to_owned());

    let runner_name = std::env::var("RUNNER_NAME").unwrap_or_else(|_| "none".to_owned());

    let attempt_id = std::env::var("NEXTEST_ATTEMPT_ID").unwrap_or_else(|_| "none".to_owned());

    let test_entropy_raw = format!(
        "thread={thread_name}, workspace_root={workspace_root}, runner={runner_name}, attempt={attempt_id}, context={test_context:?}",
    );

    format!(
        "process_start_nonce={}, test_entropy={}",
        process_start_nonce(),
        hash_str(&test_entropy_raw)
    )
}

fn test_port_allocator_slot() -> &'static Mutex<Option<ReservedPortBlock>> {
    TEST_PORT_ALLOCATOR.get_or_init(|| Mutex::new(None))
}

fn with_test_port_allocator<T>(f: impl FnOnce(&mut ReservedPortBlock) -> Option<T>) -> Option<T> {
    let slot = test_port_allocator_slot();
    let mut guard = slot.lock().ok()?;

    if guard.is_none() {
        *guard = Some(ReservedPortBlock::try_new()?);
    }

    f(guard.as_mut().expect("allocator just initialized"))
}
/// Returns an available TCP port from this process's reserved port block.
#[must_use]
pub fn get_reserved_available_tcp_port() -> Option<u16> {
    with_test_port_allocator(ReservedPortBlock::next_tcp_port)
}

/// Returns an available UDP port from this process's reserved port block.
#[must_use]
pub fn get_reserved_available_udp_port() -> Option<u16> {
    with_test_port_allocator(ReservedPortBlock::next_udp_port)
}

/// Explicitly releases this process's reserved port block.
///
/// Call this near the end of the test process or harness teardown to release
/// the block's kernel-owned socket lease immediately.
pub fn release_reserved_port_block() {
    let slot = test_port_allocator_slot();

    let Ok(mut guard) = slot.lock() else {
        return;
    };

    // Taking the allocator out of the slot drops the socket lease here.
    drop(guard.take());
}

/// Create a short 8-byte hash from string
#[must_use]
pub fn hash_str(s: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}
