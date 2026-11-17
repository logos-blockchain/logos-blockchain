use lb_core::{era::ForkDigest, mantle::transactions::genesis_tx::ChainId};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The chain this node runs on.
///
/// The chain ID is fixed by the node's deployment settings, so this body is
/// constant for the lifetime of the process.
#[derive(Serialize, Deserialize)]
pub struct ChainIdResponseBody {
    pub chain_id: ChainId,
}

/// The fork the node's clock is in: the fork digest a transaction signed now
/// carries.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ChainForkResponseBody {
    pub fork_digest: ForkDigest,
}
