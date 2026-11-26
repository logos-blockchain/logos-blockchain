use std::time::Duration;

use overwatch::{
    overwatch::handle::OverwatchHandle,
    services::{
        AsServiceId, ServiceData,
        status::{ServiceStatus, WaitPolicy},
    },
};
use tokio::sync::oneshot;

use crate::RuntimeServiceId;

pub mod blend;
pub mod blocks;
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

pub type Handle = OverwatchHandle<RuntimeServiceId>;

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

const READY_TIMEOUT: Duration = Duration::from_millis(100);

/// Fails unless `Service` is ready, or gets there shortly, so that a request
/// to a service that is not running fails instead of waiting for it.
pub(crate) async fn ensure_ready<Service>(handle: &Handle) -> Result<(), Error>
where
    RuntimeServiceId: AsServiceId<Service>,
{
    let name = <RuntimeServiceId as AsServiceId<Service>>::SERVICE_ID;
    handle
        .status_watcher::<Service>()
        .await?
        .wait_with_policy(
            ServiceStatus::Ready,
            Some(READY_TIMEOUT),
            WaitPolicy::RestartFriendly,
        )
        .await
        .map(|_| ())
        .map_err(|error| Error::unavailable(format!("Service {name} is not ready: {error}")))
}
