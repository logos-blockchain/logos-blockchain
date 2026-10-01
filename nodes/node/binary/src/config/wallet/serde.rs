use std::collections::{HashMap, HashSet};

use lb_key_management_system_service::{backend::preload, keys::ZkPublicKey};
use lb_wallet_service::default_pending_note_expiry_blocks;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Config {
    /// The ZK keys preloaded in the KMS whose notes the wallet tracks, by
    /// their id in the KMS. The HD keys are tracked without being listed.
    pub static_keys: HashMap<preload::KeyId, ZkPublicKey>,
    /// The keys whose notes are not spent unless the keys to fund from are
    /// named, e.g. the keys that hold the stake to preserve aging.
    pub unspendable_keys: HashSet<ZkPublicKey>,
    pub pending_note_expiry_blocks: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            static_keys: HashMap::new(),
            unspendable_keys: HashSet::new(),
            pending_note_expiry_blocks: default_pending_note_expiry_blocks(),
        }
    }
}
