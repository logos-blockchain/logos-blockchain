use std::{
    collections::HashSet,
    net::{TcpListener, UdpSocket},
    sync::{LazyLock, Mutex},
};

const TEST_PORT_BLOCK_SIZE: u16 = 256;
const TEST_PORT_RANGE_START: u16 = 20_000;
const TEST_PORT_RANGE_END: u16 = 55_000;
const TEST_PORT_CLAIM_RANGE_START: u16 = TEST_PORT_RANGE_END + 1;

static USED_TCP_PORTS: LazyLock<Mutex<HashSet<u16>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static USED_UDP_PORTS: LazyLock<Mutex<HashSet<u16>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// A process-local allocator backed by a kernel-owned TCP socket lease.
///
/// Each block maps to a dedicated claim port above the test data-port range.
/// The lease remains bound for the lifetime of the allocator, so concurrent
/// processes cannot reserve the same block. The operating system releases it
/// automatically if the process terminates abnormally. Before accepting a
/// block, all data ports are probed so blocks still used by orphaned child
/// processes are skipped.
#[derive(Debug)]
pub struct ReservedPortBlock {
    lease: TcpListener,
    block_start: u16,
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

        for (block_index, block_start) in (TEST_PORT_RANGE_START..=max_block_start)
            .step_by(usize::from(TEST_PORT_BLOCK_SIZE))
            .enumerate()
        {
            let tcp_end = block_start + (TEST_PORT_BLOCK_SIZE / 2) - 1;
            let udp_next = tcp_end + 1;
            let udp_end = block_start + TEST_PORT_BLOCK_SIZE - 1;
            let claim_port =
                TEST_PORT_CLAIM_RANGE_START.checked_add(u16::try_from(block_index).ok()?)?;

            // Holding this socket is the cross-process claim. Unlike a claim
            // file, it cannot become stale or be removed by an ABA race.
            let Ok(lease) = TcpListener::bind(("127.0.0.1", claim_port)) else {
                continue;
            };

            if !all_ports_available(block_start, tcp_end, udp_next, udp_end) {
                continue;
            }

            return Some(Self {
                lease,
                block_start,
                tcp_next: block_start,
                tcp_end,
                udp_next,
                udp_end,
            });
        }

        None
    }

    /// Returns the first port in this allocator's reserved block.
    #[must_use]
    pub const fn block_start(&self) -> u16 {
        self.block_start
    }

    /// Returns the dedicated TCP port that owns this block's kernel lease.
    #[must_use]
    pub fn claim_port(&self) -> Option<u16> {
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
        net::{TcpListener, UdpSocket},
        path::PathBuf,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use super::{
        ReservedPortBlock, TEST_PORT_BLOCK_SIZE, TEST_PORT_CLAIM_RANGE_START, TEST_PORT_RANGE_END,
        TEST_PORT_RANGE_START,
    };

    const CHILD_MARKER_ENV: &str = "LOGOS_PORT_BLOCK_TEST_MARKER";
    const CHILD_RELEASE_ENV: &str = "LOGOS_PORT_BLOCK_TEST_RELEASE";
    const CHILD_PORT_COUNT: usize = 16;

    #[derive(Debug)]
    struct ChildReservation {
        block_start: u16,
        claim_port: u16,
        tcp_ports: Vec<u16>,
        udp_ports: Vec<u16>,
    }

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
        let marker_contents = if all_children_ready {
            marker_paths
                .iter()
                .map(fs::read_to_string)
                .collect::<Result<Vec<_>, _>>()
        } else {
            Ok(Vec::new())
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

        let reservations = marker_contents
            .expect("child markers should be readable")
            .iter()
            .map(|contents| parse_child_reservation(contents))
            .collect::<Vec<_>>();
        assert_child_reservations(&reservations, CHILD_COUNT);
    }

    fn assert_child_reservations(reservations: &[ChildReservation], child_count: usize) {
        let block_starts = reservations
            .iter()
            .map(|reservation| reservation.block_start)
            .collect::<HashSet<_>>();
        assert_eq!(
            block_starts.len(),
            child_count,
            "concurrent processes must reserve distinct port blocks"
        );

        let claim_ports = reservations
            .iter()
            .map(|reservation| reservation.claim_port)
            .collect::<HashSet<_>>();
        assert_eq!(
            claim_ports.len(),
            child_count,
            "concurrent processes must hold distinct claim ports"
        );

        let mut allocated_ports = HashSet::new();
        for reservation in reservations {
            let tcp_end = reservation.block_start + TEST_PORT_BLOCK_SIZE / 2;
            let udp_end = reservation.block_start + TEST_PORT_BLOCK_SIZE;

            assert!(
                (TEST_PORT_RANGE_START..=TEST_PORT_RANGE_END).contains(&reservation.block_start),
                "child block must start inside the configured data-port range"
            );
            assert_eq!(
                (reservation.block_start - TEST_PORT_RANGE_START) % TEST_PORT_BLOCK_SIZE,
                0,
                "child block must start on a configured block boundary"
            );
            assert!(
                reservation.claim_port > TEST_PORT_RANGE_END,
                "the claim port must be outside the data-port range"
            );
            assert_eq!(reservation.tcp_ports.len(), CHILD_PORT_COUNT);
            assert_eq!(reservation.udp_ports.len(), CHILD_PORT_COUNT);
            assert!(
                reservation
                    .tcp_ports
                    .iter()
                    .all(|port| (reservation.block_start..tcp_end).contains(port)),
                "TCP ports must remain inside the child process's TCP half"
            );
            assert!(
                reservation
                    .udp_ports
                    .iter()
                    .all(|port| (tcp_end..udp_end).contains(port)),
                "UDP ports must remain inside the child process's UDP half"
            );

            for port in reservation.tcp_ports.iter().chain(&reservation.udp_ports) {
                assert!(
                    allocated_ports.insert(*port),
                    "data ports must be globally unique across child processes"
                );
            }
        }
        assert_eq!(
            allocated_ports.len(),
            child_count * CHILD_PORT_COUNT * 2,
            "every child must retain all allocated TCP and UDP ports"
        );
    }

    #[test]
    fn reservation_preserves_full_tcp_and_udp_halves() {
        let mut allocator = ReservedPortBlock::try_new().expect("a test port block should exist");
        let block_start = allocator.block_start();
        let half_block_size = TEST_PORT_BLOCK_SIZE / 2;
        let expected_tcp_end = block_start + half_block_size - 1;
        let expected_udp_start = block_start + half_block_size;
        let expected_block_end = block_start + TEST_PORT_BLOCK_SIZE - 1;
        let claim_port = allocator
            .claim_port()
            .expect("lease address should be available");

        assert_eq!(allocator.tcp_next, block_start);
        assert_eq!(allocator.tcp_end, expected_tcp_end);
        assert_eq!(allocator.udp_next, expected_udp_start);
        assert_eq!(allocator.udp_end, expected_block_end);
        assert_eq!(allocator.tcp_end - allocator.tcp_next + 1, half_block_size);
        assert_eq!(allocator.udp_end - allocator.udp_next + 1, half_block_size);
        assert_eq!(
            allocator.next_tcp_port(),
            Some(block_start),
            "the first TCP data port must remain allocatable"
        );
        assert!(
            claim_port >= TEST_PORT_CLAIM_RANGE_START,
            "the lease must use the dedicated claim range"
        );
        assert!(
            claim_port > TEST_PORT_RANGE_END,
            "the lease must not consume a data port"
        );

        let candidate_count =
            ((TEST_PORT_RANGE_END - TEST_PORT_RANGE_START + 1) / TEST_PORT_BLOCK_SIZE) as usize;
        let claim_port_capacity = usize::from(u16::MAX - TEST_PORT_CLAIM_RANGE_START) + 1;
        let claim_range_end = TEST_PORT_CLAIM_RANGE_START
            + u16::try_from(candidate_count).expect("candidate count should fit in u16")
            - 1;
        assert!(
            claim_port_capacity >= candidate_count,
            "the claim range must cover every candidate block"
        );
        assert!(
            claim_port <= claim_range_end,
            "the lease must map into the candidate block's claim range"
        );
    }

    #[test]
    fn reservation_skips_a_block_with_orphaned_data_ports() {
        let max_block_start = TEST_PORT_RANGE_END - (TEST_PORT_BLOCK_SIZE - 1);
        let mut preceding_claims = Vec::new();
        let (occupied_block_start, occupied_claim, tcp_socket, udp_socket) = (TEST_PORT_RANGE_START
            ..=max_block_start)
            .step_by(usize::from(TEST_PORT_BLOCK_SIZE))
            .enumerate()
            .find_map(|(block_index, block_start)| {
                let claim_port = TEST_PORT_CLAIM_RANGE_START
                    + u16::try_from(block_index).expect("block index should fit in u16");
                let claim = TcpListener::bind(("127.0.0.1", claim_port)).ok()?;
                let Ok(tcp_socket) = TcpListener::bind(("127.0.0.1", block_start)) else {
                    preceding_claims.push(claim);
                    return None;
                };
                let udp_port = block_start + TEST_PORT_BLOCK_SIZE / 2;
                let Ok(udp_socket) = UdpSocket::bind(("127.0.0.1", udp_port)) else {
                    preceding_claims.push(claim);
                    return None;
                };

                Some((block_start, claim, tcp_socket, udp_socket))
            })
            .expect("a block should be available for the orphaned-port test");

        drop(occupied_claim);

        let allocator = ReservedPortBlock::try_new().expect("a later test port block should exist");

        assert_ne!(
            allocator.block_start(),
            occupied_block_start,
            "an occupied data port must make the allocator skip the entire block"
        );
        drop((preceding_claims, tcp_socket, udp_socket));
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
        let mut allocator =
            ReservedPortBlock::try_new().expect("child should reserve a port block");

        let block_start = allocator.block_start();
        let claim_port = allocator
            .claim_port()
            .expect("child claim address should be available");
        let mut tcp_ports = Vec::with_capacity(CHILD_PORT_COUNT);
        let mut tcp_sockets = Vec::with_capacity(CHILD_PORT_COUNT);
        let mut udp_ports = Vec::with_capacity(CHILD_PORT_COUNT);
        let mut udp_sockets = Vec::with_capacity(CHILD_PORT_COUNT);

        for _ in 0..CHILD_PORT_COUNT {
            let port = allocator
                .next_tcp_port()
                .expect("child should allocate a TCP port");
            let socket = TcpListener::bind(("127.0.0.1", port))
                .expect("child should immediately bind its allocated TCP port");
            tcp_ports.push(port);
            tcp_sockets.push(socket);
        }
        for _ in 0..CHILD_PORT_COUNT {
            let port = allocator
                .next_udp_port()
                .expect("child should allocate a UDP port");
            let socket = UdpSocket::bind(("127.0.0.1", port))
                .expect("child should immediately bind its allocated UDP port");
            udp_ports.push(port);
            udp_sockets.push(socket);
        }

        let marker = format!(
            "{block_start}\n{claim_port}\n{}\n{}\n",
            join_ports(&tcp_ports),
            join_ports(&udp_ports)
        );
        let marker_temp_path = marker_path.with_extension("tmp");
        fs::write(&marker_temp_path, marker).expect("child marker should be written");
        fs::rename(marker_temp_path, &marker_path).expect("child marker should be published");

        let deadline = Instant::now() + TIMEOUT;
        while !release_path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }

        assert!(release_path.exists(), "parent should release child process");
        drop((tcp_sockets, udp_sockets));
    }

    fn join_ports(ports: &[u16]) -> String {
        ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }

    fn parse_child_reservation(contents: &str) -> ChildReservation {
        let mut lines = contents.lines();
        let block_start = lines
            .next()
            .expect("child marker should contain a block start")
            .parse()
            .expect("child block start should be a port");
        let claim_port = lines
            .next()
            .expect("child marker should contain a claim port")
            .parse()
            .expect("child claim port should be a port");
        let tcp_ports = parse_ports(lines.next().expect("child marker should contain TCP ports"));
        let udp_ports = parse_ports(lines.next().expect("child marker should contain UDP ports"));
        assert!(
            lines.next().is_none(),
            "child marker should not contain extra fields"
        );

        ChildReservation {
            block_start,
            claim_port,
            tcp_ports,
            udp_ports,
        }
    }

    fn parse_ports(ports: &str) -> Vec<u16> {
        ports
            .split(',')
            .map(|port| port.parse().expect("child data port should be valid"))
            .collect()
    }
}
