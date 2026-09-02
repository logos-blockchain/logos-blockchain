use lb_pow_service::{AutoClaimSettings, PoWMiningSettings};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Config {
    /// Tuning for the CPU-heavy ticket search (thread pool and per-block
    /// concurrency). Optional: omitting it keeps the defaults.
    #[serde(default)]
    pub mining: PoWMiningSettings,
    /// Unattended claiming: the keys to pay, the balance each should reach,
    /// and how often to try. Optional: omitting it leaves auto-claim off, so
    /// rewards are only claimed through the `PoW` claim endpoint.
    #[serde(default)]
    pub auto_claim: AutoClaimSettings,
}
