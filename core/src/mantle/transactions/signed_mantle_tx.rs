use std::marker::PhantomData;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    crypto::{Digest as _, Hasher},
    mantle::{
        RawMantleTx, Value, VerificationError,
        batch::DeferredZkpVerification,
        gas::{Gas, GasCost, GasOverflow, GasProfile, TxGasCalculator},
        ledger::{PreverifiableOperation, VerifiableOperation, verification_mode::StandardMode},
        ops::{
            Op, OpProof,
            channel::{
                channel_transfer::ChannelTransferValidationContext,
                config::ChannelConfigValidationContext,
                deposit::DepositValidationContext,
                inscribe::{InscriptionPreverificationContext, InscriptionValidationContext},
                withdraw::WithdrawValidationContext,
            },
            leader_claim::{LeaderClaimPreverificationContext, LeaderClaimVerificationContext},
            pow::ClaimPoWRewardVerificationContext,
            sdp::{
                SDPActiveValidationContext, SDPDeclareOp, SDPDeclareVerificationContext,
                SDPWithdrawValidationContext, declare::SDPDeclarePreverificationContext,
            },
            transfer::TransferValidationContext,
        },
        traits::{
            Hashable, MantleTxWithProofs, PreverifiedMantleTx, StorageSize, hashable,
            mantle_tx::OpWithProof,
        },
        transactions::{
            GasPrices, OperationVerificationHelper, OpsProofs, VerifiedOps,
            codec::{decode_signed_mantle_tx, encode_signed_mantle_tx},
            hash::{TxHash, TxHashView},
            mantle_tx::MantleTx as _,
            states::{Preverified, Unverified, VerificationState},
        },
    },
};

// TODO: Increase test coverage after type state refactor.
//   The current tests behave just like the old code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedMantleTx<State: VerificationState> {
    pub(crate) mantle_tx: RawMantleTx,
    // TODO: make this more efficient
    ops_proofs: OpsProofs,
    state: PhantomData<State>,
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
    pub const fn mantle_tx(&self) -> &RawMantleTx {
        &self.mantle_tx
    }

    #[must_use]
    pub const fn ops_proofs(&self) -> &OpsProofs {
        &self.ops_proofs
    }

    #[must_use]
    pub fn into_parts(self) -> (RawMantleTx, OpsProofs) {
        (self.mantle_tx, self.ops_proofs)
    }
}

impl SignedMantleTx<Unverified> {
    #[must_use]
    pub const fn new(mantle_tx: RawMantleTx, ops_proofs: OpsProofs) -> Self {
        Self {
            mantle_tx,
            ops_proofs,
            state: PhantomData,
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

    // TODO: Might drop proofs after verification.
    //  This is carried over from the original code.
    fn preverify_op(
        op_index: usize,
        op: &Op,
        proof: &OpProof,
        tx_hash_view: &TxHashView,
    ) -> Result<(), VerificationError> {
        // TODO: Add more info to errors (e.g. op_index)
        match (op, proof) {
            (Op::ChannelInscribe(op), OpProof::Ed25519Sig(proof)) => {
                let context = InscriptionPreverificationContext { tx_hash_view };
                op.preverify(proof, &context)
                    .map_err(VerificationError::ChannelVerificationError)
            }
            (Op::ChannelConfig(op), OpProof::ChannelMultiSigProof(proof)) => op
                .preverify(proof, &())
                .map_err(VerificationError::ChannelVerificationError),
            (Op::ChannelDeposit(op), OpProof::ZkSig(proof)) => op
                .preverify(proof, &())
                .map_err(VerificationError::ChannelVerificationError),
            (Op::ChannelWithdraw(op), OpProof::ChannelMultiSigProof(proof)) => op
                .preverify(proof, &())
                .map_err(VerificationError::ChannelVerificationError),
            (Op::ChannelTransfer(op), OpProof::ChannelMultiSigProof(proof)) => op
                .preverify(proof, &())
                .map_err(VerificationError::ChannelVerificationError),
            (Op::SDPDeclare(op), OpProof::ZkAndEd25519Sigs(proof)) => {
                let context = SDPDeclarePreverificationContext { tx_hash_view };
                <SDPDeclareOp as PreverifiableOperation<StandardMode>>::preverify(
                    op, proof, &context,
                )
                .map_err(VerificationError::SDPVerificationError)
            }
            (Op::SDPWithdraw(op), OpProof::ZkSig(proof)) => op
                .preverify(proof, &())
                .map_err(VerificationError::SDPVerificationError),
            (Op::SDPActive(op), OpProof::ZkSig(proof)) => op
                .preverify(proof, &())
                .map_err(VerificationError::SDPVerificationError),
            (Op::LeaderClaim(op), OpProof::PoC(proof)) => {
                let context = LeaderClaimPreverificationContext { tx_hash_view };
                op.preverify(proof, &context)
                    .map_err(VerificationError::LeaderClaimVerificationError)
            }
            (Op::Transfer(op), OpProof::ZkSig(proof)) => op
                .preverify(proof, &())
                .map_err(VerificationError::TransferVerificationError),
            (Op::ClaimPowReward(op), OpProof::None(proof)) => op
                .preverify(proof, &())
                .map_err(VerificationError::ClaimPowRewardError),
            _ => Err(VerificationError::IncorrectProofType {
                op_type: op.as_str(),
                op_index,
            }),
        }
    }

    fn preverify_ops(&self) -> Result<(), VerificationError> {
        let tx_hash = self.hash();
        let tx_hash_view = TxHashView::new(tx_hash);
        for (op_index, (op, proof)) in self.ops_with_proof().enumerate() {
            Self::preverify_op(op_index, op, proof, &tx_hash_view)?;
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
        self.preverify_ops()?;
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
    pub const fn new_trusted(mantle_tx: RawMantleTx, ops_proofs: OpsProofs) -> Self {
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
        tx_hash_view: &TxHashView,
        helper: &impl OperationVerificationHelper,
    ) -> Result<Option<DeferredZkpVerification>, VerificationError> {
        match (op, proof) {
            (Op::ChannelInscribe(op), OpProof::Ed25519Sig(proof)) => {
                let channel_inscribe_context = InscriptionValidationContext {
                    channels: helper.get_channels(),
                    block_slot: helper.get_block_slot(),
                };
                op.verify(proof, &channel_inscribe_context)
                    .map_err(VerificationError::ChannelVerificationError)
            }
            (Op::ChannelConfig(op), OpProof::ChannelMultiSigProof(proof)) => {
                let channel_config_context = ChannelConfigValidationContext {
                    channels: helper.get_channels(),
                    tx_hash_view,
                };
                op.verify(proof, &channel_config_context)
                    .map_err(VerificationError::ChannelVerificationError)
            }
            (Op::ChannelDeposit(op), OpProof::ZkSig(proof)) => {
                let channel_deposit_context = DepositValidationContext {
                    channels: helper.get_channels(),
                    locked_notes: helper.get_locked_notes(),
                    utxos: helper.get_utxos(),
                    tx_hash_view,
                };
                op.verify(proof, &channel_deposit_context)
                    .map_err(VerificationError::ChannelVerificationError)
            }
            (Op::ChannelWithdraw(channel_withdraw_op), OpProof::ChannelMultiSigProof(proof)) => {
                let channel_withdraw_context = WithdrawValidationContext {
                    channels: helper.get_channels(),
                    locked_notes: helper.get_locked_notes(),
                    utxos: helper.get_utxos(),
                    tx_hash_view,
                    helper,
                    op_index,
                };
                channel_withdraw_op
                    .verify(proof, &channel_withdraw_context)
                    .map_err(VerificationError::ChannelVerificationError)
            }
            (Op::ChannelTransfer(op), OpProof::ChannelMultiSigProof(proof)) => {
                let context = ChannelTransferValidationContext {
                    locked_notes: helper.get_locked_notes(),
                    channels: helper.get_channels(),
                    utxos: helper.get_utxos(),
                    tx_hash_view,
                    op_index,
                    helper,
                };
                op.verify(proof, &context)
                    .map_err(VerificationError::ChannelVerificationError)
            }
            (Op::SDPDeclare(op), OpProof::ZkAndEd25519Sigs(proof)) => {
                let context = SDPDeclareVerificationContext {
                    utxo_tree: helper.get_utxos(),
                    channels: helper.get_channels(),
                    locked_notes: helper.get_locked_notes(),
                    tx_hash_view,
                    declarations: helper.get_declarations_by_service(op.service_type)?,
                    min_stake: helper.get_min_stake(),
                };
                <SDPDeclareOp as VerifiableOperation<StandardMode>>::verify(op, proof, &context)
                    .map_err(VerificationError::SDPVerificationError)
            }
            (Op::SDPWithdraw(op), OpProof::ZkSig(proof)) => {
                let context = SDPWithdrawValidationContext {
                    declarations: helper.get_declarations_by_id(&op.declaration_id)?,
                    epoch: helper.get_epoch(),
                    locked_notes: helper.get_locked_notes(),
                    tx_hash_view,
                };
                op.verify(proof, &context)
                    .map_err(VerificationError::SDPVerificationError)
            }
            (Op::SDPActive(op), OpProof::ZkSig(proof)) => {
                let context = SDPActiveValidationContext {
                    declarations: helper.get_declarations_by_id(&op.declaration_id)?,
                    tx_hash_view,
                    epoch: helper.get_epoch(),
                };
                op.verify(proof, &context)
                    .map_err(VerificationError::SDPVerificationError)
            }
            (Op::LeaderClaim(op), OpProof::PoC(proof)) => {
                let context = LeaderClaimVerificationContext {
                    nullifiers: helper.get_nullifiers(),
                    claimable_vouchers_root: helper.get_claimable_vouchers_root(),
                    tx_hash_view,
                };
                op.verify(proof, &context)
                    .map_err(VerificationError::LeaderClaimVerificationError)
            }
            (Op::Transfer(op), OpProof::ZkSig(proof)) => {
                let context = TransferValidationContext {
                    locked_notes: helper.get_locked_notes(),
                    channels: helper.get_channels(),
                    utxos: helper.get_utxos(),
                    tx_hash_view,
                };
                op.verify(proof, &context)
                    .map_err(VerificationError::TransferVerificationError)
            }
            (Op::ClaimPowReward(claim_pow_op), OpProof::None(proof)) => {
                let context = ClaimPoWRewardVerificationContext {
                    current_block_slot: helper.get_block_slot(),
                    reward_difficulty: helper.get_pow_reward_difficulty(),
                    pow_nullifiers: helper.get_pow_nullifiers(),
                    epoch_pow_reward: helper.get_epoch_pow_reward(),
                    epoch_reward_pool: helper.get_pow_reward_pool(),
                    current_epoch_nonce: helper.get_current_epoch_nonce(),
                    previous_epoch_nonce: helper.get_previous_epoch_nonce(),
                    blocks_slot: helper.get_blocks_slot(),
                };
                claim_pow_op
                    .verify(proof, &context)
                    .map_err(VerificationError::ClaimPowRewardError)
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
    fn mantle_tx(&self) -> &RawMantleTx {
        &self.mantle_tx
    }

    fn ops_with_proof(&self) -> impl Iterator<Item = OpWithProof<'_>> {
        self.ops_with_proof()
    }
}

impl<State: VerificationState> TxGasCalculator for SignedMantleTx<State> {
    type Context = GasPrices;

    fn total_gas_cost<Profile: GasProfile>(
        &self,
        context: &Self::Context,
    ) -> Result<GasCost, GasOverflow> {
        let execution_gas = TxGasCalculator::execution_gas_consumption::<Profile>(self, context)?;
        let execution_gas_cost =
            GasCost::calculate(execution_gas, context.execution_base_gas_price)?;
        let storage_gas_cost = TxGasCalculator::storage_gas_cost(self, context)?;

        execution_gas_cost.checked_add(storage_gas_cost)
    }

    fn storage_gas_cost(&self, context: &Self::Context) -> Result<GasCost, GasOverflow> {
        let storage_gas = TxGasCalculator::storage_gas_consumption(self, context)?;
        GasCost::calculate(storage_gas, context.storage_gas_price)
    }

    fn execution_gas_consumption<Profile: GasProfile>(
        &self,
        _context: &Self::Context,
    ) -> Result<Gas, GasOverflow> {
        self.mantle_tx
            .ops()
            .iter()
            .zip(self.ops_proofs.iter())
            .map(|(op, proof)| signed_op_execution_gas::<Profile>(op, proof))
            .try_fold(Gas::from(0), |total, gas| total.checked_add(gas?))
    }

    fn storage_gas_consumption(&self, _context: &Self::Context) -> Result<Gas, GasOverflow> {
        Ok(self.gas_storage_size().into())
    }
}

fn signed_op_execution_gas<Profile: GasProfile>(
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
        _ => return Ok(op.execution_gas::<Profile>()),
    };
    let multiplier = Value::try_from(signature_count)
        .expect("channel multi-signature proofs are bounded to u16::MAX signatures");

    op.execution_gas::<Profile>().checked_mul(multiplier)
}

impl<State: VerificationState> StorageSize for SignedMantleTx<State> {
    fn storage_size(&self) -> usize {
        self.gas_storage_size() as usize
    }
}

impl PreverifiedMantleTx for SignedMantleTx<Preverified> {
    fn verified_ops(&self) -> VerifiedOps<'_> {
        self.verified_ops()
    }
}

#[derive(Serialize)]
#[serde(rename = "SignedMantleTx")]
struct SignedMantleTxSerde<'a> {
    mantle_tx: &'a RawMantleTx,
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
    mantle_tx: RawMantleTx,
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
            let (remaining, tx) =
                decode_signed_mantle_tx(bytes.as_slice()).map_err(serde::de::Error::custom)?;
            if remaining.is_empty() {
                Ok(tx)
            } else {
                Err(serde::de::Error::custom(
                    "Invalid length: not all bytes were consumed",
                ))
            }
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
pub mod test_utils {
    use std::sync::Arc;

    use lb_cryptarchia_engine::Slot;
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::Ed25519Key;

    use crate::mantle::{
        NoteId, Op, OpProof, RawMantleTx, SignedMantleTx,
        channel::{ChannelState, SlotTimeframe, SlotTimeout},
        ledger::Inputs,
        ops::channel::{
            ChannelId, ChannelKeyIndex, MsgId, config::Keys, inscribe::InscriptionOp,
            verification::test_utils::create_channel_multi_sig_proof, withdraw::ChannelWithdrawOp,
        },
        traits::Hashable as _,
        transactions::{Ops, states::Preverified},
    };

    #[must_use]
    pub fn create_test_mantle_tx(ops: Vec<Op>) -> RawMantleTx {
        RawMantleTx(Ops::new_unchecked(ops))
    }

    #[must_use]
    pub fn create_test_inscribe_op(signing_key: &Ed25519Key) -> InscriptionOp {
        InscriptionOp {
            channel_id: [0; 32].into(),
            inscription: [1, 2, 3].into(),
            parent: [0; 32].into(),
            signer: signing_key.public_key(),
        }
    }

    // TODO: The generated channels are bare. We should add more realistic channel
    // states for testing.
    #[must_use]
    pub fn make_channel_state(
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
            config_tip_hash: MsgId::root(),
            tip_slot: Slot::default(),
            tip_sequencer: u16::default(),
            tip_sequencer_starting_slot: Slot::default(),

            posting_timeframe: SlotTimeframe::from(0),
            posting_timeout: SlotTimeout::from(0),

            transfer_threshold,
        }
    }

    #[must_use]
    pub fn create_withdraw_tx(
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
}

#[cfg(test)]
mod tests {
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use num_bigint::BigUint;

    use super::*;
    use crate::mantle::{
        Note, NoteId, Utxo,
        channel::Error,
        gas::MainnetGasProfile,
        ledger::{Inputs, Outputs, OutputsError},
        ops::{
            channel::{
                ChannelId, MsgId, config::ChannelConfigOp, deposit::DepositOp,
                verification::test_utils::create_channel_multi_sig_proof,
                withdraw::ChannelWithdrawOp,
            },
            transfer::{TransferError, TransferOp},
        },
        transactions::{
            MantleTxGasContext,
            signed_mantle_tx::test_utils::{create_test_inscribe_op, create_test_mantle_tx},
        },
    };

    fn create_config_op(channel: ChannelId, signing_key: &Ed25519Key) -> ChannelConfigOp {
        ChannelConfigOp {
            channel,
            parent: MsgId::root(),
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

        let gas = mantle_tx
            .minimum_execution_gas_consumption::<MainnetGasProfile>(&context)
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
        let gas = TxGasCalculator::execution_gas_consumption::<MainnetGasProfile>(
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
            Err(VerificationError::ChannelVerificationError(
                Error::InvalidSignature
            ))
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
            Err(VerificationError::ChannelVerificationError(
                Error::InvalidSignature
            ))
        ));
    }

    #[test]
    fn test_signed_mantle_tx_new_rejects_zero_value_transfer_output() {
        let input_sk = ZkKey::from(BigUint::from(1u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10000, input_sk.to_public_key()),
        };

        let transfer_op = TransferOp::new(
            Inputs::new([input_utxo.id()]),
            Outputs::new([Note::new(0, Fr::from(BigUint::from(2u8)).into())]),
        );
        let mantle_tx = create_test_mantle_tx(vec![Op::Transfer(transfer_op)]);
        let transfer_sig = ZkKey::multi_sign(&[input_sk], &mantle_tx.hash().to_fr())
            .expect("Signing should succeed");
        let result =
            SignedMantleTx::new(mantle_tx, [OpProof::ZkSig(transfer_sig)].into()).preverify();

        assert_eq!(
            result,
            Err(VerificationError::TransferVerificationError(
                TransferError::Outputs(OutputsError::ZeroValueNote)
            ))
        );
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
}
