use lb_key_management_system_service::backend::preload::KeyId;
use lb_libp2p::Multiaddr;
use serde::{Deserialize, Serialize};

use crate::config::blend::serde::{
    core::{BackendConfig, Config as CoreConfig, ZkSettings},
    edge::Config as EdgeConfig,
};

pub mod core;
pub mod edge;

/// Config object that is part of the global config file.
///
/// This includes all values that are not strictly related to any specific
/// deployment and that users have to specify when starting up the node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// The non-ephemeral signing key (NSK) ID corresponding to the public key
    /// registered in the membership (SDP).
    pub non_ephemeral_signing_key_id: KeyId,
    pub core: CoreConfig,
    #[serde(default)]
    pub edge: EdgeConfig,
}

pub struct RequiredValues {
    pub non_ephemeral_signing_key_id: KeyId,
    pub secret_key_kms_id: KeyId,
}

impl Config {
    #[must_use]
    pub fn with_required_values(
        RequiredValues {
            non_ephemeral_signing_key_id,
            secret_key_kms_id,
        }: RequiredValues,
    ) -> Self {
        Self {
            non_ephemeral_signing_key_id,
            core: CoreConfig {
                zk: ZkSettings { secret_key_kms_id },
                backend: BackendConfig::default(),
            },
            edge: EdgeConfig::default(),
        }
    }
    pub fn set_listening_address(&mut self, addr: Multiaddr) {
        self.core.backend.listening_address = addr;
    }
}
