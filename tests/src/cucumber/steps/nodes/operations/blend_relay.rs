use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
};

use lb_core::sdp::ServiceType;
use lb_libp2p::{Multiaddr, Protocol};
use lb_node::config::RunConfig;
use lb_testing_framework::get_reserved_available_udp_port;
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tracing::warn;

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::nodes::diagnostics::log_blend_relay_event,
    world::{BlendDiagnosticPhase, CucumberWorld},
};

const UDP_BUFFER_SIZE: usize = 65_536;

#[derive(Clone, Copy, Debug)]
pub struct BlendRelayMetadata {
    pub declared_addr: SocketAddr,
    pub backend_addr: SocketAddr,
}

#[derive(Clone, Default)]
pub struct BlendRelayRegistry {
    inner: Arc<Mutex<RelayRegistryInner>>,
}

#[derive(Default)]
struct RelayRegistryInner {
    enabled: bool,
    relays: HashMap<String, RelayEntry>,
}

struct RelayEntry {
    metadata: BlendRelayMetadata,
    relay: BlendRelay,
    enabled: bool,
}

impl BlendRelayRegistry {
    pub fn enable(&self) -> Result<(), io::Error> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("Blend relay registry lock poisoned"))?;
        if inner.enabled {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "controllable Blend provider relays are already enabled",
            ));
        }
        inner.enabled = true;
        drop(inner);
        Ok(())
    }

    pub fn is_enabled(&self) -> Result<bool, io::Error> {
        self.inner
            .lock()
            .map(|inner| inner.enabled)
            .map_err(|_| io::Error::other("Blend relay registry lock poisoned"))
    }

    pub fn configure_provider(
        &self,
        node_name: &str,
        config: &mut RunConfig,
        declared_address: &Multiaddr,
    ) -> Result<(), io::Error> {
        if !self.is_enabled()? || !is_declared_blend_provider(config)? {
            return Ok(());
        }

        let declared_addr = socket_addr_from_multiaddr(declared_address)?;
        let existing_metadata = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| io::Error::other("Blend relay registry lock poisoned"))?;
            inner.relays.get(node_name).map(|entry| entry.metadata)
        };
        if let Some(metadata) = existing_metadata {
            if metadata.declared_addr != declared_addr {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "Blend relay for node `{node_name}` is already configured for `{}`, not `{declared_addr}`",
                        metadata.declared_addr
                    ),
                ));
            }
            let IpAddr::V4(backend_ip) = metadata.backend_addr.ip() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "Blend relay backend address `{}` is not an IPv4 address",
                        metadata.backend_addr
                    ),
                ));
            };
            config.user.blend.core.backend.listening_address =
                lb_libp2p::multiaddr(backend_ip, metadata.backend_addr.port());
            return Ok(());
        }
        let backend_port = get_reserved_available_udp_port().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "failed to allocate a backend Blend relay UDP port",
            )
        })?;
        let backend_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, backend_port));
        let relay = BlendRelay::bind(declared_addr, backend_addr)?;

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("Blend relay registry lock poisoned"))?;
        if inner.relays.contains_key(node_name) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("Blend relay for node `{node_name}` is already configured"),
            ));
        }
        inner.relays.insert(
            node_name.to_owned(),
            RelayEntry {
                metadata: BlendRelayMetadata {
                    declared_addr,
                    backend_addr,
                },
                relay,
                enabled: true,
            },
        );
        drop(inner);
        config.user.blend.core.backend.listening_address =
            lb_libp2p::multiaddr(Ipv4Addr::LOCALHOST, backend_port);
        Ok(())
    }

    pub fn metadata(&self, node_name: &str) -> Result<Option<BlendRelayMetadata>, io::Error> {
        self.inner
            .lock()
            .map(|inner| inner.relays.get(node_name).map(|entry| entry.metadata))
            .map_err(|_| io::Error::other("Blend relay registry lock poisoned"))
    }

    pub fn remove_provider(&self, node_name: &str) -> Result<bool, io::Error> {
        let relay = {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| io::Error::other("Blend relay registry lock poisoned"))?;
            inner.relays.remove(node_name)
        };
        let Some(relay) = relay else {
            return Ok(false);
        };
        drop(relay);
        Ok(true)
    }

    pub async fn set_enabled(&self, node_name: &str, enabled: bool) -> Result<(), io::Error> {
        let control = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| io::Error::other("Blend relay registry lock poisoned"))?;
            let entry = inner.relays.get(node_name).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no controllable Blend relay is configured for node `{node_name}`"),
                )
            })?;
            if entry.enabled == enabled {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "Blend reachability for node `{node_name}` is already {}",
                        if enabled { "enabled" } else { "disabled" }
                    ),
                ));
            }
            let control = entry.relay.control.clone();
            drop(inner);
            control
        };

        control.set_enabled(enabled).await?;

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("Blend relay registry lock poisoned"))?;
        if let Some(entry) = inner.relays.get_mut(node_name) {
            entry.enabled = enabled;
        }
        drop(inner);
        Ok(())
    }

    pub fn shutdown(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.relays.clear();
            inner.enabled = false;
        }
    }
}

pub async fn set_blend_reachability(
    world: &mut CucumberWorld,
    node_name: &str,
    reachable: bool,
) -> StepResult {
    if !world.blend_relays.is_enabled()? {
        return Err(StepError::InvalidArgument {
            message: "controllable Blend provider relays were not enabled for this scenario"
                .to_owned(),
        });
    }
    if !world.nodes_info.contains_key(node_name) {
        return Err(StepError::LogicalError {
            message: format!("Node `{node_name}` is not running"),
        });
    }
    if world.blend_diagnostics.phase.is_none() {
        return Err(StepError::InvalidArgument {
            message: "Blend reachability operations require an active Blend diagnostic".to_owned(),
        });
    }
    if world.blend_diagnostics.stopped_nodes.contains(node_name) {
        return Err(StepError::InvalidArgument {
            message: format!("Node `{node_name}` is stopped; Blend relay cannot be controlled"),
        });
    }

    let metadata =
        world
            .blend_relays
            .metadata(node_name)?
            .ok_or_else(|| StepError::InvalidArgument {
                message: format!("Node `{node_name}` does not have a controllable Blend relay"),
            })?;
    world
        .blend_relays
        .set_enabled(node_name, reachable)
        .await
        .map_err(|error| StepError::LogicalError {
            message: format!("failed to change Blend reachability for `{node_name}`: {error}"),
        })?;

    if reachable {
        world
            .blend_diagnostics
            .blend_unreachable_nodes
            .remove(node_name);
        if world.blend_diagnostics.phase == Some(BlendDiagnosticPhase::Outage) {
            world.blend_diagnostics.phase = Some(BlendDiagnosticPhase::Recovery);
        }
    } else {
        if world.blend_diagnostics.blend_unreachable_nodes.is_empty() {
            world.blend_diagnostics.phase = Some(BlendDiagnosticPhase::Outage);
        }
        world
            .blend_diagnostics
            .blend_unreachable_nodes
            .insert(node_name.to_owned());
    }

    let event = if reachable {
        "blend_relay_enabled"
    } else {
        "blend_relay_disabled"
    };
    log_blend_relay_event(
        world,
        event,
        node_name,
        metadata.declared_addr,
        metadata.backend_addr,
        if reachable { "recovery" } else { "outage" },
    );
    Ok(())
}

fn is_declared_blend_provider(config: &RunConfig) -> Result<bool, io::Error> {
    let provider_id = config.user.blend_provider_id().map_err(io::Error::other)?;
    Ok(config
        .deployment
        .cryptarchia
        .genesis_block
        .genesis_tx()
        .sdp_declarations()
        .any(|declaration| {
            let declaration = declaration.operation();
            declaration.service_type == ServiceType::BlendNetwork
                && declaration.provider_id == provider_id
        }))
}

fn socket_addr_from_multiaddr(address: &Multiaddr) -> Result<SocketAddr, io::Error> {
    let mut ip = None;
    let mut port = None;
    for protocol in address {
        match protocol {
            Protocol::Ip4(value) => ip = Some(IpAddr::V4(value)),
            Protocol::Ip6(value) => ip = Some(IpAddr::V6(value)),
            Protocol::Udp(value) => port = Some(value),
            _ => {}
        }
    }

    let ip = ip.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Blend address `{address}` has no IP component"),
        )
    })?;
    let port = port.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Blend address `{address}` has no UDP port"),
        )
    })?;
    Ok(SocketAddr::new(ip, port))
}

struct BlendRelay {
    control: RelayControl,
    task: JoinHandle<()>,
}

impl BlendRelay {
    fn bind(declared_addr: SocketAddr, backend_addr: SocketAddr) -> Result<Self, io::Error> {
        let declared_socket = std::net::UdpSocket::bind(declared_addr)?;
        declared_socket.set_nonblocking(true)?;
        let declared_socket = Arc::new(UdpSocket::from_std(declared_socket)?);
        let (commands, command_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(
            RelayTask {
                declared_socket,
                backend_addr,
                command_rx,
                sessions: HashMap::new(),
                enabled: true,
            }
            .run(),
        );

        Ok(Self {
            control: RelayControl { commands },
            task,
        })
    }
}

impl Drop for BlendRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
struct RelayControl {
    commands: mpsc::UnboundedSender<RelayCommand>,
}

impl RelayControl {
    async fn set_enabled(&self, enabled: bool) -> Result<(), io::Error> {
        let (ack, response) = oneshot::channel();
        self.commands
            .send(RelayCommand::SetEnabled { enabled, ack })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "Blend relay task stopped"))?;
        response
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "Blend relay task stopped"))
    }
}

enum RelayCommand {
    SetEnabled {
        enabled: bool,
        ack: oneshot::Sender<()>,
    },
}

struct RelayTask {
    declared_socket: Arc<UdpSocket>,
    backend_addr: SocketAddr,
    command_rx: mpsc::UnboundedReceiver<RelayCommand>,
    sessions: HashMap<SocketAddr, BackendSession>,
    enabled: bool,
}

struct BackendSession {
    socket: Arc<UdpSocket>,
    task: JoinHandle<()>,
}

impl RelayTask {
    async fn run(mut self) {
        let mut buffer = vec![0u8; UDP_BUFFER_SIZE];
        loop {
            tokio::select! {
                command = self.command_rx.recv() => match command {
                    Some(RelayCommand::SetEnabled { enabled, ack }) => {
                        self.enabled = enabled;
                        if !enabled {
                            self.clear_sessions();
                        }
                        let _ = ack.send(());
                    }
                    None => return,
                },
                received = self.declared_socket.recv_from(&mut buffer) => match received {
                    Ok((size, client_addr)) if self.enabled => {
                        if let Err(error) = self.forward_to_backend(client_addr, &buffer[..size]).await {
                            warn!(%error, ?client_addr, "Blend relay failed to forward datagram");
                            self.remove_session(client_addr);
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        warn!(%error, "Blend relay failed to receive datagram");
                    }
                }
            }
        }
    }

    async fn forward_to_backend(
        &mut self,
        client_addr: SocketAddr,
        payload: &[u8],
    ) -> Result<(), io::Error> {
        let socket = if let Some(session) = self.sessions.get(&client_addr) {
            Arc::clone(&session.socket)
        } else {
            self.create_session(client_addr).await?
        };
        socket.send(payload).await.map(|_| ())
    }

    async fn create_session(
        &mut self,
        client_addr: SocketAddr,
    ) -> Result<Arc<UdpSocket>, io::Error> {
        let socket = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?);
        socket.connect(self.backend_addr).await?;

        let backend_socket = Arc::clone(&socket);
        let declared_socket = Arc::clone(&self.declared_socket);
        let task = tokio::spawn(async move {
            let mut buffer = vec![0u8; UDP_BUFFER_SIZE];
            while let Ok(size) = backend_socket.recv(&mut buffer).await {
                if declared_socket
                    .send_to(&buffer[..size], client_addr)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        self.sessions.insert(
            client_addr,
            BackendSession {
                socket: Arc::clone(&socket),
                task,
            },
        );
        Ok(socket)
    }

    fn remove_session(&mut self, client_addr: SocketAddr) {
        if let Some(session) = self.sessions.remove(&client_addr) {
            session.task.abort();
        }
    }

    fn clear_sessions(&mut self) {
        for (_, session) in self.sessions.drain() {
            session.task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use lb_config::{
        create_general_configs, deployment::e2e_deployment_settings_with_genesis_block,
        node::create_node_user_config,
    };
    use lb_core::mantle::GenesisTime;
    use time::OffsetDateTime;
    use tokio::time::{Duration, timeout};

    use super::*;

    fn test_run_configs(test_context: &str) -> Vec<RunConfig> {
        let genesis_time = GenesisTime::try_from(OffsetDateTime::now_utc())
            .expect("current time should fit in GenesisTime");
        let (configs, genesis_block) = create_general_configs(2, Some(test_context), genesis_time);
        let deployment = e2e_deployment_settings_with_genesis_block(&genesis_block);
        configs
            .into_iter()
            .map(|config| RunConfig {
                deployment: deployment.clone(),
                user: create_node_user_config(config),
            })
            .collect()
    }

    #[tokio::test]
    async fn enabled_relay_forwards_client_and_backend_datagrams() {
        let backend = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("backend should bind");
        let relay_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let relay_addr = std::net::UdpSocket::bind(relay_addr)
            .expect("relay port should bind")
            .local_addr()
            .expect("relay address should be available");
        let relay =
            BlendRelay::bind(relay_addr, backend.local_addr().unwrap()).expect("relay should bind");
        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("client should bind");

        client
            .send_to(b"client-to-backend", relay_addr)
            .await
            .expect("client datagram should send");
        let (size, backend_client_addr) =
            timeout(Duration::from_secs(1), backend.recv_from(&mut [0; 64]))
                .await
                .expect("backend should receive before timeout")
                .expect("backend receive should succeed");
        assert_eq!(size, "client-to-backend".len());

        backend
            .send_to(b"backend-to-client", backend_client_addr)
            .await
            .expect("backend reply should send");
        let mut response = [0; 64];
        let (size, source) = timeout(Duration::from_secs(1), client.recv_from(&mut response))
            .await
            .expect("client should receive before timeout")
            .expect("client receive should succeed");
        assert_eq!(&response[..size], b"backend-to-client");
        assert_eq!(source, relay_addr);

        drop(relay);
    }

    #[tokio::test]
    async fn disabled_relay_blackholes_and_restore_creates_a_fresh_session() {
        let backend = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("backend should bind");
        let relay_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let relay_addr = std::net::UdpSocket::bind(relay_addr)
            .expect("relay port should bind")
            .local_addr()
            .expect("relay address should be available");
        let relay =
            BlendRelay::bind(relay_addr, backend.local_addr().unwrap()).expect("relay should bind");
        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("client should bind");

        client
            .send_to(b"before-disable", relay_addr)
            .await
            .expect("initial datagram should send");
        let mut request = [0; 64];
        let (_, backend_client_addr) =
            timeout(Duration::from_secs(1), backend.recv_from(&mut request))
                .await
                .expect("backend should receive before timeout")
                .expect("backend receive should succeed");

        relay
            .control
            .set_enabled(false)
            .await
            .expect("disable should be acknowledged");
        client
            .send_to(b"while-disabled", relay_addr)
            .await
            .expect("disabled datagram should send to relay");
        assert!(
            timeout(Duration::from_millis(200), backend.recv_from(&mut request))
                .await
                .is_err()
        );

        backend
            .send_to(b"stale-reply", backend_client_addr)
            .await
            .expect("stale backend reply should send");
        let mut response = [0; 64];
        assert!(
            timeout(Duration::from_millis(200), client.recv_from(&mut response))
                .await
                .is_err()
        );

        relay
            .control
            .set_enabled(true)
            .await
            .expect("restore should be acknowledged");
        client
            .send_to(b"after-restore", relay_addr)
            .await
            .expect("restored datagram should send");
        let (size, restored_backend_client_addr) =
            timeout(Duration::from_secs(1), backend.recv_from(&mut request))
                .await
                .expect("backend should receive after restore")
                .expect("backend receive should succeed");
        assert_eq!(&request[..size], b"after-restore");
        assert_ne!(backend_client_addr, restored_backend_client_addr);

        drop(relay);
    }

    #[tokio::test]
    async fn removing_provider_releases_only_that_relay() {
        let mut configs = test_run_configs("blend-relay-remove-provider");
        let mut first_config = configs.remove(0);
        let mut second_config = configs.remove(0);
        let first_declared_address = first_config
            .user
            .blend
            .core
            .backend
            .listening_address
            .clone();
        let second_declared_address = second_config
            .user
            .blend
            .core
            .backend
            .listening_address
            .clone();
        let registry = BlendRelayRegistry::default();
        registry.enable().expect("relay registry should enable");

        registry
            .configure_provider("NODE_1", &mut first_config, &first_declared_address)
            .expect("first provider relay should configure");
        registry
            .configure_provider("NODE_2", &mut second_config, &second_declared_address)
            .expect("second provider relay should configure");
        let first_metadata = registry
            .metadata("NODE_1")
            .expect("first metadata query should succeed")
            .expect("first relay metadata should exist");

        assert!(
            !registry
                .remove_provider("UNKNOWN")
                .expect("unknown provider removal should succeed")
        );
        assert!(
            registry
                .metadata("NODE_2")
                .expect("second metadata query should succeed")
                .is_some()
        );
        assert!(
            registry
                .remove_provider("NODE_1")
                .expect("first provider removal should succeed")
        );
        assert!(
            registry
                .metadata("NODE_1")
                .expect("removed metadata query should succeed")
                .is_none()
        );
        assert!(
            registry
                .metadata("NODE_2")
                .expect("second metadata query should succeed")
                .is_some()
        );

        let address_reusable = timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(socket) = std::net::UdpSocket::bind(first_metadata.declared_addr) {
                    drop(socket);
                    break true;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("removed relay socket should become reusable");
        assert!(address_reusable);

        registry.shutdown();
    }

    #[tokio::test]
    async fn reconfiguring_provider_reuses_existing_relay() {
        let mut configs = test_run_configs("blend-relay-reconfigure-provider");
        let mut initial_config = configs.remove(0);
        let mut restarted_config = initial_config.clone();
        let declared_address = initial_config
            .user
            .blend
            .core
            .backend
            .listening_address
            .clone();
        let registry = BlendRelayRegistry::default();
        registry.enable().expect("relay registry should enable");

        registry
            .configure_provider("NODE_1", &mut initial_config, &declared_address)
            .expect("initial provider relay should configure");
        let initial_metadata = registry
            .metadata("NODE_1")
            .expect("initial metadata query should succeed")
            .expect("initial relay metadata should exist");

        registry
            .configure_provider("NODE_1", &mut restarted_config, &declared_address)
            .expect("restarted provider relay should reuse existing configuration");
        let restarted_metadata = registry
            .metadata("NODE_1")
            .expect("restarted metadata query should succeed")
            .expect("restarted relay metadata should exist");
        assert_eq!(
            restarted_metadata.declared_addr,
            initial_metadata.declared_addr
        );
        assert_eq!(
            restarted_metadata.backend_addr,
            initial_metadata.backend_addr
        );
        assert_eq!(
            restarted_config.user.blend.core.backend.listening_address,
            lb_libp2p::multiaddr(Ipv4Addr::LOCALHOST, initial_metadata.backend_addr.port())
        );

        registry.shutdown();
    }
}
