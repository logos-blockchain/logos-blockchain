use std::sync::LazyLock;

use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_groth16::{fr_from_bytes, fr_to_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_poseidon2::{Digest, Fr, ZkHash};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    crypto::ZkHasher,
    events::{TxEvent, TxEventPayload},
    mantle::{
        Note, Utxo, Value,
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, Utxos,
            VerifiableOperation, verification_mode, verification_mode::VerificationMode,
        },
        ops::{OpId, SignedOp},
        transactions::{
            hash::{TxHash, TxHashView},
            states::VerificationState,
        },
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize, BinaryCodec)]
pub struct RewardsRoot(#[serde(with = "serde_fr")] ZkHash);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct VoucherSecret(#[serde(with = "serde_fr")] pub Fr);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct VoucherNullifier(#[serde(with = "serde_fr")] ZkHash);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize, BinaryCodec)]
pub struct VoucherCm(#[serde(with = "serde_fr")] ZkHash);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
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
        self.encode_to_vec()
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
        Self(<ZkHasher as Digest>::compress(&[
            *VOUCHER_NF,
            voucher_secret.into(),
        ]))
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
        Self(<ZkHasher as Digest>::compress(&[
            *REWARD_VOUCHER,
            voucher_secret.into(),
        ]))
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

pub struct LeaderClaimPreverificationContext<'a> {
    pub tx_hash_view: &'a TxHashView,
}

pub struct LeaderClaimVerificationContext<'a> {
    pub nullifiers: &'a rpds::HashTrieSetSync<VoucherNullifier>,
    pub claimable_vouchers_root: &'a RewardsRoot,
    pub tx_hash_view: &'a TxHashView,
}

pub struct LeaderClaimExecutionContext {
    pub nullifiers: rpds::HashTrieSetSync<VoucherNullifier>,
    pub reward_amount: Value,
    pub claimable_rewards: Value,
    pub utxos: Utxos,
    pub tx_hash: TxHash,
}

impl ProvableOperation for LeaderClaimOp {
    type Proof = Groth16LeaderClaimProof;
}

impl OperationGas<MainnetGasProfile> for LeaderClaimOp {
    const GAS_COST: Gas = Gas::new(580);
}

impl PreverifiableOperation<verification_mode::StandardMode> for LeaderClaimOp {
    type Context<'a> = LeaderClaimPreverificationContext<'a>;
    type Error = LeaderClaimError;

    fn preverify(
        &self,
        proof: &Self::Proof,
        context: &Self::Context<'_>,
    ) -> Result<(), Self::Error> {
        let is_verified = proof.verify(&LeaderClaimPublic {
            voucher_nullifier: self.voucher_nullifier.into(),
            voucher_root: self.rewards_root.into(),
            mantle_tx_hash: *context.tx_hash_view.as_fr(),
        });

        if is_verified {
            Ok(())
        } else {
            Err(LeaderClaimError::InvalidPoC)
        }
    }
}

impl VerifiableOperation<verification_mode::StandardMode> for LeaderClaimOp {
    type Context<'a> = LeaderClaimVerificationContext<'a>;
    type Error = LeaderClaimError;

    fn verify(&self, proof: &Self::Proof, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        // Check that the nullifier isn't in the set
        if context.nullifiers.contains(&self.voucher_nullifier) {
            return Err(LeaderClaimError::DuplicatedVoucherNullifier);
        }

        // Check that the voucher root is the same as in the ledger
        if context.claimable_vouchers_root != &self.rewards_root {
            return Err(LeaderClaimError::VouchersRootMismatch);
        }

        // Check the proof of claim
        if !proof.verify(&LeaderClaimPublic {
            voucher_nullifier: self.voucher_nullifier.into(),
            voucher_root: context.claimable_vouchers_root.0,
            mantle_tx_hash: *context.tx_hash_view.as_fr(),
        }) {
            return Err(LeaderClaimError::InvalidPoC);
        }

        Ok(())
    }
}

impl ExecutableOperation for LeaderClaimOp {
    type Context<'a> = LeaderClaimExecutionContext;
    type Error = LeaderClaimError;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        // Add the nullifier to the nullifier set
        context.nullifiers = context.nullifiers.insert(self.voucher_nullifier);

        // Distribute the reward
        let utxo = self.utxo(context.reward_amount);
        context.utxos = context.utxos.insert(utxo.id(), utxo).0;

        // Remove the distributed rewards from the pool
        context.claimable_rewards -= context.reward_amount;
        let tx_hash = context.tx_hash;

        Ok((
            context,
            vec![TxEvent::new(
                tx_hash,
                self.op_id(),
                TxEventPayload::LeaderRewardClaimed {
                    voucher_nullifier: self.voucher_nullifier,
                    utxo,
                },
            )],
        ))
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOp<LeaderClaimOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        1
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
        let tx_hash = TxHash::from([11u8; 32]);
        let proof = Groth16LeaderClaimProof::prove(
            LeaderClaimPrivate::try_new(
                LeaderClaimPublic::new(
                    VoucherNullifier::from_secret(voucher_secret).into(),
                    voucher_root.into(),
                    tx_hash.to_fr(),
                ),
                &voucher_path,
                voucher_secret,
            )
            .expect("voucher path should match the PoC circuit height"),
        )
        .expect("proof generation should succeed");
        let op = LeaderClaimOp {
            rewards_root: voucher_root,
            voucher_nullifier: VoucherNullifier::from_secret(voucher_secret),
            pk: ZkPublicKey::zero(),
        };
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let tx_hash_view = TxHashView::from(tx_hash);
        let context = LeaderClaimVerificationContext {
            nullifiers: &nullifiers,
            claimable_vouchers_root: &voucher_root,
            tx_hash_view: &tx_hash_view,
        };

        assert_eq!(op.verify(&proof, &context), Ok(()));
    }

    #[test]
    fn execute_emits_leader_reward_claimed_event() {
        let voucher_secret = VoucherSecret::from(Fr::from(7u64));
        let reward_amount = 38;
        let pk = ZkPublicKey::zero();
        let tx_hash = TxHash::from([11u8; 32]);
        let op = LeaderClaimOp {
            rewards_root: RewardsRoot::default(),
            voucher_nullifier: VoucherNullifier::from_secret(voucher_secret),
            pk,
        };

        let (context, events) = op
            .execute(LeaderClaimExecutionContext {
                nullifiers: rpds::HashTrieSetSync::new_sync(),
                reward_amount,
                claimable_rewards: 100,
                utxos: Utxos::new(),
                tx_hash,
            })
            .expect("leader claim execution should succeed");

        assert!(context.nullifiers.contains(&op.voucher_nullifier));
        assert_eq!(context.claimable_rewards, 62);
        assert_eq!(
            context.utxos.get(&op.utxo(reward_amount).id()),
            Some(op.utxo(reward_amount))
        );

        let mut events = events.iter();
        let Some(TxEvent {
            tx_hash: event_tx_hash,
            op_id,
            payload:
                TxEventPayload::LeaderRewardClaimed {
                    voucher_nullifier,
                    utxo,
                },
        }) = events.next()
        else {
            panic!("expected LeaderRewardClaimed tx event");
        };
        assert_eq!(*event_tx_hash, tx_hash);
        assert_eq!(*op_id, op.op_id());
        assert_eq!(*voucher_nullifier, op.voucher_nullifier);
        assert_eq!(*utxo, op.utxo(reward_amount));
        assert!(events.next().is_none());
    }

    /// Regression test for #2990 (reward double-claim).
    ///
    /// The op's `voucher_nullifier` is fed in as the proof's public input, so a
    /// claim that supplies a nullifier other than the one the proof commits to
    /// fails verification (`InvalidPoC`). This is what prevents re-claiming a
    /// voucher under a different, unused nullifier to bypass the double-spend
    /// set: the op's dedup key is bound to the proven voucher.
    #[test]
    fn validate_rejects_op_nullifier_not_matching_proof() {
        let voucher_secret = VoucherSecret::from(Fr::from(7u64));
        let voucher_cm = VoucherCm::from_secret(voucher_secret);
        let (mmr, voucher_path) = MerkleMountainRange::<VoucherCm, ZkHasher>::new()
            .push_with_paths(voucher_cm, &mut [])
            .expect("MMR shouldn't be full");
        let voucher_root = RewardsRoot::from(mmr.frontier_root());
        let tx_hash = TxHash::from([11u8; 32]);
        // Proof proves ownership of the voucher whose nullifier is
        // `from_secret(voucher_secret)`.
        let proof = Groth16LeaderClaimProof::prove(
            LeaderClaimPrivate::try_new(
                LeaderClaimPublic::new(
                    VoucherNullifier::from_secret(voucher_secret).into(),
                    voucher_root.into(),
                    tx_hash.to_fr(),
                ),
                &voucher_path,
                voucher_secret,
            )
            .expect("voucher path should match the PoC circuit height"),
        )
        .expect("proof generation should succeed");

        // The claim supplies a DIFFERENT nullifier than the one the proof proves.
        let bogus_nf = VoucherNullifier::from_secret(VoucherSecret::from(Fr::from(999u64)));
        assert_ne!(bogus_nf, VoucherNullifier::from_secret(voucher_secret));
        let op = LeaderClaimOp {
            rewards_root: voucher_root,
            voucher_nullifier: bogus_nf,
            pk: ZkPublicKey::zero(),
        };
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let tx_hash_view = TxHashView::from(tx_hash);
        let context = LeaderClaimVerificationContext {
            nullifiers: &nullifiers,
            claimable_vouchers_root: &voucher_root,
            tx_hash_view: &tx_hash_view,
        };

        // The proof is verified against `op.voucher_nullifier`, which does not
        // match the proven voucher -> rejected. A voucher cannot be claimed under
        // a substituted nullifier.
        assert_eq!(
            op.verify(&proof, &context),
            Err(LeaderClaimError::InvalidPoC)
        );
    }
}
