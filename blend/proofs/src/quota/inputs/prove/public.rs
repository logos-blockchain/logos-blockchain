use core::fmt::{self, Debug, Formatter};

use lb_groth16::{Fr, fr_to_bytes};
use serde::{Deserialize, Serialize};

use crate::{ZkHash, quota::Ed25519PublicKey};

/// Public inputs for all types of Proof of Quota. Spec: <https://www.notion.so/nomos-tech/Proof-of-Quota-Specification-215261aa09df81d88118ee22205cbafe?source=copy_link#25a261aa09df80ce943dce35dd5403ac>.
#[derive(Clone, Copy)]
pub struct Inputs {
    pub signing_key: Ed25519PublicKey,
    pub session: u64,
    pub core: CoreInputs,
    pub leader: LeaderInputs,
}

impl Debug for Inputs {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inputs")
            .field("signing_key", &hex::encode(self.signing_key.as_bytes()))
            .field("session", &self.session)
            .field("core", &self.core)
            .field("leader", &self.leader)
            .finish()
    }
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

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub struct CoreInputs {
    #[serde(with = "lb_groth16::serde::serde_fr")]
    pub zk_root: ZkHash,
    pub quota: u64,
}

impl Debug for CoreInputs {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreInputs")
            .field("zk_root", &hex::encode(fr_to_bytes(&self.zk_root)))
            .field("quota", &self.quota)
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub struct LeaderInputs {
    #[serde(with = "lb_groth16::serde::serde_fr")]
    pub pol_ledger_aged: ZkHash,
    #[serde(with = "lb_groth16::serde::serde_fr")]
    pub pol_epoch_nonce: ZkHash,
    pub message_quota: u64,
    #[serde(with = "lb_groth16::serde::serde_fr")]
    pub lottery_0: Fr,
    #[serde(with = "lb_groth16::serde::serde_fr")]
    pub lottery_1: Fr,
}

impl Debug for LeaderInputs {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaderInputs")
            .field(
                "pol_ledger_aged",
                &hex::encode(fr_to_bytes(&self.pol_ledger_aged)),
            )
            .field(
                "pol_epoch_nonce",
                &hex::encode(fr_to_bytes(&self.pol_epoch_nonce)),
            )
            .field("message_quota", &self.message_quota)
            .field("lottery_0", &hex::encode(fr_to_bytes(&self.lottery_0)))
            .field("lottery_1", &hex::encode(fr_to_bytes(&self.lottery_1)))
            .finish()
    }
}
