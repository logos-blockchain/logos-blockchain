use lb_codec::BinaryCodec;
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BinaryCodec)]
pub struct ZkAndEd25519Proof {
    pub zk_sig: ZkSignature,
    pub ed25519_sig: Ed25519Signature,
}

impl ZkAndEd25519Proof {
    #[must_use]
    pub const fn new(zk_sig: ZkSignature, ed25519_sig: Ed25519Signature) -> Self {
        Self {
            zk_sig,
            ed25519_sig,
        }
    }
}
