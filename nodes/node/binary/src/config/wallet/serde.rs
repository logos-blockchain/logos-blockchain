use std::collections::HashMap;

use lb_key_management_system_service::{backend::preload::KeyId, keys::ZkPublicKey};
use lb_wallet_service::default_pending_note_expiry_blocks;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    #[serde(default)]
    pub known_keys: HashMap<KeyId, ZkPublicKey>,
    pub voucher_master_key_id: KeyId,
    #[serde(default = "default_pending_note_expiry_blocks")]
    pub pending_note_expiry_blocks: u64,
}

pub struct RequiredValues {
    pub voucher_master_key_id: KeyId,
}

impl Config {
    #[must_use]
    pub fn with_required_values(
        RequiredValues {
            voucher_master_key_id,
        }: RequiredValues,
    ) -> Self {
        Self {
            known_keys: HashMap::new(),
            voucher_master_key_id,
            pending_note_expiry_blocks: default_pending_note_expiry_blocks(),
        }
    }
}
