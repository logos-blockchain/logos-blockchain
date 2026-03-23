use lb_core::{mantle::Value, sdp::DeclarationId};
use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    /// Declaration ID (if set, full declaration info will be fetched from
    /// ledger on startup).
    #[serde(default)]
    pub declaration_id: Option<DeclarationId>,
    pub wallet: WalletConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WalletConfig {
    #[serde(default = "default_max_tx_fee")]
    pub max_tx_fee: Value,
    pub funding_pk: ZkPublicKey,
}

const fn default_max_tx_fee() -> Value {
    Value::MAX
}

pub struct RequiredValues {
    pub funding_pk: ZkPublicKey,
}

impl Config {
    #[must_use]
    pub const fn with_required_values(RequiredValues { funding_pk }: RequiredValues) -> Self {
        Self {
            wallet: WalletConfig {
                funding_pk,
                max_tx_fee: default_max_tx_fee(),
            },
            declaration_id: None,
        }
    }
}
