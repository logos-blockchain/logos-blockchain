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
            VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        ops::{OpId, SignedOperation},
        transactions::{
            hash::{TxHash, TxHashView},
            states::{Preverified, Unverified, VerificationState, Verified},
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

    #[cfg(any(test, feature = "samples"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            rewards_root: Fr::from(32u64).into(),
            voucher_nullifier: Fr::from(33u64).into(),
            pk: ZkPublicKey::from(Fr::from(34u64)),
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
    const CODE: u8 = 0x30;
}

impl OperationGas<MainnetGasProfile> for LeaderClaimOp {
    const GAS_COST: Gas = Gas::new(580);
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<LeaderClaimOp, Unverified, StandardMode>
{
    type Context<'a> = LeaderClaimPreverificationContext<'a>;
    type Error = LeaderClaimError;

    fn preverify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        let is_verified = self.proof().verify(&LeaderClaimPublic {
            voucher_nullifier: operation.voucher_nullifier.into(),
            voucher_root: operation.rewards_root.into(),
            mantle_tx_hash: *context.tx_hash_view.as_fr(),
        });

        if is_verified {
            Ok(())
        } else {
            Err(LeaderClaimError::InvalidPoC)
        }
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<LeaderClaimOp, Preverified, StandardMode>
{
    type Context<'a> = LeaderClaimVerificationContext<'a>;
    type Error = LeaderClaimError;

    fn verify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Check that the nullifier isn't in the set
        if context.nullifiers.contains(&operation.voucher_nullifier) {
            return Err(LeaderClaimError::DuplicatedVoucherNullifier);
        }

        // Check that the voucher root is the same as in the ledger
        if context.claimable_vouchers_root != &operation.rewards_root {
            return Err(LeaderClaimError::VouchersRootMismatch);
        }

        // Check the proof of claim
        // TODO: Remove. Already checked in preverify.
        if !self.proof().verify(&LeaderClaimPublic {
            voucher_nullifier: operation.voucher_nullifier.into(),
            voucher_root: context.claimable_vouchers_root.0,
            mantle_tx_hash: *context.tx_hash_view.as_fr(),
        }) {
            return Err(LeaderClaimError::InvalidPoC);
        }

        Ok(())
    }
}

impl<Mode: VerificationMode> ExecutableOperation
    for SignedOperation<LeaderClaimOp, Verified, Mode>
{
    type Context<'a> = LeaderClaimExecutionContext;
    type Error = LeaderClaimError;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // Add the nullifier to the nullifier set
        context.nullifiers = context.nullifiers.insert(operation.voucher_nullifier);

        // Distribute the reward
        let utxo = operation.utxo(context.reward_amount);
        context.utxos = context.utxos.insert(utxo.id(), utxo).0;

        // Remove the distributed rewards from the pool
        context.claimable_rewards -= context.reward_amount;
        let tx_hash = context.tx_hash;

        Ok((
            context,
            vec![TxEvent::new(
                tx_hash,
                operation.op_id(),
                TxEventPayload::LeaderRewardClaimed {
                    voucher_nullifier: operation.voucher_nullifier,
                    utxo,
                },
            )],
        ))
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOperation<LeaderClaimOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        1
    }
}

#[cfg(test)]
mod tests {
    use lb_mmr::MerkleMountainRange;

    use super::*;
    use crate::{
        mantle::ops::op_proof::samples::SampleProof as _,
        proofs::leader_claim_proof::LeaderClaimPrivate,
    };

    /// Regression test for #2990 (reward double-claim).
    ///
    /// The op's `voucher_nullifier` is fed in as the proof's public input, so a
    /// claim that supplies a nullifier other than the one the proof commits to
    /// fails verification (`InvalidPoC`). This is what prevents re-claiming a
    /// voucher under a different, unused nullifier to bypass the double-spend
    /// set: the op's dedup key is bound to the proven voucher.
    #[test]
    fn preverify_rejects_a_nullifier_the_proof_does_not_prove() {
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
        let tx_hash_view = TxHashView::from(tx_hash);
        let preverify_context = LeaderClaimPreverificationContext {
            tx_hash_view: &tx_hash_view,
        };

        // The proof is verified against `op.voucher_nullifier`, which does not
        // match the proven voucher -> rejected during preverify. A voucher
        // cannot be claimed under a substituted nullifier.
        let unverified_signed_operation = SignedOperation::new(op, proof);
        let preverify_result = unverified_signed_operation.into_preverified(&preverify_context);
        assert_eq!(preverify_result.err(), Some(LeaderClaimError::InvalidPoC));
    }

    #[test]
    fn preverify_rejects_a_rewards_root_the_proof_does_not_prove() {
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

        let other_root = RewardsRoot::from(Fr::from(42u64));
        assert_ne!(other_root, voucher_root);
        let operation = LeaderClaimOp {
            rewards_root: other_root,
            voucher_nullifier: VoucherNullifier::from_secret(voucher_secret),
            pk: ZkPublicKey::zero(),
        };
        let tx_hash_view = TxHashView::from(tx_hash);

        assert_eq!(
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof).preverify(
                &LeaderClaimPreverificationContext {
                    tx_hash_view: &tx_hash_view,
                }
            ),
            Err(LeaderClaimError::InvalidPoC)
        );
    }

    #[test]
    fn preverify_rejects_a_proof_over_another_transaction() {
        let voucher_secret = VoucherSecret::from(Fr::from(7u64));
        let voucher_cm = VoucherCm::from_secret(voucher_secret);
        let (mmr, voucher_path) = MerkleMountainRange::<VoucherCm, ZkHasher>::new()
            .push_with_paths(voucher_cm, &mut [])
            .expect("MMR shouldn't be full");
        let voucher_root = RewardsRoot::from(mmr.frontier_root());
        let signed_hash = TxHash::from([11u8; 32]);
        let proof = Groth16LeaderClaimProof::prove(
            LeaderClaimPrivate::try_new(
                LeaderClaimPublic::new(
                    VoucherNullifier::from_secret(voucher_secret).into(),
                    voucher_root.into(),
                    signed_hash.to_fr(),
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
        let other_view = TxHashView::from(TxHash::from([12u8; 32]));

        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(op, proof);

        assert_eq!(
            signed_operation.preverify(&LeaderClaimPreverificationContext {
                tx_hash_view: &other_view
            }),
            Err(LeaderClaimError::InvalidPoC)
        );
    }

    fn preverified_claim(
        tx_hash: TxHash,
    ) -> (
        RewardsRoot,
        VoucherNullifier,
        SignedOperation<LeaderClaimOp, Preverified, StandardMode>,
    ) {
        let voucher_secret = VoucherSecret::from(Fr::from(7u64));
        let voucher_cm = VoucherCm::from_secret(voucher_secret);
        let (mmr, voucher_path) = MerkleMountainRange::<VoucherCm, ZkHasher>::new()
            .push_with_paths(voucher_cm, &mut [])
            .expect("MMR shouldn't be full");
        let voucher_root = RewardsRoot::from(mmr.frontier_root());
        let voucher_nullifier = VoucherNullifier::from_secret(voucher_secret);

        let proof = Groth16LeaderClaimProof::prove(
            LeaderClaimPrivate::try_new(
                LeaderClaimPublic::new(
                    voucher_nullifier.into(),
                    voucher_root.into(),
                    tx_hash.to_fr(),
                ),
                &voucher_path,
                voucher_secret,
            )
            .expect("voucher path should match the PoC circuit height"),
        )
        .expect("proof generation should succeed");

        let operation = LeaderClaimOp {
            rewards_root: voucher_root,
            voucher_nullifier,
            pk: ZkPublicKey::zero(),
        };
        let tx_hash_view = TxHashView::from(tx_hash);
        let signed_operation = SignedOperation::new(operation, proof)
            .into_preverified(&LeaderClaimPreverificationContext {
                tx_hash_view: &tx_hash_view,
            })
            .expect("preverify should accept a valid proof");

        (voucher_root, voucher_nullifier, signed_operation)
    }

    #[test]
    fn verify_rejects_a_voucher_that_was_already_claimed() {
        let tx_hash = TxHash::from([11u8; 32]);
        let (voucher_root, voucher_nullifier, signed_operation) = preverified_claim(tx_hash);

        let nullifiers = rpds::HashTrieSetSync::new_sync().insert(voucher_nullifier);

        assert_eq!(
            signed_operation.verify(&LeaderClaimVerificationContext {
                nullifiers: &nullifiers,
                claimable_vouchers_root: &voucher_root,
                tx_hash_view: &TxHashView::from(tx_hash),
            }),
            Err(LeaderClaimError::DuplicatedVoucherNullifier)
        );
    }

    #[test]
    fn verify_rejects_a_claim_against_a_stale_rewards_root() {
        let tx_hash = TxHash::from([11u8; 32]);
        let (_, _, signed_operation) = preverified_claim(tx_hash);

        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let other_root = RewardsRoot::from(Fr::from(42u64));

        assert_eq!(
            signed_operation.verify(&LeaderClaimVerificationContext {
                nullifiers: &nullifiers,
                claimable_vouchers_root: &other_root,
                tx_hash_view: &TxHashView::from(tx_hash),
            }),
            Err(LeaderClaimError::VouchersRootMismatch)
        );
    }

    #[test]
    fn verify_rejects_a_proof_over_another_transaction() {
        let tx_hash = TxHash::from([11u8; 32]);
        let (rewards_root, _, signed_operation) = preverified_claim(tx_hash);

        let nullifiers = rpds::HashTrieSetSync::new_sync();

        assert_eq!(
            signed_operation.verify(&LeaderClaimVerificationContext {
                nullifiers: &nullifiers,
                claimable_vouchers_root: &rewards_root,
                tx_hash_view: &TxHashView::from(TxHash::from([12u8; 32])),
            }),
            Err(LeaderClaimError::InvalidPoC)
        );
    }

    #[test]
    fn verify_accepts_a_valid_proof_of_claim() {
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
        let preverify_context = LeaderClaimPreverificationContext {
            tx_hash_view: &tx_hash_view,
        };
        let verify_context = LeaderClaimVerificationContext {
            nullifiers: &nullifiers,
            claimable_vouchers_root: &voucher_root,
            tx_hash_view: &tx_hash_view,
        };

        let unverified_signed_operation = SignedOperation::new(op, proof);
        let preverified_signed_operation = unverified_signed_operation
            .into_preverified(&preverify_context)
            .expect("preverify should accept a valid proof");
        let _verified_signed_operation = preverified_signed_operation
            .into_verified(&verify_context)
            .expect("verify should accept a valid claim");
    }

    #[test]
    fn execute_emits_leader_reward_claimed_event() {
        let voucher_secret = VoucherSecret::from(Fr::from(7u64));
        let voucher_cm = VoucherCm::from_secret(voucher_secret);
        let (mmr, voucher_path) = MerkleMountainRange::<VoucherCm, ZkHasher>::new()
            .push_with_paths(voucher_cm, &mut [])
            .expect("MMR shouldn't be full");
        let voucher_root = RewardsRoot::from(mmr.frontier_root());
        let reward_amount = 38;
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
        let preverify_context = LeaderClaimPreverificationContext {
            tx_hash_view: &tx_hash_view,
        };
        let verify_context = LeaderClaimVerificationContext {
            nullifiers: &nullifiers,
            claimable_vouchers_root: &voucher_root,
            tx_hash_view: &tx_hash_view,
        };

        let unverified_signed_operation = SignedOperation::new(op, proof);
        let preverified_signed_operation = unverified_signed_operation
            .into_preverified(&preverify_context)
            .expect("preverify should accept a valid proof");
        let verified_signed_operation = preverified_signed_operation
            .into_verified(&verify_context)
            .expect("verify should accept a valid claim");
        let operation = verified_signed_operation.operation().clone();

        let (context, events) = verified_signed_operation
            .execute(LeaderClaimExecutionContext {
                nullifiers: rpds::HashTrieSetSync::new_sync(),
                reward_amount,
                claimable_rewards: 100,
                utxos: Utxos::new(),
                tx_hash,
            })
            .expect("leader claim execution should succeed");

        assert!(context.nullifiers.contains(&operation.voucher_nullifier));
        assert_eq!(context.claimable_rewards, 62);
        assert_eq!(
            context.utxos.get(&operation.utxo(reward_amount).id()),
            Some(operation.utxo(reward_amount))
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
        assert_eq!(*op_id, operation.op_id());
        assert_eq!(*voucher_nullifier, operation.voucher_nullifier);
        assert_eq!(*utxo, operation.utxo(reward_amount));
        assert!(events.next().is_none());
    }

    #[test]
    fn leader_claim_op_execution_gas() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            LeaderClaimOp::sample(),
            <LeaderClaimOp as ProvableOperation>::Proof::sample(),
        );

        assert_eq!(
            signed_operation.execution_gas::<MainnetGasProfile>(),
            Ok(Gas::new(580))
        );
    }
}
