use std::{ops::Deref, sync::Arc};

use lb_core::{era::ForkDigest, mantle::transactions::genesis_tx::ChainId};
use overwatch::{
    overwatch::handle::OverwatchHandle,
    services::{AsServiceId, ServiceData},
};
use tokio::sync::oneshot;

use crate::RuntimeServiceId;

pub mod blend;
pub mod blocks;
pub mod blocks_range;
pub mod channel;
pub mod cryptarchia;
pub mod leader;
pub mod mempool;
pub mod network;
pub mod node;
pub mod pow;
pub mod sdp;
pub mod time;
pub mod wallet;

pub use overwatch::DynError;

/// What every call works with: the node's services and the constants of the
/// deployment it runs.
#[derive(Clone)]
pub struct Handle {
    overwatch: OverwatchHandle<RuntimeServiceId>,
    chain_id: ChainId,
    /// The fork digest of every era of the deployment, era `n` at position
    /// `n`.
    fork_digests: Arc<[ForkDigest]>,
}

impl Handle {
    #[must_use]
    pub fn new(
        overwatch: OverwatchHandle<RuntimeServiceId>,
        chain_id: ChainId,
        fork_digests: Vec<ForkDigest>,
    ) -> Self {
        Self {
            overwatch,
            chain_id,
            fork_digests: fork_digests.into(),
        }
    }
}

impl Deref for Handle {
    type Target = OverwatchHandle<RuntimeServiceId>;

    fn deref(&self) -> &Self::Target {
        &self.overwatch
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error(transparent)]
    Unavailable(DynError),
    #[error(transparent)]
    Internal(DynError),
}

impl Error {
    pub fn internal(error: impl Into<DynError>) -> Self {
        Self::Internal(error.into())
    }

    pub fn unavailable(error: impl Into<DynError>) -> Self {
        Self::Unavailable(error.into())
    }
}

impl From<overwatch::overwatch::Error> for Error {
    fn from(error: overwatch::overwatch::Error) -> Self {
        Self::unavailable(error)
    }
}

impl From<oneshot::error::RecvError> for Error {
    fn from(error: oneshot::error::RecvError) -> Self {
        Self::unavailable(error)
    }
}

impl From<DynError> for Error {
    fn from(error: DynError) -> Self {
        Self::Internal(error)
    }
}

/// Sends `Service` the message built around a reply channel and waits for
/// the answer.
pub(crate) async fn request<Service, Reply>(
    handle: &Handle,
    message: impl FnOnce(oneshot::Sender<Reply>) -> Service::Message,
) -> Result<Reply, Error>
where
    Service: ServiceData,
    Service::Message: Send + 'static,
    RuntimeServiceId: AsServiceId<Service>,
{
    let relay = handle.relay::<Service>().await?;
    let (sender, receiver) = oneshot::channel();
    relay
        .send(message(sender))
        .await
        .map_err(|error| Error::unavailable(error.to_string()))?;
    Ok(receiver.await?)
}
