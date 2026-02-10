use std::{collections::HashMap, path::PathBuf};

use lb_key_management_system_service::{backend::preload::KeyId, keys::ZkPublicKey};
use lb_wallet_service::WalletServiceSettings;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub known_keys: HashMap<KeyId, ZkPublicKey>,
    pub voucher_master_key_id: KeyId,
    pub recovery_path: PathBuf,
}

impl From<Config> for WalletServiceSettings {
    fn from(value: Config) -> Self {
        Self {
            known_keys: value.known_keys,
            voucher_master_key_id: value.voucher_master_key_id,
            recovery_path: value.recovery_path,
        }
    }
}

impl From<WalletServiceSettings> for Config {
    fn from(value: WalletServiceSettings) -> Self {
        Self {
            known_keys: value.known_keys,
            voucher_master_key_id: value.voucher_master_key_id,
            recovery_path: value.recovery_path,
        }
    }
}
