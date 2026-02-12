use lb_core::{
    mantle::{NoteId, Value},
    sdp::DeclarationId,
};
use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    #[serde(default)]
    pub declaration: Option<Declaration>,
    pub wallet: WalletConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Declaration {
    pub id: DeclarationId,
    pub zk_id: ZkPublicKey,
    pub locked_note_id: NoteId,
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
            declaration: None,
        }
    }
}
