use std::{
    hash::{Hash as _, Hasher as _},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use lb_core::mantle::transactions::genesis_tx::ChainId;
use lb_utils::net::ReservedPortBlock;

static TEST_PORT_ALLOCATOR: OnceLock<Mutex<Option<ReservedPortBlock>>> = OnceLock::new();
static PROCESS_START_NONCE: OnceLock<String> = OnceLock::new();

fn process_start_nonce() -> &'static str {
    PROCESS_START_NONCE.get_or_init(|| {
        let started_at_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();

        format!("{started_at_ns:016x}-{:08x}", std::process::id())
    })
}

#[must_use]
pub fn unique_test_context(test_context: Option<&str>) -> ChainId {
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
    .try_into()
    .expect("Unique test context should always be a valid ChainId")
}

#[must_use]
pub fn get_reserved_available_tcp_port() -> Option<u16> {
    with_test_port_allocator(ReservedPortBlock::next_tcp_port)
}

#[must_use]
pub fn get_reserved_available_udp_port() -> Option<u16> {
    with_test_port_allocator(ReservedPortBlock::next_udp_port)
}

fn test_port_allocator_slot() -> &'static Mutex<Option<ReservedPortBlock>> {
    TEST_PORT_ALLOCATOR.get_or_init(|| Mutex::new(ReservedPortBlock::try_new()))
}

fn with_test_port_allocator(f: impl FnOnce(&mut ReservedPortBlock) -> Option<u16>) -> Option<u16> {
    let slot = test_port_allocator_slot();
    let Ok(mut guard) = slot.lock() else {
        return None;
    };
    let allocator = guard.as_mut()?;
    f(allocator)
}

fn hash_str(s: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}
