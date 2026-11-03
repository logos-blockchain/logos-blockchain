use lb_binary_codec::canonical::BinaryCodec;
use lb_key_management_system_keys::keys::Ed25519Signature;
use serde::{Deserialize, Serialize};

use crate::mantle::ops::ZkTransferProof;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BinaryCodec)]
pub struct ZkAndEd25519Proof {
    // ZkTransferAndEd25519SigProof = ZkTransfer Ed25519Signature
    pub zk_proof: ZkTransferProof,
    pub ed25519_sig: Ed25519Signature,
}

impl ZkAndEd25519Proof {
    #[must_use]
    pub const fn new(zk_proof: ZkTransferProof, ed25519_sig: Ed25519Signature) -> Self {
        Self {
            zk_proof,
            ed25519_sig,
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl crate::mantle::ops::op_proof::samples::SampleProof for ZkAndEd25519Proof {
    fn sample() -> Self {
        Self::new(ZkTransferProof::sample(), Ed25519Signature::sample())
    }
}
