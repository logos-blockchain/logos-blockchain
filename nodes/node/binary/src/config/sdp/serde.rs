#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde conditional serialization skip requires a specific function signature."
)]

use lb_core::{
    mantle::{NoteId, Value},
    sdp::DeclarationId,
};
use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

use crate::config::utils;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub wallet: WalletConfig,

    #[serde(default)]
    #[serde(skip_serializing_if = "utils::is_default")]
    pub declaration: Option<Declaration>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct WalletConfig {
    pub funding_pk: ZkPublicKey,

    #[serde(default = "default_max_tx_fee")]
    #[serde(skip_serializing_if = "is_default_max_tx_fee")]
    pub max_tx_fee: Value,
}

const fn default_max_tx_fee() -> Value {
    Value::MAX
}

const fn is_default_max_tx_fee(value: &Value) -> bool {
    *value == default_max_tx_fee()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub id: DeclarationId,
    pub zk_id: ZkPublicKey,
    pub locked_note_id: NoteId,
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
