use std::{
    fs,
    fs::OpenOptions,
    hash::{Hash as _, Hasher as _},
    io::Write as _,
    net::{TcpListener, UdpSocket},
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use lb_utils::net::{get_available_tcp_port, get_available_udp_port};

const PORT_BLOCK_SIZE: u16 = 256;
const PORT_RANGE_START: u16 = 20_000;
const PORT_RANGE_END: u16 = 55_000;
const MALFORMED_CLAIM_FILE_REAP_GRACE: Duration = Duration::from_mins(1);

static TEST_PORT_ALLOCATOR: OnceLock<Mutex<Option<TestPortAllocator>>> = OnceLock::new();
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

#[must_use]
pub fn get_reserved_available_tcp_port() -> Option<u16> {
    with_test_port_allocator(TestPortAllocator::next_tcp_port)
}

#[must_use]
pub fn get_reserved_available_udp_port() -> Option<u16> {
    with_test_port_allocator(TestPortAllocator::next_udp_port)
}

fn test_port_allocator_slot() -> &'static Mutex<Option<TestPortAllocator>> {
    TEST_PORT_ALLOCATOR.get_or_init(|| Mutex::new(TestPortAllocator::new()))
}

fn with_test_port_allocator(f: impl FnOnce(&mut TestPortAllocator) -> Option<u16>) -> Option<u16> {
    let slot = test_port_allocator_slot();
    let Ok(mut guard) = slot.lock() else {
        return None;
    };
    let allocator = guard.as_mut()?;
    f(allocator)
}

#[derive(Debug)]
struct TestPortAllocator {
    claim_file: PathBuf,
    tcp_next: u16,
    tcp_end: u16,
    udp_next: u16,
    udp_end: u16,
}

impl TestPortAllocator {
    fn new() -> Option<Self> {
        fs::create_dir_all(handshake_dir()).ok()?;

        let owner = format!("process_start_nonce={}", process_start_nonce());
        let max_block_start = PORT_RANGE_END.checked_sub(PORT_BLOCK_SIZE - 1)?;

        for block_start in (PORT_RANGE_START..=max_block_start).step_by(PORT_BLOCK_SIZE as usize) {
            let block_end = block_start + PORT_BLOCK_SIZE - 1;
            let claim_file = handshake_dir().join(format!("{block_start}.lock"));

            if claim_file.exists() {
                try_reap_stale_port_claim_file(&claim_file);
            }

            if let Ok(mut file) = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&claim_file)
            {
                write_port_claim_metadata(&mut file, &owner, block_start, block_end).ok()?;

                let tcp_next = block_start;
                let tcp_end = block_start + (PORT_BLOCK_SIZE / 2) - 1;
                let udp_next = tcp_end + 1;
                let udp_end = block_end;

                return Some(Self {
                    claim_file,
                    tcp_next,
                    tcp_end,
                    udp_next,
                    udp_end,
                });
            }
        }

        None
    }

    fn next_tcp_port(&mut self) -> Option<u16> {
        while self.tcp_next <= self.tcp_end {
            let candidate = self.tcp_next;
            self.tcp_next = self.tcp_next.saturating_add(1);

            if is_tcp_port_available(candidate) {
                return Some(candidate);
            }
        }

        get_available_tcp_port()
    }

    fn next_udp_port(&mut self) -> Option<u16> {
        while self.udp_next <= self.udp_end {
            let candidate = self.udp_next;
            self.udp_next = self.udp_next.saturating_add(1);

            if is_udp_port_available(candidate) {
                return Some(candidate);
            }
        }

        get_available_udp_port()
    }
}

impl Drop for TestPortAllocator {
    fn drop(&mut self) {
        drop(fs::remove_file(&self.claim_file));
    }
}

fn handshake_dir() -> PathBuf {
    std::env::temp_dir().join("logos-e2e-port-blocks")
}

fn write_port_claim_metadata(
    file: &mut fs::File,
    owner: &str,
    block_start: u16,
    block_end: u16,
) -> std::io::Result<()> {
    let pid = std::process::id();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    writeln!(
        file,
        "pid={pid}\nowner={owner}\nblock_start={block_start}\nblock_end={block_end}\ncwd={}",
        cwd.display()
    )
}

fn try_reap_stale_port_claim_file(claim_file: &Path) {
    let Ok(contents) = fs::read_to_string(claim_file) else {
        try_reap_malformed_claim_file(claim_file);
        return;
    };

    let pid = contents
        .lines()
        .find_map(|line| line.strip_prefix("pid="))
        .and_then(|pid| pid.parse::<u32>().ok());

    let Some(pid) = pid else {
        try_reap_malformed_claim_file(claim_file);
        return;
    };

    if !process_exists(pid) {
        drop(fs::remove_file(claim_file));
    }
}

fn try_reap_malformed_claim_file(claim_file: &Path) {
    // A claim file is visible before its metadata is fully written. Give fresh
    // malformed files time to become valid instead of deleting a live claim.
    if claim_file_age(claim_file).is_some_and(|age| age >= MALFORMED_CLAIM_FILE_REAP_GRACE) {
        drop(fs::remove_file(claim_file));
    }
}

fn claim_file_age(claim_file: &Path) -> Option<Duration> {
    fs::metadata(claim_file)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
}

fn process_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

fn is_tcp_port_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn is_udp_port_available(port: u16) -> bool {
    UdpSocket::bind(("127.0.0.1", port)).is_ok()
}

fn hash_str(s: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}
