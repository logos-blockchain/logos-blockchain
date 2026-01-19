mod command;
pub mod config;
pub(crate) mod swarm;

pub use lb_libp2p::{
    PeerId,
    libp2p::gossipsub::{Message, TopicHash},
};
use overwatch::overwatch::handle::OverwatchHandle;
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
impl<RuntimeServiceId> NetworkBackend<RuntimeServiceId> for Libp2p {
    type Settings = Libp2pConfig;
    type Message = Command;
    type PubSubEvent = Message;
    type ChainSyncEvent = ChainSyncEvent;

    fn new(config: Self::Settings, overwatch_handle: OverwatchHandle<RuntimeServiceId>) -> Self {
        let rng = ChaCha20Rng::from_entropy();
        let (commands_tx, commands_rx) = mpsc::channel(BUFFER_SIZE);

        let (pubsub_events_tx, _) = broadcast::channel(BUFFER_SIZE);
        let (chainsync_events_tx, _) = broadcast::channel(BUFFER_SIZE);

        let initial_peers = config.initial_peers.clone();

        let mut swarm_handler = SwarmHandler::new(
            config,
            commands_tx.clone(),
            commands_rx,
            pubsub_events_tx.clone(),
            chainsync_events_tx.clone(),
            rng,
        );

        overwatch_handle.runtime().spawn(async move {
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
