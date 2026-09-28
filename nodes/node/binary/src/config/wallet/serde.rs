use lb_key_management_system_service::backend::preload::KeyId;
use lb_wallet_service::{
    default_funding_start_index, default_pending_note_expiry_blocks, hd::Index,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    /// The keys of the KMS that the wallet signs with. The notes of the ZK
    /// keys among them are tracked.
    #[serde(default)]
    pub known_keys: Vec<KeyId>,
    /// The first receive index that funding spends from. The receive
    /// addresses below it hold the stake, which funding never spends.
    #[serde(default = "default_funding_start_index")]
    pub funding_start_index: Index,
    #[serde(default = "default_pending_note_expiry_blocks")]
    pub pending_note_expiry_blocks: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            known_keys: Vec::new(),
            funding_start_index: default_funding_start_index(),
            pending_note_expiry_blocks: default_pending_note_expiry_blocks(),
        }
    }
}
