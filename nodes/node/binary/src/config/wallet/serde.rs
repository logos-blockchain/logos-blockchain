use std::{collections::HashMap, path::PathBuf};

use lb_key_management_system_service::{backend::preload::KeyId, keys::ZkPublicKey};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub known_keys: HashMap<KeyId, ZkPublicKey>,
    pub voucher_master_key_id: KeyId,
    pub recovery_path: PathBuf,
}
