use std::{collections::HashMap, path::PathBuf};

use lb_key_management_system_service::{backend::preload::KeyId, keys::ZkPublicKey};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    #[serde(default)]
    pub known_keys: HashMap<KeyId, ZkPublicKey>,
    pub voucher_master_key_id: KeyId,
    pub recovery_path: PathBuf,
}

const fn default_recovery_path() -> PathBuf {
    PathBuf::from("./wallet_recovery.json")
}
