use std::sync::LazyLock;

use lb_groth16::{fr_from_bytes, fr_to_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_poseidon2::{Fr, ZkHash};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    crypto::ZkHasher,
    mantle::{
        Note, TxHash, Utxo, Value,
        encoding::encode_leader_claim,
        ledger::{Operation, Utxos},
        ops::OpId,
    },
    proofs::leader_claim_proof::{
        Groth16LeaderClaimProof, LeaderClaimProof as _, LeaderClaimPublic,
    },
};

static REWARD_VOUCHER: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"REWARD_VOUCHER").expect("BigUint should load from constant string")
});

static VOUCHER_NF: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"VOUCHER_NF").expect("BigUint should load from constant string")
});

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize)]
pub struct RewardsRoot(#[serde(with = "serde_fr")] ZkHash);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct VoucherSecret(#[serde(with = "serde_fr")] pub Fr);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct VoucherNullifier(#[serde(with = "serde_fr")] ZkHash);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize)]
pub struct VoucherCm(#[serde(with = "serde_fr")] ZkHash);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct LeaderClaimOp {
    pub rewards_root: RewardsRoot,
    pub voucher_nullifier: VoucherNullifier,
    pub pk: ZkPublicKey,
}

impl LeaderClaimOp {
    #[must_use]
    pub fn utxo(&self, amount: Value) -> Utxo {
        Utxo {
            op_id: self.op_id(),
            output_index: 0,
            note: Note {
                value: amount,
                pk: self.pk,
            },
        }
    }
}

impl OpId for LeaderClaimOp {
    fn op_bytes(&self) -> Vec<u8> {
        encode_leader_claim(self)
    }
}

impl From<Fr> for VoucherSecret {
    fn from(value: Fr) -> Self {
        Self(value)
    }
}

impl From<VoucherSecret> for Fr {
    fn from(value: VoucherSecret) -> Self {
        value.0
    }
}

impl AsRef<Fr> for VoucherCm {
    fn as_ref(&self) -> &Fr {
        &self.0
    }
}

impl From<Fr> for VoucherCm {
    fn from(value: Fr) -> Self {
        Self(value)
    }
}

impl From<Fr> for RewardsRoot {
    fn from(value: Fr) -> Self {
        Self(value)
    }
}

impl From<Fr> for VoucherNullifier {
    fn from(value: Fr) -> Self {
        Self(value)
    }
}

impl From<RewardsRoot> for Fr {
    fn from(value: RewardsRoot) -> Self {
        value.0
    }
}

impl From<VoucherNullifier> for Fr {
    fn from(value: VoucherNullifier) -> Self {
        value.0
    }
}

impl VoucherNullifier {
    #[must_use]
    pub fn from_secret(voucher_secret: VoucherSecret) -> Self {
        let mut hash = ZkHasher::new();
        hash.compress(&[*VOUCHER_NF, voucher_secret.into()]);
        hash.finalize().into()
    }
}

impl From<VoucherCm> for Fr {
    fn from(value: VoucherCm) -> Self {
        value.0
    }
}

impl VoucherCm {
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        fr_to_bytes(&self.0)
    }

    #[must_use]
    pub fn from_secret(voucher_secret: VoucherSecret) -> Self {
        let mut hash = ZkHasher::new();
        hash.compress(&[*REWARD_VOUCHER, voucher_secret.into()]);
        hash.finalize().into()
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LeaderClaimError {
    #[error("voucher nullifier already used")]
    DuplicatedVoucherNullifier,
    #[error("vouchers merkle root mismatch")]
    VouchersRootMismatch,
    #[error("Invalid Proof of Claim")]
    InvalidPoC,
}

pub struct LeaderClaimValidationContext<'a> {
    pub nullifiers: &'a rpds::HashTrieSetSync<VoucherNullifier>,
    pub claimable_vouchers_root: &'a RewardsRoot,
    pub proof_of_claim: &'a Groth16LeaderClaimProof,
    pub tx_hash: &'a TxHash,
}

pub struct LeaderClaimExecutionContext {
    pub nullifiers: rpds::HashTrieSetSync<VoucherNullifier>,
    pub reward_amount: Value,
    pub claimable_rewards: Value,
    pub utxos: Utxos,
}

impl Operation for LeaderClaimOp {
    type ValidationContext<'a>
        = LeaderClaimValidationContext<'a>
    where
        Self: 'a;
    type ExecutionContext<'a>
        = LeaderClaimExecutionContext
    where
        Self: 'a;
    type Error = LeaderClaimError;

    fn validate(&self, ctx: &Self::ValidationContext<'_>) -> Result<(), Self::Error> {
        // Check that the nullifier isn't in the set
        if ctx.nullifiers.contains(&self.voucher_nullifier) {
            return Err(LeaderClaimError::DuplicatedVoucherNullifier);
        }

        // Check that the voucher root is the same as in the ledger
        if ctx.claimable_vouchers_root != &self.rewards_root {
            return Err(LeaderClaimError::VouchersRootMismatch);
        }

        // Check the proof of claim
        if !ctx.proof_of_claim.verify(&LeaderClaimPublic {
            voucher_root: ctx.claimable_vouchers_root.0,
            mantle_tx_hash: ctx.tx_hash.0,
        }) {
            return Err(LeaderClaimError::InvalidPoC);
        }

        Ok(())
    }

    fn execute(
        &self,
        mut ctx: Self::ExecutionContext<'_>,
    ) -> Result<Self::ExecutionContext<'_>, Self::Error> {
        // Add the nullifier to the nullifier set
        ctx.nullifiers = ctx.nullifiers.insert(self.voucher_nullifier);

        // Distribute the reward
        let utxo = self.utxo(ctx.reward_amount);
        ctx.utxos = ctx.utxos.insert(utxo.id(), utxo).0;

        // Remove the distributed rewards from the pool
        ctx.claimable_rewards -= ctx.reward_amount;

        Ok(ctx)
    }
}

#[cfg(test)]
mod tests {
    use lb_mmr::MerkleMountainRange;

    use super::*;
    use crate::proofs::leader_claim_proof::LeaderClaimPrivate;

    #[test]
    fn validate_accepts_valid_proof_of_claim() {
        let voucher_secret = VoucherSecret::from(Fr::from(7u64));
        let voucher_cm = VoucherCm::from_secret(voucher_secret);
        let (mmr, voucher_path) = MerkleMountainRange::<VoucherCm, ZkHasher>::new()
            .push_with_paths(voucher_cm, &mut [])
            .expect("MMR shouldn't be full");
        let voucher_root = RewardsRoot::from(mmr.frontier_root());
        let tx_hash = TxHash::from(Fr::from(11u64));
        let proof = Groth16LeaderClaimProof::prove(LeaderClaimPrivate::new(
            LeaderClaimPublic::new(voucher_root.into(), tx_hash.0),
            &voucher_path,
            voucher_secret,
        ))
        .expect("proof generation should succeed");
        let op = LeaderClaimOp {
            rewards_root: voucher_root,
            voucher_nullifier: VoucherNullifier::from_secret(voucher_secret),
            pk: ZkPublicKey::zero(),
        };
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let ctx = LeaderClaimValidationContext {
            nullifiers: &nullifiers,
            claimable_vouchers_root: &voucher_root,
            proof_of_claim: &proof,
            tx_hash: &tx_hash,
        };

        assert_eq!(op.validate(&ctx), Ok(()));
    }
}
