#![allow(
    clippy::multiple_inherent_impl,
    reason = "We spilt the impl in different blocks on purpose to ease localizing changes."
)]

// This macro must be on top if it is accessed by child modules, else if the
// modules are defined before it, they will fail to see it.
macro_rules! log_error {
    ($e:expr) => {
        if let Err(e) = $e {
            tracing::error!(
                target: LOG_TARGET,
                "error while processing {}: {e:?}",
                stringify!($e)
            );
        }
    };
}

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use lb_libp2p::{
    Multiaddr, PeerId, Protocol, Swarm, SwarmEvent,
    behaviour::BehaviourEvent,
    libp2p::{
        kad::QueryId,
        swarm::{ConnectionId, DialError},
    },
};
use lb_log_targets::network_service;
use lb_utils::tokio::task::spawn;
use rand::RngCore;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_stream::StreamExt as _;

use super::{
    Libp2pConfig, Message,
    command::{Command, Dial, DialPurpose, NetworkCommand},
};
use crate::backends::libp2p::{Libp2pInfo, swarm::kademlia::PendingQueryData};

mod chainsync;
mod gossipsub;
mod identify;
mod kademlia;

pub use chainsync::ChainSyncCommand;
pub use gossipsub::PubSubCommand;
pub use kademlia::DiscoveryCommand;

use crate::message::ChainSyncEvent;

const LOG_TARGET: &str = network_service::backends::libp2p::ROOT;

const MAX_CONCURRENT_IDENTITY_PROBES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PeerProtocolState {
    Supported,
    Unsupported,
}

#[derive(Debug)]
pub(super) struct ProtocolContract {
    pub(super) identify_protocol_version: String,
    pub(super) kademlia_protocol: String,
    pub(super) chain_sync_protocol: String,
}

impl ProtocolContract {
    fn from_config(config: &lb_libp2p::SwarmConfig) -> Self {
        Self {
            identify_protocol_version: config.identify_protocol_name.to_string(),
            kademlia_protocol: config.kad_protocol_name.to_string(),
            chain_sync_protocol: config.chain_sync_protocol_name.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct IdentityProbe {
    peer_id: PeerId,
    addresses: Vec<Multiaddr>,
}

pub struct SwarmHandler<R: Clone + Send + RngCore + 'static> {
    pub swarm: Swarm<R>,
    pub pending_dials: HashMap<ConnectionId, Dial>,
    pub commands_tx: mpsc::Sender<Command>,
    pub commands_rx: mpsc::Receiver<Command>,
    pub pubsub_messages_tx: broadcast::Sender<Message>,
    pub chainsync_events_tx: broadcast::Sender<ChainSyncEvent>,

    pending_queries: HashMap<QueryId, PendingQueryData>,
    protocol_contract: ProtocolContract,
    allow_non_public_identify_addresses: bool,
    peer_protocol_states: HashMap<PeerId, PeerProtocolState>,
    identity_probe_queue: HashMap<PeerId, Vec<Multiaddr>>,
    identity_probe_peers: HashSet<PeerId>,
    identity_probe_connections: HashMap<ConnectionId, IdentityProbe>,
}

// TODO: make this configurable
const BACKOFF: u64 = 5;
// TODO: make this configurable
const MAX_RETRY: usize = 3;

impl<R: Clone + Send + RngCore + 'static> SwarmHandler<R> {
    pub fn new(
        config: Libp2pConfig,
        commands_tx: mpsc::Sender<Command>,
        commands_rx: mpsc::Receiver<Command>,
        pubsub_events_tx: broadcast::Sender<Message>,
        chainsync_events_tx: broadcast::Sender<ChainSyncEvent>,
        rng: R,
    ) -> Self {
        let protocol_contract = ProtocolContract::from_config(&config.inner);
        let allow_non_public_identify_addresses = config
            .inner
            .identify_config
            .allow_non_public_identify_addresses;
        let swarm = Swarm::build(config.inner, rng).unwrap();

        // Keep the dialing history since swarm.connect doesn't return the result
        // synchronously
        let pending_dials = HashMap::<ConnectionId, Dial>::new();

        Self {
            swarm,
            pending_dials,
            commands_tx,
            commands_rx,
            pubsub_messages_tx: pubsub_events_tx,
            chainsync_events_tx,
            pending_queries: HashMap::new(),
            protocol_contract,
            allow_non_public_identify_addresses,
            peer_protocol_states: HashMap::new(),
            identity_probe_queue: HashMap::new(),
            identity_probe_peers: HashSet::new(),
            identity_probe_connections: HashMap::new(),
        }
    }

    pub async fn run(&mut self, initial_peers: Vec<Multiaddr>) {
        self.bootstrap_kad_from_peers(&initial_peers);

        for initial_peer in &initial_peers {
            if let Some(peer_id) = peer_id_from_address(initial_peer) {
                // The existing startup dial will also trigger Identify. Mark it
                // as pending so RoutingUpdated cannot schedule a second dial.
                self.identity_probe_peers.insert(peer_id);
            }
            let (tx, _) = oneshot::channel();
            let dial = Dial {
                addr: initial_peer.clone(),
                retry_count: 0,
                result_sender: tx,
                purpose: DialPurpose::IdentityProbe,
            };
            Self::schedule_connect(dial, self.commands_tx.clone()).await;
        }

        loop {
            tokio::select! {
                Some(event) = self.swarm.next() => {
                    self.handle_event(event);
                }
                Some(command) = self.commands_rx.recv() => {
                    self.handle_command(command);
                }
            }
        }
    }

    fn handle_event(&mut self, event: SwarmEvent<BehaviourEvent<R>>) {
        match event {
            SwarmEvent::Behaviour(behaviour_event) => {
                self.handle_behaviour_event(behaviour_event);
            }
            _ => {
                self.handle_swarm_event(event);
            }
        }
    }

    fn handle_behaviour_event(&mut self, behaviour_event: BehaviourEvent<R>) {
        match behaviour_event {
            BehaviourEvent::Gossipsub(event) => {
                self.handle_gossipsub_event(event);
            }
            BehaviourEvent::Identify(event) => {
                self.handle_identify_event(event);
            }
            BehaviourEvent::Kademlia(event) => {
                self.handle_kademlia_event(event);
            }
            BehaviourEvent::ChainSync(event) => {
                self.handle_chainsync_event(event);
            }
            BehaviourEvent::AutonatServer(_) | BehaviourEvent::Nat(_) => {}
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: Address this at some point."
    )]
    fn handle_swarm_event(&mut self, event: SwarmEvent<BehaviourEvent<R>>) {
        match event {
            SwarmEvent::ConnectionEstablished {
                peer_id,
                connection_id,
                endpoint,
                ..
            } => {
                tracing::trace!(
                    target: LOG_TARGET,
                    "connected to peer:{peer_id}, connection_id:{connection_id:?}"
                );
                if endpoint.is_dialer() {
                    self.complete_connect(connection_id, peer_id);
                }

                let swarm = self.swarm.swarm();
                crate::metrics::consensus_report_connectivity(swarm);
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                connection_id,
                num_established,
                cause,
                ..
            } => {
                tracing::trace!(
                    target: LOG_TARGET,
                    "connection closed from peer: {peer_id} {connection_id:?} due to {cause:?}"
                );

                if let Some(probe) = self.identity_probe_connections.remove(&connection_id) {
                    self.clear_identity_probe(probe.peer_id);
                }
                if num_established == 0 {
                    self.prune_peer_protocol_state(peer_id);
                }

                let swarm = self.swarm.swarm();
                crate::metrics::consensus_report_connectivity(swarm);
            }
            SwarmEvent::OutgoingConnectionError {
                peer_id,
                connection_id,
                error,
                ..
            } => {
                crate::metrics::network_dial_failures();

                let identity_probe = self.identity_probe_connections.remove(&connection_id);
                let identity_probe_peer = identity_probe.as_ref().map(|probe| probe.peer_id);

                match error {
                    // A `WrongPeerId` failure is permanent for that exact
                    // `/p2p/<id>@addr`: the node at that address rotated its
                    // identity key, so retrying can never succeed. Evict the
                    // stale address from Kademlia immediately instead of retrying.
                    DialError::WrongPeerId { obtained, address } => {
                        let dial_addr = &address;
                        tracing::debug!(
                            target: LOG_TARGET,
                            "Evicting stale address after WrongPeerId (expected {peer_id:?}, obtained {obtained}): {dial_addr}"
                        );
                        self.remove_kademlia_address_for_dial(
                            peer_id.or(identity_probe_peer),
                            dial_addr,
                        );
                        // Drop any matching pending dial so it is not also retried.
                        self.pending_dials.remove(&connection_id);
                        if let Some(peer_id) = identity_probe_peer
                            && identity_probe.is_some()
                        {
                            self.clear_identity_probe(peer_id);
                            self.prune_peer_protocol_state(peer_id);
                        }
                    }
                    error => {
                        tracing::debug!(
                            target: LOG_TARGET,
                            "Failed to connect to peer: {peer_id:?} {connection_id:?} due to: {error}"
                        );
                        let retry_scheduled = self.retry_connect(connection_id, peer_id);
                        if !retry_scheduled
                            && identity_probe.is_some()
                            && let Some(peer_id) = identity_probe_peer
                        {
                            self.clear_identity_probe(peer_id);
                            self.prune_peer_protocol_state(peer_id);
                        }
                    }
                }
            }
            SwarmEvent::ExternalAddrConfirmed { address } => {
                self.handle_external_addr_confirmed(&address);
            }
            _ => {}
        }
    }

    fn handle_external_addr_confirmed(&mut self, address: &Multiaddr) {
        let local_peer_id = *self.swarm.swarm().local_peer_id();
        self.swarm.kademlia_add_address(local_peer_id, address);
        tracing::debug!(target: LOG_TARGET, %address, "added confirmed external address to Kademlia");
    }

    fn remove_kademlia_address_for_dial(&mut self, peer_id: Option<PeerId>, dial_addr: &Multiaddr) {
        let address_peer_id = dial_addr.iter().find_map(|protocol| match protocol {
            Protocol::P2p(multihash) => PeerId::from_multihash(multihash.into()).ok(),
            _ => None,
        });

        let resolved_peer_id = peer_id.or(address_peer_id);
        let Some(peer_id) = resolved_peer_id else {
            tracing::trace!(
                target: LOG_TARGET,
                "Skipping Kademlia removal for failed dial; peer id unavailable: {}",
                dial_addr
            );
            return;
        };

        self.swarm.kademlia_remove_address(peer_id, dial_addr);
        self.prune_peer_protocol_state(peer_id);
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::Network(network_cmd) => self.handle_network_command(network_cmd),
            Command::PubSub(pubsub_cmd) => self.handle_pubsub_command(pubsub_cmd),
            Command::Discovery(discovery_cmd) => self.handle_discovery_command(discovery_cmd),
            Command::ChainSync(chainsync_cmd) => self.handle_chainsync_command(chainsync_cmd),
        }
    }

    fn handle_network_command(&mut self, command: NetworkCommand) {
        match command {
            NetworkCommand::Connect(dial) => {
                self.connect(dial);
            }
            NetworkCommand::Info { reply } => {
                let discovered_peers: Vec<PeerId> = self
                    .swarm
                    .kademlia_discovered_peers()
                    .into_iter()
                    .map(|peer_info| peer_info.peer_id)
                    .collect();
                let n_discovered_peers = discovered_peers.len();
                let swarm = self.swarm.swarm();
                let network_info = swarm.network_info();
                let counters = network_info.connection_counters();
                let info = Libp2pInfo {
                    listen_addresses: swarm.listeners().cloned().collect(),
                    peer_id: *swarm.local_peer_id(),
                    connected_peers: swarm.connected_peers().copied().collect(),
                    n_peers: network_info.num_peers(),
                    n_connections: counters.num_connections(),
                    n_pending_connections: counters.num_pending(),
                    discovered_peers,
                    n_discovered_peers,
                };
                log_error!(reply.send(info));
            }
            NetworkCommand::ConnectedPeers { reply } => {
                let connected_peers = self.swarm.swarm().connected_peers().copied().collect();
                log_error!(reply.send(connected_peers));
            }
        }
    }

    pub(super) fn handle_routing_updated(
        &mut self,
        peer_id: PeerId,
        addresses: impl IntoIterator<Item = Multiaddr>,
    ) {
        // An unsupported result is not a permanent blacklist. A fresh
        // routing update is an opportunity to revalidate the peer, which
        // permits an upgraded node with the same PeerId to become supported
        // again. Do not remove the peer from Kademlia: Kademlia membership
        // and chainsync eligibility are independent capabilities.
        match self.peer_protocol_states.get(&peer_id) {
            Some(PeerProtocolState::Supported) => return,
            Some(PeerProtocolState::Unsupported) | None => {}
        }

        if self
            .swarm
            .swarm()
            .connected_peers()
            .any(|connected_peer| *connected_peer == peer_id)
            || self.identity_probe_peers.contains(&peer_id)
        {
            return;
        }

        let addresses = addresses.into_iter().collect::<Vec<_>>();
        if addresses.is_empty() {
            self.prune_peer_protocol_state(peer_id);
            return;
        }

        let queued_addresses = self.identity_probe_queue.entry(peer_id).or_default();
        for address in addresses {
            if !queued_addresses.contains(&address) {
                queued_addresses.push(address);
            }
        }
        self.drain_identity_probe_queue();
    }

    pub(super) fn chainsync_eligible_peers(&self) -> HashSet<PeerId> {
        self.peer_protocol_states
            .iter()
            .filter_map(|(peer_id, state)| {
                (*state == PeerProtocolState::Supported).then_some(*peer_id)
            })
            .collect()
    }

    fn drain_identity_probe_queue(&mut self) {
        while self.identity_probe_peers.len() < MAX_CONCURRENT_IDENTITY_PROBES {
            let Some((peer_id, addresses)) = self
                .identity_probe_queue
                .iter()
                .next()
                .map(|(peer_id, addresses)| (*peer_id, addresses.clone()))
            else {
                break;
            };
            self.identity_probe_queue.remove(&peer_id);

            if self.peer_protocol_states.get(&peer_id) == Some(&PeerProtocolState::Supported)
                || self
                    .swarm
                    .swarm()
                    .connected_peers()
                    .any(|connected_peer| *connected_peer == peer_id)
            {
                continue;
            }

            self.identity_probe_peers.insert(peer_id);
            let probe = IdentityProbe { peer_id, addresses };
            match self.swarm.connect_peer(peer_id, probe.addresses.clone()) {
                Ok(connection_id) => {
                    self.identity_probe_connections.insert(connection_id, probe);
                }
                Err(error) => {
                    self.identity_probe_peers.remove(&peer_id);
                    tracing::debug!(
                        target: LOG_TARGET,
                        "Failed to schedule identity probe for peer {peer_id}: {error}"
                    );
                    self.prune_peer_protocol_state(peer_id);
                }
            }
        }
    }

    fn clear_identity_probe(&mut self, peer_id: PeerId) -> Vec<Multiaddr> {
        self.identity_probe_peers.remove(&peer_id);
        let mut addresses = self
            .identity_probe_queue
            .remove(&peer_id)
            .unwrap_or_default();
        for probe in self.identity_probe_connections.values() {
            if probe.peer_id == peer_id {
                for address in &probe.addresses {
                    if !addresses.contains(address) {
                        addresses.push(address.clone());
                    }
                }
            }
        }
        self.identity_probe_connections
            .retain(|_, probe| probe.peer_id != peer_id);
        self.drain_identity_probe_queue();
        addresses
    }

    fn prune_peer_protocol_state(&mut self, peer_id: PeerId) {
        if !self.peer_protocol_states.contains_key(&peer_id) {
            return;
        }

        let is_connected = self
            .swarm
            .swarm()
            .connected_peers()
            .any(|connected_peer| *connected_peer == peer_id);
        let is_in_kademlia = self
            .swarm
            .kademlia_discovered_peers()
            .iter()
            .any(|peer| peer.peer_id == peer_id);
        let is_being_probed = self.identity_probe_peers.contains(&peer_id)
            || self.identity_probe_queue.contains_key(&peer_id);

        if !is_connected && !is_in_kademlia && !is_being_probed {
            self.peer_protocol_states.remove(&peer_id);
        }
    }

    fn handle_kademlia_peer_evicted(&mut self, peer_id: PeerId) {
        self.identity_probe_queue.remove(&peer_id);
        self.prune_peer_protocol_state(peer_id);
    }

    async fn schedule_connect(dial: Dial, commands_tx: mpsc::Sender<Command>) {
        commands_tx
            .send(Command::Network(NetworkCommand::Connect(dial)))
            .await
            .unwrap_or_else(|_| tracing::error!(target: LOG_TARGET, "could not schedule connect"));
    }

    fn connect(&mut self, dial: Dial) {
        tracing::debug!(target: LOG_TARGET, "Connecting to {}", dial.addr);

        let peer_id = peer_id_from_address(&dial.addr);
        let is_identity_probe = dial.purpose == DialPurpose::IdentityProbe;
        let dial_addr = dial.addr.clone();

        match self.swarm.connect(&dial.addr) {
            Ok(connection_id) => {
                // Dialing has been scheduled. The result will be notified as a SwarmEvent.
                self.pending_dials.insert(connection_id, dial);
                if is_identity_probe && let Some(peer_id) = peer_id {
                    self.identity_probe_connections.insert(
                        connection_id,
                        IdentityProbe {
                            peer_id,
                            addresses: vec![dial_addr],
                        },
                    );
                }
            }
            Err(e) => {
                if let Err(err) = dial.result_sender.send(Err(e)) {
                    tracing::warn!(
                        target: LOG_TARGET,
                        "failed to send the Err result of dialing: {err:?}"
                    );
                }
                if is_identity_probe && let Some(peer_id) = peer_id {
                    self.clear_identity_probe(peer_id);
                    self.prune_peer_protocol_state(peer_id);
                }
            }
        }
    }

    fn complete_connect(&mut self, connection_id: ConnectionId, peer_id: PeerId) {
        if let Some(dial) = self.pending_dials.remove(&connection_id)
            && let Err(e) = dial.result_sender.send(Ok(peer_id))
        {
            tracing::warn!(
                target: LOG_TARGET,
                "failed to send the Ok result of dialing: {e:?}"
            );
        }
    }

    // TODO: Consider a common retry module for all use cases
    fn retry_connect(&mut self, connection_id: ConnectionId, peer_id: Option<PeerId>) -> bool {
        let Some(mut dial) = self.pending_dials.remove(&connection_id) else {
            return false;
        };
        let Some(new_retry_count) = dial.retry_count.checked_add(1) else {
            tracing::debug!(target: LOG_TARGET, "Retry count overflow.");
            return false;
        };
        if new_retry_count > MAX_RETRY {
            tracing::debug!(
                target: LOG_TARGET,
                "Max retry({MAX_RETRY}) has been reached: {dial:?}"
            );
            self.remove_kademlia_address_for_dial(peer_id, &dial.addr);
            return false;
        }
        dial.retry_count = new_retry_count;

        let wait = exp_backoff(dial.retry_count);
        tracing::debug!(target: LOG_TARGET, "Retry dialing in {wait:?}: {dial:?}");

        let commands_tx = self.commands_tx.clone();
        spawn("logos/network/dial-retry", async move {
            tokio::time::sleep(wait).await;
            Self::schedule_connect(dial, commands_tx).await;
        });
        true
    }
}

const fn exp_backoff(retry: usize) -> Duration {
    Duration::from_secs(BACKOFF.pow(retry as u32))
}

fn peer_id_from_address(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|protocol| match protocol {
        Protocol::P2p(multihash) => PeerId::from_multihash(multihash.into()).ok(),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv4Addr, sync::Once, time::Instant};

    use lb_libp2p::protocol_name::StreamProtocol;
    use lb_utils::net::get_available_udp_port;
    use rand::rngs::OsRng;
    use tracing_subscriber::EnvFilter;

    use super::*;

    static INIT: Once = Once::new();

    fn init_tracing() {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

        INIT.call_once(|| {
            tracing_subscriber::fmt().with_env_filter(filter).init();
        });
    }

    fn create_swarm_config(port: u16, is_boot: bool) -> lb_libp2p::SwarmConfig {
        lb_libp2p::SwarmConfig {
            host: Ipv4Addr::LOCALHOST,
            port,
            node_key: lb_libp2p::ed25519::SecretKey::generate(),
            gossipsub_config: lb_libp2p::gossipsub::Config::default(),
            // Use a tighter bootstrap interval for the first node if requested,
            // otherwise fall back to defaults.
            kademlia_config: if is_boot {
                lb_libp2p::KademliaSettings {
                    periodic_bootstrap_interval_secs: Some(1),
                    ..Default::default()
                }
            } else {
                lb_libp2p::KademliaSettings::default()
            },
            kad_protocol_name: StreamProtocol::new("/kademlia/test"),
            identify_protocol_name: StreamProtocol::new("/identify/test"),
            chain_sync_protocol_name: StreamProtocol::new("/chainsync/test"),
            identify_config: lb_libp2p::IdentifySettings::default(),
            chain_sync_config: lb_cryptarchia_sync::Config {
                peer_response_timeout: Duration::from_secs(5),
                max_inbound_requests: 10.try_into().unwrap(),
            },
            nat_config: lb_libp2p::NatSettings::Traversal(lb_libp2p::TraversalSettings {
                autonat: lb_libp2p::AutonatClientSettings {
                    probe_interval_millisecs: Some(1000),
                    ..Default::default()
                },
                ..Default::default()
            }),
        }
    }

    fn create_libp2p_config(initial_peers: Vec<Multiaddr>, port: u16) -> Libp2pConfig {
        Libp2pConfig {
            inner: create_swarm_config(port, !initial_peers.is_empty()),
            initial_peers,
        }
    }

    fn create_handler() -> SwarmHandler<OsRng> {
        let (tx, rx) = mpsc::channel(10);
        let (pubsub_events_tx, _) = broadcast::channel(10);
        let (chainsync_events_tx, _) = broadcast::channel(10);
        let config = create_libp2p_config(vec![], get_available_udp_port().unwrap());

        SwarmHandler::new(config, tx, rx, pubsub_events_tx, chainsync_events_tx, OsRng)
    }

    fn create_probe_address() -> Multiaddr {
        format!(
            "/ip4/127.0.0.1/udp/{}/quic-v1",
            get_available_udp_port().unwrap()
        )
        .parse()
        .unwrap()
    }

    fn identify_event(
        peer_id: PeerId,
        advertised_protocols: &[&'static str],
    ) -> lb_libp2p::libp2p::identify::Event {
        let keypair = lb_libp2p::libp2p::identity::Keypair::generate_ed25519();
        let address = create_probe_address();
        let protocols = advertised_protocols
            .iter()
            .map(|protocol| lb_libp2p::libp2p::StreamProtocol::new(protocol))
            .collect();

        lb_libp2p::libp2p::identify::Event::Received {
            connection_id: ConnectionId::new_unchecked(1),
            peer_id,
            info: lb_libp2p::libp2p::identify::Info {
                public_key: keypair.public(),
                protocol_version: "/identify/test".into(),
                agent_version: "test".into(),
                listen_addrs: vec![address],
                protocols,
                observed_addr: "/ip4/127.0.0.1/udp/1".parse().unwrap(),
                signed_peer_record: None,
            },
        }
    }

    const NODE_COUNT: usize = 10;

    #[tokio::test]
    async fn repeated_routing_updates_schedule_one_probe_per_peer() {
        let mut handler = create_handler();
        let peer_id = PeerId::random();
        let address = create_probe_address();

        handler.handle_routing_updated(peer_id, [address.clone()]);
        handler.handle_routing_updated(peer_id, [address]);

        assert_eq!(handler.identity_probe_peers.len(), 1);
        assert_eq!(handler.identity_probe_connections.len(), 1);
        assert!(handler.identity_probe_queue.is_empty());
    }

    #[tokio::test]
    async fn unrelated_dial_error_does_not_clear_identity_probe() {
        let mut handler = create_handler();
        let peer_id = PeerId::random();
        handler.handle_routing_updated(peer_id, [create_probe_address()]);
        let probe_connection = *handler
            .identity_probe_connections
            .keys()
            .next()
            .expect("expected an active identity probe");

        handler.handle_swarm_event(SwarmEvent::OutgoingConnectionError {
            peer_id: Some(peer_id),
            connection_id: ConnectionId::new_unchecked(999),
            error: DialError::NoAddresses,
        });

        assert!(handler.identity_probe_peers.contains(&peer_id));
        assert!(
            handler
                .identity_probe_connections
                .contains_key(&probe_connection)
        );
    }

    #[tokio::test]
    async fn identity_probe_concurrency_is_bounded() {
        let mut handler = create_handler();

        for _ in 0..MAX_CONCURRENT_IDENTITY_PROBES {
            handler.handle_routing_updated(PeerId::random(), [create_probe_address()]);
        }

        let queued_peer = PeerId::random();
        let queued_addresses = vec![create_probe_address(), create_probe_address()];
        handler.handle_routing_updated(queued_peer, queued_addresses.clone());

        assert_eq!(
            handler.identity_probe_peers.len(),
            MAX_CONCURRENT_IDENTITY_PROBES
        );
        assert_eq!(handler.identity_probe_queue.len(), 1);
        assert_eq!(
            handler.identity_probe_queue.get(&queued_peer),
            Some(&queued_addresses)
        );

        handler.handle_kademlia_peer_evicted(queued_peer);

        assert!(!handler.identity_probe_queue.contains_key(&queued_peer));
    }

    #[tokio::test]
    async fn whole_peer_kademlia_eviction_removes_all_addresses() {
        let mut handler = create_handler();
        let peer_id = PeerId::random();
        let first_address = create_probe_address();
        let second_address = create_probe_address();

        handler.swarm.kademlia_add_address(peer_id, &first_address);
        handler.swarm.kademlia_add_address(peer_id, &second_address);
        assert_eq!(handler.swarm.kademlia_discovered_peers().len(), 1);
    }

    #[tokio::test]
    async fn only_supported_peers_are_chainsync_eligible() {
        let mut handler = create_handler();
        let supported = PeerId::random();
        let unsupported = PeerId::random();
        handler
            .peer_protocol_states
            .insert(supported, PeerProtocolState::Supported);
        handler
            .peer_protocol_states
            .insert(unsupported, PeerProtocolState::Unsupported);

        assert_eq!(
            handler.chainsync_eligible_peers(),
            HashSet::from([supported])
        );
    }

    #[tokio::test]
    async fn identify_updates_peer_state_without_kademlia_eviction() {
        let mut handler = create_handler();
        let peer_id = PeerId::random();
        let address = create_probe_address().with(Protocol::P2p(peer_id));
        handler.swarm.kademlia_add_address(peer_id, &address);

        handler.handle_identify_event(identify_event(
            peer_id,
            &["/kademlia/test", "/chainsync/test"],
        ));
        assert_eq!(
            handler.peer_protocol_states.get(&peer_id),
            Some(&PeerProtocolState::Supported)
        );
        assert!(handler.chainsync_eligible_peers().contains(&peer_id));

        handler.handle_identify_event(identify_event(peer_id, &["/kademlia/test"]));
        assert_eq!(
            handler.peer_protocol_states.get(&peer_id),
            Some(&PeerProtocolState::Unsupported)
        );
        assert!(!handler.chainsync_eligible_peers().contains(&peer_id));
        assert!(
            handler
                .swarm
                .kademlia_discovered_peers()
                .iter()
                .any(|peer| peer.peer_id == peer_id)
        );

        handler.handle_routing_updated(peer_id, [address.clone()]);
        assert!(handler.identity_probe_peers.contains(&peer_id));

        handler.handle_identify_event(identify_event(
            peer_id,
            &["/kademlia/test", "/chainsync/test"],
        ));
        assert_eq!(
            handler.peer_protocol_states.get(&peer_id),
            Some(&PeerProtocolState::Supported)
        );
        assert!(handler.chainsync_eligible_peers().contains(&peer_id));
        assert!(
            handler
                .swarm
                .kademlia_discovered_peers()
                .iter()
                .any(|peer| peer.peer_id == peer_id && peer.addrs.contains(&address))
        );
    }

    #[tokio::test]
    async fn chainsync_support_does_not_require_kademlia_advertisement() {
        let mut handler = create_handler();
        let peer_id = PeerId::random();

        handler.handle_identify_event(identify_event(peer_id, &["/chainsync/test"]));

        assert_eq!(
            handler.peer_protocol_states.get(&peer_id),
            Some(&PeerProtocolState::Supported)
        );
        assert!(handler.chainsync_eligible_peers().contains(&peer_id));
        assert!(handler.swarm.kademlia_discovered_peers().is_empty());
    }

    #[tokio::test]
    async fn protocol_state_is_pruned_only_when_peer_is_no_longer_known() {
        let mut handler = create_handler();
        let peer_id = PeerId::random();
        let address = create_probe_address();

        handler
            .peer_protocol_states
            .insert(peer_id, PeerProtocolState::Supported);
        handler.prune_peer_protocol_state(peer_id);
        assert!(!handler.peer_protocol_states.contains_key(&peer_id));

        handler.swarm.kademlia_add_address(peer_id, &address);
        handler
            .peer_protocol_states
            .insert(peer_id, PeerProtocolState::Supported);
        handler.prune_peer_protocol_state(peer_id);
        assert_eq!(
            handler.peer_protocol_states.get(&peer_id),
            Some(&PeerProtocolState::Supported)
        );
    }

    #[tokio::test]
    async fn removing_last_kademlia_address_prunes_disconnected_peer_state() {
        let mut handler = create_handler();
        let peer_id = PeerId::random();
        let address = create_probe_address().with(Protocol::P2p(peer_id));

        handler.swarm.kademlia_add_address(peer_id, &address);
        handler
            .peer_protocol_states
            .insert(peer_id, PeerProtocolState::Supported);

        handler.remove_kademlia_address_for_dial(Some(peer_id), &address);

        assert!(handler.swarm.kademlia_discovered_peers().is_empty());
        assert!(!handler.peer_protocol_states.contains_key(&peer_id));
    }

    #[tokio::test]
    #[expect(clippy::too_many_lines, reason = "Should be fixed in a separate PR")]
    async fn test_kademlia_bootstrap() {
        init_tracing();

        let mut handler_tasks = Vec::with_capacity(NODE_COUNT);
        let mut txs = Vec::new();

        // Create first node (bootstrap node)
        let (tx1, rx1) = mpsc::channel(10);
        txs.push(tx1.clone());

        let (pubsub_events_tx, _) = broadcast::channel(10);
        let (chainsync_events_tx, _) = broadcast::channel(10);

        let config = create_libp2p_config(vec![], get_available_udp_port().unwrap());
        let mut bootstrap_node = SwarmHandler::new(
            config,
            tx1.clone(),
            rx1,
            pubsub_events_tx,
            chainsync_events_tx,
            OsRng,
        );

        let bootstrap_node_peer_id = *bootstrap_node.swarm.swarm().local_peer_id();

        let task1 = tokio::spawn(async move {
            bootstrap_node.run(vec![]).await;
        });
        handler_tasks.push(task1);

        // Wait for bootstrap node to start
        tokio::time::sleep(Duration::from_secs(5)).await;

        let (reply, info_rx) = oneshot::channel();
        tx1.send(Command::Network(NetworkCommand::Info { reply }))
            .await
            .expect("Failed to send info command");
        let bootstrap_info = info_rx.await.expect("Failed to get bootstrap node info");

        assert!(
            !bootstrap_info.listen_addresses.is_empty(),
            "Bootstrap node has no listening addresses"
        );

        tracing::info!(
            target: LOG_TARGET,
            "Bootstrap node listening on: {:?}",
            bootstrap_info.listen_addresses
        );

        // Use the first listening address as the bootstrap address
        let bootstrap_addr = bootstrap_info.listen_addresses[0]
            .clone()
            .with(Protocol::P2p(bootstrap_node_peer_id));

        tracing::info!(target: LOG_TARGET, "Using bootstrap address: {}", bootstrap_addr);

        let bootstrap_addr = bootstrap_addr.clone();

        // Start additional nodes
        for i in 1..NODE_COUNT {
            let (tx, rx) = mpsc::channel(10);
            txs.push(tx.clone());

            // Each node connects to the bootstrap node
            let (pubsub_events_tx, _) = broadcast::channel(10);
            let (chainsync_events_tx, _) = broadcast::channel(10);

            let config = create_libp2p_config(
                vec![bootstrap_addr.clone()],
                get_available_udp_port().unwrap(),
            );
            let mut handler = SwarmHandler::new(
                config,
                tx.clone(),
                rx,
                pubsub_events_tx,
                chainsync_events_tx,
                OsRng,
            );

            let peer_id = *handler.swarm.swarm().local_peer_id();
            tracing::info!(target: LOG_TARGET, "Starting node {} with peer ID: {}", i, peer_id);

            let bootstrap_addr = bootstrap_addr.clone();
            let task = tokio::spawn(async move {
                handler.run(vec![bootstrap_addr.clone()]).await;
            });

            handler_tasks.push(task);

            // Add small delay between node startups to avoid overloading
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        let timeout = Duration::from_secs(30);
        let poll_interval = Duration::from_secs(1);
        let start_time = Instant::now();

        while !txs.is_empty() && start_time.elapsed() < timeout {
            tokio::time::sleep(poll_interval).await;
            let mut indices_to_remove = Vec::new();

            for (idx, tx) in txs.iter().enumerate() {
                let (reply, dump_rx) = oneshot::channel();
                tx.send(Command::Discovery(DiscoveryCommand::DumpRoutingTable {
                    reply,
                }))
                .await
                .expect("Failed to send dump command");

                let routing_table = dump_rx
                    .await
                    .expect("Failed to receive routing table dump")
                    .into_values()
                    .flatten()
                    .collect::<Vec<_>>();

                if routing_table.len() >= NODE_COUNT - 1 {
                    // This node's routing table is fully populated, mark for removal
                    indices_to_remove.push(idx);
                    tracing::info!(
                        target: LOG_TARGET,
                        "Node has complete routing table with {} entries",
                        routing_table.len()
                    );
                }
            }

            for idx in indices_to_remove.iter().rev() {
                txs.remove(*idx);
            }
        }

        assert!(
            txs.is_empty(),
            "Timed out after {:?} - {} nodes still have incomplete routing tables",
            timeout,
            txs.len()
        );

        // Verify closest peers from the bootstrap node
        let (closest_tx, closest_rx) = oneshot::channel();
        tx1.send(Command::Discovery(DiscoveryCommand::GetClosestPeers {
            peer_id: bootstrap_node_peer_id,
            reply: closest_tx,
        }))
        .await
        .expect("Failed to send get closest peers command");

        let closest_peers = closest_rx.await.expect("Failed to get closest peers");

        assert!(
            closest_peers.len() >= NODE_COUNT - 1,
            "Expected at least {} closest peers, got {}",
            NODE_COUNT - 1,
            closest_peers.len()
        );

        for task in handler_tasks {
            task.abort();
        }
    }

    #[tokio::test]
    async fn removes_failed_dial_address_from_kademlia() {
        init_tracing();

        let (tx, rx) = mpsc::channel(10);
        let (pubsub_events_tx, _) = broadcast::channel(10);
        let (chainsync_events_tx, _) = broadcast::channel(10);

        let config = create_libp2p_config(vec![], get_available_udp_port().unwrap());

        let mut handler =
            SwarmHandler::new(config, tx, rx, pubsub_events_tx, chainsync_events_tx, OsRng);

        let remote_peer = PeerId::random();
        let remote_addr = format!(
            "/ip4/127.0.0.1/udp/{}/quic-v1",
            get_available_udp_port().unwrap()
        )
        .parse::<Multiaddr>()
        .unwrap()
        .with(Protocol::P2p(remote_peer));

        handler.bootstrap_kad_from_peers(&vec![remote_addr.clone()]);

        let before = handler.swarm.kademlia_discovered_peers();
        assert!(
            before
                .iter()
                .any(|p| p.peer_id == remote_peer && p.addrs.contains(&remote_addr)),
            "Expected Kademlia to contain the remote address before failure handling",
        );

        let (result_sender, _result_rx) = oneshot::channel();
        handler.connect(Dial {
            addr: remote_addr.clone(),
            retry_count: 0,
            result_sender,
            purpose: DialPurpose::Normal,
        });

        let connection_id = *handler
            .pending_dials
            .keys()
            .next()
            .expect("Expected a pending dial entry");

        handler
            .pending_dials
            .get_mut(&connection_id)
            .expect("pending dial entry should exist")
            .retry_count = MAX_RETRY;

        let event = SwarmEvent::OutgoingConnectionError {
            peer_id: Some(remote_peer),
            connection_id,
            error: DialError::NoAddresses,
        };

        handler.handle_swarm_event(event);

        let after = handler.swarm.kademlia_discovered_peers();
        assert!(
            !after
                .iter()
                .any(|p| p.peer_id == remote_peer && p.addrs.contains(&remote_addr)),
            "Expected failed dial address to be removed from Kademlia",
        );
    }

    // A peer that rotated its identity key (e.g. redeployed without a stable
    // `node_key`) keeps the same `IP:port` but answers with a new PeerId. Dials
    // to its stale `/p2p/<old-id>@addr` therefore fail with `WrongPeerId`.
    //
    // Such dials are issued by Kademlia periodic bootstrap / Identify / chain
    // sync, NOT by our own `connect()`, so there is no `pending_dials` entry.
    // The stale address must still be evicted from Kademlia, otherwise periodic
    // bootstrap re-dials it forever and spams dial errors.
    #[tokio::test]
    async fn removes_wrong_peer_id_address_without_pending_dial() {
        init_tracing();

        let (tx, rx) = mpsc::channel(10);
        let (pubsub_events_tx, _) = broadcast::channel(10);
        let (chainsync_events_tx, _) = broadcast::channel(10);

        let config = create_libp2p_config(vec![], get_available_udp_port().unwrap());

        let mut handler =
            SwarmHandler::new(config, tx, rx, pubsub_events_tx, chainsync_events_tx, OsRng);

        // A peer learned via discovery (Kademlia/Identify), i.e. NOT through our
        // own `connect()` call, so there is no `pending_dials` entry for it.
        let expected_peer = PeerId::random();
        let remote_addr = format!(
            "/ip4/127.0.0.1/udp/{}/quic-v1",
            get_available_udp_port().unwrap()
        )
        .parse::<Multiaddr>()
        .unwrap()
        .with(Protocol::P2p(expected_peer));

        handler.bootstrap_kad_from_peers(&vec![remote_addr.clone()]);

        let before = handler.swarm.kademlia_discovered_peers();
        assert!(
            before
                .iter()
                .any(|p| p.peer_id == expected_peer && p.addrs.contains(&remote_addr)),
            "Expected Kademlia to contain the remote address before failure handling",
        );

        // The node listening at `remote_addr` now reports a different PeerId.
        // This mirrors a Kademlia periodic-bootstrap dial failing with
        // `WrongPeerId`, with no corresponding `pending_dials` entry.
        let obtained_peer = PeerId::random();
        let event = SwarmEvent::OutgoingConnectionError {
            peer_id: Some(expected_peer),
            connection_id: ConnectionId::new_unchecked(1),
            error: DialError::WrongPeerId {
                obtained: obtained_peer,
                address: remote_addr.clone(),
            },
        };

        handler.handle_swarm_event(event);

        let after = handler.swarm.kademlia_discovered_peers();
        assert!(
            !after
                .iter()
                .any(|p| p.peer_id == expected_peer && p.addrs.contains(&remote_addr)),
            "Expected the stale WrongPeerId address to be removed from Kademlia, \
             even though the dial was not initiated via `connect()`",
        );
    }

    #[tokio::test]
    async fn info_reports_discovered_peers() {
        init_tracing();

        let (tx, rx) = mpsc::channel(10);
        let (pubsub_events_tx, _) = broadcast::channel(10);
        let (chainsync_events_tx, _) = broadcast::channel(10);

        let config = create_libp2p_config(vec![], get_available_udp_port().unwrap());
        let mut handler =
            SwarmHandler::new(config, tx, rx, pubsub_events_tx, chainsync_events_tx, OsRng);

        let expected_peers: Vec<(PeerId, Multiaddr)> = std::iter::repeat_with(|| {
            let peer_id = PeerId::random();
            let addr = format!(
                "/ip4/127.0.0.1/udp/{}/quic-v1",
                get_available_udp_port().unwrap()
            )
            .parse::<Multiaddr>()
            .unwrap()
            .with(Protocol::P2p(peer_id));
            (peer_id, addr)
        })
        .take(3)
        .collect();

        handler.bootstrap_kad_from_peers(
            &expected_peers
                .iter()
                .map(|(_, addr)| addr.clone())
                .collect::<Vec<_>>(),
        );

        let (reply, info_rx) = oneshot::channel();
        handler.handle_network_command(NetworkCommand::Info { reply });
        let info = info_rx.await.expect("info reply");

        let expected: HashSet<PeerId> = expected_peers.iter().map(|(id, _)| *id).collect();
        let actual: HashSet<PeerId> = info.discovered_peers.iter().copied().collect();
        assert_eq!(actual, expected);
        assert_eq!(info.n_discovered_peers, expected.len());
    }

    #[tokio::test]
    async fn info_reports_empty_discovered_peers() {
        init_tracing();

        let (tx, rx) = mpsc::channel(10);
        let (pubsub_events_tx, _) = broadcast::channel(10);
        let (chainsync_events_tx, _) = broadcast::channel(10);

        let config = create_libp2p_config(vec![], get_available_udp_port().unwrap());
        let mut handler =
            SwarmHandler::new(config, tx, rx, pubsub_events_tx, chainsync_events_tx, OsRng);

        handler.bootstrap_kad_from_peers(&vec![]);

        let (reply, info_rx) = oneshot::channel();
        handler.handle_network_command(NetworkCommand::Info { reply });
        let info = info_rx.await.expect("info reply");

        assert!(info.discovered_peers.is_empty());
        assert_eq!(info.n_discovered_peers, 0);
    }
}
