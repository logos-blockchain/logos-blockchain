use lb_codec::{BinaryDecode, BinaryEncode, DecodeError};
use lb_utils::bounded::BoundedError;

#[cfg(feature = "test-utils")]
use crate::mantle::Op;
use crate::mantle::{
    GasProfile, OpProofRef, TxGasCalculator, TxHash, VerificationError,
    gas::{Gas, GasCost, GasOverflow},
    ledger::verification_mode::{StandardMode, VerificationMode},
    ops::{SignedOp, signed_op_error::OpProofMismatch},
    traits::{
        Hashable, MantleTx, PreverifiedMantleTransaction, SignedMantleTx, StorageSize, hashable,
    },
    transactions::{
        GasPrices, OpProofRefs, VerifiedOperations,
        hash::TxHashView,
        states::{Preverified, Unverified, VerificationState},
        tx_list::{OpProofs, OpRefs, Ops, common::TxList, hash::tx_hasher},
    },
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "The number of operations ({operations}) does not match the number of proofs ({proofs})."
    )]
    LengthMismatch { operations: usize, proofs: usize },
    #[error("Proof for operation at index {index} does not match: {source}")]
    OpProofMismatch {
        index: usize,
        #[source]
        source: Box<OpProofMismatch>,
    },
    #[error(transparent)]
    Bounded(#[from] BoundedError),
    // TODO: Add SignedOpErrors here, adding the op_index as metadata
}

pub type SignedOps<State, Mode> = TxList<SignedOp<State, Mode>>;

impl<Mode: VerificationMode> SignedOps<Unverified, Mode> {
    pub fn from_parts(ops: Ops, op_proofs: OpProofs) -> Result<Self, Error> {
        if ops.len() != op_proofs.len() {
            return Err(Error::LengthMismatch {
                operations: ops.len(),
                proofs: op_proofs.len(),
            });
        }

        let signed_ops_vec = ops
            .into_iter()
            .zip(op_proofs)
            .enumerate()
            .map(|(index, (op, proof))| {
                SignedOp::<Unverified, Mode>::try_from((op, proof)).map_err(|source| {
                    Error::OpProofMismatch {
                        index,
                        source: Box::new(source),
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let signed_ops = Self::try_from(signed_ops_vec)?;
        Ok(signed_ops)
    }

    /// Converts a `SignedOps<Unverified, Mode>` into a
    /// `SignedOps<Preverified, Mode>` without performing any
    /// verification.
    ///
    /// This function is intended for
    /// [`GenesisTx`](crate::mantle::transactions::genesis_tx::GenesisTx) and
    /// testing purposes only.
    #[doc(hidden)]
    #[must_use]
    pub fn into_preverified_trusted(self) -> SignedOps<Preverified, Mode> {
        self.into_state_trusted()
    }

    /// Pairs every op with a placeholder proof of the kind that op requires.
    ///
    /// The proofs are structurally valid but cryptographically meaningless, so
    /// the result is only useful to tests that exercise op extraction rather
    /// than verification — `preverify` will reject it.
    #[cfg(feature = "test-utils")]
    #[must_use]
    pub fn from_ops_with_placeholder_proofs(ops: Ops) -> Self {
        let proofs = ops
            .iter()
            .map(Op::generate_placeholder_proof)
            .collect::<Vec<_>>();
        let op_proofs = OpProofs::new_unchecked(proofs);
        Self::from_parts(ops, op_proofs)
            .expect("Placeholder proofs pair with their ops by construction.")
    }
}

impl SignedOps<Unverified, StandardMode> {
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
    pub fn preverify(self) -> Result<SignedOps<Preverified, StandardMode>, VerificationError> {
        let tx_hash_view = TxHashView::new(self.hash());
        let preverified_signed_op_vec = self
            .into_iter()
            .map(|signed_op| signed_op.into_preverified(&tx_hash_view))
            .collect::<Result<Vec<SignedOp<Preverified, StandardMode>>, VerificationError>>()?;
        Ok(SignedOps::new_unchecked(preverified_signed_op_vec))
    }
}

impl<Mode: VerificationMode> SignedOps<Preverified, Mode> {
    /// Creates a new `SignedMantleTx<Preverified>` without performing any
    /// verification.
    ///
    /// This function is intended for
    /// [`GenesisTx`](crate::mantle::transactions::genesis_tx::GenesisTx) and
    /// testing purposes only.
    #[doc(hidden)]
    pub fn from_parts_trusted(ops: Ops, ops_proofs: OpProofs) -> Result<Self, Error> {
        Ok(SignedOps::from_parts(ops, ops_proofs)?.into_preverified_trusted())
    }
}

impl SignedOps<Preverified, StandardMode> {
    #[must_use]
    pub fn into_verified(self) -> VerifiedOperations {
        self.into()
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOps<State, Mode> {
    #[must_use]
    pub fn op_refs(&self) -> OpRefs<'_> {
        TxList(self.0.map_ref(SignedOp::operation))
    }

    #[must_use]
    pub fn op_proof_refs(&self) -> OpProofRefs<'_> {
        TxList(self.0.map_ref(SignedOp::proof))
    }

    pub fn op_proof_refs_iter(&self) -> impl Iterator<Item = OpProofRef<'_>> {
        self.iter().map(SignedOp::proof)
    }

    #[must_use]
    fn gas_storage_size(&self) -> u64 {
        self.storage_size() as u64
    }

    /// Converts a `SignedOps<State, Mode>` into a
    /// `SignedOps<NewState, Mode>` without performing any
    /// verification.
    ///
    /// This function is intended for
    /// [`GenesisTx`](crate::mantle::transactions::genesis_tx::GenesisTx) and
    /// testing purposes only.
    #[doc(hidden)]
    fn into_state_trusted<NewState: VerificationState>(self) -> SignedOps<NewState, Mode> {
        let new_state_signed_ops = self
            .into_iter()
            .map(SignedOp::into_state_trusted)
            .collect::<Vec<SignedOp<NewState, Mode>>>();
        SignedOps::new_unchecked(new_state_signed_ops)
    }
}

/// A list of [`SignedOp`] encodes columnar: `[count][ops...][proofs...]`.
/// The single count covers both columns, so only the list can encode them.
/// [`OpProofs`], [`OpProofRefs`] and [`SignedOp`] deliberately implement
/// neither codec trait, which is what makes a second count unrepresentable.
impl<State: VerificationState, Mode: VerificationMode> BinaryEncode
    for TxList<SignedOp<State, Mode>>
{
    fn encoded_length(&self) -> usize {
        self.op_refs().encoded_length()
            + self
                .op_proof_refs_iter()
                .map(|proof| proof.encoded_length())
                .sum::<usize>()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.op_refs().encode_into(out);
        for proof in self.op_proof_refs_iter() {
            proof.encode_into(out);
        }
    }
}

impl<Mode: VerificationMode> BinaryDecode for SignedOps<Unverified, Mode> {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        _context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (remaining_input, ops) = Ops::decode(input, &())?;
        let (remaining_input, proofs) = OpProofs::decode_with_ops(remaining_input, &ops)?;

        let signed_ops = Self::from_parts(ops, proofs).map_err(|error| {
            DecodeError::custom(format!("Failed to construct SignedOps: {error}"))
        })?;

        Ok((remaining_input, signed_ops))
    }
}

impl<State: VerificationState, Mode: VerificationMode> Hashable for TxList<SignedOp<State, Mode>> {
    //noinspection RsTypeCheck: The type is correct, but the linter is confused by
    // the closure.
    const HASHER: hashable::Hasher<Self> = tx_hasher;
    type Hash = TxHash;

    fn as_signing(&self) -> Vec<u8> {
        self.op_refs().as_signing()
    }
}

impl<State: VerificationState, Mode: VerificationMode> StorageSize for SignedOps<State, Mode> {
    fn storage_size(&self) -> usize {
        self.encoded_length()
    }
}

impl<State: VerificationState, Mode: VerificationMode> MantleTx for SignedOps<State, Mode> {
    fn op_refs(&self) -> OpRefs<'_> {
        self.op_refs()
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedMantleTx<State, Mode>
    for SignedOps<State, Mode>
{
    fn signed_ops(&self) -> &Self {
        self
    }

    fn op_proof_refs(&self) -> OpProofRefs<'_> {
        self.op_proof_refs()
    }
}

impl<State: VerificationState, Mode: VerificationMode> TxGasCalculator for SignedOps<State, Mode> {
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
        self.iter()
            .map(SignedOp::execution_gas)
            .try_fold(Gas::from(0), |total, gas| total.checked_add(gas?))
    }

    fn storage_gas_consumption(&self, _context: &Self::Context) -> Result<Gas, GasOverflow> {
        Ok(self.gas_storage_size().into())
    }
}

impl PreverifiedMantleTransaction for SignedOps<Preverified, StandardMode> {
    fn into_verified_operations(self) -> VerifiedOperations {
        self.into_verified()
    }
}

mod mantle_spec {
    //! Mantle specification serde definition for [`SignedOps`], in the shape
    //! of:
    //!
    //! ```json
    //! { "mantle_tx": { "ops": [ ... ] }, "ops_proofs": [ ... ] }
    //! ```
    //!
    //! [`SignedOps`] breaks the standard serialization pattern, where each
    //! entity is transformed into its own row-based shape.
    //!
    //! Every other sibling entity here is a list of rows and serializes as a
    //! bare sequence of them. This one is split into two named columns, and
    //! those columns sit at different levels: `mantle_tx` is the spec's
    //! *unsigned* transaction, the op column on its own, and a signed
    //! transaction is that plus the proof column beside it.
    //!
    //! [`ops::mantle_spec`] builds that unsigned half, so the level comes from
    //! there rather than from [`Ops`], which stays bare.
    //!
    //! The binary arm carries none of this:
    //! [`BinaryEncode`](lb_codec::BinaryEncode) writes a single count covering
    //! both columns, so position alone identifies them.
    //!
    //! ```text
    //! [count][ops...][proofs...]
    //! ```

    use lb_codec::{BinaryDecodeExt as _, BinaryEncode as _};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::mantle::{
        ledger::verification_mode::{StandardMode, VerificationMode},
        transactions::{
            OpProofRefs, OpProofs, OpRefs, Ops, SignedOps,
            states::{Preverified, Unverified, VerificationState},
            tx_list::ops,
        },
    };

    #[derive(Serialize)]
    struct SignedOpsSerde<'a> {
        #[serde(rename = "mantle_tx", with = "ops::mantle_spec")]
        op_refs: OpRefs<'a>,
        ops_proofs: OpProofRefs<'a>,
    }

    impl<'a, State: VerificationState, Mode: VerificationMode> From<&'a SignedOps<State, Mode>>
        for SignedOpsSerde<'a>
    {
        fn from(signed_ops: &'a SignedOps<State, Mode>) -> Self {
            Self {
                op_refs: signed_ops.op_refs(),
                ops_proofs: signed_ops.op_proof_refs(),
            }
        }
    }

    impl<State: VerificationState, Mode: VerificationMode> Serialize for SignedOps<State, Mode> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            if serializer.is_human_readable() {
                SignedOpsSerde::from(self).serialize(serializer)
            } else {
                self.encode().serialize(serializer)
            }
        }
    }

    #[derive(Deserialize)]
    struct OwnedSignedOpsSerde {
        #[serde(rename = "mantle_tx", with = "ops::mantle_spec")]
        ops: Ops,
        ops_proofs: OpProofs,
    }

    impl<'de, Mode: VerificationMode> Deserialize<'de> for SignedOps<Unverified, Mode> {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            if deserializer.is_human_readable() {
                let helper = OwnedSignedOpsSerde::deserialize(deserializer)?;
                Self::from_parts(helper.ops, helper.ops_proofs).map_err(serde::de::Error::custom)
            } else {
                let bytes: Vec<u8> = Deserialize::deserialize(deserializer)?;
                Self::decode_all(bytes.as_slice()).map_err(serde::de::Error::custom)
            }
        }
    }

    impl<'de> Deserialize<'de> for SignedOps<Preverified, StandardMode> {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let unverified_signed_ops =
                SignedOps::<Unverified, StandardMode>::deserialize(deserializer)?;
            unverified_signed_ops
                .preverify()
                .map_err(serde::de::Error::custom)
        }
    }
}

#[cfg(test)]
pub mod test_utils {
    use std::sync::Arc;

    use lb_cryptarchia_engine::Slot;
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::Ed25519Key;

    use crate::mantle::{
        NoteId, Op, OpProof,
        channel::{ChannelState, SlotTimeframe, SlotTimeout},
        ledger::{Inputs, verification_mode::StandardMode},
        ops::channel::{
            ChannelId, ChannelKeyIndex, MsgId, config::Keys, inscribe::InscriptionOp,
            verification::test_utils::create_channel_multi_sig_proof, withdraw::ChannelWithdrawOp,
        },
        traits::Hashable as _,
        transactions::{OpProofs, Ops, SignedOps, states::Preverified},
    };

    #[must_use]
    pub fn create_test_mantle_tx(ops: Vec<Op>) -> Ops {
        Ops::new_unchecked(ops)
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
    ) -> SignedOps<Preverified, StandardMode> {
        let inputs = inputs.unwrap_or_else(|| Inputs::new([NoteId(Fr::from(0u64))]));
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelWithdraw(ChannelWithdrawOp {
            channel_id,
            inputs,
        })]);

        let tx_hash = mantle_tx.hash();
        let proof = create_channel_multi_sig_proof(&tx_hash, signing_keys);
        let op_proofs = OpProofs::from([OpProof::ChannelMultiSigProof(proof)]);

        let signed_ops = SignedOps::from_parts(mantle_tx, op_proofs)
            .unwrap()
            .preverify()
            .unwrap();
        assert_eq!(
            signed_ops.inner().len(),
            1,
            "The tests that rely on this function assume that the transaction has exactly one operation."
        );
        signed_ops
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use num_bigint::BigUint;

    use crate::mantle::{
        Note, NoteId, Op, OpProof, SignedOps, TxGasCalculator, Utxo, VerificationError,
        channel::Error,
        gas::MainnetGasProfile,
        ledger::{Inputs, Outputs, OutputsError, verification_mode::StandardMode},
        ops::{
            channel::{
                ChannelId, config::ChannelConfigOp, deposit::DepositOp,
                verification::test_utils::create_channel_multi_sig_proof,
                withdraw::ChannelWithdrawOp,
            },
            transfer::{TransferError, TransferOp},
        },
        traits::Hashable as _,
        transactions::{
            GasPrices, OpProofs,
            states::{Preverified, Unverified},
            tx_list::{
                ops::OpsGasContext,
                signed_ops::test_utils::{create_test_inscribe_op, create_test_mantle_tx},
            },
        },
    };

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
        let context = OpsGasContext::new(
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

        let op_proofs = OpProofs::from([
            OpProof::ChannelMultiSigProof(config_proof),
            OpProof::ZkSig(deposit_proof),
            OpProof::ChannelMultiSigProof(withdraw_proof),
        ]);
        let signed_ops = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();

        let gas_prices = GasPrices::new(1, 0);
        let gas = TxGasCalculator::execution_gas_consumption::<MainnetGasProfile>(
            &signed_ops,
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

        let op_proofs = OpProofs::from([OpProof::Ed25519Sig(signature)]);
        let result = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs)
            .unwrap()
            .preverify();

        assert!(result.is_ok());
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

        let op_proofs = OpProofs::from([OpProof::Ed25519Sig(signature)]);
        let result = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs)
            .unwrap()
            .preverify();

        assert!(matches!(
            result,
            Err(VerificationError::ChannelVerificationError(
                Error::InvalidSignature
            ))
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

        let op_proofs = OpProofs::from([OpProof::Ed25519Sig(sig1), OpProof::Ed25519Sig(sig2)]);
        let result = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs)
            .unwrap()
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

        let op_proofs = OpProofs::from([OpProof::Ed25519Sig(sig1), OpProof::Ed25519Sig(sig2)]);
        let result = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs)
            .unwrap()
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
        let op_proofs = OpProofs::from([OpProof::ZkSig(transfer_sig)]);
        let result = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs)
            .unwrap()
            .preverify();

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

        let op_proofs = OpProofs::from([OpProof::Ed25519Sig(signature)]);
        let signed_ops = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs)
            .unwrap()
            .preverify()
            .unwrap();

        // Serialize and deserialize
        let serialized = serde_json::to_string(&signed_ops).unwrap();
        let deserialized: Result<SignedOps<Unverified, StandardMode>, _> =
            serde_json::from_str(&serialized);
        let deserialized_signed_ops = deserialized.unwrap().preverify().unwrap();

        assert_eq!(deserialized_signed_ops, signed_ops);
    }

    #[test]
    fn test_signed_mantle_tx_deserialize_rejects_missing_proof() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);
        let tx_hash = mantle_tx.hash();
        let signature = signing_key.sign_payload(&tx_hash.as_signing_bytes());

        let op_proofs = OpProofs::from([OpProof::Ed25519Sig(signature)]);
        let valid =
            SignedOps::<Unverified, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();

        // Dropping a proof can only come from the wire.
        let mut json = serde_json::to_value(&valid).unwrap();
        json["ops_proofs"] = serde_json::json!([]);

        // Both deserialization entrypoints reject it
        let unverified =
            serde_json::from_value::<SignedOps<Unverified, StandardMode>>(json.clone())
                .expect_err("Unverified deserialization should fail");

        assert!(
            unverified
                .to_string()
                .contains("does not match the number of proofs")
        );

        serde_json::from_value::<SignedOps<Preverified, StandardMode>>(json)
            .expect_err("Preverified deserialization should fail");
    }

    #[test]
    fn test_signed_mantle_tx_deserialize_preverified_with_invalid_signature() {
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let wrong_key = Ed25519Key::from_bytes(&[2; 32]);
        let inscribe_op = create_test_inscribe_op(&signing_key);
        let mantle_tx = create_test_mantle_tx(vec![Op::ChannelInscribe(inscribe_op)]);

        let tx_hash = mantle_tx.hash();
        let wrong_signature = wrong_key.sign_payload(&tx_hash.as_signing_bytes());

        let op_proofs = OpProofs::from([OpProof::Ed25519Sig(wrong_signature)]);
        let helper = SignedOps::<_, StandardMode>::from_parts(mantle_tx, op_proofs).unwrap();

        let serialized = serde_json::to_string(&helper).unwrap();

        // Deserialization into `SignedMantleTx<Unverified>` should succeed, even with
        // invalid signature.
        serde_json::from_str::<SignedOps<Unverified, StandardMode>>(&serialized)
            .expect("Unverified deserialization should succeed");

        // Deserialization into `SignedMantleTx<Preverified>` should fail due to invalid
        // signature.
        let deserialized: Result<SignedOps<Preverified, StandardMode>, _> =
            serde_json::from_str(&serialized);

        let err_msg = deserialized
            .expect_err("Preverified deserialization should fail")
            .to_string();
        assert!(err_msg.contains("Invalid signature"));
    }
}
