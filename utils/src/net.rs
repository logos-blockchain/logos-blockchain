use std::{
    collections::HashSet,
    net::{TcpListener, UdpSocket},
    sync::{LazyLock, Mutex},
};

const TEST_PORT_BLOCK_SIZE: u16 = 256;
const TEST_PORT_RANGE_START: u16 = 20_000;
const TEST_PORT_RANGE_END: u16 = 55_000;

static USED_TCP_PORTS: LazyLock<Mutex<HashSet<u16>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static USED_UDP_PORTS: LazyLock<Mutex<HashSet<u16>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// A process-local allocator backed by a kernel-owned TCP socket lease.
///
/// The lease remains bound for the lifetime of the allocator, so concurrent
/// processes cannot reserve the same block. The operating system releases it
/// automatically if the process terminates abnormally. Before accepting a
/// block, all remaining ports are probed so blocks still used by orphaned child
/// processes are skipped.
#[derive(Debug)]
pub struct ReservedPortBlock {
    lease: TcpListener,
    tcp_next: u16,
    tcp_end: u16,
    udp_next: u16,
    udp_end: u16,
}

impl ReservedPortBlock {
    /// Reserves one test port block for the lifetime of the returned allocator.
    #[must_use]
    pub fn try_new() -> Option<Self> {
        let max_block_start =
            TEST_PORT_RANGE_END.checked_sub(TEST_PORT_BLOCK_SIZE.saturating_sub(1))?;

        for block_start in
            (TEST_PORT_RANGE_START..=max_block_start).step_by(usize::from(TEST_PORT_BLOCK_SIZE))
        {
            let tcp_end = block_start + (TEST_PORT_BLOCK_SIZE / 2) - 1;
            let udp_next = tcp_end + 1;
            let udp_end = block_start + TEST_PORT_BLOCK_SIZE - 1;

            // Holding this socket is the cross-process claim. Unlike a claim
            // file, it cannot become stale or be removed by an ABA race.
            let Ok(lease) = TcpListener::bind(("127.0.0.1", block_start)) else {
                continue;
            };

            if !all_ports_available(block_start + 1, tcp_end, udp_next, udp_end) {
                continue;
            }

            return Some(Self {
                lease,
                // The first TCP port is kept bound as the block lease and is
                // therefore intentionally not returned to callers.
                tcp_next: block_start + 1,
                tcp_end,
                udp_next,
                udp_end,
            });
        }

        None
    }

    /// Returns the first port in this allocator's reserved block.
    #[must_use]
    pub fn block_start(&self) -> Option<u16> {
        self.lease.local_addr().ok().map(|address| address.port())
    }

    /// Returns an available TCP port from the reserved block.
    pub fn next_tcp_port(&mut self) -> Option<u16> {
        while self.tcp_next <= self.tcp_end {
            let candidate = self.tcp_next;
            self.tcp_next = self.tcp_next.saturating_add(1);

            if is_tcp_port_available(candidate) {
                return Some(candidate);
            }
        }

        get_available_tcp_port()
    }

    /// Returns an available UDP port from the reserved block.
    pub fn next_udp_port(&mut self) -> Option<u16> {
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

fn all_ports_available(tcp_start: u16, tcp_end: u16, udp_start: u16, udp_end: u16) -> bool {
    (tcp_start..=tcp_end).all(is_tcp_port_available)
        && (udp_start..=udp_end).all(is_udp_port_available)
}

fn is_tcp_port_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn is_udp_port_available(port: u16) -> bool {
    UdpSocket::bind(("127.0.0.1", port)).is_ok()
}

/// Get an available TCP port from the OS by binding to port 0.
///
/// Returns the actual port number assigned by the OS, or None if unable to get
/// one. Keeps track of used TCP ports to ensure no reuse within the same test
/// run.
pub fn get_available_tcp_port() -> Option<u16> {
    for _ in 0..100 {
        // Limit retries to avoid infinite loop
        let port = TcpListener::bind("127.0.0.1:0")
            .ok()?
            .local_addr()
            .ok()?
            .port();

        let mut used_ports = USED_TCP_PORTS.lock().ok()?;
        if used_ports.insert(port) {
            return Some(port);
        }
    }
    None
}

/// Get an available UDP port from the OS by binding to port 0.
///
/// Returns the actual port number assigned by the OS, or None if unable to get
/// one. Keeps track of used UDP ports to ensure no reuse within the same test
/// run.
pub fn get_available_udp_port() -> Option<u16> {
    for _ in 0..100 {
        // Limit retries to avoid infinite loop
        let port = UdpSocket::bind("127.0.0.1:0")
            .ok()?
            .local_addr()
            .ok()?
            .port();

        let mut used_ports = USED_UDP_PORTS.lock().ok()?;
        if used_ports.insert(port) {
            return Some(port);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        env, fs,
        path::{Path, PathBuf},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use super::ReservedPortBlock;

    const CHILD_MARKER_ENV: &str = "LOGOS_PORT_BLOCK_TEST_MARKER";
    const CHILD_RELEASE_ENV: &str = "LOGOS_PORT_BLOCK_TEST_RELEASE";

    #[test]
    fn reservations_are_unique_across_processes() {
        const CHILD_COUNT: usize = 4;
        const TIMEOUT: Duration = Duration::from_secs(20);

        let temp_dir = tempfile::tempdir().expect("temporary directory should be created");
        let release_path = temp_dir.path().join("release");
        let current_exe = env::current_exe().expect("current test executable should be available");
        let mut children = Vec::with_capacity(CHILD_COUNT);
        let mut marker_paths = Vec::with_capacity(CHILD_COUNT);

        for child_index in 0..CHILD_COUNT {
            let marker_path = temp_dir.path().join(format!("child-{child_index}"));
            let child = Command::new(&current_exe)
                .args([
                    "--ignored",
                    "--exact",
                    "net::tests::reservation_child_process",
                ])
                .env(CHILD_MARKER_ENV, &marker_path)
                .env(CHILD_RELEASE_ENV, &release_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("child test process should start");
            children.push(child);
            marker_paths.push(marker_path);
        }

        let deadline = Instant::now() + TIMEOUT;
        while marker_paths.iter().any(|path| !path.exists()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }

        let all_children_ready = marker_paths.iter().all(|path| path.exists());
        let block_starts = if all_children_ready {
            marker_paths
                .iter()
                .map(|path| read_block_start(path))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        fs::write(&release_path, b"release").expect("children should be released");
        let statuses = children
            .iter_mut()
            .map(|child| child.wait().expect("child test process should exit"))
            .collect::<Vec<_>>();

        assert!(
            all_children_ready,
            "all child processes should acquire a block"
        );
        assert!(
            statuses.iter().all(std::process::ExitStatus::success),
            "all child processes should exit successfully"
        );
        assert_eq!(
            block_starts.iter().copied().collect::<HashSet<_>>().len(),
            CHILD_COUNT,
            "concurrent processes must reserve distinct port blocks"
        );
    }

    #[test]
    #[ignore = "helper process invoked by reservations_are_unique_across_processes"]
    fn reservation_child_process() {
        const TIMEOUT: Duration = Duration::from_secs(20);

        let marker_path = PathBuf::from(
            env::var_os(CHILD_MARKER_ENV).expect("child marker environment variable should exist"),
        );
        let release_path = PathBuf::from(
            env::var_os(CHILD_RELEASE_ENV)
                .expect("child release environment variable should exist"),
        );
        let allocator = ReservedPortBlock::try_new().expect("child should reserve a port block");

        let block_start = allocator
            .block_start()
            .expect("lease address should be available");
        fs::write(&marker_path, block_start.to_string()).expect("child marker should be written");

        let deadline = Instant::now() + TIMEOUT;
        while !release_path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }

        assert!(release_path.exists(), "parent should release child process");
    }

    fn read_block_start(path: &Path) -> u16 {
        fs::read_to_string(path)
            .expect("child marker should be readable")
            .parse()
            .expect("child marker should contain a port block start")
    }
}
