use std::{
    collections::HashSet,
    sync::{Arc, LazyLock},
};

use ark_ff::PrimeField as _;
use lb_binary_codec::canonical::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError};
use lb_cryptarchia_engine::Slot;
use lb_groth16::{Fr, fr_from_bytes, serde::serde_fr};
#[cfg(any(test, feature = "test-utils"))]
use lb_key_management_system_keys::keys::Ed25519Key;
use lb_key_management_system_keys::keys::UnverifiedEd25519PublicKey;
use lb_poseidon2::Digest as _;
use lb_program_spend::{
    ZkProgramSpendInputsError, ZkProgramSpendPublic, ZkProgramSpendVerifierInput,
};
use lb_utils::bounded::UpperBoundedVec;
use lb_zktransfer::{MAX_INPUTS, MAX_OUTPUTS, ZkTransferInputsError, ZkTransferVerifierInput};
use serde::{Deserialize, Serialize};

use super::{ChannelId, MsgId};
use crate::{
    block::{BlockNumber, MAX_BLOCK_TRANSACTIONS_SIZE},
    crypto::{Digest as _, Hasher, ZkHasher},
    events::TxEvent,
    mantle::{
        Value,
        batch::{
            DeferredProgramProof, DeferredZkProgramSpend, DeferredZkTransfer,
            InscriptionDeferredProofs, ProgramProofInput,
        },
        channel::{ChannelState, Channels, Error},
        gas::{
            EXECUTION_OPENING_GAS, EXECUTION_PAYMENT_GAS, EXECUTION_PROGRAM_PROOF_GAS,
            EXECUTION_PROGRAM_STEP_GAS, EXECUTION_SIGNATURE_GAS, EXECUTION_USER_STEP_GAS, Gas,
            GasOverflow, MainnetGasProfile, NoteSetKey, OpGasCalculator, OpGasContext,
            OperationGas, spend_gas,
        },
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        note_set::{NoteSet, NoteSets},
        notes::{InputNullifiers, OutputCommitments},
        ops::{
            ChannelInscribeProof, OpId, SignedOperation, StepProof, StepProofShape,
            channel::{
                NULL_CHANNEL_ID, UnverifiedChannelKeys,
                program_step::{ProgramStep, payment_msg, payments_hash},
            },
        },
        programs::Programs,
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, Verified},
        },
    },
};

/// The maximum number of bytes that can be inscribed in a single inscription
/// operation. This is derived from the maximum block transactions size,
/// allowing for some overhead.
pub const MAX_BYTES: usize = MAX_BLOCK_TRANSACTIONS_SIZE * 7 / 8;
pub type Inscription = UpperBoundedVec<u8, MAX_BYTES>;

mod serde_inscription {
    use serde::{Deserializer, Serializer};

    use super::{Inscription, MAX_BYTES};

    pub fn serialize<S>(inscription: &Inscription, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        lb_utils::serde::serde_bytes_slice::serialize(inscription, serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Inscription, D::Error>
    where
        D: Deserializer<'de>,
    {
        lb_utils::serde::serde_bytes_slice::deserialize_bounded::<Inscription, MAX_BYTES, D>(
            deserializer,
        )
    }
}

static CHANNEL_STEP_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"CHANNEL_STEP_V1").expect("BigUint should load from constant string")
});

pub const MAX_CHANNEL_STEPS: usize = u16::MAX as usize; // StepCount = UINT16
pub type ChannelSteps = UpperBoundedVec<Step, MAX_CHANNEL_STEPS>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ChannelStep {
    pub inputs: InputNullifiers,
    pub outputs: OutputCommitments,
    #[serde(with = "serde_fr")]
    pub commitment_merkle_root: Fr,
    pub buffered: u16,
}

pub const USER_STEP_KIND: u8 = 0x00;
pub const PROGRAM_STEP_KIND: u8 = 0x01;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    User(ChannelStep),
    Program(ProgramStep),
}

impl From<ChannelStep> for Step {
    fn from(step: ChannelStep) -> Self {
        Self::User(step)
    }
}

impl From<ProgramStep> for Step {
    fn from(step: ProgramStep) -> Self {
        Self::Program(step)
    }
}

impl Step {
    #[must_use]
    pub fn proof_shape(&self) -> StepProofShape {
        match self {
            Self::User(_) => StepProofShape::User,
            Self::Program(step) => StepProofShape::Program {
                payments: step.payments.len(),
                program_notes: step.program_outputs.count(),
            },
        }
    }

    #[must_use]
    pub const fn outputs(&self) -> &OutputCommitments {
        match self {
            Self::User(step) => &step.outputs,
            Self::Program(step) => &step.outputs,
        }
    }
}

// The kind byte, then the step.
impl BinaryEncode for Step {
    fn encoded_length(&self) -> usize {
        1 + match self {
            Self::User(step) => step.encoded_length(),
            Self::Program(step) => step.encoded_length(),
        }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::User(step) => {
                USER_STEP_KIND.encode_into(out);
                step.encode_into(out);
            }
            Self::Program(step) => {
                PROGRAM_STEP_KIND.encode_into(out);
                step.encode_into(out);
            }
        }
    }
}

impl BinaryDecode for Step {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, kind) = u8::decode(input, &())?;
        match kind {
            USER_STEP_KIND => {
                ChannelStep::decode(input, &()).map(|(rest, step)| (rest, Self::User(step)))
            }
            PROGRAM_STEP_KIND => {
                ProgramStep::decode(input, &()).map(|(rest, step)| (rest, Self::Program(step)))
            }
            other => Err(DecodeError::unknown_discriminant::<Self>(u64::from(other))),
        }
    }
}

// The message the ZkTransfer of a step is bound to.
#[must_use]
pub fn step_msg(step: &ChannelStep) -> Fr {
    let mut elements = Vec::with_capacity(3 + step.inputs.len() + step.outputs.len());
    elements.push(*CHANNEL_STEP_V1);
    elements.push(Fr::from(step.inputs.len() as u64));
    elements.extend(step.inputs.iter().map(|note_nullifier| note_nullifier.0));
    elements.push(Fr::from(step.outputs.len() as u64));
    elements.extend(step.outputs.iter().map(|note_commitment| note_commitment.0));
    ZkHasher::digest(&elements)
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct InscriptionOp {
    pub channel_id: ChannelId,
    /// Message to be written in the blockchain
    #[serde(with = "serde_inscription")]
    pub inscription: Inscription,
    /// Enforce that this inscription comes after this tx
    pub parent: MsgId,
    // We can't use a verified Ed25519 public key here because genesis block uses a key of all
    // `0`s, which would fail validation at deserialization time. So we allow all inscription ops
    // to have such a key.
    // This is mitigated by the `verify_strict` we use during verification, but it just would not
    // give us compile-time guarantees about the validity of such a key.
    pub signer: UnverifiedEd25519PublicKey,
    /// Moves of channel notes, in the order they apply
    pub steps: ChannelSteps,
}

impl InscriptionOp {
    #[must_use]
    pub fn id(&self) -> MsgId {
        let mut hasher = Hasher::new();
        hasher.update(self.encode().as_ref());
        MsgId(hasher.finalize().into())
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            channel_id: ChannelId::from([14u8; 32]),
            inscription: b"hello logos".into(),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[15; 32])
                .public_key()
                .into_unverified(),
            steps: ChannelSteps::default(),
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample_with_step() -> Self {
        Self {
            // The steps of the spec vector.
            steps: [
                Step::User(ChannelStep {
                    inputs: [crate::mantle::notes::NoteNullifier(Fr::from(0x23u64))].into(),
                    outputs: [crate::mantle::notes::NoteCommitment(Fr::from(0x24u64))].into(),
                    commitment_merkle_root: Fr::from(0x25u64),
                    buffered: 0x46,
                }),
                Step::Program(ProgramStep::sample()),
            ]
            .into(),
            ..Self::sample()
        }
    }
}

impl OpId for InscriptionOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

pub struct InscriptionPreverificationContext<'a> {
    pub tx_hash_view: &'a TxHashView,
}

pub struct InscriptionValidationContext<'a> {
    pub channels: &'a Channels,
    pub note_sets: &'a NoteSets,
    pub programs: &'a Programs,
    pub block_slot: Slot,
    pub block_number: BlockNumber,
}

pub struct InscriptionExecutionContext {
    pub channels: Channels,
    pub note_sets: NoteSets,
    pub block_slot: Slot,
}

impl ProvableOperation for InscriptionOp {
    type Proof = ChannelInscribeProof;
    const CODE: u8 = 0x11;
}

impl OperationGas<MainnetGasProfile> for InscriptionOp {
    const GAS_COST: Gas = Gas::new(13);
}

impl OpGasCalculator<MainnetGasProfile> for InscriptionOp {
    // The Operation and its signature, plus each step: a user step is a spend
    // over its claimed buffer, a program step is its proof, an opening per
    // program note and its spend, plus each payment as a spend over its claimed
    // buffer. The spend of a program step extends its root over the commitments
    // buffered in the channel note set before the Operation, by the earlier
    // steps and by its own payments.
    fn execution_gas(&self, context: &impl OpGasContext) -> Result<Gas, GasOverflow> {
        let mut buffered = context.buffered(&NoteSetKey::Channel(self.channel_id));
        let mut gas = Self::GAS_COST.checked_add(EXECUTION_SIGNATURE_GAS)?;
        for step in self.steps.iter() {
            match step {
                Step::User(step) => {
                    gas = gas
                        .checked_add(EXECUTION_USER_STEP_GAS)?
                        .checked_add(spend_gas(
                            step.inputs.len(),
                            step.outputs.len(),
                            usize::from(step.buffered),
                        )?)?;
                }
                Step::Program(step) => {
                    gas = gas
                        .checked_add(EXECUTION_PROGRAM_STEP_GAS)?
                        .checked_add(EXECUTION_PROGRAM_PROOF_GAS)?
                        .checked_add(EXECUTION_OPENING_GAS.checked_mul(
                            Value::try_from(step.program_outputs.count())
                                .map_err(|_| GasOverflow)?,
                        )?)?;
                    for payment in step.payments.iter() {
                        gas = gas
                            .checked_add(EXECUTION_PAYMENT_GAS)?
                            .checked_add(spend_gas(
                                payment.inputs.len(),
                                payment.outputs.len(),
                                usize::from(payment.buffered),
                            )?)?;
                        buffered = buffered.saturating_add(payment.outputs.len());
                    }
                    gas = gas.checked_add(spend_gas(
                        step.inputs.len(),
                        step.outputs.len(),
                        buffered,
                    )?)?;
                }
            }
            buffered = buffered.saturating_add(step.outputs().len());
        }
        Ok(gas)
    }
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<InscriptionOp, Unverified, StandardMode>
{
    type Context<'a> = InscriptionPreverificationContext<'a>;
    type Error = Error;

    fn preverify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        // Even if the signer weakness is also checked as part of the `.verify()`
        // function below, we still explicitly check it here, in case something changes
        // within the `.verify()` implementation.
        if self.operation().signer.is_weak() {
            return Err(Error::InvalidSigner);
        }

        // Check the signature
        self.operation()
            .signer
            .verify(context.tx_hash_view.as_bytes(), &self.proof().signature)
            .map_err(|_error| Error::InvalidSignature)?;

        // One ZkTransfer per step
        let steps = &self.operation().steps;
        if self.proof().step_proofs.len() != steps.len() {
            return Err(Error::StepProofCount {
                steps: steps.len(),
                proofs: self.proof().step_proofs.len(),
            });
        }

        // Ensure each step proof is of the kind of its step, and the ZkTransfer circuit
        // fits the inputs and the outputs of each user step
        for (index, (step, proof)) in steps.iter().zip(&self.proof().step_proofs).enumerate() {
            match (step, proof) {
                (Step::User(step), StepProof::User(_)) => {
                    if step.inputs.len() > MAX_INPUTS {
                        return Err(ZkTransferInputsError::TooManyInputs(step.inputs.len()).into());
                    }
                    if step.outputs.len() > MAX_OUTPUTS {
                        return Err(
                            ZkTransferInputsError::TooManyOutputs(step.outputs.len()).into()
                        );
                    }
                }
                (Step::Program(step), StepProof::Program(proof)) => {
                    if step.outputs.len() > lb_program_spend::MAX_OUTPUTS
                        || !step.program_outputs.fits(step.outputs.len())
                    {
                        return Err(Error::ProgramOutputsOutOfRange { step: index });
                    }
                    if step.inputs.len() > lb_program_spend::MAX_INPUTS {
                        return Err(
                            ZkProgramSpendInputsError::TooManyInputs(step.inputs.len()).into()
                        );
                    }
                    // The ZkTransfer circuit fits each payment, as a user step
                    for payment in step.payments.iter() {
                        if payment.inputs.len() > MAX_INPUTS {
                            return Err(
                                ZkTransferInputsError::TooManyInputs(payment.inputs.len()).into()
                            );
                        }
                        if payment.outputs.len() > MAX_OUTPUTS {
                            return Err(ZkTransferInputsError::TooManyOutputs(
                                payment.outputs.len(),
                            )
                            .into());
                        }
                    }
                    if proof.payment_proofs.len() != step.payments.len() {
                        return Err(Error::PaymentProofCount {
                            step: index,
                            payments: step.payments.len(),
                            proofs: proof.payment_proofs.len(),
                        });
                    }
                    if proof.program_notes.len() != step.program_outputs.count() {
                        return Err(Error::ProgramNoteCount {
                            step: index,
                            openings: proof.program_notes.len(),
                            program_outputs: step.program_outputs.count(),
                        });
                    }
                    // Every note of the program the step creates is opened, for the
                    // availability of its state; the check reads the step alone, so a copy
                    // failing it is dropped when it deserializes.
                    for ((output, commitment), opening) in
                        step.program_commitments().zip(&proof.program_notes)
                    {
                        if opening.note(step.program_id).commitment() != *commitment {
                            return Err(Error::ProgramNoteOpening {
                                step: index,
                                output,
                            });
                        }
                    }
                }
                _ => return Err(Error::StepProofKind { step: index }),
            }
        }

        Ok(())
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<InscriptionOp, Preverified, StandardMode>
{
    type DeferredProof = InscriptionDeferredProofs;
    type Context<'a> = InscriptionValidationContext<'a>;
    type Error = Error;

    #[expect(
        clippy::too_many_lines,
        reason = "the channel checks, then the steps of the two kinds in order"
    )]
    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        let operation = self.operation();

        // Check if the channel exist otherwise the inscription is valid only if and
        // only if parent == ZERO
        let note_set = if let Some(channel) = context
            .channels
            .channels
            .get(&operation.channel_id)
            .cloned()
        {
            // Check the parent corresponds to the payload
            if operation.parent != channel.tip_message {
                return Err(Error::InvalidParent {
                    channel_id: operation.channel_id,
                    parent: operation.parent.into(),
                    actual: channel.tip_message.into(),
                });
            }

            // Check that the signer is the authorized one
            if operation.signer
                != channel.accredited_keys[channel.round_robin(context.block_slot).0 as usize]
            {
                return Err(Error::UnauthorizedSigner {
                    channel_id: operation.channel_id,
                    signer: format!("{signer:?}", signer = operation.signer),
                });
            }

            channel.note_set.map(|index| {
                context
                    .note_sets
                    .get(index)
                    .expect("the note set of a channel exists")
                    .clone()
            })
        } else {
            if operation.parent != MsgId::root() {
                // Checked that the parent is ZERO because channel doesn't exist
                return Err(Error::InvalidParent {
                    channel_id: operation.channel_id,
                    parent: operation.parent.into(),
                    actual: MsgId::root().into(),
                });
            }

            // The set the channel gets at its creation, none for the null channel
            (operation.channel_id != NULL_CHANNEL_ID).then(NoteSet::empty)
        };

        // The signature is cheap and checked by `preverify`, only the step proofs
        // are deferred.
        if operation.steps.is_empty() {
            return Ok(InscriptionDeferredProofs::default());
        }
        let mut note_set = note_set.ok_or(Error::NoNoteSet {
            channel_id: operation.channel_id,
        })?;
        let mut step_nullifiers = HashSet::new();
        let tx_roots = context.note_sets.tx_roots();
        let mut deferred = InscriptionDeferredProofs::default();
        // The message of the program spends and the third input of the program proofs
        let op_id_fr = Fr::from_le_bytes_mod_order(&operation.op_id());
        for (index, (step, proof)) in operation
            .steps
            .iter()
            .zip(&self.proof().step_proofs)
            .enumerate()
        {
            match (step, proof) {
                (Step::User(step), StepProof::User(proof)) => {
                    note_set.assert_spendable(
                        step.inputs.as_slice(),
                        &step.commitment_merkle_root,
                        context.block_number,
                    )?;
                    for note_nullifier in step.inputs.iter() {
                        if !step_nullifiers.insert(*note_nullifier) {
                            return Err(Error::StepDoubleSpend(*note_nullifier));
                        }
                    }
                    // `buffered <= len(tx_commitment_buffer)` is checked by
                    // `cached_step_commitment_root`
                    let tx_commitment_root = note_set.cached_step_commitment_root(
                        tx_roots,
                        &step.commitment_merkle_root,
                        usize::from(step.buffered),
                        context.block_number,
                    )?;
                    let nullifiers: Vec<Fr> =
                        step.inputs.iter().map(|nullifier| nullifier.0).collect();
                    let commitments: Vec<Fr> =
                        step.outputs.iter().map(|commitment| commitment.0).collect();
                    let public = ZkTransferVerifierInput::new(
                        &nullifiers,
                        &commitments,
                        0,
                        tx_commitment_root,
                        step_msg(step),
                    )?;
                    deferred
                        .zk_transfers
                        .push(DeferredZkTransfer::new(*proof.as_proof(), public));
                }
                (Step::Program(step), StepProof::Program(proof)) => {
                    let program = context
                        .programs
                        .get(&step.program_id)
                        .ok_or(Error::UnknownProgram(step.program_id))?;
                    let mut paid_value: Value = 0;
                    for (payment, payment_proof) in step.payments.iter().zip(&proof.payment_proofs)
                    {
                        note_set.assert_spendable(
                            payment.inputs.as_slice(),
                            &payment.commitment_merkle_root,
                            context.block_number,
                        )?;
                        for note_nullifier in payment.inputs.iter() {
                            if !step_nullifiers.insert(*note_nullifier) {
                                return Err(Error::StepDoubleSpend(*note_nullifier));
                            }
                        }
                        let tx_commitment_root = note_set.cached_step_commitment_root(
                            tx_roots,
                            &payment.commitment_merkle_root,
                            usize::from(payment.buffered),
                            context.block_number,
                        )?;
                        let nullifiers: Vec<Fr> =
                            payment.inputs.iter().map(|nullifier| nullifier.0).collect();
                        let commitments: Vec<Fr> = payment
                            .outputs
                            .iter()
                            .map(|commitment| commitment.0)
                            .collect();
                        let public = ZkTransferVerifierInput::new(
                            &nullifiers,
                            &commitments,
                            payment.paid_value,
                            tx_commitment_root,
                            payment_msg(&step.program_id, payment),
                        )?;
                        deferred
                            .zk_transfers
                            .push(DeferredZkTransfer::new(*payment_proof.as_proof(), public));
                        // The program spend is verified with the change of the payments
                        note_set = note_set.buffer_outputs(payment.outputs.as_slice());
                        paid_value = paid_value
                            .checked_add(payment.paid_value)
                            .ok_or(Error::PaidValueOverflow { step: index })?;
                    }
                    if !step.inputs.is_empty() {
                        note_set.assert_spendable(
                            step.inputs.as_slice(),
                            &step.commitment_merkle_root,
                            context.block_number,
                        )?;
                    }
                    for note_nullifier in step.inputs.iter() {
                        if !step_nullifiers.insert(*note_nullifier) {
                            return Err(Error::StepDoubleSpend(*note_nullifier));
                        }
                    }
                    // Rejects a root outside the window when the spend consumes nothing
                    let tx_commitment_root = note_set.cached_tx_commitment_root(
                        tx_roots,
                        &step.commitment_merkle_root,
                        context.block_number,
                    )?;
                    let nullifiers: Vec<Fr> =
                        step.inputs.iter().map(|nullifier| nullifier.0).collect();
                    let commitments: Vec<Fr> =
                        step.outputs.iter().map(|commitment| commitment.0).collect();
                    let public = ZkProgramSpendVerifierInput::new(&ZkProgramSpendPublic {
                        inputs: &nullifiers,
                        outputs: &commitments,
                        outputs_of_program: &step.program_outputs.flags(step.outputs.len()),
                        tx_commitment_root,
                        program_id: step.program_id,
                        paid_value,
                        spend_notes_hash: step.spend_notes_hash,
                        msg: op_id_fr,
                    })?;
                    deferred.program_spends.push(DeferredZkProgramSpend::new(
                        *proof.spend_proof.as_proof(),
                        public,
                    ));
                    deferred.program_proofs.push(DeferredProgramProof::new(
                        *proof.program_proof.as_proof(),
                        ProgramProofInput {
                            program: program.clone(),
                            public: [step.spend_notes_hash, payments_hash(step), op_id_fr],
                        },
                    ));
                }
                _ => return Err(Error::StepProofKind { step: index }),
            }
            // The next steps can cover the outputs of this one with their buffered
            note_set = note_set.buffer_outputs(step.outputs().as_slice());
        }
        Ok(deferred)
    }
}

impl<Mode: VerificationMode> ExecutableOperation
    for SignedOperation<InscriptionOp, Verified, Mode>
{
    type Context<'a> = InscriptionExecutionContext;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // if the channel doesn't exist, create it
        if context
            .channels
            .channel_state(&operation.channel_id)
            .is_none()
        {
            (context.channels, context.note_sets) = context.channels.create_channel(
                operation.channel_id,
                context.block_slot,
                UnverifiedChannelKeys::from(operation.signer),
                context.note_sets,
            );
        }
        let channel = context
            .channels
            .channels
            .get(&operation.channel_id)
            .cloned()
            .ok_or(Error::ChannelNotFound {
                channel_id: operation.channel_id,
            })?;

        // Apply the steps to the channel note set, in order
        if !operation.steps.is_empty() {
            let index = channel.note_set.ok_or(Error::NoNoteSet {
                channel_id: operation.channel_id,
            })?;
            let mut note_set = context
                .note_sets
                .get(index)
                .expect("the note set of a channel exists")
                .clone();
            for step in operation.steps.iter() {
                let inputs = match step {
                    Step::User(step) => &step.inputs,
                    // The payments of a program step apply before its program spend
                    Step::Program(step) => {
                        for payment in step.payments.iter() {
                            note_set = note_set
                                .execute_spending(payment.inputs.as_slice())?
                                .execute_adding(payment.outputs.as_slice())?
                                .buffer_outputs(payment.outputs.as_slice());
                        }
                        &step.inputs
                    }
                };
                note_set = note_set
                    .execute_spending(inputs.as_slice())?
                    .execute_adding(step.outputs().as_slice())?
                    // The outputs extend the buffer the next Operations of the
                    // transaction are proven against.
                    .buffer_outputs(step.outputs().as_slice());
            }
            context.note_sets = context.note_sets.update(index, note_set);
        }

        // Update the channel sequencer, its starting slot, the tip message and the tip
        // slot
        let (new_sequencer, new_starting_slot) = channel.round_robin(context.block_slot);
        context.channels.channels = context.channels.channels.insert(
            operation.channel_id,
            ChannelState {
                tip_message: operation.id(),
                accredited_keys: Arc::clone(&channel.accredited_keys),
                tip_sequencer: new_sequencer,
                tip_sequencer_starting_slot: new_starting_slot,
                tip_slot: context.block_slot,
                ..channel
            },
        );
        Ok((context, Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::{
        AdditiveGroup as _, CompressedGroth16Proof, Field as _, Groth16Input,
        test_utils::{three_input_prove, three_input_setup},
    };
    use lb_key_management_system_keys::keys::{ZkKey, ZkPublicKey};
    use lb_utils::bounded::BoundedError;

    use super::*;
    use crate::{
        crypto::Hash,
        mantle::{
            TxHash,
            batch::{DeferrableProof as _, DeferredProofs},
            gas::test_utils::FixedThresholds,
            note_set::{CommitmentMmr, NoteSetError},
            notes::{self, NoteCommitment, NoteNullifier, derive_note_nullifier},
            ops::{
                ProgramNoteOpening, ProgramProof, ProgramStepProof, ZkProgramSpendProof,
                ZkTransferProof,
                channel::program_step::{Payment, ProgramOutputs, test_utils::prove_program_spend},
                op_proof::samples::SampleProof as _,
                transfer::test_utils::prove_transfer,
            },
            programs::{
                ProgramVerificationKey, RegisteredProgram, SpendInputFields, SpendOutputFields,
                derive_program_id, spend_notes_hash,
            },
            transactions::tx_list::signed_ops::test_utils::make_channel_state,
        },
    };

    fn preverified(
        operation: InscriptionOp,
    ) -> SignedOperation<InscriptionOp, Preverified, StandardMode> {
        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof = ChannelInscribeProof::without_steps(
            Ed25519Key::from_bytes(&[15; 32]).sign_payload(tx_hash_view.as_bytes().as_ref()),
        );

        SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view,
            })
            .expect("the sample signer signed this transaction hash")
    }

    fn channels(channel_id: ChannelId, state: ChannelState) -> Channels {
        let mut channels = Channels::new();
        channels.channels.insert_mut(channel_id, state);

        channels
    }

    fn sample() -> InscriptionOp {
        InscriptionOp {
            channel_id: ChannelId([0u8; 32]),
            inscription: b"genesis".into(),
            parent: MsgId([0u8; 32]),
            signer: UnverifiedEd25519PublicKey::from_bytes(&[0u8; 32]).unwrap(),
            steps: ChannelSteps::default(),
        }
    }

    #[test]
    fn oversized_inscription_rejected_at_construction() {
        let oversized = vec![0u8; MAX_BYTES + 1];
        let err = Inscription::try_from(oversized).unwrap_err();
        assert!(
            matches!(err, BoundedError::TooManyItems { count, max } if count == MAX_BYTES + 1 && max == MAX_BYTES)
        );
    }

    #[test]
    fn oversized_inscription_rejected_on_deserialize() {
        let oversized = vec![0u8; MAX_BYTES + 1];
        let bytes = bincode::serialize(&oversized).unwrap();
        let err = bincode::deserialize::<Inscription>(&bytes).unwrap_err();
        assert!(
            format!("{err}").contains(
                format!(
                    "Item count {} exceeds static maximum of {MAX_BYTES}",
                    MAX_BYTES + 1,
                )
                .as_str()
            ),
            "{err:?}",
        );
    }

    #[test]
    fn json_round_trip() {
        let op = sample();
        let json = serde_json::to_string(&op).unwrap();
        assert!(
            json.contains("\"67656e65736973\""),
            "inscription should be hex in JSON"
        );
        let recovered: InscriptionOp = serde_json::from_str(&json).unwrap();
        assert_eq!(op, recovered);
    }

    #[test]
    fn bincode_round_trip() {
        let op = sample();
        let bytes = bincode::serialize(&op).unwrap();
        let recovered: InscriptionOp = bincode::deserialize(&bytes).unwrap();
        assert_eq!(op, recovered);
    }

    #[test]
    fn preverify_rejects_a_signature_over_another_transaction() {
        let signing_key = Ed25519Key::from_bytes(&[15; 32]);
        let signed_hash: Hash = Hasher::digest(b"signed").into();
        let other_hash: Hash = Hasher::digest(b"other").into();
        let signed_view = TxHashView::new(TxHash::from(signed_hash));
        let other_view = TxHashView::new(TxHash::from(other_hash));

        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            InscriptionOp::sample(),
            ChannelInscribeProof::without_steps(
                signing_key.sign_payload(signed_view.as_bytes().as_ref()),
            ),
        );

        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &signed_view
            }),
            Ok(())
        );
        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &other_view
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn preverify_rejects_a_signature_unrelated_to_the_op() {
        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof = ChannelInscribeProof::without_steps(
            Ed25519Key::from_bytes(&[16; 32]).sign_payload(tx_hash_view.as_bytes().as_ref()),
        );

        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(InscriptionOp::sample(), proof);

        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn verify_accepts_an_unregistered_channel_rooted_inscription() {
        let signed_operation = preverified(InscriptionOp::sample());

        assert!(
            signed_operation
                .verify(&InscriptionValidationContext {
                    channels: &Channels::new(),
                    note_sets: &NoteSets::new(),
                    programs: &Programs::new(),
                    block_slot: Slot::from(0),
                    block_number: 1,
                })
                .is_ok()
        );
    }

    #[test]
    fn verify_rejects_an_unrooted_inscription_on_an_unregistered_channel() {
        let operation = InscriptionOp {
            parent: MsgId([1u8; 32]),
            ..InscriptionOp::sample()
        };
        let channel_id = operation.channel_id;
        let parent = operation.parent;
        let signed_operation = preverified(operation);

        assert_eq!(
            signed_operation
                .verify(&InscriptionValidationContext {
                    channels: &Channels::new(),
                    note_sets: &NoteSets::new(),
                    programs: &Programs::new(),
                    block_slot: Slot::from(0),
                    block_number: 1,
                })
                .unwrap_err(),
            Error::InvalidParent {
                channel_id,
                parent: parent.into(),
                actual: MsgId::root().into(),
            }
        );
    }

    #[test]
    fn verify_rejects_an_inscription_that_does_not_follow_the_channel_tip() {
        let operation = InscriptionOp::sample();
        let channel_id = operation.channel_id;
        let parent = operation.parent;
        let signed_operation = preverified(operation);

        let tip_message = MsgId([2u8; 32]);
        let channels = channels(
            channel_id,
            ChannelState {
                tip_message,
                ..make_channel_state(Some(
                    [Ed25519Key::from_bytes(&[15; 32])
                        .public_key()
                        .into_unverified()]
                    .into(),
                ))
            },
        );

        assert_eq!(
            signed_operation
                .verify(&InscriptionValidationContext {
                    channels: &channels,
                    note_sets: &NoteSets::new(),
                    programs: &Programs::new(),
                    block_slot: Slot::from(0),
                    block_number: 1,
                })
                .unwrap_err(),
            Error::InvalidParent {
                channel_id,
                parent: parent.into(),
                actual: tip_message.into(),
            }
        );
    }

    #[test]
    fn verify_rejects_an_inscription_from_a_key_the_channel_does_not_accredit() {
        let operation = InscriptionOp::sample();
        let channel_id = operation.channel_id;
        let signer = operation.signer;
        let signed_operation = preverified(operation);

        let channels = channels(
            channel_id,
            make_channel_state(Some(
                [Ed25519Key::from_bytes(&[16; 32])
                    .public_key()
                    .into_unverified()]
                .into(),
            )),
        );

        assert_eq!(
            signed_operation
                .verify(&InscriptionValidationContext {
                    channels: &channels,
                    note_sets: &NoteSets::new(),
                    programs: &Programs::new(),
                    block_slot: Slot::from(0),
                    block_number: 1,
                })
                .unwrap_err(),
            Error::UnauthorizedSigner {
                channel_id,
                signer: format!("{signer:?}"),
            }
        );
    }

    #[test]
    fn verify_accepts_an_inscription_from_the_sequencer_on_duty() {
        let operation = InscriptionOp::sample();
        let channel_id = operation.channel_id;
        let signer = operation.signer;
        let signed_operation = preverified(operation);

        let channels = channels(
            channel_id,
            ChannelState {
                tip_sequencer: 1,
                ..make_channel_state(Some(
                    [
                        Ed25519Key::from_bytes(&[16; 32])
                            .public_key()
                            .into_unverified(),
                        signer,
                    ]
                    .into(),
                ))
            },
        );

        assert!(
            signed_operation
                .verify(&InscriptionValidationContext {
                    channels: &channels,
                    note_sets: &NoteSets::new(),
                    programs: &Programs::new(),
                    block_slot: Slot::from(0),
                    block_number: 1,
                })
                .is_ok()
        );
    }

    #[test]
    fn execute_bootstraps_a_channel_the_ledger_does_not_hold_yet() {
        let operation = InscriptionOp::sample();
        let channel_id = operation.channel_id;
        let signer = operation.signer;
        let message_id = operation.id();
        let block_slot = Slot::from(7u64);

        let signed_operation: SignedOperation<_, _, StandardMode> = SignedOperation::new(
            operation,
            <InscriptionOp as ProvableOperation>::Proof::sample(),
        )
        .into_state_trusted();

        let (context, events) = signed_operation
            .execute(InscriptionExecutionContext {
                channels: Channels::new(),
                note_sets: NoteSets::new(),
                block_slot,
            })
            .expect("inscribing never fails");

        assert_eq!(
            context.channels.channel_state(&channel_id),
            Some(&ChannelState {
                accredited_keys: Arc::new(UnverifiedChannelKeys::from(signer)),
                configuration_threshold: 1,
                tip_message: message_id,
                config_tip_hash: MsgId::root(),
                tip_slot: block_slot,
                tip_sequencer: 0,
                tip_sequencer_starting_slot: block_slot,
                posting_timeframe: 0.into(),
                posting_timeout: 0.into(),
                note_set: Some(2),
            })
        );
        assert_eq!(context.note_sets.get(2), Some(&NoteSet::empty()));
        assert_eq!(events, []);
    }

    #[test]
    fn execute_bootstraps_the_null_channel_without_a_note_set() {
        let operation = InscriptionOp {
            channel_id: NULL_CHANNEL_ID,
            ..InscriptionOp::sample()
        };
        let signed_operation: SignedOperation<_, _, StandardMode> = SignedOperation::new(
            operation,
            <InscriptionOp as ProvableOperation>::Proof::sample(),
        )
        .into_state_trusted();

        let (context, _) = signed_operation
            .execute(InscriptionExecutionContext {
                channels: Channels::new(),
                note_sets: NoteSets::new(),
                block_slot: Slot::from(7u64),
            })
            .expect("inscribing never fails");

        assert_eq!(
            context
                .channels
                .channel_state(&NULL_CHANNEL_ID)
                .unwrap()
                .note_set,
            None
        );
        assert_eq!(context.note_sets, NoteSets::new());
    }

    #[test]
    fn execute_rotates_the_sequencer_of_an_existing_channel() {
        let operation = InscriptionOp::sample();
        let channel_id = operation.channel_id;
        let message_id = operation.id();
        let keys: UnverifiedChannelKeys = [
            Ed25519Key::from_bytes(&[16; 32])
                .public_key()
                .into_unverified(),
            operation.signer,
        ]
        .into();

        let channels = channels(
            channel_id,
            ChannelState {
                configuration_threshold: 2,
                posting_timeframe: 1.into(),
                ..make_channel_state(Some(keys.clone()))
            },
        );

        let signed_operation: SignedOperation<_, _, StandardMode> = SignedOperation::new(
            operation,
            <InscriptionOp as ProvableOperation>::Proof::sample(),
        )
        .into_state_trusted();

        let (context, events) = signed_operation
            .execute(InscriptionExecutionContext {
                channels,
                note_sets: NoteSets::new(),
                block_slot: Slot::from(1u64),
            })
            .expect("inscribing never fails");

        assert_eq!(
            context.channels.channel_state(&channel_id),
            Some(&ChannelState {
                accredited_keys: Arc::new(keys),
                configuration_threshold: 2,
                tip_message: message_id,
                config_tip_hash: MsgId::root(),
                tip_slot: Slot::from(1u64),
                tip_sequencer: 1,
                tip_sequencer_starting_slot: Slot::from(1u64),
                posting_timeframe: 1.into(),
                posting_timeout: 0.into(),
                note_set: None,
            })
        );
        assert_eq!(events, []);
    }

    #[test]
    fn inscription_op_execution_gas_does_not_scale_with_the_threshold() {
        for threshold in [0, 1, 3] {
            assert_eq!(
                InscriptionOp::sample().execution_gas(&FixedThresholds(threshold)),
                // 13 + 167
                Ok(Gas::new(180))
            );
        }
    }

    #[test]
    fn inscription_op_execution_gas_charges_a_spend_per_user_step() {
        let step = ChannelStep {
            inputs: [NoteNullifier(Fr::from(1u64))].into(),
            outputs: OutputCommitments::default(),
            commitment_merkle_root: Fr::ZERO,
            buffered: 0,
        };
        // 180 + steps * (137 + 1946 + 207), one input, no output, nothing buffered
        for (steps, expected) in [(0usize, 180u64), (1, 2_470), (3, 7_050)] {
            let operation = InscriptionOp {
                steps: ChannelSteps::try_from(vec![Step::User(step.clone()); steps]).unwrap(),
                ..InscriptionOp::sample()
            };
            for threshold in [0, 3] {
                assert_eq!(
                    operation.execution_gas(&FixedThresholds(threshold)),
                    Ok(Gas::new(expected))
                );
            }
        }
    }

    #[test]
    fn inscription_op_execution_gas_charges_each_program_step() {
        let user = Step::User(step(&[nullifier(1)], &[], Fr::ZERO, 0));
        let program = |payments: usize| {
            Step::Program(ProgramStep {
                payments: vec![Payment::sample(); payments].try_into().unwrap(),
                ..ProgramStep::sample()
            })
        };
        // A program step is 60 + 6305 + 157 (one opening), its spend of one input
        // and one output (1946 + 207 + 25, plus 887 + 23 * b when b commitments are
        // buffered) and, per payment of one input and one output claiming 71
        // buffered, 211 + 1946 + 207 + 25 + 887 + 23 * 71 = 4_909.
        for (steps, expected) in [
            (vec![], 180u64),
            // 180 + 6_522 + 2_178
            (vec![program(0)], 8_880),
            // 180 + 6_522 + 4_909 + (2_178 + 887 + 23)
            (vec![program(1)], 14_699),
            // 180 + 2_290 + 14_519, the spec vector inscription
            (vec![user, program(1)], 16_989),
            // 14_699 + (6_522 + 3 * 4_909 + 2_178 + 887 + 23 * 5): the second program
            // step finds the two outputs of the first (its payment and itself) and
            // its own three payments buffered
            (vec![program(1), program(3)], 39_128),
        ] {
            let operation = InscriptionOp {
                steps: steps.try_into().unwrap(),
                ..InscriptionOp::sample()
            };
            assert_eq!(
                operation.execution_gas(&FixedThresholds(3)),
                Ok(Gas::new(expected))
            );
        }
    }

    #[test]
    fn inscription_program_step_counts_its_payment_buffer() {
        let payment = Payment {
            buffered: 0,
            ..Payment::sample()
        };
        let operation = InscriptionOp {
            steps: vec![Step::Program(ProgramStep {
                payments: vec![payment].try_into().unwrap(),
                ..ProgramStep::sample()
            })]
            .try_into()
            .unwrap(),
            ..InscriptionOp::sample()
        };
        // 180 + (60 + 6305 + 157) + the payment (211 + 1946 + 207 + 25) + the spend
        // of the step, its root extended over the one output of the payment
        // (1946 + 887 + 23 + 207 + 25)
        assert_eq!(
            operation.execution_gas(&FixedThresholds(3)),
            Ok(Gas::new(180 + 6_522 + 2_389 + 3_088))
        );
    }

    fn commitment(n: u64) -> NoteCommitment {
        NoteCommitment(Fr::from(n))
    }

    fn nullifier(n: u64) -> NoteNullifier {
        NoteNullifier(Fr::from(n))
    }

    fn step(
        inputs: &[NoteNullifier],
        outputs: &[NoteCommitment],
        commitment_merkle_root: Fr,
        buffered: u16,
    ) -> ChannelStep {
        ChannelStep {
            inputs: InputNullifiers::try_from_iter(inputs.iter().copied()).unwrap(),
            outputs: OutputCommitments::try_from_iter(outputs.iter().copied()).unwrap(),
            commitment_merkle_root,
            buffered,
        }
    }

    fn with_steps(steps: Vec<Step>) -> InscriptionOp {
        InscriptionOp {
            steps: ChannelSteps::try_from(steps).unwrap(),
            ..InscriptionOp::sample()
        }
    }

    fn signer() -> Ed25519Key {
        Ed25519Key::from_bytes(&[15; 32])
    }

    // The sample channel, accrediting the sample signer, owns note set 2, which
    // holds commitment(1), recorded at the end of block 0.
    fn state() -> (Channels, NoteSets) {
        let channel_id = InscriptionOp::sample().channel_id;
        let keys = UnverifiedChannelKeys::from(signer().public_key().into_unverified());
        let (channels, note_sets) =
            Channels::new().create_channel(channel_id, Slot::from(0u64), keys, NoteSets::new());
        let set = NoteSet::empty().execute_adding(&[commitment(1)]).unwrap();
        (channels, note_sets.update(2, set).end_block(0))
    }

    fn root() -> Fr {
        CommitmentMmr::new()
            .push(commitment(1))
            .unwrap()
            .frontier_root()
    }

    fn tx_hash_view() -> TxHashView {
        TxHashView::from(TxHash::from([9u8; 32]))
    }

    fn signed(
        operation: InscriptionOp,
        step_proofs: Vec<StepProof>,
    ) -> SignedOperation<InscriptionOp, Unverified, StandardMode> {
        let signature = signer().sign_payload(tx_hash_view().as_bytes().as_ref());
        SignedOperation::new(operation, ChannelInscribeProof::new(signature, step_proofs))
    }

    fn verify_steps(
        steps: Vec<Step>,
        channels: &Channels,
        note_sets: &NoteSets,
    ) -> Result<InscriptionDeferredProofs, Error> {
        let proofs = steps
            .iter()
            .map(|step| StepProof::sample_for(step.proof_shape()))
            .collect();
        signed(with_steps(steps), proofs)
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view(),
            })
            .expect("the sample signer signed the transaction hash")
            .verify(&InscriptionValidationContext {
                channels,
                note_sets,
                programs: &Programs::new(),
                block_slot: Slot::from(0u64),
                block_number: 1,
            })
    }

    #[test]
    fn step_msg_hashes_the_counts_and_the_notes() {
        let dst = fr_from_bytes(b"CHANNEL_STEP_V1").unwrap();
        assert_eq!(
            step_msg(&step(
                &[nullifier(0x23)],
                &[commitment(0x24), commitment(0x25)],
                Fr::ZERO,
                0
            )),
            ZkHasher::digest(&[
                dst,
                Fr::from(1u64),
                Fr::from(0x23u64),
                Fr::from(2u64),
                Fr::from(0x24u64),
                Fr::from(0x25u64)
            ])
        );
        assert_ne!(
            step_msg(&step(&[nullifier(1), nullifier(2)], &[], Fr::ZERO, 0)),
            step_msg(&step(&[nullifier(1)], &[commitment(2)], Fr::ZERO, 0))
        );
        assert_eq!(
            step_msg(&step(&[nullifier(1)], &[], Fr::ZERO, 0)),
            step_msg(&step(&[nullifier(1)], &[], Fr::ONE, 7))
        );
    }

    #[test]
    fn preverify_rejects_a_step_proof_count_other_than_the_step_count() {
        let signed_operation = signed(
            with_steps(vec![Step::User(step(
                &[nullifier(2)],
                &[commitment(3)],
                root(),
                0,
            ))]),
            vec![],
        );
        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view()
            }),
            Err(Error::StepProofCount {
                steps: 1,
                proofs: 0
            })
        );
    }

    #[test]
    fn preverify_rejects_a_step_with_more_than_eight_outputs() {
        let outputs: Vec<NoteCommitment> = (1..=9).map(commitment).collect();
        let signed_operation = signed(
            with_steps(vec![Step::User(step(&[nullifier(2)], &outputs, root(), 0))]),
            vec![StepProof::User(ZkTransferProof::sample())],
        );
        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view()
            }),
            Err(Error::ZkTransferInputs(
                ZkTransferInputsError::TooManyOutputs(9)
            ))
        );
    }

    #[test]
    fn verify_rejects_steps_on_the_null_channel() {
        let operation = InscriptionOp {
            channel_id: NULL_CHANNEL_ID,
            ..with_steps(vec![Step::User(step(
                &[nullifier(2)],
                &[commitment(3)],
                root(),
                0,
            ))])
        };
        let deferred = signed(operation, vec![StepProof::User(ZkTransferProof::sample())])
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view(),
            })
            .unwrap()
            .verify(&InscriptionValidationContext {
                channels: &Channels::new(),
                note_sets: &NoteSets::new(),
                programs: &Programs::new(),
                block_slot: Slot::from(0u64),
                block_number: 1,
            });
        assert_eq!(
            deferred.unwrap_err(),
            Error::NoNoteSet {
                channel_id: NULL_CHANNEL_ID
            }
        );
    }

    #[test]
    fn verify_defers_one_balanced_transfer_per_step_bound_to_its_step_msg() {
        let (channels, note_sets) = state();
        let steps = vec![
            step(&[nullifier(2)], &[commitment(3)], root(), 0),
            step(&[nullifier(4)], &[commitment(5)], root(), 0),
        ];
        let deferred = verify_steps(
            steps.iter().cloned().map(Step::User).collect(),
            &channels,
            &note_sets,
        )
        .unwrap();
        assert_eq!(deferred.zk_transfers.len(), 2);
        for (deferred, step) in deferred.zk_transfers.iter().zip(&steps) {
            assert_eq!(deferred.inputs().excess_value.into_inner(), Fr::ZERO);
            assert_eq!(deferred.inputs().msg.into_inner(), step_msg(step));
            assert_eq!(deferred.inputs().tx_commitment_root.into_inner(), root());
            assert_eq!(
                deferred.inputs().inputs[0].into_inner(),
                step.inputs.as_slice()[0].0
            );
        }
    }

    #[test]
    fn verify_extends_each_step_root_with_its_buffered_prefix() {
        let (channels, note_sets) = state();
        // An earlier Operation of the transaction deposited commitment(7) into the
        // channel.
        let set = note_sets
            .get(2)
            .unwrap()
            .clone()
            .buffer_outputs(&[commitment(7)]);
        let note_sets = note_sets.update(2, set);
        let first = Step::User(step(&[nullifier(2)], &[commitment(3)], root(), 0));
        let second = Step::User(step(&[nullifier(4)], &[commitment(5)], root(), 2));
        let deferred = verify_steps(vec![first.clone(), second], &channels, &note_sets).unwrap();
        let extended = CommitmentMmr::new()
            .push(commitment(1))
            .unwrap()
            .push(commitment(7))
            .unwrap()
            .push(commitment(3))
            .unwrap()
            .frontier_root();
        assert_eq!(
            deferred.zk_transfers[0]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            root()
        );
        assert_eq!(
            deferred.zk_transfers[1]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            extended
        );
        assert_eq!(
            verify_steps(
                vec![
                    first,
                    Step::User(step(&[nullifier(4)], &[commitment(5)], root(), 3))
                ],
                &channels,
                &note_sets
            )
            .unwrap_err(),
            Error::NoteSet(NoteSetError::BufferedTooLarge {
                buffered: 3,
                len: 2
            })
        );
    }

    #[test]
    fn verify_rejects_a_note_consumed_by_an_earlier_step() {
        let (channels, note_sets) = state();
        let steps = vec![
            Step::User(step(&[nullifier(2)], &[commitment(3)], root(), 0)),
            Step::User(step(&[nullifier(2)], &[commitment(4)], root(), 0)),
        ];
        assert_eq!(
            verify_steps(steps, &channels, &note_sets).unwrap_err(),
            Error::StepDoubleSpend(nullifier(2))
        );
    }

    #[test]
    fn verify_rejects_a_spent_channel_note() {
        let (channels, note_sets) = state();
        let set = note_sets
            .get(2)
            .unwrap()
            .clone()
            .execute_spending(&[nullifier(2)])
            .unwrap();
        let note_sets = note_sets.update(2, set);
        assert_eq!(
            verify_steps(
                vec![Step::User(step(
                    &[nullifier(2)],
                    &[commitment(3)],
                    root(),
                    0
                ))],
                &channels,
                &note_sets
            )
            .unwrap_err(),
            Error::NoteSet(NoteSetError::AlreadySpent(nullifier(2)))
        );
    }

    #[test]
    fn execute_spends_adds_and_buffers_every_step_in_order() {
        let (channels, note_sets) = state();
        let operation = with_steps(vec![
            Step::User(step(&[nullifier(2)], &[commitment(3)], root(), 0)),
            Step::User(step(&[nullifier(4)], &[commitment(5)], root(), 1)),
        ]);
        let signed_operation: SignedOperation<_, _, StandardMode> = signed(
            operation,
            vec![
                StepProof::User(ZkTransferProof::sample()),
                StepProof::User(ZkTransferProof::sample()),
            ],
        )
        .into_state_trusted();

        let (context, _) = signed_operation
            .execute(InscriptionExecutionContext {
                channels,
                note_sets,
                block_slot: Slot::from(1u64),
            })
            .expect("the channel has a note set");

        let set = context.note_sets.get(2).unwrap();
        let expected = CommitmentMmr::new()
            .push(commitment(1))
            .unwrap()
            .push(commitment(3))
            .unwrap()
            .push(commitment(5))
            .unwrap()
            .frontier_root();
        assert!(set.contains_nullifier(&nullifier(2)) && set.contains_nullifier(&nullifier(4)));
        assert_eq!(set.commitment_root(), expected);
        assert_eq!(set.tx_commitment_root(&root(), 1), Ok(expected));
    }

    #[test]
    fn step_proof_is_bound_to_its_step_msg() {
        let secret_key = Fr::from(1u64);
        let input = notes::Note::new(
            1_000,
            Fr::from(11u64),
            ZkKey::new(secret_key).to_public_key(),
        );
        let (mmr, path) = CommitmentMmr::new()
            .push_with_paths(input.commitment(), &mut [])
            .unwrap();
        let (channels, note_sets) = state();
        let set = NoteSet::empty()
            .execute_adding(&[input.commitment()])
            .unwrap();
        let note_sets = note_sets.update(2, set).end_block(1);
        let output = notes::Note::new(1_000, Fr::from(12u64), ZkPublicKey::new(Fr::from(2u64)));
        let step = step(
            &[derive_note_nullifier(&input.commitment(), &secret_key)],
            &[output.commitment()],
            mmr.frontier_root(),
            0,
        );
        let proof = prove_transfer(
            &[(input, secret_key, path)],
            &[output],
            mmr.frontier_root(),
            step_msg(&step),
        );

        let deferred = signed(
            with_steps(vec![Step::User(step)]),
            vec![StepProof::User(ZkTransferProof::new(proof))],
        )
        .into_preverified(&InscriptionPreverificationContext {
            tx_hash_view: &tx_hash_view(),
        })
        .unwrap()
        .verify(&InscriptionValidationContext {
            channels: &channels,
            note_sets: &note_sets,
            programs: &Programs::new(),
            block_slot: Slot::from(0u64),
            block_number: 2,
        })
        .unwrap();
        let deferred = &deferred.zk_transfers;
        assert!(lb_zktransfer::verify(deferred[0].proof(), deferred[0].inputs()).unwrap());

        let as_a_transfer = ZkTransferVerifierInput {
            msg: Groth16Input::new(*tx_hash_view().as_fr()),
            ..*deferred[0].inputs()
        };
        assert!(!lb_zktransfer::verify(deferred[0].proof(), &as_a_transfer).unwrap());
    }

    #[test]
    fn step_decode_reads_the_kind_byte() {
        use lb_binary_codec::canonical::{BinaryDecode as _, BinaryEncode as _, DecodeError};

        let program = Step::Program(ProgramStep::sample());
        assert_eq!(program.encode()[0], PROGRAM_STEP_KIND);
        assert_eq!(
            Step::decode(&program.encode(), &()).unwrap(),
            (&[][..], program)
        );
        let user = Step::User(step(&[nullifier(2)], &[commitment(3)], Fr::ZERO, 0));
        assert_eq!(user.encode()[0], USER_STEP_KIND);
        let mut bytes = user.encode();
        bytes[0] = 0x02;
        assert_eq!(
            Step::decode(&bytes, &()).unwrap_err(),
            DecodeError::unknown_discriminant::<Step>(2)
        );
    }

    fn program_key() -> ProgramVerificationKey {
        ProgramVerificationKey::try_from(&three_input_setup(1).0).unwrap()
    }

    fn program_id() -> Fr {
        derive_program_id(&program_key())
    }

    fn programs() -> Programs {
        Programs::new()
            .register(RegisteredProgram::new(program_key()).unwrap())
            .0
    }

    fn payment(
        inputs: &[NoteNullifier],
        outputs: &[NoteCommitment],
        commitment_merkle_root: Fr,
        buffered: u16,
        paid_value: u64,
    ) -> Payment {
        Payment {
            inputs: InputNullifiers::try_from_iter(inputs.iter().copied()).unwrap(),
            outputs: OutputCommitments::try_from_iter(outputs.iter().copied()).unwrap(),
            commitment_merkle_root,
            buffered,
            paid_value,
            intent: Fr::from(paid_value),
        }
    }

    fn program_step(
        payments: Vec<Payment>,
        inputs: &[NoteNullifier],
        outputs: &[NoteCommitment],
        program_outputs: u8,
        commitment_merkle_root: Fr,
    ) -> ProgramStep {
        ProgramStep {
            program_id: program_id(),
            payments: payments.try_into().unwrap(),
            inputs: InputNullifiers::try_from_iter(inputs.iter().copied()).unwrap(),
            outputs: OutputCommitments::try_from_iter(outputs.iter().copied()).unwrap(),
            program_outputs: ProgramOutputs::from(program_outputs),
            commitment_merkle_root,
            spend_notes_hash: Fr::from(77u64),
        }
    }

    fn verify_operation(
        operation: InscriptionOp,
        proofs: Vec<StepProof>,
        note_sets: &NoteSets,
    ) -> Result<InscriptionDeferredProofs, Error> {
        let (channels, _) = state();
        signed(operation, proofs)
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view(),
            })
            .expect("preverify accepts the inscription")
            .verify(&InscriptionValidationContext {
                channels: &channels,
                note_sets,
                programs: &programs(),
                block_slot: Slot::from(0u64),
                block_number: 1,
            })
    }

    fn sample_proofs(steps: &[Step]) -> Vec<StepProof> {
        steps
            .iter()
            .map(|step| StepProof::sample_for(step.proof_shape()))
            .collect()
    }

    #[test]
    fn verify_rejects_an_unregistered_program() {
        let (_, note_sets) = state();
        let step = Step::Program(ProgramStep {
            program_id: Fr::from(5u64),
            ..program_step(vec![], &[], &[], 0, root())
        });
        assert_eq!(
            verify_operation(
                with_steps(vec![step.clone()]),
                sample_proofs(&[step]),
                &note_sets
            )
            .unwrap_err(),
            Error::UnknownProgram(Fr::from(5u64))
        );
    }

    #[test]
    fn verify_defers_payments_and_the_spend_with_the_summed_paid_value() {
        let (_, note_sets) = state();
        let step = program_step(
            vec![
                payment(&[nullifier(2)], &[commitment(3)], root(), 0, 5),
                payment(&[nullifier(4)], &[], root(), 0, 7),
            ],
            &[],
            &[commitment(8), commitment(9)],
            0b00,
            root(),
        );
        let operation = with_steps(vec![step.clone().into()]);
        let op_id = Fr::from_le_bytes_mod_order(&operation.op_id());
        let deferred =
            verify_operation(operation, sample_proofs(&[step.clone().into()]), &note_sets).unwrap();

        assert_eq!(deferred.zk_transfers.len(), 2);
        for (deferred, payment) in deferred.zk_transfers.iter().zip(step.payments.iter()) {
            assert_eq!(
                deferred.inputs().excess_value.into_inner(),
                Fr::from(payment.paid_value)
            );
            assert_eq!(
                deferred.inputs().msg.into_inner(),
                payment_msg(&program_id(), payment)
            );
        }
        let spend = deferred.program_spends[0].inputs();
        assert_eq!(spend.paid_value.into_inner(), Fr::from(12u64));
        assert_eq!(spend.program_id.into_inner(), program_id());
        assert_eq!(spend.msg.into_inner(), op_id);
        assert_eq!(spend.spend_notes_hash.into_inner(), step.spend_notes_hash);
        assert_eq!(
            spend.inputs_active.map(Groth16Input::into_inner),
            [Fr::ZERO; 4]
        );
        assert_eq!(
            deferred.program_proofs[0].inputs().public,
            [step.spend_notes_hash, payments_hash(&step), op_id]
        );
        assert_eq!(
            deferred.program_proofs[0].inputs().program.key(),
            &program_key()
        );
    }

    #[test]
    fn verify_extends_payment_roots_with_buffered_and_the_spend_root_with_the_whole_buffer() {
        let (_, note_sets) = state();
        // An earlier Operation of the transaction deposited commitment(7) into the
        // channel.
        let set = note_sets
            .get(2)
            .unwrap()
            .clone()
            .buffer_outputs(&[commitment(7)]);
        let note_sets = note_sets.update(2, set);
        let program = Step::Program(program_step(
            vec![
                payment(&[nullifier(2)], &[commitment(3)], root(), 0, 5),
                payment(&[nullifier(4)], &[commitment(5)], root(), 2, 7),
            ],
            &[nullifier(6)],
            &[commitment(8)],
            0b0,
            root(),
        ));
        let user = Step::User(step(&[nullifier(9)], &[commitment(10)], root(), 4));
        let steps = vec![program, user];
        let deferred =
            verify_operation(with_steps(steps.clone()), sample_proofs(&steps), &note_sets).unwrap();

        let extended = |commitments: &[u64]| {
            commitments
                .iter()
                .fold(
                    CommitmentMmr::new().push(commitment(1)).unwrap(),
                    |mmr, n| mmr.push(commitment(*n)).unwrap(),
                )
                .frontier_root()
        };
        assert_eq!(
            deferred.zk_transfers[0]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            root()
        );
        assert_eq!(
            deferred.zk_transfers[1]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            extended(&[7, 3])
        );
        assert_eq!(
            deferred.program_spends[0]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            extended(&[7, 3, 5])
        );
        assert_eq!(
            deferred.zk_transfers[2]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            extended(&[7, 3, 5, 8])
        );
    }

    #[test]
    fn verify_needs_a_recent_root_for_a_spend_consuming_nothing() {
        let (_, note_sets) = state();
        let step: Step = program_step(vec![], &[], &[commitment(8)], 0, Fr::from(99u64)).into();
        assert_eq!(
            verify_operation(
                with_steps(vec![step.clone()]),
                sample_proofs(&[step]),
                &note_sets
            )
            .unwrap_err(),
            Error::NoteSet(NoteSetError::UnknownRoot(Fr::from(99u64)))
        );
    }

    #[test]
    fn verify_rejects_a_note_consumed_by_a_payment_and_the_spend() {
        let (_, note_sets) = state();
        let step: Step = program_step(
            vec![payment(&[nullifier(2)], &[], root(), 0, 5)],
            &[nullifier(2)],
            &[commitment(8)],
            0,
            root(),
        )
        .into();
        assert_eq!(
            verify_operation(
                with_steps(vec![step.clone()]),
                sample_proofs(&[step]),
                &note_sets
            )
            .unwrap_err(),
            Error::StepDoubleSpend(nullifier(2))
        );
    }

    #[test]
    fn verify_rejects_paid_values_whose_sum_overflows() {
        let (_, note_sets) = state();
        let step: Step = program_step(
            vec![
                payment(&[nullifier(2)], &[], root(), 0, u64::MAX),
                payment(&[nullifier(4)], &[], root(), 0, 1),
            ],
            &[],
            &[],
            0,
            root(),
        )
        .into();
        assert_eq!(
            verify_operation(
                with_steps(vec![step.clone()]),
                sample_proofs(&[step]),
                &note_sets
            )
            .unwrap_err(),
            Error::PaidValueOverflow { step: 0 }
        );
    }

    #[test]
    fn preverify_checks_the_shape_and_the_openings_of_a_program_step() {
        let note = notes::Note {
            value: 10,
            nonce: Fr::from(2u64),
            public_key: ZkPublicKey::new(Fr::from(3u64)),
            program: program_id(),
            data: Fr::from(4u64),
        };
        let opening = ProgramNoteOpening {
            value: 10,
            nonce: Fr::from(2u64),
            public_key: note.public_key,
            data: Fr::from(4u64),
        };
        let step = program_step(
            vec![payment(&[nullifier(2)], &[], root(), 0, 5)],
            &[],
            &[commitment(9), note.commitment()],
            0b10,
            root(),
        );
        let proof = |payments: usize, openings: Vec<ProgramNoteOpening>| {
            StepProof::Program(ProgramStepProof {
                payment_proofs: vec![ZkTransferProof::sample(); payments],
                spend_proof: ZkProgramSpendProof::sample(),
                program_proof: ProgramProof::sample(),
                program_notes: openings,
            })
        };
        let preverify = |step: ProgramStep, proof: StepProof| {
            signed(with_steps(vec![step.into()]), vec![proof]).preverify(
                &InscriptionPreverificationContext {
                    tx_hash_view: &tx_hash_view(),
                },
            )
        };

        assert_eq!(preverify(step.clone(), proof(1, vec![opening])), Ok(()));
        assert_eq!(
            preverify(
                step.clone(),
                proof(
                    1,
                    vec![ProgramNoteOpening {
                        data: Fr::from(5u64),
                        ..opening
                    }]
                )
            ),
            Err(Error::ProgramNoteOpening { step: 0, output: 1 })
        );
        assert_eq!(
            preverify(step.clone(), proof(1, vec![])),
            Err(Error::ProgramNoteCount {
                step: 0,
                openings: 0,
                program_outputs: 1
            })
        );
        assert_eq!(
            preverify(step.clone(), proof(0, vec![opening])),
            Err(Error::PaymentProofCount {
                step: 0,
                payments: 1,
                proofs: 0
            })
        );
        assert_eq!(
            preverify(step.clone(), StepProof::User(ZkTransferProof::sample())),
            Err(Error::StepProofKind { step: 0 })
        );
        assert_eq!(
            preverify(
                ProgramStep {
                    program_outputs: ProgramOutputs::from(0b100),
                    ..step.clone()
                },
                proof(1, vec![opening])
            ),
            Err(Error::ProgramOutputsOutOfRange { step: 0 })
        );
        let five_inputs = ProgramStep {
            inputs: (1..=5)
                .map(nullifier)
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
            ..step
        };
        assert_eq!(
            preverify(five_inputs, proof(1, vec![opening])),
            Err(Error::ZkProgramSpendInputs(
                ZkProgramSpendInputsError::TooManyInputs(5)
            ))
        );
    }

    #[test]
    fn execute_applies_the_payments_then_the_spend_and_buffers_in_that_order() {
        let (channels, note_sets) = state();
        let program = Step::Program(program_step(
            vec![
                payment(&[nullifier(2)], &[commitment(3)], root(), 0, 5),
                payment(&[nullifier(4)], &[commitment(5)], root(), 0, 7),
            ],
            &[nullifier(6)],
            &[commitment(8)],
            0,
            root(),
        ));
        let user = Step::User(step(&[nullifier(9)], &[commitment(10)], root(), 0));
        let steps = vec![program, user];
        let signed_operation: SignedOperation<_, _, StandardMode> =
            signed(with_steps(steps.clone()), sample_proofs(&steps)).into_state_trusted();

        let (context, _) = signed_operation
            .execute(InscriptionExecutionContext {
                channels,
                note_sets,
                block_slot: Slot::from(1u64),
            })
            .expect("the channel has a note set");

        let set = context.note_sets.get(2).unwrap();
        let expected = [3, 5, 8, 10]
            .iter()
            .fold(
                CommitmentMmr::new().push(commitment(1)).unwrap(),
                |mmr, n| mmr.push(commitment(*n)).unwrap(),
            )
            .frontier_root();
        assert!(
            [2, 4, 6, 9]
                .iter()
                .all(|n| set.contains_nullifier(&nullifier(*n)))
        );
        assert_eq!(set.commitment_root(), expected);
        assert_eq!(set.tx_commitment_root(&root(), 1), Ok(expected));
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the payment, the spend and the program proof of one step"
    )]
    fn program_step_proofs_verify_end_to_end() {
        // A user note of 1,000 in the channel pays it all to the program, whose spend
        // consumes no note and creates one program note of 1,000, open to everyone.
        let secret_key = Fr::from(1u64);
        let user_note = notes::Note::new(
            1_000,
            Fr::from(11u64),
            ZkKey::new(secret_key).to_public_key(),
        );
        let (mmr, path) = CommitmentMmr::new()
            .push_with_paths(user_note.commitment(), &mut [])
            .unwrap();
        let root = mmr.frontier_root();
        let (channels, note_sets) = state();
        let set = NoteSet::empty()
            .execute_adding(&[user_note.commitment()])
            .unwrap();
        let note_sets = note_sets.update(2, set).end_block(1);
        let (_, proving_key) = three_input_setup(1);
        let program_note = notes::Note {
            value: 1_000,
            nonce: Fr::from(21u64),
            public_key: ZkKey::new(Fr::ZERO).to_public_key(),
            program: program_id(),
            data: Fr::from(5u64),
        };

        let payment = Payment {
            inputs: [derive_note_nullifier(&user_note.commitment(), &secret_key)].into(),
            outputs: OutputCommitments::default(),
            commitment_merkle_root: root,
            buffered: 0,
            paid_value: 1_000,
            intent: Fr::from(9u64),
        };
        let payment_proof = prove_transfer(
            &[(user_note, secret_key, path)],
            &[],
            root,
            payment_msg(&program_id(), &payment),
        );
        let outputs = core::array::from_fn(|j| {
            if j == 0 {
                SpendOutputFields::of(&program_note)
            } else {
                SpendOutputFields::inactive()
            }
        });
        let step = ProgramStep {
            program_id: program_id(),
            payments: [payment].into(),
            inputs: InputNullifiers::default(),
            outputs: [program_note.commitment()].into(),
            program_outputs: ProgramOutputs::from(0b1),
            commitment_merkle_root: root,
            spend_notes_hash: spend_notes_hash(
                &core::array::from_fn(|_| SpendInputFields::inactive()),
                &outputs,
            ),
        };
        let operation = with_steps(vec![step.clone().into()]);
        let op_id = Fr::from_le_bytes_mod_order(&operation.op_id());
        let (spend_proof, spend_public) = prove_program_spend(
            &[],
            &[program_note],
            &[true],
            root,
            program_id(),
            1_000,
            op_id,
        );
        assert_eq!(
            spend_public.spend_notes_hash.into_inner(),
            step.spend_notes_hash
        );
        let program_proof = three_input_prove(
            &proving_key,
            [step.spend_notes_hash, payments_hash(&step), op_id],
            5,
        );
        let proof = StepProof::Program(ProgramStepProof {
            payment_proofs: vec![ZkTransferProof::new(payment_proof)],
            spend_proof: ZkProgramSpendProof::new(spend_proof),
            program_proof: ProgramProof::new(
                CompressedGroth16Proof::try_from(&program_proof).unwrap(),
            ),
            program_notes: vec![ProgramNoteOpening {
                value: 1_000,
                nonce: Fr::from(21u64),
                public_key: program_note.public_key,
                data: Fr::from(5u64),
            }],
        });

        let deferred = signed(operation, vec![proof])
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view(),
            })
            .unwrap()
            .verify(&InscriptionValidationContext {
                channels: &channels,
                note_sets: &note_sets,
                programs: &programs(),
                block_slot: Slot::from(0u64),
                block_number: 2,
            })
            .unwrap();
        let mut batch = DeferredProofs::new();
        deferred.defer_into(&mut batch);
        batch
            .verify()
            .expect("the payment, the spend and the program proof are valid");
    }
}
