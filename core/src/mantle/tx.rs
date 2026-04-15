use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

use bytes::Bytes;
use lb_groth16::{Fr, fr_from_bytes, fr_from_bytes_unchecked, fr_to_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::Ed25519PublicKey;
use lb_poseidon2::{Digest, ZkHash};
use num_bigint::BigUint;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    crypto::{Digest as _, HALF_BLAKE_DIGEST_BYTES_SIZE, Hasher, ZkHasher},
    mantle::{
        AuthenticatedMantleTx, StorageSize, Transaction, TransactionHasher, Value,
        encoding::{decode_mantle_tx, encode_mantle_tx, encode_signed_mantle_tx},
        gas::{Gas, GasCalculator, GasConstants, GasCost, GasOverflow, GasPrice},
        ops::{
            Op, OpProof,
            channel::{ChannelId, ChannelKeyIndex, withdraw::ChannelWithdrawOp},
            transfer::TransferOp,
        },
    },
    proofs::{
        channel_withdraw_proof::ChannelWithdrawProof,
        leader_claim_proof::{LeaderClaimProof as _, LeaderClaimPublic},
    },
};

/// The hash of a transaction
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TxHash(#[serde(with = "serde_fr")] pub ZkHash);

impl From<ZkHash> for TxHash {
    fn from(fr: ZkHash) -> Self {
        Self(fr)
    }
}

impl From<BigUint> for TxHash {
    fn from(value: BigUint) -> Self {
        Self(value.into())
    }
}

impl From<TxHash> for ZkHash {
    fn from(hash: TxHash) -> Self {
        hash.0
    }
}

impl AsRef<ZkHash> for TxHash {
    fn as_ref(&self) -> &ZkHash {
        &self.0
    }
}

impl From<TxHash> for Bytes {
    fn from(tx_hash: TxHash) -> Self {
        Self::copy_from_slice(&fr_to_bytes(tx_hash.as_ref()))
    }
}

impl From<TxHash> for [u8; 32] {
    fn from(tx_hash: TxHash) -> Self {
        fr_to_bytes(tx_hash.as_ref())
    }
}

impl TxHash {
    /// For testing purposes
    #[cfg(test)]
    pub fn random(mut rng: impl rand::RngCore) -> Self {
        Self(BigUint::from(rng.next_u64()).into())
    }

    #[must_use]
    pub fn as_signing_bytes(&self) -> Bytes {
        self.0.0.0.iter().flat_map(|b| b.to_le_bytes()).collect()
    }
}

#[derive(Serialize, Deserialize)]
struct MantleTxDeSerImpl {
    pub ops: Vec<Op>,
    pub execution_gas_price: GasPrice,
    pub storage_gas_price: GasPrice,
}

#[derive(Debug, Clone, Default)]
pub struct MantleTxContext {
    pub gas_context: MantleTxGasContext,
    pub leader_reward_amount: Value,
}

#[derive(Debug, Clone, Default)]
pub struct MantleTxGasContext {
    withdraw_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
}

impl MantleTxGasContext {
    #[must_use]
    pub const fn new(withdraw_thresholds: HashMap<ChannelId, ChannelKeyIndex>) -> Self {
        Self {
            withdraw_thresholds,
        }
    }

    #[must_use]
    pub fn withdraw_threshold(&self, channel_id: &ChannelId) -> Option<ChannelKeyIndex> {
        self.withdraw_thresholds.get(channel_id).copied()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MantleTx {
    pub ops: Vec<Op>,
    pub execution_gas_price: GasPrice,
    pub storage_gas_price: GasPrice,
}

impl From<MantleTxDeSerImpl> for MantleTx {
    fn from(
        MantleTxDeSerImpl {
            ops,
            execution_gas_price,
            storage_gas_price,
        }: MantleTxDeSerImpl,
    ) -> Self {
        Self {
            ops,
            execution_gas_price,
            storage_gas_price,
        }
    }
}

impl From<MantleTx> for MantleTxDeSerImpl {
    fn from(
        MantleTx {
            ops,
            execution_gas_price,
            storage_gas_price,
        }: MantleTx,
    ) -> Self {
        Self {
            ops,
            execution_gas_price,
            storage_gas_price,
        }
    }
}

impl Serialize for MantleTx {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            let tx_deser: MantleTxDeSerImpl = self.clone().into();
            tx_deser.serialize(serializer)
        } else {
            let bytes = encode_mantle_tx(self);
            serializer.serialize_bytes(&bytes)
        }
    }
}

impl<'de> Deserialize<'de> for MantleTx {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            <MantleTxDeSerImpl as Deserialize>::deserialize(deserializer).map(Into::into)
        } else {
            let bytes: Vec<u8> = <Vec<u8>>::deserialize(deserializer)?;
            decode_mantle_tx(&bytes)
                .map(|(_, tx)| tx)
                .map_err(serde::de::Error::custom)
        }
    }
}

impl GasCalculator for MantleTx {
    type Context = MantleTxGasContext;

    fn total_gas_cost<Constants: GasConstants>(
        &self,
        context: &Self::Context,
    ) -> Result<GasCost, GasOverflow> {
        let execution_gas = self.execution_gas_consumption::<Constants>(context);
        let execution_gas_cost = GasCost::calculate(execution_gas?, self.execution_gas_price)?;
        let storage_gas_cost = self.storage_gas_cost(context)?;

        execution_gas_cost.checked_add(storage_gas_cost)
    }

    fn storage_gas_cost(&self, context: &Self::Context) -> Result<GasCost, GasOverflow> {
        GasCost::calculate(
            self.storage_gas_consumption(context)?,
            self.storage_gas_price,
        )
    }

    fn execution_gas_consumption<Constants: GasConstants>(
        &self,
        _context: &Self::Context,
    ) -> Result<Gas, GasOverflow> {
        self.ops
            .iter()
            .map(Op::execution_gas::<Constants>)
            .try_fold(Gas::from(0), Gas::checked_add)
    }

    fn storage_gas_consumption(&self, context: &Self::Context) -> Result<Gas, GasOverflow> {
        Ok(self.signed_serialized_size(context).into())
    }
}

impl MantleTx {
    #[must_use]
    pub fn signed_serialized_size(&self, context: &<Self as GasCalculator>::Context) -> u64 {
        super::encoding::predict_signed_mantle_tx_size(self, context) as u64
    }

    #[must_use]
    pub fn transfers(&self) -> Vec<TransferOp> {
        let mut transfers: Vec<TransferOp> = vec![];
        for op in self.ops.clone() {
            if let Op::Transfer(transfer_op) = op {
                transfers.push(transfer_op);
            }
        }
        transfers
    }
}

static MANTLE_TXHASH_V1_FR: LazyLock<Fr> =
    LazyLock::new(|| fr_from_bytes(b"MANTLE_TXHASH_V1").expect("Constant should be valid Fr"));

impl Transaction for MantleTx {
    const HASHER: TransactionHasher<Self> =
        |tx| <ZkHasher as Digest>::digest(&tx.as_signing_frs()).into();
    type Hash = TxHash;

    fn as_signing_frs(&self) -> Vec<Fr> {
        // constant and structure as defined in the Mantle specification:
        // https://www.notion.so/nomos-tech/v1-3-Mantle-Specification-31e261aa09df818f9327ee87e5a6d433#31e261aa09df80aea7cff4eb98d61b6e
        let encoded_bytes = encode_mantle_tx(self);
        let first_blake_hash = Hasher::digest(encoded_bytes);
        let frs = first_blake_hash
            .as_slice()
            .chunks(HALF_BLAKE_DIGEST_BYTES_SIZE)
            .map(fr_from_bytes_unchecked);
        std::iter::once(*MANTLE_TXHASH_V1_FR).chain(frs).collect()
    }
}

impl From<SignedMantleTx> for MantleTx {
    fn from(signed_tx: SignedMantleTx) -> Self {
        signed_tx.mantle_tx
    }
}

// Deserializing here is dangerous, as it bypasses the verification without
// confirmation.
// TODO: Split entity into a system that allows for verification in different
// stages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignedMantleTx {
    pub mantle_tx: MantleTx,
    // TODO: make this more efficient
    pub ops_proofs: Vec<OpProof>,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum VerificationError {
    #[error("Invalid signature for operation at index {op_index}")]
    InvalidSignature { op_index: usize },
    #[error("Invalid proof of claim for operation at index {op_index}")]
    InvalidProofOfClaim { op_index: usize },
    #[error("Missing required proof for {op_type} operation at index {op_index}")]
    MissingProof {
        op_type: &'static str,
        op_index: usize,
    },
    #[error("Incorrect proof type for {op_type} operation at index {op_index}")]
    IncorrectProofType {
        op_type: &'static str,
        op_index: usize,
    },
    #[error(
        "The number of proofs ({proofs_count}) does not match the number of operations ({ops_count})"
    )]
    ProofCountMismatch {
        ops_count: usize,
        proofs_count: usize,
    },
    #[error("Channel {channel_id} could not be found")]
    ChannelNotFound { channel_id: ChannelId },
    #[error("Key {key_index} could not be found in channel {channel_id}")]
    KeyNotFound {
        channel_id: ChannelId,
        key_index: ChannelKeyIndex,
    },
    #[error(
        "Not enough signatures in ChannelWithdrawProof at index {op_index}: got {actual}, required {required}"
    )]
    ChannelWithdrawProofNotEnoughSignatures {
        op_index: usize,
        actual: usize,
        required: ChannelKeyIndex,
    },
    #[error("Duplicate signature indices in ChannelWithdrawProof at index {op_index}")]
    ChannelWithdrawProofDuplicateIndices { op_index: usize },
    #[error(
        "Invalid signature in ChannelWithdrawProof at index {op_index} for signature index {signature_index}"
    )]
    ChannelWithdrawProofInvalidSignature {
        op_index: usize,
        signature_index: usize,
    },
}

pub trait OperationVerificationHelper {
    fn get_channel_withdraw_threshold(
        &self,
        channel_id: &ChannelId,
    ) -> Result<ChannelKeyIndex, VerificationError>;

    fn get_key_from_channel_at_index(
        &self,
        channel_id: &ChannelId,
        key_index: &ChannelKeyIndex,
    ) -> Result<Ed25519PublicKey, VerificationError>;
}

impl SignedMantleTx {
    /// Create a new `SignedMantleTx` and verify that all required proofs are
    /// present and valid.
    ///
    /// This enforces at construction time that:
    /// - `ChannelInscribe` operations have a valid Ed25519 signature from the
    ///   declared signer
    pub fn new(mantle_tx: MantleTx, ops_proofs: Vec<OpProof>) -> Result<Self, VerificationError> {
        let tx = Self {
            mantle_tx,
            ops_proofs,
        };
        tx.verify_ops_proofs()?;
        Ok(tx)
    }

    /// Create a `SignedMantleTx` without verifying proofs.
    /// This should only be used for `GenesisTx` or in tests.
    #[doc(hidden)]
    #[must_use]
    pub const fn new_unverified(mantle_tx: MantleTx, ops_proofs: Vec<OpProof>) -> Self {
        Self {
            mantle_tx,
            ops_proofs,
        }
    }

    // TODO: might drop proofs after verification
    fn verify_ops_proofs(&self) -> Result<(), VerificationError> {
        // Check that we have the same number of proofs as ops
        if self.mantle_tx.ops.len() != self.ops_proofs.len() {
            return Err(VerificationError::ProofCountMismatch {
                ops_count: self.mantle_tx.ops.len(),
                proofs_count: self.ops_proofs.len(),
            });
        }

        let tx_hash = self.hash();
        let tx_hash_bytes = tx_hash.as_signing_bytes();

        for (idx, (op, proof)) in self
            .mantle_tx
            .ops
            .iter()
            .zip(self.ops_proofs.iter())
            .enumerate()
        {
            match (op, proof) {
                (Op::ChannelInscribe(inscribe_op), OpProof::Ed25519Sig(sig)) => {
                    // Inscription operations require an Ed25519 signature
                    inscribe_op
                        .signer
                        .verify(tx_hash_bytes.as_ref(), sig)
                        .map_err(|_| VerificationError::InvalidSignature { op_index: idx })?;
                }
                v @ (Op::ChannelInscribe(_), OpProof::ZkSig(_)) => {
                    return Err(VerificationError::IncorrectProofType {
                        op_type: v.0.as_str(),
                        op_index: idx,
                    });
                }
                (Op::LeaderClaim(leader_claim_op), OpProof::PoC(poc)) => {
                    let ok = poc.verify(&LeaderClaimPublic {
                        voucher_root: leader_claim_op.rewards_root.into(),
                        mantle_tx_hash: tx_hash.into(),
                    });
                    if !ok {
                        return Err(VerificationError::InvalidProofOfClaim { op_index: idx });
                    }
                }
                // Other operations are checked by the ledger or don't require verification here
                _ => {
                    // TODO: If the op and proof don't match, we are silently
                    // delaying the error
                    //  until tx execution.
                }
            }
        }

        Ok(())
    }

    pub fn verify_ops_proofs_with_helper(
        &self,
        operation_verification_helper: &impl OperationVerificationHelper,
    ) -> Result<(), VerificationError> {
        let tx_hash = self.hash();
        let tx_hash_bytes = tx_hash.as_signing_bytes();

        for (idx, (op, proof)) in self
            .mantle_tx
            .ops
            .iter()
            .zip(self.ops_proofs.iter())
            .enumerate()
        {
            #[expect(
                clippy::single_match_else,
                reason = "Clearer and follows the pattern of verify_ops_proofs."
            )]
            match (op, proof) {
                (
                    Op::ChannelWithdraw(channel_withdraw_op),
                    OpProof::ChannelWithdrawProof(proof),
                ) => {
                    verify_channel_withdraw(
                        channel_withdraw_op,
                        proof,
                        &tx_hash_bytes,
                        operation_verification_helper,
                        idx,
                    )?;
                }
                // Other operations don't require verification here
                _ => {
                    // TODO: If the op and proof don't match, we are silently
                    //  delaying the error until tx execution.
                }
            }
        }

        Ok(())
    }

    fn gas_storage_size(&self) -> u64 {
        encode_signed_mantle_tx(self).len() as u64
    }
}

fn verify_channel_withdraw(
    operation: &ChannelWithdrawOp,
    proof: &ChannelWithdrawProof,
    tx_hash_bytes: &Bytes,
    helper: &impl OperationVerificationHelper,
    op_index: usize,
) -> Result<(), VerificationError> {
    let channel_id = &operation.channel_id;
    let withdraw_threshold = helper.get_channel_withdraw_threshold(channel_id)?;

    let signatures = proof.signatures();
    let signatures_len = signatures.len();
    if signatures_len < withdraw_threshold as usize {
        return Err(VerificationError::ChannelWithdrawProofNotEnoughSignatures {
            op_index,
            actual: signatures_len,
            required: withdraw_threshold,
        });
    }

    let indices_set = signatures
        .iter()
        .map(|signature| signature.channel_key_index)
        .collect::<HashSet<_>>();
    let indices_set_len = indices_set.len();
    if indices_set_len != signatures_len {
        return Err(VerificationError::ChannelWithdrawProofDuplicateIndices { op_index });
    }

    for (i, signature) in signatures.iter().enumerate() {
        let public_key =
            helper.get_key_from_channel_at_index(channel_id, &signature.channel_key_index)?;
        if let Err(_error) = public_key.verify(tx_hash_bytes.as_ref(), &signature.signature) {
            return Err(VerificationError::ChannelWithdrawProofInvalidSignature {
                op_index,
                signature_index: i,
            });
        }
    }

    Ok(())
}

impl Transaction for SignedMantleTx {
    const HASHER: TransactionHasher<Self> =
        |tx| <ZkHasher as Digest>::digest(&tx.as_signing_frs()).into();
    type Hash = TxHash;

    fn as_signing_frs(&self) -> Vec<Fr> {
        self.mantle_tx.as_signing_frs()
    }
}

impl AuthenticatedMantleTx for SignedMantleTx {
    fn mantle_tx(&self) -> &MantleTx {
        &self.mantle_tx
    }

    fn ops_with_proof(&self) -> impl Iterator<Item = (&Op, &OpProof)> {
        self.mantle_tx.ops.iter().zip(self.ops_proofs.iter())
    }

    fn total_gas_cost<Constants: GasConstants>(&self) -> Result<GasCost, GasOverflow> {
        GasCalculator::total_gas_cost::<Constants>(&self, &())
    }

    fn storage_gas_cost(&self) -> Result<GasCost, GasOverflow> {
        GasCalculator::storage_gas_cost(&self, &())
    }

    fn execution_gas_consumption<Constants: GasConstants>(&self) -> Result<Gas, GasOverflow> {
        GasCalculator::execution_gas_consumption::<Constants>(&self, &())
    }

    fn storage_gas_consumption(&self) -> Result<Gas, GasOverflow> {
        GasCalculator::storage_gas_consumption(&self, &())
    }

    fn verify_ops_proofs_with_helper(
        &self,
        operation_verification_helper: &impl OperationVerificationHelper,
    ) -> Result<(), VerificationError> {
        Self::verify_ops_proofs_with_helper(self, operation_verification_helper)
    }
}

impl GasCalculator for SignedMantleTx {
    type Context = ();

    fn total_gas_cost<Constants: GasConstants>(
        &self,
        context: &Self::Context,
    ) -> Result<GasCost, GasOverflow> {
        let execution_gas = GasCalculator::execution_gas_consumption::<Constants>(&self, context)?;
        let execution_gas_cost =
            GasCost::calculate(execution_gas, self.mantle_tx.execution_gas_price)?;
        let storage_gas_cost = GasCalculator::storage_gas_cost(self, context)?;

        execution_gas_cost.checked_add(storage_gas_cost)
    }

    fn storage_gas_cost(&self, context: &Self::Context) -> Result<GasCost, GasOverflow> {
        let storage_gas = GasCalculator::storage_gas_consumption(&self, context)?;
        GasCost::calculate(storage_gas, self.mantle_tx.storage_gas_price)
    }

    fn execution_gas_consumption<Constants: GasConstants>(
        &self,
        _context: &Self::Context,
    ) -> Result<Gas, GasOverflow> {
        self.mantle_tx
            .ops
            .iter()
            .map(Op::execution_gas::<Constants>)
            .try_fold(Gas::from(0), Gas::checked_add)
    }

    fn storage_gas_consumption(&self, _context: &Self::Context) -> Result<Gas, GasOverflow> {
        Ok(self.gas_storage_size().into())
    }
}

impl StorageSize for SignedMantleTx {
    fn storage_size(&self) -> usize {
        self.gas_storage_size() as usize
    }
}

impl<'de> Deserialize<'de> for SignedMantleTx {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SignedMantleTxHelper {
            mantle_tx: MantleTx,
            ops_proofs: Vec<OpProof>,
        }

        let helper = SignedMantleTxHelper::deserialize(deserializer)?;
        Self::new(helper.mantle_tx, helper.ops_proofs).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey, ZkPublicKey};

    use super::*;
    use crate::{
        mantle::{Note, ledger::Outputs, ops::channel::inscribe::InscriptionOp},
        proofs::channel_withdraw_proof::WithdrawSignature,
    };

    fn create_test_mantle_tx(ops: Vec<Op>) -> MantleTx {
        MantleTx {
            ops,
            execution_gas_price: 1.into(),
            storage_gas_price: 1.into(),
        }
    }

    fn create_test_inscribe_op(signing_key: &Ed25519Key) -> InscriptionOp {
        InscriptionOp {
            channel_id: [0; 32].into(),
            inscription: vec![1, 2, 3],
            parent: [0; 32].into(),
            signer: signing_key.public_key(),
        }
    }

    struct TestOperationVerificationHelper {
        thresholds: HashMap<ChannelId, ChannelKeyIndex>,
        keys: HashMap<(ChannelId, ChannelKeyIndex), Ed25519PublicKey>,
    }

    impl TestOperationVerificationHelper {
        fn new(
            thresholds: impl IntoIterator<Item = (ChannelId, ChannelKeyIndex)>,
            keys: impl IntoIterator<Item = ((ChannelId, ChannelKeyIndex), Ed25519PublicKey)>,
        ) -> Self {
            Self {
                thresholds: thresholds.into_iter().collect(),
                keys: keys.into_iter().collect(),
            }
        }
    }

    impl OperationVerificationHelper for TestOperationVerificationHelper {
        fn get_channel_withdraw_threshold(
            &self,
            channel_id: &ChannelId,
        ) -> Result<ChannelKeyIndex, VerificationError> {
            self.thresholds
                .get(channel_id)
                .copied()
                .ok_or(VerificationError::ChannelNotFound {
                    channel_id: *channel_id,
                })
        }

        fn get_key_from_channel_at_index(
            &self,
            channel_id: &ChannelId,
            key_index: &ChannelKeyIndex,
        ) -> Result<Ed25519PublicKey, VerificationError> {
            self.keys.get(&(*channel_id, *key_index)).copied().ok_or(
                VerificationError::KeyNotFound {
                    channel_id: *channel_id,
                    key_index: *key_index,
                },
            )
        }
    }

    fn create_withdraw_tx(channel_id: ChannelId, signing_keys: &[&Ed25519Key]) -> SignedMantleTx {
        let withdraw_note = Note {
            value: 5,
            pk: ZkPublicKey::from(Fr::from(BigUint::from(0u32))),
        };
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelWithdraw(ChannelWithdrawOp {
            channel_id,
            outputs: Outputs::new(vec![withdraw_note]),
            withdraw_nonce: 0,
        })]);
        let tx_hash = mantle_tx.hash();
        let signatures = signing_keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                WithdrawSignature::new(
                    index as ChannelKeyIndex,
                    key.sign_payload(tx_hash.as_signing_bytes().as_ref()),
                )
            })
            .collect();
        let proof = ChannelWithdrawProof::new(signatures).unwrap();
        SignedMantleTx::new(mantle_tx, vec![OpProof::ChannelWithdrawProof(proof)]).unwrap()
    }

    #[test]
    fn test_signed_mantle_tx_new_with_valid_inscribe_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        // Sign the transaction hash
        let tx_hash = mantle_tx.hash();
        let signature = signing_key.sign_payload(&tx_hash.as_signing_bytes());

        let result = SignedMantleTx::new(mantle_tx, vec![OpProof::Ed25519Sig(signature)]);

        assert!(result.is_ok());
    }

    #[test]
    fn test_signed_mantle_tx_new_missing_inscribe_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);
        let result = SignedMantleTx::new(mantle_tx, vec![]);

        assert!(matches!(
            result,
            Err(VerificationError::ProofCountMismatch {
                ops_count: 1,
                proofs_count: 0
            })
        ));
    }

    #[test]
    fn test_signed_mantle_tx_new_invalid_inscribe_signature() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let wrong_signing_key = Ed25519Key::from_bytes(&[2; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        // Sign with wrong key
        let tx_hash = mantle_tx.hash();
        let signature = wrong_signing_key.sign_payload(&tx_hash.as_signing_bytes());

        let result = SignedMantleTx::new(mantle_tx, vec![OpProof::Ed25519Sig(signature)]);

        assert!(matches!(
            result,
            Err(VerificationError::InvalidSignature { op_index: 0 })
        ));
    }

    #[test]
    fn test_signed_mantle_tx_new_incorrect_inscribe_proof_type() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        // Use wrong proof type
        let tx_hash = mantle_tx.hash();
        let zk_sig = OpProof::ZkSig(ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap());
        let result = SignedMantleTx::new(mantle_tx, vec![zk_sig]);

        assert!(matches!(
            result,
            Err(VerificationError::IncorrectProofType {
                op_type: "ChannelInscribe",
                op_index: 0
            })
        ));
    }

    #[test]
    fn test_signed_mantle_tx_new_multiple_ops_valid() {
        let signing_key1 = Ed25519Key::from_bytes(&[1; 32]);
        let signing_key2 = Ed25519Key::from_bytes(&[2; 32]);

        let inscribe_op1 = create_test_inscribe_op(&signing_key1);
        let inscribe_op2 = create_test_inscribe_op(&signing_key2);

        let mantle_tx = create_test_mantle_tx(vec![
            Op::ChannelInscribe(inscribe_op1),
            Op::ChannelInscribe(inscribe_op2),
        ]);

        let tx_hash = mantle_tx.hash();
        let sig1 = signing_key1.sign_payload(&tx_hash.as_signing_bytes());
        let sig2 = signing_key2.sign_payload(&tx_hash.as_signing_bytes());

        let result = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(sig1), OpProof::Ed25519Sig(sig2)],
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_signed_mantle_tx_new_multiple_ops_one_invalid() {
        let signing_key1 = Ed25519Key::from_bytes(&[1; 32]);
        let signing_key2 = Ed25519Key::from_bytes(&[2; 32]);
        let wrong_key = Ed25519Key::from_bytes(&[3; 32]);

        let inscribe_op1 = create_test_inscribe_op(&signing_key1);
        let inscribe_op2 = create_test_inscribe_op(&signing_key2);

        let mantle_tx = create_test_mantle_tx(vec![
            Op::ChannelInscribe(inscribe_op1),
            Op::ChannelInscribe(inscribe_op2),
        ]);

        let tx_hash = mantle_tx.hash();
        let sig1 = signing_key1.sign_payload(&tx_hash.as_signing_bytes());
        let sig2 = wrong_key.sign_payload(&tx_hash.as_signing_bytes()); // Wrong signature

        let result = SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::Ed25519Sig(sig1), OpProof::Ed25519Sig(sig2)],
        );

        assert!(matches!(
            result,
            Err(VerificationError::InvalidSignature { op_index: 1 })
        ));
    }

    #[test]
    fn test_signed_mantle_tx_deserialize_with_valid_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        let tx_hash = mantle_tx.hash();
        let signature = signing_key.sign_payload(&tx_hash.as_signing_bytes());

        let signed_tx =
            SignedMantleTx::new(mantle_tx, vec![OpProof::Ed25519Sig(signature)]).unwrap();

        // Serialize and deserialize
        let serialized = serde_json::to_string(&signed_tx).unwrap();
        let deserialized: Result<SignedMantleTx, _> = serde_json::from_str(&serialized);

        assert!(deserialized.is_ok());
        assert_eq!(deserialized.unwrap(), signed_tx);
    }

    #[test]
    fn test_signed_mantle_tx_deserialize_with_missing_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        let helper = SignedMantleTx {
            mantle_tx,
            ops_proofs: vec![],
        };

        let serialized = serde_json::to_string(&helper).unwrap();
        let deserialized: Result<SignedMantleTx, _> = serde_json::from_str(&serialized);

        assert!(deserialized.is_err());
        let err_msg = deserialized.unwrap_err().to_string();
        assert_eq!(
            err_msg,
            "The number of proofs (0) does not match the number of operations (1)"
        );
    }

    #[test]
    fn test_signed_mantle_tx_deserialize_with_invalid_signature() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let wrong_key = Ed25519Key::from_bytes(&[2; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        let tx_hash = mantle_tx.hash();
        let wrong_signature = wrong_key.sign_payload(&tx_hash.as_signing_bytes());

        let helper = SignedMantleTx {
            mantle_tx,
            ops_proofs: vec![OpProof::Ed25519Sig(wrong_signature)],
        };

        let serialized = serde_json::to_string(&helper).unwrap();
        let deserialized: Result<SignedMantleTx, _> = serde_json::from_str(&serialized);

        assert!(deserialized.is_err());
        let err_msg = deserialized.unwrap_err().to_string();
        assert!(err_msg.contains("Invalid signature"));
    }

    #[test]
    fn test_signed_mantle_tx_new_proof_count_mismatch() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);
        let tx_hash = mantle_tx.hash();
        let signature = signing_key.sign_payload(&tx_hash.as_signing_bytes());

        // Test too few proofs
        let result = SignedMantleTx::new(mantle_tx.clone(), vec![]);
        assert!(matches!(
            result,
            Err(VerificationError::ProofCountMismatch {
                ops_count: 1,
                proofs_count: 0
            })
        ));

        // Test too many proofs
        let result = SignedMantleTx::new(
            mantle_tx,
            vec![
                OpProof::Ed25519Sig(signature),
                OpProof::Ed25519Sig(signature),
            ],
        );
        assert!(matches!(
            result,
            Err(VerificationError::ProofCountMismatch {
                ops_count: 1,
                proofs_count: 2
            })
        ));
    }

    #[test]
    fn helper_backed_verification_accepts_valid_channel_withdraw() {
        let channel_id = ChannelId::from([8u8; 32]);
        let key0 = Ed25519Key::from_bytes(&[8; 32]);
        let key1 = Ed25519Key::from_bytes(&[9; 32]);
        let signed_tx = create_withdraw_tx(channel_id, &[&key0, &key1]);

        let helper = TestOperationVerificationHelper::new(
            [(channel_id, 2)],
            [
                ((channel_id, 0), key0.public_key()),
                ((channel_id, 1), key1.public_key()),
            ],
        );

        assert!(signed_tx.verify_ops_proofs_with_helper(&helper).is_ok());
    }

    #[test]
    fn helper_backed_verification_rejects_missing_channel() {
        let channel_id = ChannelId::from([10u8; 32]);
        let key0 = Ed25519Key::from_bytes(&[0; 32]);
        let signed_tx = create_withdraw_tx(channel_id, &[&key0]);

        let helper = TestOperationVerificationHelper::new([], []);

        let verification_result = signed_tx.verify_ops_proofs_with_helper(&helper);
        assert_eq!(
            verification_result,
            Err(VerificationError::ChannelNotFound { channel_id })
        );
    }

    #[test]
    fn helper_backed_verification_rejects_missing_key() {
        let channel_id = ChannelId::from([10u8; 32]);
        let key0 = Ed25519Key::from_bytes(&[0; 32]);
        let key1 = Ed25519Key::from_bytes(&[1; 32]);
        let signed_tx = create_withdraw_tx(channel_id, &[&key0, &key1]);

        let helper = TestOperationVerificationHelper::new(
            [(channel_id, 2)],
            [((channel_id, 0), key0.public_key())],
        );

        let verification_result = signed_tx.verify_ops_proofs_with_helper(&helper);
        assert_eq!(
            verification_result,
            Err(VerificationError::KeyNotFound {
                channel_id,
                key_index: 1
            })
        );
    }

    #[test]
    fn helper_backed_verification_rejects_not_enough_signatures() {
        let channel_id = ChannelId::from([10u8; 32]);
        let key0 = Ed25519Key::from_bytes(&[0; 32]);
        let signed_tx = create_withdraw_tx(channel_id, &[&key0]);

        let helper = TestOperationVerificationHelper::new(
            [(channel_id, 2)],
            [((channel_id, 0), key0.public_key())],
        );

        let verification_result = signed_tx.verify_ops_proofs_with_helper(&helper);
        assert_eq!(
            verification_result,
            Err(VerificationError::ChannelWithdrawProofNotEnoughSignatures {
                op_index: 0,
                actual: 1,
                required: 2
            })
        );
    }

    #[test]
    fn helper_backed_verification_rejects_invalid_signature() {
        let channel_id = ChannelId::from([10u8; 32]);
        let expected_key = Ed25519Key::from_bytes(&[0; 32]);
        let wrong_key = Ed25519Key::from_bytes(&[9; 32]);
        let signed_tx = create_withdraw_tx(channel_id, &[&wrong_key]);

        let helper = TestOperationVerificationHelper::new(
            [(channel_id, 1)],
            [((channel_id, 0), expected_key.public_key())],
        );

        let verification_result = signed_tx.verify_ops_proofs_with_helper(&helper);
        assert_eq!(
            verification_result,
            Err(VerificationError::ChannelWithdrawProofInvalidSignature {
                op_index: 0,
                signature_index: 0
            })
        );
    }
}
