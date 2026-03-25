use std::{
    fs,
    fs::OpenOptions,
    io::Write as _,
    net::{TcpListener, UdpSocket},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

/// Total size of one reserved block per test process.
///
/// Half is used for TCP, half for UDP.
const PORT_BLOCK_SIZE: u16 = 256;

/// Inclusive start of the overall test port range.
const PORT_RANGE_START: u16 = 20_000;

/// Inclusive end of the overall test port range.
const PORT_RANGE_END: u16 = 55_000;

/// One allocator slot per process.
///
/// Cross-process coordination is done via lock files in the temp directory.
/// We keep the allocator inside an Option so it can be explicitly released.
static TEST_PORT_ALLOCATOR: OnceLock<Mutex<Option<TestPortAllocator>>> = OnceLock::new();

static PROCESS_START_NONCE: OnceLock<String> = OnceLock::new();

// A nonce that is unique to the current process and start time.
fn process_start_nonce() -> &'static str {
    PROCESS_START_NONCE.get_or_init(|| {
        let started_at_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();

        format!("{started_at_ns:016x}-{:08x}", std::process::id())
    })
}

/// Returns a unique string keyed to the currently-running test start time,
/// process id, thread/test name, optional nextest control parameters and
/// optional GitHub runner name.
#[must_use]
pub fn owner_for_current_test() -> String {
    let current_thread = std::thread::current();
    let thread_name = current_thread.name().unwrap_or("genesis");

    // Example test owner string:
    //   "
    //     nonce=18a1a082a644b828-000e545b, \
    //     attempt=3c3c0f35-db4b-41d5-8668-7aafa89e55fb:logos-blockchain-tests::\
    //     test_cli_restart$node_restart_w_init_peers, \
    //     workspace_root=/home/pluto/Code/logos/logos-blockchain, \
    //     thread=node_restart_w_init_peers, \
    //     runner=none
    //   "
    format!(
        "nonce={}, attempt={}, workspace_root={}, thread={}, runner={}",
        process_start_nonce(),
        // Nextest sets these vars per-test-run; they are empty (and harmless) under
        // plain `cargo test`.
        std::env::var("NEXTEST_ATTEMPT_ID").unwrap_or_else(|_| "none".to_owned()),
        std::env::var("NEXTEST_WORKSPACE_ROOT").unwrap_or_else(|_| "none".to_owned()),
        thread_name,
        // Set by GitHub Actions on self-hosted runners; empty outside CI.
        std::env::var("RUNNER_NAME").unwrap_or_else(|_| "none".to_owned()),
    )
}

#[derive(Debug)]
struct TestPortAllocator {
    /// The lock file that proves this process owns its port block.
    claim_file: PathBuf,

    /// Next TCP port candidate in this process's reserved block.
    tcp_next: u16,

    /// Final TCP port in this process's reserved block.
    tcp_end: u16,

    /// Next UDP port candidate in this process's reserved block.
    udp_next: u16,

    /// Final UDP port in this process's reserved block.
    udp_end: u16,
}

impl TestPortAllocator {
    fn new() -> Option<Self> {
        let handshake_dir = std::env::temp_dir().join("logos-e2e-port-blocks");
        fs::create_dir_all(&handshake_dir).ok()?;

        let owner = owner_for_current_test();

        // Example block starts for PORT_BLOCK_SIZE=256:
        // 20000, 20256, 20512, ...
        let max_block_start = PORT_RANGE_END.checked_sub(PORT_BLOCK_SIZE - 1)?;

        for block_start in (PORT_RANGE_START..=max_block_start).step_by(PORT_BLOCK_SIZE as usize) {
            let block_end = block_start + PORT_BLOCK_SIZE - 1;
            let claim_file = handshake_dir.join(format!("{block_start}.lock"));

            // First try to reap an obviously stale lock from a dead pid.
            if claim_file.exists() {
                try_reap_stale_claim_file(&claim_file);
            }

            // The existence of this file is the reservation.
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&claim_file)
            {
                Ok(mut file) => {
                    write_claim_metadata(&mut file, &owner, block_start, block_end).ok()?;

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
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // This block is currently claimed by another live process,
                    // or a race occurred while
                    // reaping/claiming. Try the next block.
                }
                Err(_) => {
                    return None;
                }
            }
        }

        None
    }

    /// Returns an available TCP port from this allocator's reserved block.
    fn next_tcp_port(&mut self) -> Option<u16> {
        while self.tcp_next <= self.tcp_end {
            let port = self.tcp_next;
            self.tcp_next += 1;

            if TcpListener::bind(("127.0.0.1", port)).is_ok() {
                return Some(port);
            }
        }

        None
    }

    /// Returns an available UDP port from this allocator's reserved block.
    fn next_udp_port(&mut self) -> Option<u16> {
        while self.udp_next <= self.udp_end {
            let port = self.udp_next;
            self.udp_next += 1;

            if UdpSocket::bind(("127.0.0.1", port)).is_ok() {
                return Some(port);
            }
        }

        None
    }
}

impl Drop for TestPortAllocator {
    fn drop(&mut self) {
        drop(fs::remove_file(&self.claim_file));
    }
}

fn test_port_allocator_slot() -> &'static Mutex<Option<TestPortAllocator>> {
    TEST_PORT_ALLOCATOR.get_or_init(|| Mutex::new(None))
}

fn with_test_port_allocator<T>(f: impl FnOnce(&mut TestPortAllocator) -> Option<T>) -> Option<T> {
    let slot = test_port_allocator_slot();
    let mut guard = slot.lock().ok()?;

    if guard.is_none() {
        *guard = Some(TestPortAllocator::new()?);
    }

    f(guard.as_mut().expect("allocator just initialized"))
}

fn write_claim_metadata(
    file: &mut fs::File,
    owner: &str,
    block_start: u16,
    block_end: u16,
) -> std::io::Result<()> {
    let tcp_start = block_start;
    let tcp_end = block_start + (PORT_BLOCK_SIZE / 2) - 1;
    let udp_start = tcp_end + 1;
    let udp_end = block_end;

    writeln!(file, "owner={owner}")?;
    writeln!(file, "pid={}", std::process::id())?;
    writeln!(file, "block_start={block_start}")?;
    writeln!(file, "block_end={block_end}")?;
    writeln!(file, "tcp_range={tcp_start}-{tcp_end}")?;
    writeln!(file, "udp_range={udp_start}-{udp_end}")?;
    Ok(())
}

fn read_pid_from_claim_file(path: &Path) -> Option<u32> {
    let contents = fs::read_to_string(path).ok()?;

    for line in contents.lines() {
        if let Some(pid) = line.strip_prefix("pid=") {
            return pid.trim().parse::<u32>().ok();
        }
    }

    None
}

fn is_pid_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

fn try_reap_stale_claim_file(path: &Path) {
    let Some(pid) = read_pid_from_claim_file(path) else {
        return;
    };

    if !is_pid_alive(pid) {
        drop(fs::remove_file(path));
    }
}

/// Returns an available TCP port from this process's reserved port block.
#[must_use]
pub fn get_reserved_available_tcp_port() -> Option<u16> {
    with_test_port_allocator(TestPortAllocator::next_tcp_port)
}

/// Returns an available UDP port from this process's reserved port block.
#[must_use]
pub fn get_reserved_available_udp_port() -> Option<u16> {
    with_test_port_allocator(TestPortAllocator::next_udp_port)
}

/// Explicitly releases this process's reserved port block.
///
/// Call this near the end of the test process or harness teardown so that
/// normal exits do not leave lock files behind.
pub fn release_reserved_port_block() {
    let slot = test_port_allocator_slot();

    let Ok(mut guard) = slot.lock() else {
        return;
    };

    // Taking the allocator out of the slot drops it here, which removes the
    // claim file via Drop.
    drop(guard.take());
}
