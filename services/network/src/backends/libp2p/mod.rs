mod command;
pub mod config;
pub(crate) mod swarm;

use std::fmt::{Debug, Display};

use lb_banning_service::BanningService;
pub use lb_libp2p::{
    PeerId,
    libp2p::gossipsub::{Message, TopicHash},
};
use overwatch::{overwatch::handle::OverwatchHandle, services::AsServiceId};
use rand::SeedableRng as _;
use rand_chacha::ChaCha20Rng;
use tokio::sync::{broadcast, broadcast::Sender, mpsc};
use tokio_stream::wrappers::BroadcastStream;

use self::swarm::SwarmHandler;
pub use self::{
    command::{
        ChainSyncCommand, Command, Dial, DiscoveryCommand, Libp2pInfo, NetworkCommand,
        PubSubCommand,
    },
    config::Libp2pConfig,
};
use super::NetworkBackend;
use crate::message::ChainSyncEvent;

pub struct Libp2p {
    pubsub_events_tx: Sender<Message>,
    chainsync_events_tx: Sender<ChainSyncEvent>,
    commands_tx: mpsc::Sender<Command>,
}
const BUFFER_SIZE: usize = 64;

#[async_trait::async_trait]
impl<RuntimeServiceId> NetworkBackend<RuntimeServiceId> for Libp2p
where
    RuntimeServiceId:
        AsServiceId<BanningService<RuntimeServiceId>> + Display + Sync + Debug + Send + 'static,
{
    type Settings = Libp2pConfig;
    type Message = Command;
    type PubSubEvent = Message;
    type ChainSyncEvent = ChainSyncEvent;

    fn new(config: Self::Settings, overwatch_handle: OverwatchHandle<RuntimeServiceId>) -> Self {
        let (commands_tx, commands_rx) = mpsc::channel(BUFFER_SIZE);

        let (pubsub_events_tx, _) = broadcast::channel(BUFFER_SIZE);
        let (chainsync_events_tx, _) = broadcast::channel(BUFFER_SIZE);

        let initial_peers = config.initial_peers.clone();

        // Clone the senders for the spawned task
        let pubsub_events_tx_clone = pubsub_events_tx.clone();
        let chainsync_events_tx_clone = chainsync_events_tx.clone();
        let commands_tx_clone = commands_tx.clone();

        // Get the runtime handle before moving overwatch_handle into the async block
        let runtime = overwatch_handle.runtime().clone();

        // Spawn the swarm handler task. The banning relay is acquired inside the async
        // block to avoid blocking execution.
        runtime.spawn(async move {
            let banning_relay = overwatch_handle
                .relay::<BanningService<_>>()
                .await
                .expect("Banning service must be available");

            let rng = ChaCha20Rng::from_entropy();
            let mut swarm_handler = SwarmHandler::new(
                config,
                commands_tx_clone,
                commands_rx,
                pubsub_events_tx_clone,
                chainsync_events_tx_clone,
                rng,
                Some(banning_relay),
            );

            swarm_handler.run(initial_peers).await;
        });

        Self {
            pubsub_events_tx,
            chainsync_events_tx,
            commands_tx,
        }
    }

    async fn process(&self, msg: Self::Message) {
        if let Err(e) = self.commands_tx.send(msg).await {
            tracing::error!("failed to send command to logos-blockchain-libp2p: {e:?}");
        }
    }

    async fn subscribe_to_pubsub(&mut self) -> BroadcastStream<Self::PubSubEvent> {
        BroadcastStream::new(self.pubsub_events_tx.subscribe())
    }

    async fn subscribe_to_chainsync(&mut self) -> BroadcastStream<Self::ChainSyncEvent> {
        BroadcastStream::new(self.chainsync_events_tx.subscribe())
    }
}
