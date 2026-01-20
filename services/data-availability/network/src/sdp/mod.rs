pub mod adapters;

use async_trait::async_trait;
use overwatch::{DynError, overwatch::OverwatchHandle};
use thiserror::Error;

use crate::opinion_aggregator::Opinions;

#[derive(Debug, Error)]
pub enum SdpAdapterError {
    #[error(transparent)]
    Other(#[from] DynError),
}

#[async_trait]
pub trait SdpAdapter<RuntimeServiceId>: Sized {
    async fn new(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, SdpAdapterError>;
    async fn post_activity(&self, activity_proof: Opinions) -> Result<(), SdpAdapterError>;
}
