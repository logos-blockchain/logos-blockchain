use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

use ark_ff::PrimeField as _;
use blake2::digest::Mac;
use bytes::Bytes;
use lb_core_macros::NomCodec;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::Ed25519PublicKey;
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    crypto::{Digest as _, Hash, Hasher},
    mantle::{
        AuthenticatedMantleTx, StorageSize, Transaction, TransactionHasher, Value,
        channel::Channels,
        gas::{Gas, GasCalculator, GasConstants, GasCost, GasOverflow, GasPrice},
        nom::{NomDecode as _, NomEncode as _},
        ops::{
            Op, OpProof,
            channel::{ChannelId, ChannelKeyIndex},
            transfer::TransferOp,
        },
        transactions::{
            MAX_OPS_PER_TX, Ops,
            codec::{
                decode_signed_mantle_tx, encode_signed_mantle_tx, predict_signed_mantle_tx_size,
            },
            genesis_tx::{GENESIS_EXECUTION_GAS_PRICE, GENESIS_STORAGE_GAS_PRICE},
            states::{Preverified, Unverified, VerificationState, Verified},
        },
    },
    proofs::{
        channel_multi_sig_proof::ChannelMultiSigProof,
        leader_claim_proof::{LeaderClaimProof as _, LeaderClaimPublic},
    },
    utils::serde_bytes_newtype,
};

/// The hash of a transaction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, PartialOrd, Ord)]
pub struct TxHash(pub Hash);
serde_bytes_newtype!(TxHash, 32);

impl From<Hash> for TxHash {
    fn from(hash: Hash) -> Self {
        Self(hash)
    }
}

impl From<TxHash> for Hash {
    fn from(hash: TxHash) -> Self {
        hash.0
    }
}

impl AsRef<Hash> for TxHash {
    fn as_ref(&self) -> &Hash {
        &self.0
    }
}

impl From<TxHash> for Bytes {
    fn from(tx_hash: TxHash) -> Self {
        Self::copy_from_slice(&tx_hash.0)
    }
}

impl TxHash {
    /// For testing purposes
    #[cfg(test)]
    pub fn random(mut rng: impl rand::RngCore) -> Self {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    #[must_use]
    pub fn as_signing_bytes(&self) -> Bytes {
        Bytes::from(self.0.to_vec())
    }

    #[must_use]
    pub fn to_fr(&self) -> Fr {
        Fr::from_le_bytes_mod_order(&self.0)
    }
}

#[derive(Serialize, Deserialize)]
struct MantleTxDeSerImpl {
    pub ops: Ops,
}

#[derive(Debug, Clone, Default)]
pub struct MantleTxContext {
    pub gas_context: MantleTxGasContext,
    pub leader_reward_amount: Value,
}

#[derive(Debug, Clone, Default)]
pub struct MantleTxGasContext {
    transfer_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
    configuration_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
    gas_prices: GasPrices,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GasPrices {
    pub execution_base_gas_price: GasPrice,
    pub storage_gas_price: GasPrice,
}

impl GasPrices {
    #[must_use]
    pub fn new(execution: u64, storage: u64) -> Self {
        Self {
            execution_base_gas_price: execution.into(),
            storage_gas_price: storage.into(),
        }
    }
}

impl Default for GasPrices {
    fn default() -> Self {
        Self {
            execution_base_gas_price: GENESIS_EXECUTION_GAS_PRICE,
            storage_gas_price: GENESIS_STORAGE_GAS_PRICE,
        }
    }
}

impl MantleTxGasContext {
    #[must_use]
    pub const fn new(
        transfer_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
        configuration_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
        gas_prices: GasPrices,
    ) -> Self {
        Self {
            transfer_thresholds,
            configuration_thresholds,
            gas_prices,
        }
    }

    #[must_use]
    pub fn transfer_threshold(&self, channel_id: &ChannelId) -> Option<ChannelKeyIndex> {
        self.transfer_thresholds.get(channel_id).copied()
    }

    #[must_use]
    pub fn configuration_threshold(&self, channel_id: &ChannelId) -> Option<ChannelKeyIndex> {
        self.configuration_thresholds.get(channel_id).copied()
    }

    #[must_use]
    pub fn from_channels(value: &Channels, base_prices: GasPrices) -> Self {
        let transfer_thresholds = value
            .channels
            .iter()
            .map(|(channel_id, channel)| (*channel_id, channel.transfer_threshold))
            .collect();
        let configuration_thresholds = value
            .channels
            .iter()
            .map(|(channel_id, channel)| (*channel_id, channel.configuration_threshold))
            .collect();
        Self::new(transfer_thresholds, configuration_thresholds, base_prices)
    }

    #[must_use]
    pub fn get_gas_prices(&self) -> GasPrices {
        self.gas_prices.clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, NomCodec)]
pub struct MantleTx(pub Ops);

impl StorageSize for MantleTx {
    fn storage_size(&self) -> usize {
        self.encode().len()
    }
}

impl From<MantleTxDeSerImpl> for MantleTx {
    fn from(MantleTxDeSerImpl { ops }: MantleTxDeSerImpl) -> Self {
        Self(ops)
    }
}

impl From<MantleTx> for MantleTxDeSerImpl {
    fn from(MantleTx(ops): MantleTx) -> Self {
        Self { ops }
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
            let bytes = self.encode();
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
            Self::decode(&bytes)
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
        let execution_gas_cost =
            GasCost::calculate(execution_gas?, context.gas_prices.execution_base_gas_price)?;
        let storage_gas_cost = self.storage_gas_cost(context)?;

        execution_gas_cost.checked_add(storage_gas_cost)
    }

    fn storage_gas_cost(&self, context: &Self::Context) -> Result<GasCost, GasOverflow> {
        GasCost::calculate(
            self.storage_gas_consumption(context)?,
            context.gas_prices.storage_gas_price,
        )
    }

    fn execution_gas_consumption<Constants: GasConstants>(
        &self,
        context: &Self::Context,
    ) -> Result<Gas, GasOverflow> {
        self.ops()
            .iter()
            .map(|op| contextual_op_execution_gas::<Constants>(op, context))
            .try_fold(Gas::from(0), |total, gas| total.checked_add(gas?))
    }

    fn storage_gas_consumption(&self, context: &Self::Context) -> Result<Gas, GasOverflow> {
        Ok(self.signed_serialized_size(context).into())
    }
}

fn contextual_op_execution_gas<Constants: GasConstants>(
    op: &Op,
    context: &MantleTxGasContext,
) -> Result<Gas, GasOverflow> {
    let multiplier = match op {
        // Existing channels require the current configuration threshold. A new
        // channel is created just-in-time and does not verify any signatures.
        Op::ChannelConfig(operation) => context
            .configuration_threshold(&operation.channel)
            .unwrap_or(0),
        Op::ChannelWithdraw(operation) => context
            .transfer_threshold(&operation.channel_id)
            .unwrap_or(0),
        Op::ChannelTransfer(operation) => context
            .transfer_threshold(&operation.channel_id)
            .unwrap_or(0),
        _ => return Ok(op.execution_gas::<Constants>()),
    };

    op.execution_gas::<Constants>()
        .checked_mul(Value::from(multiplier))
}

impl MantleTx {
    #[must_use]
    pub fn signed_serialized_size(&self, context: &<Self as GasCalculator>::Context) -> u64 {
        predict_signed_mantle_tx_size(self, context) as u64
    }

    #[must_use]
    pub fn transfers(&self) -> Vec<TransferOp> {
        let mut transfers: Vec<TransferOp> = vec![];
        for op in self.ops() {
            if let Op::Transfer(transfer_op) = op {
                transfers.push(transfer_op.clone());
            }
        }
        transfers
    }

    #[must_use]
    pub const fn ops(&self) -> &Ops {
        &self.0
    }
}

static MANTLE_TXHASH_V1_BYTES: LazyLock<Vec<u8>> = LazyLock::new(|| b"MANTLE_TXHASH_V1".to_vec());

impl Transaction for MantleTx {
    const HASHER: TransactionHasher<Self> = |tx| {
        let bytes: [u8; 32] = Hasher::digest(tx.as_signing()).into();
        TxHash::from(bytes)
    };
    type Hash = TxHash;

    fn as_signing(&self) -> Vec<u8> {
        // constant and structure as defined in the Mantle specification:
        // https://www.notion.so/nomos-tech/v1-3-Mantle-Specification-31e261aa09df818f9327ee87e5a6d433#31e261aa09df80aea7cff4eb98d61b6e
        let mut buffer = MANTLE_TXHASH_V1_BYTES.to_vec();
        buffer.extend(self.encode());
        buffer
    }
}

impl<State: VerificationState> From<SignedMantleTx<State>> for MantleTx {
    fn from(signed_tx: SignedMantleTx<State>) -> Self {
        signed_tx.mantle_tx
    }
}

pub type OpsProofs = UpperBoundedVec<OpProof, MAX_OPS_PER_TX>;

// Deserializing here is dangerous, as it bypasses the verification without
// confirmation.
// TODO: Split entity into a system that allows for verification in different
// stages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedMantleTx<State: VerificationState> {
    pub mantle_tx: MantleTx,
    // TODO: make this more efficient
    pub ops_proofs: OpsProofs,
    state: State,
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
        "Not enough signatures in ChannelMultiSigProof at index {op_index}: got {actual}, required {required}"
    )]
    ChannelMultiSigProofNotEnoughSignatures {
        op_index: usize,
        actual: usize,
        required: ChannelKeyIndex,
    },
    #[error("Duplicate signature indices in ChannelMultiSigProof at index {op_index}")]
    ChannelMultiSigProofDuplicateIndices { op_index: usize },
    #[error(
        "Invalid signature in ChannelMultiSigProof at index {op_index} for signature index {signature_index}"
    )]
    ChannelMultiSigProofInvalidSignature {
        op_index: usize,
        signature_index: usize,
    },
}

pub trait OperationVerificationHelper {
    fn get_channel_transfer_threshold(
        &self,
        channel_id: &ChannelId,
    ) -> Result<ChannelKeyIndex, VerificationError>;

    fn get_key_from_channel_at_index(
        &self,
        channel_id: &ChannelId,
        key_index: &ChannelKeyIndex,
    ) -> Result<Ed25519PublicKey, VerificationError>;
}

impl<State: VerificationState> SignedMantleTx<State> {
    fn with_state<T: VerificationState>(self, state: T) -> SignedMantleTx<T> {
        let Self {
            mantle_tx,
            ops_proofs,
            ..
        } = self;
        SignedMantleTx::<T> {
            mantle_tx,
            ops_proofs,
            state,
        }
    }

    fn gas_storage_size(&self) -> u64 {
        encode_signed_mantle_tx(self).len() as u64
    }
}

enum OpVerificationResult {
    Verified,
    StatefulRequired,
}

impl SignedMantleTx<Unverified> {
    pub fn new(mantle_tx: MantleTx, ops_proofs: OpsProofs) -> Self {
        Self {
            mantle_tx,
            ops_proofs,
            state: Unverified,
        }
    }

    fn ensure_one_proof_per_op(&self) -> Result<(), VerificationError> {
        if self.mantle_tx.ops().len() == self.ops_proofs.len() {
            return Ok(());
        }

        Err(VerificationError::ProofCountMismatch {
            ops_count: self.mantle_tx.ops().len(),
            proofs_count: self.ops_proofs.len(),
        })
    }

    // TODO: Might drop proofs after verification. This TODO is carried over from
    // the original code.
    fn verify_stateless_op(
        op_index: usize,
        op: &Op,
        proof: &OpProof,
        tx_hash: &TxHash,
        tx_hash_bytes: &Bytes,
    ) -> Result<OpVerificationResult, VerificationError> {
        match (op, proof) {
            (Op::ChannelInscribe(inscribe_op), OpProof::Ed25519Sig(sig)) => {
                // Inscription operations require an Ed25519 signature
                inscribe_op
                    .signer
                    .verify(tx_hash_bytes.as_ref(), sig)
                    .map(|_| OpVerificationResult::Verified)
                    .map_err(|_| VerificationError::InvalidSignature { op_index })
            }
            (op @ Op::ChannelInscribe(_), _) => Err(VerificationError::IncorrectProofType {
                op_type: op.as_str(),
                op_index,
            }),
            // -----
            (Op::LeaderClaim(leader_claim_op), OpProof::PoC(poc)) => {
                let is_verified = poc.verify(&LeaderClaimPublic {
                    voucher_nullifier: leader_claim_op.voucher_nullifier.into(),
                    voucher_root: leader_claim_op.rewards_root.into(),
                    mantle_tx_hash: tx_hash.to_fr(),
                });

                if is_verified {
                    Ok(OpVerificationResult::Verified)
                } else {
                    Err(VerificationError::InvalidProofOfClaim { op_index })
                }
            }
            (op @ Op::LeaderClaim(_), _) => Err(VerificationError::IncorrectProofType {
                op_type: op.as_str(),
                op_index,
            }),
            // -----
            // TODO: Fill in with the remaining operations
            // We probably benefit on a "match valid and verify/classify", else InCorrectProofType
            _ => Ok(OpVerificationResult::StatefulRequired),
        }
    }

    fn verify_stateless_ops(&self) -> Result<Vec<usize>, VerificationError> {
        let mut pending = vec![];

        let tx_hash = self.hash();
        let tx_hash_bytes = tx_hash.as_signing_bytes();
        for (op_index, (op, proof)) in self
            .mantle_tx
            .ops()
            .iter()
            .zip(self.ops_proofs.iter())
            .enumerate()
        {
            let op_verification_result =
                Self::verify_stateless_op(op_index, op, proof, &tx_hash, &tx_hash_bytes)?;
            match op_verification_result {
                OpVerificationResult::Verified => {}
                OpVerificationResult::StatefulRequired => {
                    pending.push(op_index);
                }
            }
        }

        Ok(pending)
    }

    fn into_preverified(self, pending: Vec<usize>) -> SignedMantleTx<Preverified> {
        let preverified_state = Preverified { pending };
        self.with_state(preverified_state)
    }

    pub fn preverify(self) -> Result<SignedMantleTx<Preverified>, VerificationError> {
        self.ensure_one_proof_per_op()?;
        let pending = self.verify_stateless_ops()?;
        Ok(self.into_preverified(pending))
    }
}

impl SignedMantleTx<Preverified> {
    fn pending_ops_with_proofs(&self) -> impl Iterator<Item = (usize, &Op, &OpProof)> {
        self.state.pending.iter().map(|&op_index| {
            let op = &self.mantle_tx.ops()[op_index];
            let proof = &self.ops_proofs[op_index];
            (op_index, op, proof)
        })
    }

    fn verify_stateful_op(
        op_index: usize,
        op: &Op,
        proof: &OpProof,
        tx_hash_bytes: &Bytes,
        helper: &impl OperationVerificationHelper,
    ) -> Result<(), VerificationError> {
        #[expect(
            clippy::single_match_else,
            reason = "Clearer and follows the pattern of verify_ops_proofs."
        )]
        match (op, proof) {
            (Op::ChannelWithdraw(channel_withdraw_op), OpProof::ChannelMultiSigProof(proof)) => {
                verify_channel_multi_sig(
                    &channel_withdraw_op.channel_id,
                    proof,
                    &tx_hash_bytes,
                    helper,
                    op_index,
                )
            }
            (
                Op::ChannelTransfer(channel_transfer_op),
                OpProof::ChannelMultiSigProof(proof),
            ) => {
                verify_channel_multi_sig(
                    &channel_transfer_op.channel_id,
                    proof,
                    &tx_hash_bytes,
                    helper,
                    op_index,
                )
            }
            // TODO: Explicitly handle all operations that should have been statelessly verified
            // (instead of _).   This includes matching each op with its proof type.
            _ => {
                unreachable!("All stateless verification should have been done in preverify.");
            }
        }
    }

    fn verify_stateful_ops(
        &self,
        helper: &impl OperationVerificationHelper,
    ) -> Result<(), VerificationError> {
        let tx_hash = self.hash();
        let tx_hash_bytes = tx_hash.as_signing_bytes();

        for (op_index, op, proof) in self.pending_ops_with_proofs() {
            Self::verify_stateful_op(op_index, op, proof, &tx_hash_bytes, helper)?;
        }

        Ok(())
    }

    fn into_verified(self) -> SignedMantleTx<Verified> {
        self.with_state(Verified)
    }

    pub fn verify(
        self,
        helper: &impl OperationVerificationHelper,
    ) -> Result<SignedMantleTx<Verified>, VerificationError> {
        self.verify_stateful_ops(helper)?;
        Ok(self.into_verified())
    }
}

impl SignedMantleTx<Verified> {
    /// Create a [`SignedMantleTx`] without verifying proofs.
    /// This should only be used for [`GenesisTx`] or in tests.
    #[doc(hidden)]
    #[must_use]
    pub const fn new_trusted(mantle_tx: MantleTx, ops_proofs: OpsProofs) -> Self {
        Self {
            mantle_tx,
            ops_proofs,
            state: Verified,
        }
    }
}

fn verify_channel_multi_sig(
    channel_id: &ChannelId,
    proof: &ChannelMultiSigProof,
    tx_hash_bytes: &Bytes,
    helper: &impl OperationVerificationHelper,
    op_index: usize,
) -> Result<(), VerificationError> {
    let transfer_threshold = helper.get_channel_transfer_threshold(channel_id)?;

    let signatures = proof.signatures();
    let signatures_len = signatures.len();
    if signatures_len != transfer_threshold as usize {
        return Err(VerificationError::ChannelMultiSigProofNotEnoughSignatures {
            op_index,
            actual: signatures_len,
            required: transfer_threshold,
        });
    }

    let indices_set = signatures
        .iter()
        .map(|signature| signature.channel_key_index)
        .collect::<HashSet<_>>();
    let indices_set_len = indices_set.len();
    if indices_set_len != signatures_len {
        return Err(VerificationError::ChannelMultiSigProofDuplicateIndices { op_index });
    }

    for (i, signature) in signatures.iter().enumerate() {
        let public_key =
            helper.get_key_from_channel_at_index(channel_id, &signature.channel_key_index)?;
        if let Err(_error) = public_key.verify(tx_hash_bytes.as_ref(), &signature.signature) {
            return Err(VerificationError::ChannelMultiSigProofInvalidSignature {
                op_index,
                signature_index: i,
            });
        }
    }

    Ok(())
}

impl Transaction for SignedMantleTx {
    const HASHER: TransactionHasher<Self> = |tx| {
        let bytes: [u8; 32] = Hasher::digest(tx.as_signing()).into();
        TxHash::from(bytes)
    };
    type Hash = TxHash;

    fn as_signing(&self) -> Vec<u8> {
        self.mantle_tx.as_signing()
    }
}

impl AuthenticatedMantleTx for SignedMantleTx {
    type Context = GasPrices;

    fn mantle_tx(&self) -> &MantleTx {
        &self.mantle_tx
    }

    fn ops_with_proof(&self) -> impl Iterator<Item = (&Op, &OpProof)> {
        self.mantle_tx.ops().iter().zip(self.ops_proofs.iter())
    }

    fn total_gas_cost<Constants: GasConstants>(
        &self,
        context: <Self as AuthenticatedMantleTx>::Context,
    ) -> Result<GasCost, GasOverflow> {
        GasCalculator::total_gas_cost::<Constants>(&self, &context)
    }

    fn storage_gas_cost(
        &self,
        context: <Self as AuthenticatedMantleTx>::Context,
    ) -> Result<GasCost, GasOverflow> {
        GasCalculator::storage_gas_cost(&self, &context)
    }

    fn execution_gas_consumption<Constants: GasConstants>(
        &self,
        context: <Self as AuthenticatedMantleTx>::Context,
    ) -> Result<Gas, GasOverflow> {
        GasCalculator::execution_gas_consumption::<Constants>(&self, &context)
    }

    fn storage_gas_consumption(
        &self,
        context: <Self as AuthenticatedMantleTx>::Context,
    ) -> Result<Gas, GasOverflow> {
        GasCalculator::storage_gas_consumption(&self, &context)
    }

    fn verify_ops_proofs_with_helper(
        &self,
        operation_verification_helper: &impl OperationVerificationHelper,
    ) -> Result<(), VerificationError> {
        Self::verify_ops_proofs_with_helper(self, operation_verification_helper)
    }
}

impl GasCalculator for SignedMantleTx {
    type Context = GasPrices;

    fn total_gas_cost<Constants: GasConstants>(
        &self,
        context: &Self::Context,
    ) -> Result<GasCost, GasOverflow> {
        let execution_gas = GasCalculator::execution_gas_consumption::<Constants>(&self, context)?;
        let execution_gas_cost =
            GasCost::calculate(execution_gas, context.execution_base_gas_price)?;
        let storage_gas_cost = GasCalculator::storage_gas_cost(self, context)?;

        execution_gas_cost.checked_add(storage_gas_cost)
    }

    fn storage_gas_cost(&self, context: &Self::Context) -> Result<GasCost, GasOverflow> {
        let storage_gas = GasCalculator::storage_gas_consumption(&self, context)?;
        GasCost::calculate(storage_gas, context.storage_gas_price)
    }

    fn execution_gas_consumption<Constants: GasConstants>(
        &self,
        _context: &Self::Context,
    ) -> Result<Gas, GasOverflow> {
        self.mantle_tx
            .ops()
            .iter()
            .zip(self.ops_proofs.iter())
            .map(|(op, proof)| signed_op_execution_gas::<Constants>(op, proof))
            .try_fold(Gas::from(0), |total, gas| total.checked_add(gas?))
    }

    fn storage_gas_consumption(&self, _context: &Self::Context) -> Result<Gas, GasOverflow> {
        Ok(self.gas_storage_size().into())
    }
}

fn signed_op_execution_gas<Constants: GasConstants>(
    op: &Op,
    proof: &OpProof,
) -> Result<Gas, GasOverflow> {
    // Signed transactions are charged after execution, when a config op may
    // already have replaced the channel threshold. The validated proof length
    // preserves the threshold that was actually checked.
    let signature_count = match (op, proof) {
        (
            Op::ChannelConfig(_) | Op::ChannelWithdraw(_) | Op::ChannelTransfer(_),
            OpProof::ChannelMultiSigProof(proof),
        ) => proof.signatures().len(),
        _ => return Ok(op.execution_gas::<Constants>()),
    };
    let multiplier = Value::try_from(signature_count)
        .expect("channel multi-signature proofs are bounded to u16::MAX signatures");

    op.execution_gas::<Constants>().checked_mul(multiplier)
}

impl StorageSize for SignedMantleTx {
    fn storage_size(&self) -> usize {
        self.gas_storage_size() as usize
    }
}

impl Serialize for SignedMantleTx {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            #[derive(Serialize)]
            pub struct SignedMantleTxHelper<'a> {
                pub mantle_tx: &'a MantleTx,
                pub ops_proofs: &'a [OpProof],
            }
            SignedMantleTxHelper {
                mantle_tx: &self.mantle_tx,
                ops_proofs: &self.ops_proofs,
            }
            .serialize(serializer)
        } else {
            encode_signed_mantle_tx(self).serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for SignedMantleTx {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            #[derive(Deserialize)]
            struct SignedMantleTxHelper {
                mantle_tx: MantleTx,
                ops_proofs: OpsProofs,
            }

            let helper = SignedMantleTxHelper::deserialize(deserializer)?;
            Self::new(helper.mantle_tx, helper.ops_proofs).map_err(serde::de::Error::custom)
        } else {
            let bytes: Vec<u8> = Deserialize::deserialize(deserializer)?;
            decode_signed_mantle_tx(bytes.as_slice())
                .map(|(_, tx)| tx)
                .map_err(serde::de::Error::custom)
        }
    }
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};

    use super::*;
    use crate::{
        mantle::{
            NoteId,
            gas::MainnetGasConstants,
            ledger::Inputs,
            ops::channel::{
                config::ChannelConfigOp, deposit::DepositOp, inscribe::InscriptionOp,
                withdraw::ChannelWithdrawOp,
            },
        },
        proofs::channel_multi_sig_proof::{IndexedSignature, IndexedSignatures},
    };

    fn create_test_mantle_tx(ops: Vec<Op>) -> MantleTx {
        MantleTx(Ops::new_unchecked(ops))
    }

    fn create_test_inscribe_op(signing_key: &Ed25519Key) -> InscriptionOp {
        InscriptionOp {
            channel_id: [0; 32].into(),
            inscription: [1, 2, 3].into(),
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
        fn get_channel_transfer_threshold(
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

    fn create_channel_multi_sig_proof(
        tx_hash: &TxHash,
        signing_keys: &[&Ed25519Key],
    ) -> ChannelMultiSigProof {
        let signatures: IndexedSignatures = signing_keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                IndexedSignature::new(
                    index as ChannelKeyIndex,
                    key.sign_payload(tx_hash.as_signing_bytes().as_ref()),
                )
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        ChannelMultiSigProof::try_new(signatures).unwrap()
    }

    fn create_withdraw_tx(channel_id: ChannelId, signing_keys: &[&Ed25519Key]) -> SignedMantleTx {
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelWithdraw(ChannelWithdrawOp {
            channel_id,
            inputs: Inputs::new([NoteId(Fr::from(0u64))]),
        })]);

        let tx_hash = mantle_tx.hash();
        let proof = create_channel_multi_sig_proof(&tx_hash, signing_keys);

        SignedMantleTx::new(mantle_tx, [OpProof::ChannelMultiSigProof(proof)].into()).unwrap()
    }

    fn create_config_op(channel: ChannelId, signing_key: &Ed25519Key) -> ChannelConfigOp {
        ChannelConfigOp {
            channel,
            keys: signing_key.public_key().into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
            transfer_threshold: 1,
        }
    }

    fn create_deposit_op(channel_id: ChannelId) -> DepositOp {
        DepositOp {
            channel_id,
            inputs: Inputs::new([NoteId(Fr::from(0u64))]),
            metadata: [].into(),
        }
    }

    fn create_withdraw_op(channel_id: ChannelId) -> ChannelWithdrawOp {
        ChannelWithdrawOp {
            channel_id,
            inputs: Inputs::new([NoteId(Fr::from(0u64))]),
        }
    }

    #[test]
    fn unsigned_execution_gas_uses_channel_thresholds() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);

        let config_channel = ChannelId::from([2; 32]);
        let deposit_channel = ChannelId::from([3; 32]);
        let withdraw_channel = ChannelId::from([4; 32]);

        let mantle_tx = create_test_mantle_tx(vec![
            Op::ChannelConfig(create_config_op(config_channel, &signing_key)),
            Op::ChannelDeposit(create_deposit_op(deposit_channel)),
            Op::ChannelWithdraw(create_withdraw_op(withdraw_channel)),
        ]);

        let config_threshold = 3;
        let transfer_threshold = 2;
        let context = MantleTxGasContext::new(
            [(withdraw_channel, transfer_threshold)].into(),
            [(config_channel, config_threshold)].into(),
            GasPrices::new(1, 0),
        );

        let gas =
            GasCalculator::execution_gas_consumption::<MainnetGasConstants>(&mantle_tx, &context)
                .unwrap();

        let expected_config_gas = u64::from(config_threshold) * 56;
        let expected_deposit_gas = 590;
        let expected_withdraw_gas = u64::from(transfer_threshold) * 56;
        let expected_total_gas = expected_config_gas + expected_deposit_gas + expected_withdraw_gas;

        assert_eq!(gas.into_inner(), expected_total_gas);
    }

    #[test]
    fn signed_execution_gas_uses_multi_signature_proof_lengths() {
        let config_keys = [
            Ed25519Key::from_bytes(&[1; 32]),
            Ed25519Key::from_bytes(&[2; 32]),
            Ed25519Key::from_bytes(&[3; 32]),
        ];
        let withdraw_keys = [
            Ed25519Key::from_bytes(&[4; 32]),
            Ed25519Key::from_bytes(&[5; 32]),
        ];
        let config_signers = [&config_keys[0], &config_keys[1], &config_keys[2]];
        let withdraw_signers = [&withdraw_keys[0], &withdraw_keys[1]];

        let config_channel = ChannelId::from([6; 32]);
        let deposit_channel = ChannelId::from([7; 32]);
        let withdraw_channel = ChannelId::from([8; 32]);

        let mantle_tx = create_test_mantle_tx(vec![
            Op::ChannelConfig(create_config_op(config_channel, &config_keys[0])),
            Op::ChannelDeposit(create_deposit_op(deposit_channel)),
            Op::ChannelWithdraw(create_withdraw_op(withdraw_channel)),
        ]);

        let tx_hash = mantle_tx.hash();
        let config_proof = create_channel_multi_sig_proof(&tx_hash, &config_signers);
        let deposit_proof = ZkKey::multi_sign(&[], &tx_hash.to_fr()).unwrap();
        let withdraw_proof = create_channel_multi_sig_proof(&tx_hash, &withdraw_signers);

        let signed_tx = SignedMantleTx::new(
            mantle_tx,
            [
                OpProof::ChannelMultiSigProof(config_proof),
                OpProof::ZkSig(deposit_proof),
                OpProof::ChannelMultiSigProof(withdraw_proof),
            ]
            .into(),
        )
        .unwrap();

        let gas_prices = GasPrices::new(1, 0);
        let gas = GasCalculator::execution_gas_consumption::<MainnetGasConstants>(
            &signed_tx,
            &gas_prices,
        )
        .unwrap();

        let expected_config_gas = config_keys.len() as u64 * 56;
        let expected_deposit_gas = 590;
        let expected_withdraw_gas = withdraw_keys.len() as u64 * 56;
        let expected_total_gas = expected_config_gas + expected_deposit_gas + expected_withdraw_gas;

        assert_eq!(gas.into_inner(), expected_total_gas);
    }

    #[test]
    fn test_signed_mantle_tx_new_with_valid_inscribe_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        // Sign the transaction hash
        let tx_hash = mantle_tx.hash();
        let signature = signing_key.sign_payload(&tx_hash.as_signing_bytes());

        let result = SignedMantleTx::new(mantle_tx, [OpProof::Ed25519Sig(signature)].into());

        assert!(result.is_ok());
    }

    #[test]
    fn test_signed_mantle_tx_new_missing_inscribe_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);
        let result = SignedMantleTx::new(mantle_tx, OpsProofs::empty());

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

        let result = SignedMantleTx::new(mantle_tx, [OpProof::Ed25519Sig(signature)].into());

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
        let zk_sig = OpProof::ZkSig(ZkKey::multi_sign(&[], &tx_hash.to_fr()).unwrap());
        let result = SignedMantleTx::new(mantle_tx, [zk_sig].into());

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
            [OpProof::Ed25519Sig(sig1), OpProof::Ed25519Sig(sig2)].into(),
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
            [OpProof::Ed25519Sig(sig1), OpProof::Ed25519Sig(sig2)].into(),
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
            SignedMantleTx::new(mantle_tx, [OpProof::Ed25519Sig(signature)].into()).unwrap();

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
            ops_proofs: OpsProofs::empty(),
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
            ops_proofs: [OpProof::Ed25519Sig(wrong_signature)].into(),
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
        let result = SignedMantleTx::new(mantle_tx.clone(), OpsProofs::empty());
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
            [
                OpProof::Ed25519Sig(signature),
                OpProof::Ed25519Sig(signature),
            ]
            .into(),
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
            Err(VerificationError::ChannelMultiSigProofNotEnoughSignatures {
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
            Err(VerificationError::ChannelMultiSigProofInvalidSignature {
                op_index: 0,
                signature_index: 0
            })
        );
    }
}
