use lb_core::mantle::transactions::genesis_tx::ChainId;
use serde::{Deserialize, Serialize};

/// The chain this node runs on.
///
/// The chain ID is fixed by the node's deployment settings, so this body is
/// constant for the lifetime of the process.
#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChainIdResponseBody {
    /// UTF-8 string of 1 to 255 bytes.
    #[schema(value_type = String, min_length = 1, max_length = 255)]
    pub chain_id: ChainId,
}
