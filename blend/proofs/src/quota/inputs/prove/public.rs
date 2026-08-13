use core::fmt::{self, Debug, Formatter};

use lb_groth16::{Field as _, Fr, fr_to_bytes};
use serde::{Deserialize, Serialize};

use crate::{
    ZkHash,
    quota::{Ed25519PublicKey, Quota},
};

/// Public inputs for all types of Proof of Quota. Spec: <https://lip.logos.co/blockchain/raw/proof-of-quota.html#public-values>.
#[derive(Clone, Copy)]
pub struct Inputs {
    pub signing_key: Ed25519PublicKey,
    pub core: CoreInputs,
    pub leader: LeaderInputs,
    pub pow: PowInputs,
}

impl Debug for Inputs {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inputs")
            .field("signing_key", &hex::encode(self.signing_key.as_bytes()))
            .field("core", &self.core)
            .field("leader", &self.leader)
            .field("pow", &self.pow)
            .finish()
    }
}

#[cfg(test)]
impl Default for Inputs {
    fn default() -> Self {
        use crate::quota::ED25519_PUBLIC_KEY_SIZE;

        Self {
            signing_key: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
            core: CoreInputs::default(),
            leader: LeaderInputs::default(),
            pow: PowInputs::default(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub struct CoreInputs {
    #[serde(with = "lb_groth16::serde::serde_fr")]
    pub zk_root: ZkHash,
    pub quota: Quota,
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
    pub message_quota: Quota,
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

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub struct PowInputs {
    #[serde(with = "lb_groth16::serde::serde_fr")]
    pub pow_blend_difficulty: ZkHash,
    pub pow_quota: Quota,
}

impl PowInputs {
    /// Placeholder inputs for the `PoW` quota parameters, which are not plumbed
    /// through from the chain yet.
    ///
    /// These are public inputs, so a prover and the verifier checking its
    /// proofs must use identical values. Every call site shares this single
    /// definition so they cannot drift apart.
    ///
    /// Both values are deliberately as permissive as the circuit allows, so
    /// that the branch is usable before the chain supplies its parameters:
    ///
    /// - the difficulty is `p - 1`, the largest field element, so every ticket
    ///   but one satisfies it and a solution costs a single hash to find;
    /// - the quota is the widest the circuit's 20-bit index signal admits, so
    ///   it never binds before the chain-sourced value does. The spec sets `Q_W
    ///   = ß_max`, one message per solution, which is a per-deployment value
    ///   this constant cannot see.
    // TODO: Remove once the PoW quota parameters are sourced from the chain.
    #[must_use]
    pub fn unwired_placeholder() -> Self {
        Self {
            pow_blend_difficulty: -ZkHash::ONE,
            pow_quota: Quota::new::<{ Quota::MAX }>(),
        }
    }
}

impl Debug for PowInputs {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("PowInputs")
            .field(
                "pow_blend_difficulty",
                &hex::encode(fr_to_bytes(&self.pow_blend_difficulty)),
            )
            .field("pow_quota", &self.pow_quota)
            .finish()
    }
}
