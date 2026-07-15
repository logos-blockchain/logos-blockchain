use std::marker::PhantomData;

use bytes::Bytes;
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_key_management_system_keys::keys::Ed25519PublicKey;
use nom::{Parser as _, combinator::all_consuming};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    crypto::{Digest as _, Hasher},
    mantle::{
        MantleTx, Value, VerificationError,
        channel::Channels,
        gas::{Gas, GasCalculator, GasConstants, GasCost, GasOverflow, GasPrice},
        ledger::{Declarations, Operation as _, Utxos},
        ops::{
            Op, OpProof,
            channel::{
                ChannelId, ChannelKeyIndex, channel_transfer::ChannelTransferValidationContext,
                config::ChannelConfigValidationContext, deposit::DepositValidationContext,
                inscribe::InscriptionValidationContext, verification::verify_channel_multi_sig,
                withdraw::WithdrawValidationContext,
            },
            leader_claim::{LeaderClaimValidationContext, RewardsRoot, VoucherNullifier},
            sdp::{
                SDPActiveValidationContext, SDPDeclareValidationContext,
                SDPWithdrawValidationContext,
            },
            transfer::TransferValidationContext,
        },
        traits::{
            Hashable, MantleTxWithProofs, PreverifiedMantleTx, StorageSize, hashable,
            mantle_tx::OpWithProof,
        },
        transactions::{
            OpsProofs,
            codec::{decode_signed_mantle_tx, encode_signed_mantle_tx},
            genesis_tx::{GENESIS_EXECUTION_GAS_PRICE, GENESIS_STORAGE_GAS_PRICE},
            hash::TxHash,
            states::{Preverified, Unverified, VerificationState},
        },
    },
    proofs::leader_claim_proof::{LeaderClaimProof as _, LeaderClaimPublic},
    sdp::{DeclarationId, MinStake, ServiceType, locked_notes::LockedNotes},
};

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

// TODO: Increase test coverage after type state refactor.
//   The current tests behave just like the old code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedMantleTx<State: VerificationState> {
    pub(crate) mantle_tx: MantleTx,
    // TODO: make this more efficient
    ops_proofs: OpsProofs,
    state: PhantomData<State>,
}

pub trait OperationVerificationHelper {
    fn get_channels(&self) -> &Channels;

    fn get_locked_notes(&self) -> &LockedNotes;

    fn get_utxos(&self) -> &Utxos;

    fn get_declarations_by_service(
        &self,
        service: ServiceType,
    ) -> Result<&Declarations, VerificationError>;

    fn get_declarations_by_id(
        &self,
        id: &DeclarationId,
    ) -> Result<&Declarations, VerificationError>;

    fn get_min_stake(&self) -> &MinStake;

    fn get_epoch(&self) -> Epoch;

    fn get_block_slot(&self) -> Slot;

    fn get_nullifiers(&self) -> &rpds::HashTrieSetSync<VoucherNullifier>;

    fn get_claimable_vouchers_root(&self) -> &RewardsRoot;

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
    fn into_state<T: VerificationState>(self) -> SignedMantleTx<T> {
        let Self {
            mantle_tx,
            ops_proofs,
            ..
        } = self;
        SignedMantleTx::<T> {
            mantle_tx,
            ops_proofs,
            state: PhantomData,
        }
    }

    fn gas_storage_size(&self) -> u64 {
        encode_signed_mantle_tx(self).len() as u64
    }

    pub fn ops_with_proof(&self) -> impl Iterator<Item = (&Op, &OpProof)> {
        self.mantle_tx.ops().iter().zip(self.ops_proofs.iter())
    }

    #[must_use]
    pub const fn mantle_tx(&self) -> &MantleTx {
        &self.mantle_tx
    }

    #[must_use]
    pub const fn ops_proofs(&self) -> &OpsProofs {
        &self.ops_proofs
    }

    #[must_use]
    pub fn into_parts(self) -> (MantleTx, OpsProofs) {
        (self.mantle_tx, self.ops_proofs)
    }
}

impl SignedMantleTx<Unverified> {
    #[must_use]
    pub const fn new(mantle_tx: MantleTx, ops_proofs: OpsProofs) -> Self {
        Self {
            mantle_tx,
            ops_proofs,
            state: PhantomData,
        }
    }

    const fn ensure_one_proof_per_op(&self) -> Result<(), VerificationError> {
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
    ) -> Result<(), VerificationError> {
        match (op, proof) {
            (Op::ChannelInscribe(inscribe_op), OpProof::Ed25519Sig(sig)) => inscribe_op
                .signer
                .verify(tx_hash_bytes.as_ref(), sig)
                .map_err(|_| VerificationError::InvalidSignature { op_index }),
            (Op::LeaderClaim(leader_claim_op), OpProof::PoC(poc)) => {
                let is_verified = poc.verify(&LeaderClaimPublic {
                    voucher_nullifier: leader_claim_op.voucher_nullifier.into(),
                    voucher_root: leader_claim_op.rewards_root.into(),
                    mantle_tx_hash: tx_hash.to_fr(),
                });

                if is_verified {
                    Ok(())
                } else {
                    Err(VerificationError::InvalidProofOfClaim { op_index })
                }
            }
            #[expect(
                clippy::unnested_or_patterns,
                reason = "Clarity on valid op/proof pairs."
            )]
            (Op::ChannelConfig(_), OpProof::ChannelMultiSigProof(_))
            | (Op::ChannelDeposit(_), OpProof::ZkSig(_))
            | (Op::ChannelWithdraw(_), OpProof::ChannelMultiSigProof(_))
            | (Op::SDPDeclare(_), OpProof::ZkAndEd25519Sigs { .. })
            | (Op::SDPWithdraw(_), OpProof::ZkSig(_))
            | (Op::SDPActive(_), OpProof::ZkSig(_))
            | (Op::Transfer(_), OpProof::ZkSig(_)) => Ok(()),
            _ => Err(VerificationError::IncorrectProofType {
                op_type: op.as_str(),
                op_index,
            }),
        }
    }

    fn verify_stateless_ops(&self) -> Result<(), VerificationError> {
        let tx_hash = self.hash();
        let tx_hash_bytes = tx_hash.as_signing_bytes();
        for (op_index, (op, proof)) in self.ops_with_proof().enumerate() {
            Self::verify_stateless_op(op_index, op, proof, &tx_hash, &tx_hash_bytes)?;
        }
        Ok(())
    }

    fn into_preverified(self) -> SignedMantleTx<Preverified> {
        self.into_state()
    }

    /// Runs stateless verification on the transaction, ensuring that each
    /// operation has a corresponding proof and that the proofs are of the
    /// correct type.
    ///
    /// # Invariants
    ///
    /// - `ops` and `proofs` have the same length
    /// - Each operation has a corresponding proof of the correct type
    /// - [`InscriptionOp`](crate::mantle::ops::channel::inscribe::InscriptionOp)
    ///   and [`LeaderClaimOp`](crate::mantle::ops::leader_claim::LeaderClaimOp) have valid signatures/proofs.
    pub fn preverify(self) -> Result<SignedMantleTx<Preverified>, VerificationError> {
        self.ensure_one_proof_per_op()?;
        self.verify_stateless_ops()?;
        Ok(self.into_preverified())
    }

    /// Converts a `SignedMantleTx<Unverified>` into a
    /// `SignedMantleTx<Preverified>` without performing any verification.
    ///
    /// This function is intended for
    /// [`GenesisTx`](crate::mantle::transactions::genesis_tx::GenesisTx) and
    /// testing purposes only.
    #[must_use]
    #[doc(hidden)]
    pub(crate) fn into_trusted(self) -> SignedMantleTx<Preverified> {
        SignedMantleTx::new_trusted(self.mantle_tx, self.ops_proofs)
    }
}

impl SignedMantleTx<Preverified> {
    /// Creates a new `SignedMantleTx<Preverified>` without performing any
    /// verification.
    ///
    /// This function is intended for
    /// [`GenesisTx`](crate::mantle::transactions::genesis_tx::GenesisTx) and
    /// testing purposes only.
    #[must_use]
    #[doc(hidden)]
    pub const fn new_trusted(mantle_tx: MantleTx, ops_proofs: OpsProofs) -> Self {
        Self {
            mantle_tx,
            ops_proofs,
            state: PhantomData,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "The match arms are long due to the validation context construction. Split later."
    )]
    pub(crate) fn verify_stateful_op(
        op_index: usize,
        op: &Op,
        proof: &OpProof,
        tx_hash: &TxHash,
        tx_hash_bytes: &Bytes,
        helper: &impl OperationVerificationHelper,
    ) -> Result<(), VerificationError> {
        match (op, proof) {
            (
                Op::ChannelInscribe(channel_inscribe_op),
                OpProof::Ed25519Sig(channel_inscribe_sig),
            ) => {
                let channel_inscribe_context = InscriptionValidationContext {
                    channels: helper.get_channels(),
                    tx_hash,
                    inscribe_sig: channel_inscribe_sig,
                    block_slot: helper.get_block_slot(),
                };
                channel_inscribe_op
                    .validate(&channel_inscribe_context)
                    .map_err(VerificationError::ChannelVerificationError)
            }
            (
                Op::ChannelConfig(channel_config_op),
                OpProof::ChannelMultiSigProof(channel_config_proof),
            ) => {
                let channel_config_context = ChannelConfigValidationContext {
                    channels: helper.get_channels(),
                    tx_hash,
                    config_sigs: channel_config_proof,
                };
                channel_config_op
                    .validate(&channel_config_context)
                    .map_err(VerificationError::ChannelVerificationError)
            }
            (Op::ChannelDeposit(channel_deposit_op), OpProof::ZkSig(channel_deposit_proof)) => {
                let channel_deposit_context = DepositValidationContext {
                    channels: helper.get_channels(),
                    locked_notes: helper.get_locked_notes(),
                    utxos: helper.get_utxos(),
                    tx_hash,
                    deposit_sig: channel_deposit_proof,
                };
                channel_deposit_op
                    .validate(&channel_deposit_context)
                    .map_err(VerificationError::ChannelVerificationError)
            }
            // TODO: Duplicate check. `verify_channel_withdraw` and `ChannelWithdrawOp::validate`,
            //   both called in this arm, overlap in functionality. We probably need to purge the
            //   `verify_channel_withdraw` function.
            (
                Op::ChannelWithdraw(channel_withdraw_op),
                OpProof::ChannelMultiSigProof(channel_withdraw_proof),
            ) => {
                verify_channel_multi_sig(
                    &channel_withdraw_op.channel_id,
                    channel_withdraw_proof,
                    tx_hash_bytes,
                    helper,
                    op_index,
                )?;
                let channel_withdraw_context = WithdrawValidationContext {
                    channels: helper.get_channels(),
                    locked_notes: helper.get_locked_notes(),
                    utxos: helper.get_utxos(),
                    tx_hash,
                    withdraw_sigs: channel_withdraw_proof,
                };
                channel_withdraw_op
                    .validate(&channel_withdraw_context)
                    .map_err(VerificationError::ChannelVerificationError)
            }
            (
                Op::ChannelTransfer(channel_transfer_op),
                OpProof::ChannelMultiSigProof(channel_transfer_proof),
            ) => {
                verify_channel_multi_sig(
                    &channel_transfer_op.channel_id,
                    channel_transfer_proof,
                    tx_hash_bytes,
                    helper,
                    op_index,
                )?;

                let context = ChannelTransferValidationContext {
                    locked_notes: helper.get_locked_notes(),
                    channels: helper.get_channels(),
                    utxos: helper.get_utxos(),
                    tx_hash,
                    transfer_sigs: channel_transfer_proof,
                };
                channel_transfer_op
                    .validate(&context)
                    .map_err(VerificationError::ChannelVerificationError)
            }
            (
                Op::SDPDeclare(sdp_declare_op),
                OpProof::ZkAndEd25519Sigs {
                    zk_sig,
                    ed25519_sig,
                },
            ) => {
                let context = SDPDeclareValidationContext {
                    utxo_tree: helper.get_utxos(),
                    channels: helper.get_channels(),
                    locked_notes: helper.get_locked_notes(),
                    tx_hash,
                    declare_zk_sig: zk_sig,
                    declare_eddsa_sig: ed25519_sig,
                    declarations: helper
                        .get_declarations_by_service(sdp_declare_op.service_type)?,
                    min_stake: helper.get_min_stake(),
                };
                sdp_declare_op
                    .validate(&context)
                    .map_err(VerificationError::SDPVerificationError)
            }
            (Op::SDPWithdraw(sdp_withdraw_op), OpProof::ZkSig(sdp_withdraw_proof)) => {
                let context = SDPWithdrawValidationContext {
                    declarations: helper.get_declarations_by_id(&sdp_withdraw_op.declaration_id)?,
                    epoch: helper.get_epoch(),
                    locked_notes: helper.get_locked_notes(),
                    tx_hash,
                    sdp_withdraw_sig: sdp_withdraw_proof,
                };
                sdp_withdraw_op
                    .validate(&context)
                    .map_err(VerificationError::SDPVerificationError)
            }
            (Op::SDPActive(sdp_active_op), OpProof::ZkSig(sdp_active_proof)) => {
                let context = SDPActiveValidationContext {
                    declarations: helper.get_declarations_by_id(&sdp_active_op.declaration_id)?,
                    tx_hash,
                    active_sig: sdp_active_proof,
                    epoch: helper.get_epoch(),
                };
                sdp_active_op
                    .validate(&context)
                    .map_err(VerificationError::SDPVerificationError)
            }
            (Op::LeaderClaim(leader_claim_op), OpProof::PoC(leader_claim_proof)) => {
                let context = LeaderClaimValidationContext {
                    nullifiers: helper.get_nullifiers(),
                    claimable_vouchers_root: helper.get_claimable_vouchers_root(),
                    proof_of_claim: leader_claim_proof,
                    tx_hash,
                };
                leader_claim_op
                    .validate(&context)
                    .map_err(VerificationError::LeaderClaimVerificationError)
            }
            (Op::Transfer(transfer_op), OpProof::ZkSig(transfer_proof)) => {
                let context = TransferValidationContext {
                    locked_notes: helper.get_locked_notes(),
                    channels: helper.get_channels(),
                    utxos: helper.get_utxos(),
                    tx_hash,
                    transfer_sig: transfer_proof,
                };
                transfer_op
                    .validate(&context)
                    .map_err(VerificationError::TransferVerificationError)
            }
            // SignedMantleTx<Preverified> invariant: Op/Proof pairs have been verified in
            // preverify, so this branch should be unreachable.
            _ => {
                unreachable!("All stateless verification should have been done in preverify.");
            }
        }
    }

    #[must_use]
    pub fn verified_ops(&self) -> VerifiedOps<'_> {
        self.into()
    }
}

pub struct VerifiedOps<'tx> {
    ops: &'tx [Op],
    proofs: &'tx [OpProof],
    tx_hash: TxHash,
    tx_hash_bytes: Bytes,
    index: usize,
}

impl<'tx> VerifiedOps<'tx> {
    #[must_use]
    pub fn new(transaction: &'tx SignedMantleTx<Preverified>) -> Self {
        let ops = transaction.mantle_tx.ops();
        let proofs = transaction.ops_proofs();
        let tx_hash = transaction.hash();
        Self {
            ops,
            proofs,
            tx_hash,
            tx_hash_bytes: tx_hash.as_signing_bytes(),
            index: 0,
        }
    }

    /// Yields the next operation, in order, if it passes verification.
    ///
    /// # Returns
    ///
    /// - `Some(Ok(op))` if the next operation is successfully verified.
    /// - `Some(Err(error))` if the next operation fails verification.
    /// - `None` if there are no more operations to verify.
    ///
    /// # Errors
    ///
    /// Returns [`VerificationError`] if the operation at the current index
    /// fails verification. On error, the cursor is not advanced. In the
    /// current implementation, the callers are expected to abort since only
    /// linear verification is supported.
    pub fn next(
        &mut self,
        helper: &impl OperationVerificationHelper,
    ) -> Option<Result<&'tx Op, VerificationError>> {
        let index = self.index;
        let op = self.ops.get(index)?;
        let proof = self
            .proofs
            .get(index)
            .expect("SignedMantleTx<Preverified> invariant: ops and proofs have the same length");
        if let Err(error) = SignedMantleTx::<Preverified>::verify_stateful_op(
            index,
            op,
            proof,
            &self.tx_hash,
            &self.tx_hash_bytes,
            helper,
        ) {
            return Some(Err(error));
        }
        self.index += 1;
        Some(Ok(op))
    }

    #[must_use]
    pub const fn tx_hash(&self) -> &TxHash {
        &self.tx_hash
    }

    #[must_use]
    pub const fn tx_hash_bytes(&self) -> &Bytes {
        &self.tx_hash_bytes
    }
}

impl<'tx> From<&'tx SignedMantleTx<Preverified>> for VerifiedOps<'tx> {
    fn from(transaction: &'tx SignedMantleTx<Preverified>) -> Self {
        VerifiedOps::new(transaction)
    }
}

impl<State: VerificationState> Hashable for SignedMantleTx<State> {
    //noinspection RsTypeCheck: The type is correct, but the linter is confused by
    // the closure.
    const HASHER: hashable::Hasher<Self> = |tx| {
        let bytes: [u8; 32] = Hasher::digest(tx.as_signing()).into();
        TxHash::from(bytes)
    };
    type Hash = TxHash;

    fn as_signing(&self) -> Vec<u8> {
        self.mantle_tx.as_signing()
    }
}

impl<State: VerificationState> MantleTxWithProofs for SignedMantleTx<State> {
    fn mantle_tx(&self) -> &MantleTx {
        &self.mantle_tx
    }

    fn ops_with_proof(&self) -> impl Iterator<Item = OpWithProof<'_>> {
        self.ops_with_proof()
    }
}

impl PreverifiedMantleTx for SignedMantleTx<Preverified> {
    fn verified_ops(&self) -> VerifiedOps<'_> {
        self.verified_ops()
    }
}

impl<State: VerificationState> GasCalculator for SignedMantleTx<State> {
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

impl<State: VerificationState> StorageSize for SignedMantleTx<State> {
    fn storage_size(&self) -> usize {
        self.gas_storage_size() as usize
    }
}

#[derive(Serialize)]
#[serde(rename = "SignedMantleTx")]
struct SignedMantleTxSerde<'a> {
    mantle_tx: &'a MantleTx,
    ops_proofs: &'a [OpProof],
}

impl<'a, State: VerificationState> From<&'a SignedMantleTx<State>> for SignedMantleTxSerde<'a> {
    fn from(signed_mantle_tx: &'a SignedMantleTx<State>) -> Self {
        Self {
            mantle_tx: &signed_mantle_tx.mantle_tx,
            ops_proofs: &signed_mantle_tx.ops_proofs,
        }
    }
}

impl<State: VerificationState> Serialize for SignedMantleTx<State> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            SignedMantleTxSerde::from(self).serialize(serializer)
        } else {
            encode_signed_mantle_tx(self).serialize(serializer)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename = "SignedMantleTx")]
struct OwnedSignedMantleTxSerde {
    mantle_tx: MantleTx,
    ops_proofs: OpsProofs,
}

impl From<OwnedSignedMantleTxSerde> for SignedMantleTx<Unverified> {
    fn from(helper: OwnedSignedMantleTxSerde) -> Self {
        Self::new(helper.mantle_tx, helper.ops_proofs)
    }
}

impl<'de> Deserialize<'de> for SignedMantleTx<Unverified> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            OwnedSignedMantleTxSerde::deserialize(deserializer).map(Self::from)
        } else {
            let bytes: Vec<u8> = Deserialize::deserialize(deserializer)?;
            all_consuming(decode_signed_mantle_tx)
                .parse(bytes.as_slice())
                .map(|(_, tx)| tx)
                .map_err(serde::de::Error::custom)
        }
    }
}

// TODO: This `impl` might be removed in favor of explicit preverification at
// specific boundaries.   E.g.: HTTP service uses `Unverify` and only runs
// `preverify` when crossing the boundary.
impl<'de> Deserialize<'de> for SignedMantleTx<Preverified> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let unverified_tx = SignedMantleTx::<Unverified>::deserialize(deserializer)?;
        unverified_tx.preverify().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use num_bigint::BigUint;
    use rpds::HashTrieSetSync;

    use super::*;
    use crate::{
        mantle::{
            Note, NoteId, Utxo,
            channel::{ChannelState, SlotTimeframe, SlotTimeout},
            gas::MainnetGasConstants,
            ledger::{Inputs, Outputs, OutputsError},
            ops::{
                channel::{
                    MsgId,
                    config::{ChannelConfigOp, Keys},
                    deposit::DepositOp,
                    inscribe::InscriptionOp,
                    withdraw::ChannelWithdrawOp,
                },
                transfer::{TransferError, TransferOp},
            },
            transactions::{MantleTxGasContext, Ops},
        },
        proofs::channel_multi_sig_proof::{
            ChannelMultiSigProof, IndexedSignature, IndexedSignatures,
        },
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
        channels: Channels,
        keys: HashMap<(ChannelId, ChannelKeyIndex), Ed25519PublicKey>,
        locked_notes: LockedNotes,
        utxos: Utxos,
        declarations: Declarations,
        min_stake: MinStake,
        epoch: Epoch,
        block_slot: Slot,
        nullifiers: HashTrieSetSync<VoucherNullifier>,
        claimable_vouchers_root: RewardsRoot,
    }

    impl TestOperationVerificationHelper {
        fn new(
            channels: Channels,
            keys: impl IntoIterator<Item = ((ChannelId, ChannelKeyIndex), Ed25519PublicKey)>,
        ) -> Self {
            Self {
                channels,
                keys: keys.into_iter().collect(),
                locked_notes: LockedNotes::new(),
                utxos: Utxos::new(),
                declarations: Declarations::new_sync(),
                min_stake: MinStake {
                    threshold: 0,
                    timestamp: 0,
                },
                epoch: Epoch::from(0u32),
                block_slot: Slot::from(0u64),
                nullifiers: HashTrieSetSync::new_sync(),
                claimable_vouchers_root: RewardsRoot::default(),
            }
        }

        fn with_utxos(mut self, utxos: impl IntoIterator<Item = Utxo>) -> Self {
            for utxo in utxos {
                self.utxos = self.utxos.insert(utxo.id(), utxo).0;
            }
            self
        }
    }

    impl OperationVerificationHelper for TestOperationVerificationHelper {
        fn get_channels(&self) -> &Channels {
            &self.channels
        }

        fn get_locked_notes(&self) -> &LockedNotes {
            &self.locked_notes
        }

        fn get_utxos(&self) -> &Utxos {
            &self.utxos
        }

        fn get_declarations_by_service(
            &self,
            _service: ServiceType,
        ) -> Result<&Declarations, VerificationError> {
            Ok(&self.declarations)
        }

        fn get_declarations_by_id(
            &self,
            _id: &DeclarationId,
        ) -> Result<&Declarations, VerificationError> {
            Ok(&self.declarations)
        }

        fn get_min_stake(&self) -> &MinStake {
            &self.min_stake
        }

        fn get_epoch(&self) -> Epoch {
            self.epoch
        }

        fn get_block_slot(&self) -> Slot {
            self.block_slot
        }

        fn get_nullifiers(&self) -> &HashTrieSetSync<VoucherNullifier> {
            &self.nullifiers
        }

        fn get_claimable_vouchers_root(&self) -> &RewardsRoot {
            &self.claimable_vouchers_root
        }

        fn get_channel_transfer_threshold(
            &self,
            channel_id: &ChannelId,
        ) -> Result<ChannelKeyIndex, VerificationError> {
            self.channels
                .channels
                .get(channel_id)
                .ok_or(VerificationError::ChannelNotFound {
                    channel_id: *channel_id,
                })
                .map(|channel| channel.transfer_threshold)
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

    // TODO: The generated channels are bare. We should add more realistic channel
    // states for testing.
    fn make_channel_state(
        transfer_threshold: ChannelKeyIndex,
        accredited_keys: Option<Keys>,
    ) -> ChannelState {
        let keys = accredited_keys.unwrap_or_else(|| {
            Keys::new_unchecked(vec![Ed25519Key::from_bytes(&[0; 32]).public_key()])
        });
        ChannelState {
            accredited_keys: Arc::new(keys),
            configuration_threshold: 0,

            tip_message: MsgId::root(),
            tip_slot: Slot::default(),
            tip_sequencer: u16::default(),
            tip_sequencer_starting_slot: Slot::default(),

            posting_timeframe: SlotTimeframe::from(0),
            posting_timeout: SlotTimeout::from(0),

            transfer_threshold,
        }
    }

    fn create_withdraw_tx(
        channel_id: ChannelId,
        signing_keys: &[&Ed25519Key],
        inputs: Option<Inputs>,
    ) -> SignedMantleTx<Preverified> {
        let inputs = inputs.unwrap_or_else(|| Inputs::new([NoteId(Fr::from(0u64))]));
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelWithdraw(ChannelWithdrawOp {
            channel_id,
            inputs,
        })]);

        let tx_hash = mantle_tx.hash();
        let proof = create_channel_multi_sig_proof(&tx_hash, signing_keys);

        let tx = SignedMantleTx::new(mantle_tx, [OpProof::ChannelMultiSigProof(proof)].into())
            .preverify()
            .unwrap();
        assert_eq!(
            tx.ops_with_proof().count(),
            1,
            "The tests that rely on this function assume that the transaction has exactly one operation."
        );
        tx
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
        );

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

        let result =
            SignedMantleTx::new(mantle_tx, [OpProof::Ed25519Sig(signature)].into()).preverify();

        assert!(result.is_ok());
    }

    #[test]
    fn test_signed_mantle_tx_new_missing_inscribe_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);
        let result = SignedMantleTx::new(mantle_tx, OpsProofs::empty()).preverify();

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

        let result =
            SignedMantleTx::new(mantle_tx, [OpProof::Ed25519Sig(signature)].into()).preverify();

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
        let result = SignedMantleTx::new(mantle_tx, [zk_sig].into()).preverify();

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
        )
        .preverify();

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
        )
        .preverify();

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

        let signed_tx = SignedMantleTx::new(mantle_tx, [OpProof::Ed25519Sig(signature)].into())
            .preverify()
            .unwrap();

        // Serialize and deserialize
        let serialized = serde_json::to_string(&signed_tx).unwrap();
        let deserialized: Result<SignedMantleTx<Unverified>, _> = serde_json::from_str(&serialized);
        let deserialized_signed_tx = deserialized.unwrap().preverify().unwrap();

        assert_eq!(deserialized_signed_tx, signed_tx);
    }

    #[test]
    fn test_signed_mantle_tx_deserialize_preverified_with_missing_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        let helper = SignedMantleTx::new(mantle_tx, OpsProofs::empty());

        let serialized = serde_json::to_string(&helper).unwrap();

        // Deserialization into `SignedMantleTx<Unverified>` should succeed, even with
        // missing proof.
        serde_json::from_str::<SignedMantleTx<Unverified>>(&serialized)
            .expect("Unverified deserialization should succeed");

        // Deserialization into `SignedMantleTx<Preverified>` should fail due to missing
        // proof.
        let deserialized: Result<SignedMantleTx<Preverified>, _> =
            serde_json::from_str(&serialized);

        let err_msg = deserialized
            .expect_err("Preverified deserialization should fail")
            .to_string();
        assert_eq!(
            err_msg,
            "The number of proofs (0) does not match the number of operations (1)"
        );
    }

    #[test]
    fn test_signed_mantle_tx_deserialize_preverified_with_invalid_signature() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let wrong_key = Ed25519Key::from_bytes(&[2; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        let tx_hash = mantle_tx.hash();
        let wrong_signature = wrong_key.sign_payload(&tx_hash.as_signing_bytes());

        let helper = SignedMantleTx::new(mantle_tx, [OpProof::Ed25519Sig(wrong_signature)].into());

        let serialized = serde_json::to_string(&helper).unwrap();

        // Deserialization into `SignedMantleTx<Unverified>` should succeed, even with
        // invalid signature.
        serde_json::from_str::<SignedMantleTx<Unverified>>(&serialized)
            .expect("Unverified deserialization should succeed");

        // Deserialization into `SignedMantleTx<Preverified>` should fail due to invalid
        // signature.
        let deserialized: Result<SignedMantleTx<Preverified>, _> =
            serde_json::from_str(&serialized);

        let err_msg = deserialized
            .expect_err("Preverified deserialization should fail")
            .to_string();
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
        let result = SignedMantleTx::new(mantle_tx.clone(), OpsProofs::empty()).preverify();
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
        )
        .preverify();
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
        let keys = Keys::new_unchecked(vec![key0.public_key(), key1.public_key()]);

        let input_sk = ZkKey::from(BigUint::from(1u8));
        let utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10, input_sk.to_public_key()),
        };
        let note_id = utxo.id();
        let withdraw_inputs = Inputs::from([note_id]);

        let signed_tx = create_withdraw_tx(channel_id, &[&key0, &key1], Some(withdraw_inputs));

        let channels = {
            let mut channels = Channels::new();
            let channel_state = make_channel_state(2, Some(keys));
            channels.channels.insert_mut(channel_id, channel_state);
            channels
                .register_channel_note(&note_id, &channel_id)
                .expect("Note should be registered.")
        };

        let helper = TestOperationVerificationHelper::new(
            channels,
            [
                ((channel_id, 0), key0.public_key()),
                ((channel_id, 1), key1.public_key()),
            ],
        )
        .with_utxos(vec![utxo]);

        signed_tx
            .verified_ops()
            .next(&helper)
            .expect("Cursor should yield the WithdrawOp")
            .expect("WithdrawOp should verify");
    }

    #[test]
    fn helper_backed_verification_rejects_zero_value_transfer_output() {
        let input_sk = ZkKey::from(BigUint::from(1u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10000, input_sk.to_public_key()),
        };

        let signed_tx = {
            let transfer_op = TransferOp::new(
                Inputs::new([input_utxo.id()]),
                Outputs::new([Note::new(0, Fr::from(BigUint::from(2u8)).into())]),
            );
            let mantle_tx = create_test_mantle_tx(vec![Op::Transfer(transfer_op)]);
            let transfer_sig = ZkKey::multi_sign(&[input_sk], &mantle_tx.hash().to_fr())
                .expect("Signing should succeed");
            SignedMantleTx::new(mantle_tx, [OpProof::ZkSig(transfer_sig)].into())
                .preverify()
                .expect("Transfer transaction should preverify")
        };

        let helper =
            TestOperationVerificationHelper::new(Channels::new(), []).with_utxos([input_utxo]);

        let verification_result = signed_tx
            .verified_ops()
            .next(&helper)
            .expect("Cursor should yield the TransferOp");
        assert_eq!(
            verification_result,
            Err(VerificationError::TransferVerificationError(
                TransferError::Outputs(OutputsError::ZeroValueNote)
            ))
        );
    }

    #[test]
    fn helper_backed_verification_rejects_missing_channel() {
        let channel_id = ChannelId::from([10u8; 32]);
        let key0 = Ed25519Key::from_bytes(&[0; 32]);
        let signed_tx = create_withdraw_tx(channel_id, &[&key0], None);

        let channels = Channels::new();
        let helper = TestOperationVerificationHelper::new(channels, []);

        let verification_result = signed_tx.verified_ops().next(&helper).unwrap();
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
        let signed_tx = create_withdraw_tx(channel_id, &[&key0, &key1], None);

        let channels = {
            let mut channels = Channels::new();
            let channel_state = make_channel_state(2, None);
            channels.channels.insert_mut(channel_id, channel_state);
            channels
        };
        let helper =
            TestOperationVerificationHelper::new(channels, [((channel_id, 0), key0.public_key())]);

        let verification_result = signed_tx.verified_ops().next(&helper).unwrap();
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
        let signed_tx = create_withdraw_tx(channel_id, &[&key0], None);

        let channels = {
            let mut channels = Channels::new();
            let channel_state = make_channel_state(2, None);
            channels.channels.insert_mut(channel_id, channel_state);
            channels
        };
        let helper =
            TestOperationVerificationHelper::new(channels, [((channel_id, 0), key0.public_key())]);

        let verification_result = signed_tx.verified_ops().next(&helper).unwrap();
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
        let signed_tx = create_withdraw_tx(channel_id, &[&wrong_key], None);

        let channels = {
            let mut channels = Channels::new();
            let channel_state = make_channel_state(1, None);
            channels.channels.insert_mut(channel_id, channel_state);
            channels
        };
        let helper = TestOperationVerificationHelper::new(
            channels,
            [((channel_id, 0), expected_key.public_key())],
        );

        let verification_result = signed_tx.verified_ops().next(&helper).unwrap();
        assert_eq!(
            verification_result,
            Err(VerificationError::ChannelMultiSigProofInvalidSignature {
                op_index: 0,
                signature_index: 0
            })
        );
    }
}
