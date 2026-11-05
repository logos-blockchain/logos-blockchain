pub mod config;
mod libp2p;
pub use libp2p::messages::BlocksRequest;
mod messages;
pub use messages::{SerialisedBlock, TipResponse, TipResponseFailureReason};

pub type DynError = Box<dyn std::error::Error + Send + Sync + 'static>;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
pub enum BlocksUnavailableReason {
    #[error("Block not found ({0:?})")]
    BlockNotFound(HeaderId),
    #[error("Start block not found")]
    StartBlockNotFound,
    #[error("Unknown error")]
    Unknown,
}

#[derive(Debug, Clone)]
pub enum ProviderResponse<Response, Reason = String> {
    Available(Response),
    Unavailable { reason: Reason },
}

pub type TipProviderResponse = ProviderResponse<TipResponse, TipResponseFailureReason>;

pub type BlocksProviderResponse = ProviderResponse<
    BoxStream<'static, Result<SerialisedBlock, DynError>>,
    BlocksUnavailableReason,
>;

pub use config::Config;
use futures::stream::BoxStream;
pub use lb_core::header::HeaderId;
pub use libp2p::{
    behaviour::{Behaviour, BoxedStream, Event},
    errors::{ChainSyncError, ChainSyncErrorKind},
};
