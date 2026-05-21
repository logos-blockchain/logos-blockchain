use core::{fmt::Debug, pin::Pin};
use std::{collections::HashMap, fmt::Display};

use async_trait::async_trait;
use derivative::Derivative;
use futures::{Stream, StreamExt as _, future::ready, stream::iter};
use lb_core::{
    header::HeaderId,
    sdp::{ProviderId, ProviderInfo},
};
use lb_cryptarchia_engine::Epoch;
use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, oneshot};
use tokio_stream::wrappers::BroadcastStream;
use tracing::{error, info, trace};

const BROADCAST_CHANNEL_SIZE: usize = 128;

pub type ActiveProvidersSubscription = Pin<Box<dyn Stream<Item = ActiveProviders> + Send + Sync>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveProviders {
    pub epoch: Epoch,
    pub providers: HashMap<ProviderId, ProviderInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockInfo {
    pub height: u64,
    pub header_id: HeaderId,
}

#[derive(Derivative)]
#[derivative(Debug)]
pub enum BlockBroadcastMsg {
    BroadcastFinalizedBlock(BlockInfo),
    BroadcastBlendProviders(ActiveProviders),
    SubscribeToFinalizedBlocks {
        result_sender: oneshot::Sender<broadcast::Receiver<BlockInfo>>,
    },
    SubscribeBlendProviders {
        #[derivative(Debug = "ignore")]
        result_sender: oneshot::Sender<ActiveProvidersSubscription>,
    },
}

pub struct BlockBroadcastService<RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    finalized_blocks: broadcast::Sender<BlockInfo>,
    blend_providers: broadcast::Sender<ActiveProviders>,
    // For sending latest blend active providers on subscription.
    last_blend_providers: Option<ActiveProviders>,
}

impl<RuntimeServiceId> ServiceData for BlockBroadcastService<RuntimeServiceId> {
    type Settings = ();
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = BlockBroadcastMsg;
}

#[async_trait]
impl<RuntimeServiceId> ServiceCore<RuntimeServiceId> for BlockBroadcastService<RuntimeServiceId>
where
    RuntimeServiceId: AsServiceId<Self> + Clone + Display + Send + Sync + 'static,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, overwatch::DynError> {
        let (finalized_blocks, _) = broadcast::channel(BROADCAST_CHANNEL_SIZE);
        let (blend_providers, _) = broadcast::channel(BROADCAST_CHANNEL_SIZE);

        Ok(Self {
            service_resources_handle,
            finalized_blocks,
            blend_providers,
            last_blend_providers: None,
        })
    }

    async fn run(mut self) -> Result<(), overwatch::DynError> {
        self.service_resources_handle.status_updater.notify_ready();
        info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        while let Some(msg) = self.service_resources_handle.inbound_relay.recv().await {
            match msg {
                BlockBroadcastMsg::BroadcastFinalizedBlock(block) => {
                    if self.finalized_blocks.send(block).is_err() {
                        trace!("No listener for finalized blocks. Not broadcasting. ");
                    }
                }
                BlockBroadcastMsg::BroadcastBlendProviders(providers) => {
                    self.last_blend_providers = Some(providers.clone());
                    if self.blend_providers.send(providers).is_err() {
                        trace!("No listener for blend active providers. Not broadcasting. ");
                    }
                }
                BlockBroadcastMsg::SubscribeToFinalizedBlocks { result_sender } => {
                    // TODO: This naively broadcast what was sent from the chain service. In case
                    // of LIB branch change (might happend during bootstrapping), blocks should be
                    // rebroadcasted from the last common header_id.
                    if let Err(err) = result_sender.send(self.finalized_blocks.subscribe()) {
                        error!("Could not subscribe to new blocks channel: {err:?}");
                    }
                }
                BlockBroadcastMsg::SubscribeBlendProviders { result_sender } => {
                    if result_sender
                        .send(create_active_providers_stream(
                            self.last_blend_providers.clone(),
                            &self.blend_providers,
                        ))
                        .is_err()
                    {
                        error!("Could not subscribe to blend active providers channel.");
                    }
                }
            }
        }

        Ok(())
    }
}

/// Create a stream from the current optional, last-processed value and the
/// broadcast sender.
///
/// The stream immediately yields the current value if `Some`, else it will wait
/// for the first `Ok` value as returned by the broadcast channel wrapper
/// stream.
fn create_active_providers_stream(
    current_value: Option<ActiveProviders>,
    sender: &broadcast::Sender<ActiveProviders>,
) -> ActiveProvidersSubscription {
    Box::pin(
        iter(current_value)
            .chain(BroadcastStream::new(sender.subscribe()).filter_map(|item| ready(item.ok()))),
    )
}
