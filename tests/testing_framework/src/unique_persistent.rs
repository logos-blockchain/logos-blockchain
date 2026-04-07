use std::{
    fs,
    fs::OpenOptions,
    hash::{Hash as _, Hasher as _},
    io::Write as _,
    net::{TcpListener, UdpSocket},
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use lb_utils::net::{get_available_tcp_port, get_available_udp_port};

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

#[derive(Debug)]
struct TestPortAllocator {
    // The lock file that proves this process owns its port block.
    claim_file: PathBuf,
    // Next TCP port candidate in this process's reserved block.
    tcp_next: u16,
    // Final TCP port in this process's reserved block.
    tcp_end: u16,
    // Next UDP port candidate in this process's reserved block.
    udp_next: u16,
    // Final UDP port in this process's reserved block.
    udp_end: u16,
}

impl TestPortAllocator {
    fn new() -> Option<Self> {
        fs::create_dir_all(handshake_dir()).ok()?;

        let owner = format!("process_start_nonce={}", process_start_nonce());

        // Example block starts for PORT_BLOCK_SIZE=256:
        // 20000, 20256, 20512, ...
        let max_block_start = PORT_RANGE_END.checked_sub(PORT_BLOCK_SIZE - 1)?;

        for block_start in (PORT_RANGE_START..=max_block_start).step_by(PORT_BLOCK_SIZE as usize) {
            let block_end = block_start + PORT_BLOCK_SIZE - 1;
            let claim_file = handshake_dir().join(format!("{block_start}.lock"));

            // First try to reap an obviously stale lock from a dead pid.
            if claim_file.exists() {
                try_reap_stale_port_claim_file(&claim_file);
            }

            // The existence of this file is the reservation.
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&claim_file)
            {
                Ok(mut file) => {
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
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // This block is currently claimed by another live process,
                    // or a race occurred while reaping/claiming. Try the next
                    // block.
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
        // The persistent blocks may be exhausted, try global allocation (failsafe)
        get_available_tcp_port()
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
        // The persistent blocks may be exhausted, try global allocation (failsafe)
        get_available_udp_port()
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

fn write_port_claim_metadata(
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
    if pid == 0 {
        return true;
    }

    #[cfg(unix)]
    {
        // `ps -p <pid> -o pid=` prints an empty line when the process is absent.
        // Be conservative: if probing fails, treat PID as alive to avoid deleting a
        // live claim.
        let output = Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("pid=")
            .output();

        match output {
            // process exists
            Ok(out) if out.status.success() => {
                !String::from_utf8_lossy(&out.stdout).trim().is_empty()
            }
            // process absent
            Ok(out) if out.status.code() == Some(1) => false,
            // probe failed -> conservative
            _ => true,
        }
    }

    #[cfg(windows)]
    {
        // PowerShell exits 0 when the PID exists and 1 when it does not.
        // Be conservative on probe errors to avoid deleting a live claim.
        let status = Command::new("powershell")
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(format!(
                "if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"
            ))
            .status();

        return match status {
            // process exists
            Ok(s) if s.code() == Some(0) => true,
            // process absent
            Ok(s) if s.code() == Some(1) => false,
            // probe failed -> conservative
            _ => true,
        };
    }

    #[cfg(not(any(unix, windows)))]
    {
        // Unknown platform: fail closed and avoid stale-lock reaping.
        true
    }
}

fn try_reap_stale_port_claim_file(path: &Path) {
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

fn handshake_dir() -> PathBuf {
    std::env::temp_dir().join("logos-e2e-port-blocks")
}

/// Reaps all stale lock files in the port-blocks directory that belong to
/// dead processes. Call this once at process startup.
pub fn reap_all_stale_port_blocks() {
    if let Ok(entries) = fs::read_dir(handshake_dir()) {
        for entry in entries.flatten() {
            try_reap_stale_port_claim_file(&entry.path());
        }
    }
}

/// Create a short 8-byte hash from string
#[must_use]
pub fn hash_str(s: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}
