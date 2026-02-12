use std::{collections::HashMap, path::PathBuf};

use lb_key_management_system_service::{backend::preload::KeyId, keys::ZkPublicKey};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    #[serde(default)]
    pub known_keys: HashMap<KeyId, ZkPublicKey>,
    pub voucher_master_key_id: KeyId,
    #[serde(default = "default_recovery_path")]
    pub recovery_path: PathBuf,
}

fn default_recovery_path() -> PathBuf {
    "./wallet_recovery.json".into()
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
            recovery_path: default_recovery_path(),
        }
    }
}
