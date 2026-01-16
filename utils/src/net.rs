use std::{
    collections::HashSet,
    net::{TcpListener, UdpSocket},
    sync::{LazyLock, Mutex},
};

static USED_TCP_PORTS: LazyLock<Mutex<HashSet<u16>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static USED_UDP_PORTS: LazyLock<Mutex<HashSet<u16>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

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
