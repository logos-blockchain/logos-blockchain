use std::fmt::{Debug, Display};

use lb_core::sdp::{DeclarationId, DeclarationMessage};
use overwatch::{
    DynError,
    overwatch::OverwatchHandle,
    services::{
        AsServiceId, ServiceData,
        relay::{OutboundRelay, RelayError},
    },
};
use tokio::sync::{oneshot, oneshot::error::RecvError};

use crate::SdpMessage;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Failed to send a message to the SDP service: {0}")]
    RelaySend(#[from] RelayError),
    #[error("Failed to receive a message from the SDP service: {0}")]
    RelayReceive(#[from] RecvError),
    #[error(transparent)]
    Other(#[from] DynError),
}

pub struct SdpServiceApi<SdpService>
where
    SdpService: ServiceData,
{
    relay: OutboundRelay<SdpService::Message>,
}

impl<SdpService> SdpServiceApi<SdpService>
where
    SdpService: ServiceData<Message = SdpMessage>,
{
    #[must_use]
    pub const fn new(relay: OutboundRelay<SdpService::Message>) -> Self {
        Self { relay }
    }

    pub async fn from_overwatch_handle<RuntimeServiceId>(
        handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Self
    where
        RuntimeServiceId: AsServiceId<SdpService> + Debug + Display + Sync,
    {
        let relay = handle
            .relay::<SdpService>()
            .await
            .expect("Relay should be available after the service is started.");
        Self::new(relay)
    }

    pub async fn publish(&self, message: SdpMessage) -> Result<(), Error> {
        self.relay
            .send(message)
            .await
            .map_err(|(error, _)| Error::RelaySend(error))
    }

    pub async fn post_declaration(
        &self,
        declaration: DeclarationMessage,
    ) -> Result<DeclarationId, Error> {
        let (reply_channel, receiver) = oneshot::channel();
        let declaration = Box::new(declaration);
        self.publish(SdpMessage::PostDeclaration {
            declaration,
            reply_channel,
        })
        .await?;
        receiver.await?.map_err(Error::Other)
    }
}
