use lb_core::mantle::{Value, gas::GasCost};
use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub wallet: WalletConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletConfig {
    // Cap on a transaction fee of the leader. The leader submits no transaction,
    // so nothing reads it.
    #[serde(default = "default_max_tx_fee")]
    pub max_tx_fee: GasCost,

    // The key the leader rewards are paid to. It must be in `wallet.known_keys`,
    // or the node refuses to start.
    pub funding_pk: ZkPublicKey,
}

#[must_use]
pub const fn default_max_tx_fee() -> GasCost {
    GasCost::new(Value::MAX)
}
