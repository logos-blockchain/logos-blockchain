use lb_time::Era;
use overwatch::overwatch::handle::OverwatchHandle;
use tokio_stream::wrappers::BroadcastStream;

use super::Debug;

pub mod libp2p;
pub mod mock;

#[async_trait::async_trait]
pub trait NetworkBackend<RuntimeServiceId> {
    type Settings: Clone + Debug + Send + Sync + 'static;
    type Message: Debug + Send + Sync + 'static;
    type PubSubEvent: Debug + Send + Sync + 'static;
    type ChainSyncEvent: Debug + Send + Sync + 'static;
    fn new(
        config: Self::Settings,
        current_era: Era,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Self;
    async fn process(&self, msg: Self::Message);

    async fn rotate_era(&mut self, era: Era);
    async fn complete_era_transition(&mut self, ended_era: Era);

    async fn subscribe_to_pubsub(&mut self) -> BroadcastStream<Self::PubSubEvent>;
    async fn subscribe_to_chainsync(&mut self) -> BroadcastStream<Self::ChainSyncEvent>;
}
