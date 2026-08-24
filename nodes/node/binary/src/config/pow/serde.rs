use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    /// Public key the mined `PoW` rewards are paid out to.
    pub claim_address: ZkPublicKey,
}

pub struct RequiredValues {
    pub claim_address: ZkPublicKey,
}

impl Config {
    #[must_use]
    pub const fn with_required_values(RequiredValues { claim_address }: RequiredValues) -> Self {
        Self { claim_address }
    }
}
