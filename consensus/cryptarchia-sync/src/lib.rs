pub mod config;
#[cfg(feature = "libp2p")]
mod libp2p;
#[cfg(feature = "libp2p")]
pub use libp2p::messages::DownloadBlocksRequest;
mod messages;
pub use messages::{GetTipResponse, SerialisedBlock};

pub type DynError = Box<dyn std::error::Error + Send + Sync + 'static>;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BlocksUnavailableReason {
    BlockNotFound(HeaderId),
    StartBlockNotFound,
    Unknown(String),
}

impl std::fmt::Display for BlocksUnavailableReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BlockNotFound(id) => write!(f, "BlockNotFound({id:?})"),
            Self::StartBlockNotFound => write!(f, "StartBlockNotFound"),
            Self::Unknown(reason) => f.write_str(reason),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProviderResponse<Response, Reason = String> {
    Available(Response),
    Unavailable { reason: Reason },
}
pub type TipResponse = ProviderResponse<GetTipResponse>;

pub type BlocksResponse = ProviderResponse<
    BoxStream<'static, Result<SerialisedBlock, DynError>>,
    BlocksUnavailableReason,
>;

pub use config::Config;
use futures::stream::BoxStream;
pub use lb_core::header::HeaderId;
#[cfg(feature = "libp2p")]
pub use libp2p::{
    behaviour::{Behaviour, BoxedStream, Event},
    errors::{ChainSyncError, ChainSyncErrorKind},
};
