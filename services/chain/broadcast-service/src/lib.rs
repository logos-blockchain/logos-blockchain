use core::{fmt::Debug, pin::Pin};
use std::{collections::HashMap, fmt::Display};

use async_trait::async_trait;
use derivative::Derivative;
use futures::{Stream, StreamExt as _, future::ready, stream::iter};
use lb_core::{
    header::HeaderId,
    sdp::{ProviderId, ProviderInfo, SessionNumber},
};
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
use tracing::{error, info};

const BROADCAST_CHANNEL_SIZE: usize = 128;

pub type SessionSubscription = Pin<Box<dyn Stream<Item = SessionUpdate> + Send + Sync>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUpdate {
    pub session_number: SessionNumber,
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
    BroadcastBlendSession(SessionUpdate),
    BroadcastDASession(SessionUpdate),
    SubscribeToFinalizedBlocks {
        result_sender: oneshot::Sender<broadcast::Receiver<BlockInfo>>,
    },
    SubscribeBlendSession {
        #[derivative(Debug = "ignore")]
        result_sender: oneshot::Sender<SessionSubscription>,
    },
    SubscribeDASession {
        #[derivative(Debug = "ignore")]
        result_sender: oneshot::Sender<SessionSubscription>,
    },
}

pub struct BlockBroadcastService<RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    finalized_blocks: broadcast::Sender<BlockInfo>,
    blend_session: broadcast::Sender<SessionUpdate>,
    da_session: broadcast::Sender<SessionUpdate>,
    // For sending latest session on subscription.
    last_blend_session: Option<SessionUpdate>,
    last_da_session: Option<SessionUpdate>,
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
        let (blend_session, _) = broadcast::channel(BROADCAST_CHANNEL_SIZE);
        let (da_session, _) = broadcast::channel(BROADCAST_CHANNEL_SIZE);

        Ok(Self {
            service_resources_handle,
            finalized_blocks,
            blend_session,
            da_session,
            last_blend_session: None,
            last_da_session: None,
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
                    if let Err(err) = self.finalized_blocks.send(block) {
                        error!("Could not send to new blocks channel: {err}");
                    }
                }
                BlockBroadcastMsg::BroadcastBlendSession(session) => {
                    self.last_blend_session = Some(session.clone());
                    if let Err(err) = self.blend_session.send(session) {
                        error!("Could not send to new blocks channel: {err}");
                    }
                }
                BlockBroadcastMsg::BroadcastDASession(session) => {
                    self.last_da_session = Some(session.clone());
                    if let Err(err) = self.da_session.send(session) {
                        error!("Could not send to new blocks channel: {err}");
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
                BlockBroadcastMsg::SubscribeBlendSession { result_sender } => {
                    if result_sender
                        .send(create_session_stream(
                            self.last_blend_session.clone(),
                            &self.blend_session,
                        ))
                        .is_err()
                    {
                        error!("Could not subscribe to blend session channel.");
                    }
                }
                BlockBroadcastMsg::SubscribeDASession { result_sender } => {
                    if result_sender
                        .send(create_session_stream(
                            self.last_da_session.clone(),
                            &self.da_session,
                        ))
                        .is_err()
                    {
                        error!("Could not subscribe to DA session channel.");
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
fn create_session_stream(
    current_value: Option<SessionUpdate>,
    sender: &broadcast::Sender<SessionUpdate>,
) -> SessionSubscription {
    Box::pin(
        iter(current_value)
            .chain(BroadcastStream::new(sender.subscribe()).filter_map(|item| ready(item.ok()))),
    )
}
