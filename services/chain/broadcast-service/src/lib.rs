use core::fmt::Debug;
use std::fmt::Display;

use async_trait::async_trait;
use derivative::Derivative;
use lb_core::header::HeaderId;
use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, oneshot};
use tracing::{error, info, trace};

const BROADCAST_CHANNEL_SIZE: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockInfo {
    pub height: u64,
    pub header_id: HeaderId,
}

#[derive(Derivative)]
#[derivative(Debug)]
pub enum BlockBroadcastMsg {
    BroadcastFinalizedBlock(BlockInfo),
    SubscribeToFinalizedBlocks {
        result_sender: oneshot::Sender<broadcast::Receiver<BlockInfo>>,
    },
}

pub struct BlockBroadcastService<RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    finalized_blocks: broadcast::Sender<BlockInfo>,
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

        Ok(Self {
            service_resources_handle,
            finalized_blocks,
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
                BlockBroadcastMsg::SubscribeToFinalizedBlocks { result_sender } => {
                    // TODO: This naively broadcast what was sent from the chain service. In case
                    // of LIB branch change (might happend during bootstrapping), blocks should be
                    // rebroadcasted from the last common header_id.
                    if let Err(err) = result_sender.send(self.finalized_blocks.subscribe()) {
                        error!("Could not subscribe to new blocks channel: {err:?}");
                    }
                }
            }
        }

        Ok(())
    }
}
