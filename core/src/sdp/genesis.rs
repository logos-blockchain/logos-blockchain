use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkPublicKey};
use serde::{Deserialize, Serialize};

use crate::{
    mantle::Value as NoteValue,
    sdp::{Locators, ServiceType},
};

/// Used to distribute Notes of `NoteValue` at genesis.
#[derive(Clone, Serialize, Deserialize)]
pub struct StakeHolderInfo {
    pub zk_id: ZkPublicKey,
    pub stake: NoteValue,
}

/// Used to register a service provider at genesis.
/// The `zk_id` must match the `StakeHolderInfo` entry so the locked note can
/// be identified.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub provider_id: Ed25519PublicKey,
    pub zk_id: ZkPublicKey,
    pub locators: Locators,
    pub service_type: ServiceType,
}
