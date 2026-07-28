use lb_core_macros::NomCodec;
use lb_groth16::{fr_from_mod_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use nom::AsBytes as _;
use serde::{Deserialize, Serialize};

use crate::crypto::{Hash, ZkDigest as _, ZkHash, ZkHasher};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, NomCodec)]
pub struct ClaimPowRewardOp {
    #[serde(with = "serde_fr")]
    pub epoch_nonce: ZkHash,
    pub block_hash: Hash,
    pub public_key: ZkPublicKey,
}

impl ClaimPowRewardOp {
    #[must_use]
    pub fn get_puzzle_ticket(&self) -> ZkHash {
        ZkHasher::digest(&[
            self.epoch_nonce,
            fr_from_mod_bytes(self.block_hash.as_bytes()),
            *self.public_key.as_fr(),
        ])
    }
}
