use lb_key_management_system_service::keys::ZkPublicKey;
use lb_pow_service::PoWMiningSettings;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    /// Public key the mined `PoW` rewards are paid out to.
    pub claim_address: ZkPublicKey,
    /// Tuning for the CPU-heavy ticket search (thread pool and per-block
    /// concurrency). Optional: omitting it keeps the defaults.
    #[serde(default)]
    pub mining: PoWMiningSettings,
}

pub struct RequiredValues {
    pub claim_address: ZkPublicKey,
}

impl Config {
    #[must_use]
    pub fn with_required_values(RequiredValues { claim_address }: RequiredValues) -> Self {
        Self {
            claim_address,
            mining: PoWMiningSettings::default(),
        }
    }
}
