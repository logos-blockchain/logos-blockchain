use serde::{Deserialize, Serialize};

use crate::{ZkHash, quota::Ed25519PublicKey};

/// Public inputs for all types of Proof of Quota. Spec: <https://www.notion.so/nomos-tech/Proof-of-Quota-Specification-215261aa09df81d88118ee22205cbafe?source=copy_link#25a261aa09df80ce943dce35dd5403ac>.
#[derive(Debug, Clone, Copy)]
pub struct Inputs {
    pub signing_key: Ed25519PublicKey,
    pub session: u64,
    pub core: CoreInputs,
    pub leader: LeaderInputs,
}

#[cfg(test)]
impl Default for Inputs {
    fn default() -> Self {
        use crate::quota::ED25519_PUBLIC_KEY_SIZE;

        Self {
            signing_key: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
            session: 1,
            core: CoreInputs::default(),
            leader: LeaderInputs::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub struct CoreInputs {
    #[serde(with = "groth16::serde::serde_fr")]
    pub zk_root: ZkHash,
    pub quota: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub struct LeaderInputs {
    #[serde(with = "groth16::serde::serde_fr")]
    pub pol_ledger_aged: ZkHash,
    #[serde(with = "groth16::serde::serde_fr")]
    pub pol_epoch_nonce: ZkHash,
    pub message_quota: u64,
    pub total_stake: u64,
}
