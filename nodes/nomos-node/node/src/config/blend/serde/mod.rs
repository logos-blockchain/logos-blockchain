use std::path::PathBuf;

use derivative::Derivative;
use key_management_system_service::keys::UnsecuredEd25519Key;
use nomos_libp2p::Multiaddr;
use serde::{Deserialize, Serialize};

use crate::config::blend::serde::{core::Config as CoreConfig, edge::Config as EdgeConfig};

pub mod core;
pub mod edge;

/// Config object that is part of the global config file.
///
/// This includes all values that are not strictly related to any specific
/// deployment and that users have to specify when starting up the node.
#[derive(Clone, Derivative, Serialize, Deserialize)]
#[derivative(Debug)]
pub struct Config {
    /// The non-ephemeral signing key (NSK) corresponding to the public key
    /// registered in the membership (SDP).
    #[derivative(Debug = "ignore")]
    pub non_ephemeral_signing_key: UnsecuredEd25519Key,
    pub recovery_path_prefix: PathBuf,
    pub core: CoreConfig,
    pub edge: EdgeConfig,
}

impl Config {
    pub fn set_listening_address(&mut self, addr: Multiaddr) {
        self.core.backend.listening_address = addr;
    }
}
