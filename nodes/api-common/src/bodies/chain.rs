use lb_core::mantle::transactions::genesis_tx::ChainId;
use serde::{Deserialize, Serialize};

/// The chain this node runs on.
///
/// The chain ID is fixed by the node's deployment settings, so this body is
/// constant for the lifetime of the process.
#[derive(Serialize, Deserialize)]
pub struct ChainIdResponseBody {
    pub chain_id: ChainId,
}
