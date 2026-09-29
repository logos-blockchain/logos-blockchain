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
        StreamProtocol as Libp2pStreamProtocol,
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
    Libp2pConfig, Message, TopicHash,
    command::{Command, Dial, NetworkCommand},
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

#[derive(Debug)]
struct ProtocolContract {
    kademlia_protocol: Libp2pStreamProtocol,
    chain_sync_protocol: Libp2pStreamProtocol,
}

impl ProtocolContract {
    fn from_config(config: &lb_libp2p::SwarmConfig) -> Self {
        Self {
            kademlia_protocol: config.kad_protocol_name.clone().into_inner(),
            chain_sync_protocol: config.chain_sync_protocol_name.clone().into_inner(),
        }
    }
}

pub struct SwarmHandler<R: Clone + Send + RngCore + 'static> {
    pub swarm: Swarm<R>,
    pub pending_dials: HashMap<ConnectionId, Dial>,
    pub commands_tx: mpsc::Sender<Command>,
    pub commands_rx: mpsc::Receiver<Command>,
    pub pubsub_messages_tx: broadcast::Sender<Message>,
    pub chainsync_events_tx: broadcast::Sender<ChainSyncEvent>,
    pub max_data_size_by_topic: HashMap<TopicHash, usize>,

    pending_queries: HashMap<QueryId, PendingQueryData>,
    protocol_contract: ProtocolContract,
    peer_advertised_protocols: HashMap<PeerId, HashSet<Libp2pStreamProtocol>>,
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
        let Libp2pConfig {
            inner,
            max_data_size_by_topic,
            ..
        } = config;
        let protocol_contract = ProtocolContract::from_config(&inner);
        let swarm = Swarm::build(inner, max_data_size_by_topic.clone(), rng).unwrap();

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
            max_data_size_by_topic,
            pending_queries: HashMap::new(),
            protocol_contract,
            peer_advertised_protocols: HashMap::new(),
        }
    }

    pub async fn run(&mut self, initial_peers: Vec<Multiaddr>) {
        self.bootstrap_kad_from_peers(&initial_peers);

        for initial_peer in &initial_peers {
            let (tx, _) = oneshot::channel();
            let dial = Dial {
                addr: initial_peer.clone(),
                retry_count: 0,
                result_sender: tx,
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

                if num_established == 0 {
                    self.prune_peer_advertised_protocols(peer_id);
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
                        self.remove_kademlia_address_for_dial(peer_id, dial_addr);
                        // Drop any matching pending dial so it is not also retried.
                        self.pending_dials.remove(&connection_id);
                    }
                    error => {
                        tracing::debug!(
                            target: LOG_TARGET,
                            "Failed to connect to peer: {peer_id:?} {connection_id:?} due to: {error}"
                        );
                        self.retry_connect(connection_id, peer_id);
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
        self.prune_peer_advertised_protocols(peer_id);
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

    fn chainsync_eligible_peers(&self) -> HashSet<PeerId> {
        self.peers_supporting_protocol(&self.protocol_contract.chain_sync_protocol)
    }

    fn peers_supporting_protocol(&self, protocol: &Libp2pStreamProtocol) -> HashSet<PeerId> {
        self.peer_advertised_protocols
            .iter()
            .filter_map(|(peer_id, protocols)| protocols.contains(protocol).then_some(*peer_id))
            .collect()
    }

    /// A peer can be disconnected but still remain known through Kademlia and
    /// therefore appear among discovered peers. We retain its latest
    /// advertised protocols so we can still filter it for chainsync before
    /// attempting a new connection. We only prune that information once the
    /// peer is both disconnected and no longer known through discovery.
    fn prune_peer_advertised_protocols(&mut self, peer_id: PeerId) {
        if !self.peer_advertised_protocols.contains_key(&peer_id) {
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
        if !is_connected && !is_in_kademlia {
            self.peer_advertised_protocols.remove(&peer_id);
        }
    }

    async fn schedule_connect(dial: Dial, commands_tx: mpsc::Sender<Command>) {
        commands_tx
            .send(Command::Network(NetworkCommand::Connect(dial)))
            .await
            .unwrap_or_else(|_| tracing::error!(target: LOG_TARGET, "could not schedule connect"));
    }

    fn connect(&mut self, dial: Dial) {
        tracing::debug!(target: LOG_TARGET, "Connecting to {}", dial.addr);

        match self.swarm.connect(&dial.addr) {
            Ok(connection_id) => {
                // Dialing has been scheduled. The result will be notified as a SwarmEvent.
                self.pending_dials.insert(connection_id, dial);
            }
            Err(e) => {
                if let Err(err) = dial.result_sender.send(Err(e)) {
                    tracing::warn!(
                        target: LOG_TARGET,
                        "failed to send the Err result of dialing: {err:?}"
                    );
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
            max_data_size_by_topic: HashMap::new(),
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

    fn create_test_address() -> Multiaddr {
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
        let address = create_test_address();
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

    fn advertised_protocols(
        protocols: &[&'static str],
    ) -> HashSet<lb_libp2p::libp2p::StreamProtocol> {
        protocols
            .iter()
            .map(|protocol| lb_libp2p::libp2p::StreamProtocol::new(protocol))
            .collect()
    }

    fn create_gossipsub_handler(
        topic: TopicHash,
        max_data_size: usize,
    ) -> (SwarmHandler<OsRng>, broadcast::Receiver<Message>) {
        let (commands_tx, commands_rx) = mpsc::channel(1);
        let (pubsub_events_tx, pubsub_events_rx) = broadcast::channel(1);
        let (chainsync_events_tx, _) = broadcast::channel(1);
        let mut config = create_libp2p_config(vec![], get_available_udp_port().unwrap());
        config.max_data_size_by_topic.insert(topic, max_data_size);

        let handler = SwarmHandler::new(
            config,
            commands_tx,
            commands_rx,
            pubsub_events_tx,
            chainsync_events_tx,
            OsRng,
        );

        (handler, pubsub_events_rx)
    }

    fn gossipsub_message_event(topic: TopicHash, data_size: usize) -> lb_libp2p::gossipsub::Event {
        lb_libp2p::gossipsub::Event::Message {
            propagation_source: PeerId::random(),
            message_id: lb_libp2p::gossipsub::MessageId::from("test"),
            message: Message {
                source: None,
                data: vec![0; data_size],
                sequence_number: None,
                topic,
            },
        }
    }

    #[tokio::test]
    async fn forwards_inbound_application_data_at_the_topic_limit() {
        let topic = lb_libp2p::gossipsub::IdentTopic::new("transactions").hash();
        let max_data_size = 512;
        let (handler, mut pubsub_events_rx) =
            create_gossipsub_handler(topic.clone(), max_data_size);

        handler.handle_gossipsub_event(gossipsub_message_event(topic, max_data_size));

        assert_eq!(
            pubsub_events_rx.try_recv().unwrap().data.len(),
            max_data_size
        );
    }

    #[tokio::test]
    async fn drops_inbound_application_data_above_the_topic_limit() {
        let topic = lb_libp2p::gossipsub::IdentTopic::new("transactions").hash();
        let max_data_size = 512;
        let (handler, mut pubsub_events_rx) =
            create_gossipsub_handler(topic.clone(), max_data_size);

        handler.handle_gossipsub_event(gossipsub_message_event(topic, max_data_size + 1));

        assert!(matches!(
            pubsub_events_rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn drops_inbound_application_data_for_an_unconfigured_topic() {
        let configured_topic = lb_libp2p::gossipsub::IdentTopic::new("transactions").hash();
        let unconfigured_topic = lb_libp2p::gossipsub::IdentTopic::new("proposals").hash();
        let (handler, mut pubsub_events_rx) = create_gossipsub_handler(configured_topic, 512);

        handler.handle_gossipsub_event(gossipsub_message_event(unconfigured_topic, 512));

        assert!(matches!(
            pubsub_events_rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn rejects_outbound_application_data_above_the_topic_limit() {
        let topic = lb_libp2p::gossipsub::IdentTopic::new("transactions").hash();
        let max_data_size = 512;
        let (mut handler, mut pubsub_events_rx) = create_gossipsub_handler(topic, max_data_size);

        handler.broadcast_and_retry(
            "transactions".to_owned(),
            vec![0; max_data_size + 1].into_boxed_slice(),
            MAX_RETRY,
        );

        assert!(matches!(
            pubsub_events_rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            handler.commands_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn rejects_outbound_application_data_for_an_unconfigured_topic() {
        let configured_topic = lb_libp2p::gossipsub::IdentTopic::new("transactions").hash();
        let (mut handler, mut pubsub_events_rx) = create_gossipsub_handler(configured_topic, 512);

        handler.broadcast_and_retry(
            "proposals".to_owned(),
            vec![0; 512].into_boxed_slice(),
            MAX_RETRY,
        );

        assert!(matches!(
            pubsub_events_rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            handler.commands_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    async fn wait_for_network_info(
        commands_tx: &mpsc::Sender<Command>,
        ready: impl Fn(&Libp2pInfo) -> bool,
    ) -> Libp2pInfo {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (reply, info_rx) = oneshot::channel();
            commands_tx
                .send(Command::Network(NetworkCommand::Info { reply }))
                .await
                .expect("network handler should still be running");
            let info = info_rx.await.expect("network info response");
            if ready(&info) {
                return info;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for network state: {info:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn forwards_exact_limit_outbound_data_after_successful_publish() {
        let topic = "transactions";
        let topic_hash = lb_libp2p::gossipsub::IdentTopic::new(topic).hash();
        let max_data_size = 512;

        let (bootstrap_commands_tx, bootstrap_commands_rx) = mpsc::channel(10);
        let (bootstrap_pubsub_events_tx, _) = broadcast::channel(10);
        let (bootstrap_chainsync_events_tx, _) = broadcast::channel(10);
        let mut bootstrap_config = create_libp2p_config(vec![], get_available_udp_port().unwrap());
        bootstrap_config
            .max_data_size_by_topic
            .insert(topic_hash.clone(), max_data_size);
        let mut bootstrap = SwarmHandler::new(
            bootstrap_config,
            bootstrap_commands_tx.clone(),
            bootstrap_commands_rx,
            bootstrap_pubsub_events_tx,
            bootstrap_chainsync_events_tx,
            OsRng,
        );
        bootstrap.handle_pubsub_command(PubSubCommand::Subscribe(topic.to_owned()));
        let bootstrap_peer_id = *bootstrap.swarm.swarm().local_peer_id();
        let bootstrap_task = tokio::spawn(async move {
            bootstrap.run(vec![]).await;
        });

        let bootstrap_info = wait_for_network_info(&bootstrap_commands_tx, |info| {
            !info.listen_addresses.is_empty()
        })
        .await;
        let bootstrap_address = bootstrap_info.listen_addresses[0]
            .clone()
            .with(Protocol::P2p(bootstrap_peer_id));

        let (peer_commands_tx, peer_commands_rx) = mpsc::channel(10);
        let (peer_pubsub_events_tx, mut peer_pubsub_events_rx) = broadcast::channel(10);
        let (peer_chainsync_events_tx, _) = broadcast::channel(10);
        let mut peer_config = create_libp2p_config(
            vec![bootstrap_address.clone()],
            get_available_udp_port().unwrap(),
        );
        peer_config
            .max_data_size_by_topic
            .insert(topic_hash.clone(), max_data_size);
        let mut peer = SwarmHandler::new(
            peer_config,
            peer_commands_tx.clone(),
            peer_commands_rx,
            peer_pubsub_events_tx,
            peer_chainsync_events_tx,
            OsRng,
        );
        peer.handle_pubsub_command(PubSubCommand::Subscribe(topic.to_owned()));
        let peer_task = tokio::spawn(async move {
            peer.run(vec![bootstrap_address]).await;
        });

        wait_for_network_info(&peer_commands_tx, |info| {
            info.connected_peers.contains(&bootstrap_peer_id)
        })
        .await;

        peer_commands_tx
            .send(Command::PubSub(PubSubCommand::Broadcast {
                topic: topic.to_owned(),
                message: vec![0; max_data_size].into_boxed_slice(),
            }))
            .await
            .expect("peer network handler should still be running");

        let message = tokio::time::timeout(Duration::from_secs(10), peer_pubsub_events_rx.recv())
            .await
            .expect("timed out waiting for self-notification")
            .expect("self-notification channel should remain open");

        assert!(message.source.is_none());
        assert!(message.sequence_number.is_none());
        assert_eq!(message.topic, topic_hash);
        assert_eq!(message.data.len(), max_data_size);

        bootstrap_task.abort();
        peer_task.abort();
    }

    #[tokio::test]
    async fn refuses_subscription_to_an_unconfigured_topic() {
        let configured_topic = lb_libp2p::gossipsub::IdentTopic::new("transactions").hash();
        let (mut handler, _) = create_gossipsub_handler(configured_topic, 512);

        handler.handle_pubsub_command(PubSubCommand::Subscribe("proposals".to_owned()));

        assert!(!handler.swarm.is_subscribed("proposals"));
    }

    const NODE_COUNT: usize = 10;

    #[tokio::test]
    async fn only_peers_advertising_chainsync_are_eligible() {
        let mut handler = create_handler();
        let supported = PeerId::random();
        let unsupported = PeerId::random();
        handler
            .peer_advertised_protocols
            .insert(supported, advertised_protocols(&["/chainsync/test"]));
        handler
            .peer_advertised_protocols
            .insert(unsupported, advertised_protocols(&["/kademlia/test"]));

        assert_eq!(
            handler.chainsync_eligible_peers(),
            HashSet::from([supported])
        );
    }

    #[tokio::test]
    async fn identify_updates_advertised_protocols_without_kademlia_eviction() {
        let mut handler = create_handler();
        let peer_id = PeerId::random();
        let address = create_test_address().with(Protocol::P2p(peer_id));
        handler.swarm.kademlia_add_address(peer_id, &address);

        handler.handle_identify_event(identify_event(
            peer_id,
            &["/kademlia/test", "/chainsync/test"],
        ));
        assert_eq!(
            handler.peer_advertised_protocols.get(&peer_id),
            Some(&advertised_protocols(&[
                "/kademlia/test",
                "/chainsync/test"
            ]))
        );
        assert!(handler.chainsync_eligible_peers().contains(&peer_id));

        handler.handle_identify_event(identify_event(peer_id, &["/kademlia/test"]));
        assert_eq!(
            handler.peer_advertised_protocols.get(&peer_id),
            Some(&advertised_protocols(&["/kademlia/test"]))
        );
        assert!(!handler.chainsync_eligible_peers().contains(&peer_id));
        assert!(
            handler
                .swarm
                .kademlia_discovered_peers()
                .iter()
                .any(|peer| peer.peer_id == peer_id)
        );
    }

    #[tokio::test]
    async fn chainsync_support_does_not_require_kademlia_advertisement() {
        let mut handler = create_handler();
        let peer_id = PeerId::random();

        handler.handle_identify_event(identify_event(peer_id, &["/chainsync/test"]));

        assert_eq!(
            handler.peer_advertised_protocols.get(&peer_id),
            Some(&advertised_protocols(&["/chainsync/test"]))
        );
        assert!(handler.chainsync_eligible_peers().contains(&peer_id));
        assert!(handler.swarm.kademlia_discovered_peers().is_empty());
    }

    #[tokio::test]
    async fn advertised_protocols_are_pruned_only_when_peer_is_no_longer_known() {
        let mut handler = create_handler();
        let peer_id = PeerId::random();
        let address = create_test_address();

        handler
            .peer_advertised_protocols
            .insert(peer_id, advertised_protocols(&["/chainsync/test"]));
        handler.prune_peer_advertised_protocols(peer_id);
        assert!(!handler.peer_advertised_protocols.contains_key(&peer_id));

        handler.swarm.kademlia_add_address(peer_id, &address);
        handler
            .peer_advertised_protocols
            .insert(peer_id, advertised_protocols(&["/chainsync/test"]));
        handler.prune_peer_advertised_protocols(peer_id);
        assert_eq!(
            handler.peer_advertised_protocols.get(&peer_id),
            Some(&advertised_protocols(&["/chainsync/test"]))
        );
    }

    #[tokio::test]
    async fn removing_last_kademlia_address_prunes_disconnected_peer_protocols() {
        let mut handler = create_handler();
        let peer_id = PeerId::random();
        let address = create_test_address().with(Protocol::P2p(peer_id));

        handler.swarm.kademlia_add_address(peer_id, &address);
        handler
            .peer_advertised_protocols
            .insert(peer_id, advertised_protocols(&["/chainsync/test"]));

        handler.remove_kademlia_address_for_dial(Some(peer_id), &address);

        assert!(handler.swarm.kademlia_discovered_peers().is_empty());
        assert!(!handler.peer_advertised_protocols.contains_key(&peer_id));
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
