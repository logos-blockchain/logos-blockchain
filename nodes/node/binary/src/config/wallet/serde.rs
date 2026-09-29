use std::collections::HashSet;

use lb_key_management_system_keys::hd::HardenedIndex;
use lb_key_management_system_service::backend::preload::KeyId;
use lb_wallet_service::default_pending_note_expiry_blocks;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Config {
    /// The keys in the KMS that the wallet signs with.
    pub known_keys: HashSet<KeyId>,
    /// The first receive index that funding spends from.
    /// The wallet never spends from index below this value.
    pub funding_start_index: HardenedIndex,
    pub pending_note_expiry_blocks: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            known_keys: HashSet::new(),
            funding_start_index: HardenedIndex::new(1u32.try_into().expect("must be u31")),
            pending_note_expiry_blocks: default_pending_note_expiry_blocks(),
        }
    }
}
