#![allow(
    clippy::multiple_inherent_impl,
    reason = "We spilt the impl in different blocks on purpose to ease localizing changes."
)]

// This macro must be on top if it is accessed by child modules, else if the
// modules are defined before it, they will fail to see it.
macro_rules! log_error {
    ($e:expr) => {
        if let Err(e) = $e {
            tracing::error!("error while processing {}: {e:?}", stringify!($e));
        }
    };
}

use std::{collections::HashMap, time::Duration};

use nomos_libp2p::{
    Multiaddr, PeerId, Swarm, SwarmEvent,
    behaviour::BehaviourEvent,
    libp2p::{kad::QueryId, swarm::ConnectionId},
};
use rand::RngCore;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_stream::StreamExt as _;
use tracing::info;

use super::{
    Libp2pConfig, Message,
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

pub struct SwarmHandler<R: Clone + Send + RngCore + 'static> {
    pub swarm: Swarm<R>,
    pub pending_dials: HashMap<ConnectionId, Dial>,
    pub commands_tx: mpsc::Sender<Command>,
    pub commands_rx: mpsc::Receiver<Command>,
    pub pubsub_messages_tx: broadcast::Sender<Message>,
    pub chainsync_events_tx: broadcast::Sender<ChainSyncEvent>,

    pending_queries: HashMap<QueryId, PendingQueryData>,
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

    fn handle_swarm_event(&mut self, event: SwarmEvent<BehaviourEvent<R>>) {
        match event {
            SwarmEvent::ConnectionEstablished {
                peer_id,
                connection_id,
                endpoint,
                ..
            } => {
                tracing::debug!("connected to peer:{peer_id}, connection_id:{connection_id:?}");
                if endpoint.is_dialer() {
                    self.complete_connect(connection_id, peer_id);
                }
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                connection_id,
                cause,
                ..
            } => {
                tracing::debug!(
                    "connection closed from peer: {peer_id} {connection_id:?} due to {cause:?}"
                );
            }
            SwarmEvent::OutgoingConnectionError {
                peer_id,
                connection_id,
                error,
                ..
            } => {
                tracing::error!(
                    "Failed to connect to peer: {peer_id:?} {connection_id:?} due to: {error}"
                );
                self.retry_connect(connection_id);
            }
            SwarmEvent::ExternalAddrConfirmed { address } => {
                self.handle_external_addr_confirmed(&address);
            }
            _ => {}
        }
    }

    fn handle_external_addr_confirmed(&mut self, address: &Multiaddr) {
        let local_peer_id = *self.swarm.swarm().local_peer_id();
        self.swarm
            .kademlia_add_address(local_peer_id, address.clone());
        info!("Added confirmed external address to Kademlia: {address}");
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
                let swarm = self.swarm.swarm();
                let network_info = swarm.network_info();
                let counters = network_info.connection_counters();
                let info = Libp2pInfo {
                    listen_addresses: swarm.listeners().cloned().collect(),
                    n_peers: network_info.num_peers(),
                    n_connections: counters.num_connections(),
                    n_pending_connections: counters.num_pending(),
                };
                log_error!(reply.send(info));
            }
            NetworkCommand::ConnectedPeers { reply } => {
                let connected_peers = self.swarm.swarm().connected_peers().copied().collect();
                log_error!(reply.send(connected_peers));
            }
        }
    }

    async fn schedule_connect(dial: Dial, commands_tx: mpsc::Sender<Command>) {
        commands_tx
            .send(Command::Network(NetworkCommand::Connect(dial)))
            .await
            .unwrap_or_else(|_| tracing::error!("could not schedule connect"));
    }

    fn connect(&mut self, dial: Dial) {
        tracing::debug!("Connecting to {}", dial.addr);

        match self.swarm.connect(&dial.addr) {
            Ok(connection_id) => {
                // Dialing has been scheduled. The result will be notified as a SwarmEvent.
                self.pending_dials.insert(connection_id, dial);
            }
            Err(e) => {
                if let Err(err) = dial.result_sender.send(Err(e)) {
                    tracing::warn!("failed to send the Err result of dialing: {err:?}");
                }
            }
        }
    }

    fn complete_connect(&mut self, connection_id: ConnectionId, peer_id: PeerId) {
        if let Some(dial) = self.pending_dials.remove(&connection_id)
            && let Err(e) = dial.result_sender.send(Ok(peer_id))
        {
            tracing::warn!("failed to send the Ok result of dialing: {e:?}");
        }
    }

    // TODO: Consider a common retry module for all use cases
    fn retry_connect(&mut self, connection_id: ConnectionId) {
        let Some(mut dial) = self.pending_dials.remove(&connection_id) else {
            return;
        };
        let Some(new_retry_count) = dial.retry_count.checked_add(1) else {
            tracing::debug!("Retry count overflow.");
            return;
        };
        if new_retry_count > MAX_RETRY {
            tracing::debug!("Max retry({MAX_RETRY}) has been reached: {dial:?}");
            return;
        }
        dial.retry_count = new_retry_count;

        let wait = exp_backoff(dial.retry_count);
        tracing::debug!("Retry dialing in {wait:?}: {dial:?}");

        let commands_tx = self.commands_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            Self::schedule_connect(dial, commands_tx).await;
        });
    }
}

const fn exp_backoff(retry: usize) -> Duration {
    Duration::from_secs(BACKOFF.pow(retry as u32))
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv4Addr, sync::Once, time::Instant};

    use nomos_libp2p::{Protocol, protocol_name::StreamProtocol};
    use nomos_utils::net::get_available_udp_port;
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

    fn create_swarm_config(port: u16, is_boot: bool) -> nomos_libp2p::SwarmConfig {
        nomos_libp2p::SwarmConfig {
            host: Ipv4Addr::LOCALHOST,
            port,
            node_key: nomos_libp2p::ed25519::SecretKey::generate(),
            gossipsub_config: nomos_libp2p::gossipsub::Config::default(),
            // Use a tighter bootstrap interval for the first node if requested,
            // otherwise fall back to defaults.
            kademlia_config: if is_boot {
                nomos_libp2p::KademliaSettings {
                    periodic_bootstrap_interval_secs: Some(1),
                    ..Default::default()
                }
            } else {
                nomos_libp2p::KademliaSettings::default()
            },
            kad_protocol_name: StreamProtocol::new("/kademlia/test"),
            identify_protocol_name: StreamProtocol::new("/identify/test"),
            chain_sync_protocol_name: StreamProtocol::new("/chainsync/test"),
            identify_config: nomos_libp2p::IdentifySettings::default(),
            chain_sync_config: cryptarchia_sync::Config::default(),
            nat_config: nomos_libp2p::NatSettings::Traversal(nomos_libp2p::TraversalSettings {
                autonat: nomos_libp2p::AutonatClientSettings {
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

    const NODE_COUNT: usize = 10;

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

        info!(
            "Bootstrap node listening on: {:?}",
            bootstrap_info.listen_addresses
        );

        // Use the first listening address as the bootstrap address
        let bootstrap_addr = bootstrap_info.listen_addresses[0]
            .clone()
            .with(Protocol::P2p(bootstrap_node_peer_id));

        info!("Using bootstrap address: {}", bootstrap_addr);

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
            info!("Starting node {} with peer ID: {}", i, peer_id);

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

                let routing_table = dump_rx.await.expect("Failed to receive routing table dump");

                let routing_table = routing_table
                    .into_iter()
                    .flat_map(|(_, peers)| peers)
                    .collect::<Vec<_>>();

                if routing_table.len() >= NODE_COUNT - 1 {
                    // This node's routing table is fully populated, mark for removal
                    indices_to_remove.push(idx);
                    info!(
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
}
