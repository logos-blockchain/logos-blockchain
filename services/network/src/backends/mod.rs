use overwatch::overwatch::handle::OverwatchHandle;
use tokio_stream::wrappers::BroadcastStream;

use super::Debug;

#[cfg(feature = "libp2p")]
pub mod libp2p;

#[cfg(feature = "mock")]
pub mod mock;

#[async_trait::async_trait]
pub trait NetworkBackend<RuntimeServiceId> {
    type Settings: Clone + Debug + Send + Sync + 'static;
    type Message: Debug + Send + Sync + 'static;
    type PubSubEvent: Debug + Send + Sync + 'static;
    type ChainSyncEvent: Debug + Send + Sync + 'static;
    fn new(config: Self::Settings, overwatch_handle: OverwatchHandle<RuntimeServiceId>) -> Self;
    async fn process(&self, msg: Self::Message);
    async fn subscribe_to_pubsub(&mut self) -> BroadcastStream<Self::PubSubEvent>;

    async fn subscribe_to_chainsync(&mut self) -> BroadcastStream<Self::ChainSyncEvent>;
}
