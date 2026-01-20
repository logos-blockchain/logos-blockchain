pub mod service;

use std::pin::Pin;

use futures::Stream;
use lb_core::sdp::SessionNumber;
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};
use thiserror::Error;

pub type SessionStream = Pin<Box<dyn Stream<Item = SessionNumber> + Send + Sync + 'static>>;

#[derive(Error, Debug)]
pub enum SessionAdapterError {
    #[error("Channel error: {0}")]
    Channel(#[from] DynError),
}

#[async_trait::async_trait]
pub trait SessionAdapter {
    type Service: ServiceData;

    fn new(relay: OutboundRelay<<Self::Service as ServiceData>::Message>) -> Self;

    async fn subscribe(&self) -> Result<SessionStream, SessionAdapterError>;
}
