use lb_core::mantle::transactions::hash::TxHash;
use lb_key_management_system_keys::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Where a claim request pays the rewards.
///
/// The whole body is optional, and so is the key inside it: both omitted mean
/// "use the node's auto-claim target".
#[derive(Serialize, Deserialize, Default, ToSchema)]
pub struct PoWClaimRequestBody {
    #[serde(default)]
    pub claim_address: Option<ZkPublicKey>,
}

/// The id of the reward-claim transaction submitted by a claim request, or
/// `null` when there were no rewards to claim.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct PoWClaimResponseBody {
    pub tx_hash: Option<TxHash>,
}
