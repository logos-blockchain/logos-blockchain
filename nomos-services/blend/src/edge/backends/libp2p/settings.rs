use core::num::NonZeroU64;

use libp2p::identity::Keypair;
use nomos_libp2p::protocol_name::StreamProtocol;
use serde::{Deserialize, Serialize};

use crate::edge::settings::BlendConfig;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde_with::serde_as]
pub struct Libp2pBlendBackendSettings {
    pub max_dial_attempts_per_peer_per_message: NonZeroU64,
    pub protocol_name: StreamProtocol,
    // $\Phi_{EC}$: the minimum number of connections that the edge node establishes with
    // core nodes to send a single message that needs to be blended.
    pub replication_factor: NonZeroU64,
}

impl BlendConfig<Libp2pBlendBackendSettings> {
    #[must_use]
    pub fn keypair(&self) -> Keypair {
        let mut secret_key_bytes = *self.crypto.non_ephemeral_signing_key.as_bytes();
        Keypair::ed25519_from_bytes(&mut secret_key_bytes)
            .expect("Cryptographic secret key should be a valid Ed25519 private key.")
    }
}
