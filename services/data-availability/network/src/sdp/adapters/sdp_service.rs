use std::{
    fmt::{Debug, Display},
    marker::PhantomData,
};

use async_trait::async_trait;
use lb_sdp_service::{SdpMessage, SdpService, adapters::mempool::SdpMempoolAdapter};
use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, relay::OutboundRelay},
};

use crate::{
    opinion_aggregator::Opinions,
    sdp::{SdpAdapter, SdpAdapterError},
};

pub struct SdpServiceAdapter<MempoolAdapter, RuntimeServiceId> {
    relay: OutboundRelay<SdpMessage>,
    _phantom: PhantomData<(RuntimeServiceId, MempoolAdapter)>,
}

#[async_trait]
impl<MempoolAdapter, RuntimeServiceId> SdpAdapter<RuntimeServiceId>
    for SdpServiceAdapter<MempoolAdapter, RuntimeServiceId>
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    RuntimeServiceId: AsServiceId<MempoolAdapter::MempoolService>
        + AsServiceId<SdpService<MempoolAdapter, RuntimeServiceId>>
        + Send
        + Sync
        + Debug
        + Display
        + 'static,
{
    async fn new(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, SdpAdapterError> {
        let relay = overwatch_handle
            .relay::<SdpService<MempoolAdapter, RuntimeServiceId>>()
            .await
            .map_err(|e| SdpAdapterError::Other(Box::new(e)))?;

        Ok(Self {
            relay,
            _phantom: PhantomData,
        })
    }

    async fn post_activity(&self, opinions: Opinions) -> Result<(), SdpAdapterError> {
        let metadata = opinions.into();
        self.relay
            .send(SdpMessage::PostActivity { metadata })
            .await
            .map_err(|(e, _)| SdpAdapterError::Other(Box::new(e)))?;

        Ok(())
    }
}
