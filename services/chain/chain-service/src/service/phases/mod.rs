mod awaiting_genesis_time;
mod following;
mod ibd;
mod pbp;

use core::fmt::Debug;

pub use following::Following;
pub use ibd::InitialBlockDownload;
use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode};
use lb_core::mantle::{
    ledger::verification_mode::StandardMode,
    traits::{PreverifiedMantleTransaction, SignedMantleTx, StorageSize},
    transactions::states::Preverified,
};
pub use pbp::ProlongedBootstrapPeriod;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use self::awaiting_genesis_time::AwaitingGenesisTime;
use super::Service;
use crate::Error;

/// A phase of the chain service.
pub trait Phase: Send + Sync + Debug {
    /// The tag reported by [`Query::Info`](crate::Query::Info) while this
    /// phase is running.
    const TAG: PhaseTag;
}

/// Phase tags
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum PhaseTag {
    /// The genesis time is in the future. Only read-only queries are served.
    AwaitingGenesisTime,
    /// Applying blocks while waiting for Initial Block Download to complete.
    InitialBlockDownload,
    /// The Prolonged Bootstrap Period is running.
    ProlongedBootstrapPeriod,
    /// Following the chain in real time
    Following,
}

/// Run the service through its phases in order.
pub async fn run_phases<Tx>(service: Service<AwaitingGenesisTime, Tx>) -> Result<(), Error>
where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = lb_core::mantle::transactions::TxDecodeContext>
        + Send
        + Sync
        + Unpin
        + 'static,
{
    service
        .process_awaiting_genesis_time()
        .await
        .process_initial_block_download()
        .await
        .process_prolonged_bootstrap_period()
        .await?
        .process_following()
        .await;

    Ok(())
}
