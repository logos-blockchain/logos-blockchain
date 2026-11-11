use std::{collections::HashSet, iter::Chain, slice, sync::Arc};

use ark_ff::PrimeField as _;
use lb_binary_codec::canonical::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError};
use lb_cryptarchia_engine::Slot;
use lb_groth16::{Fr, serde::serde_fr};
#[cfg(any(test, feature = "test-utils"))]
use lb_key_management_system_keys::keys::Ed25519Key;
use lb_key_management_system_keys::keys::UnverifiedEd25519PublicKey;
use lb_program_spend::{ZkProgramSpendPublic, ZkProgramSpendVerifierInput};
use lb_utils::bounded::{Bounded, NonEmptyBoundedVec, UpperBoundedVec};
use serde::{Deserialize, Serialize};

use super::{ChannelId, MAX_OPERATION_DATA_SIZE, MsgId, step_id};
use crate::{
    block::BlockNumber,
    crypto::{Digest as _, Hasher},
    events::TxEvent,
    mantle::{
        Value,
        batch::{
            DeferredProgramProof, DeferredZkProgramSpend, DeferredZkTransfer,
            InscriptionDeferredProofs, ProgramProofInput,
        },
        channel::{ChannelState, Channels, Error, channel_note_set},
        gas::{
            EXECUTION_OPENING_GAS, EXECUTION_PROGRAM_DEPOSIT_GAS, EXECUTION_PROGRAM_STEP_GAS,
            EXECUTION_SIGNATURE_GAS, EXECUTION_USER_STEP_GAS, Gas, GasOverflow, MainnetGasProfile,
            OpGasCalculator, OpGasContext, OperationGas, spend_gas,
        },
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        note_set::{NoteSet, NoteSetIndex, NoteSets, TxBuffer, TxNoteState},
        notes::{InputNullifiers, NoteCommitment, NoteNullifier, OutputCommitments},
        ops::{
            ChannelInscribeProof, OpId, ProgramStepProof, SignedOperation, StepProof,
            ZkTransferProof,
            channel::{
                UnverifiedChannelKeys,
                program_step::{
                    ProgramDeposit, ProgramStep, program_deposit_id, program_deposits_hash,
                },
            },
            zk_transfer::{check_zk_transfer_bounds, defer_zk_transfer},
        },
        programs::Programs,
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, Verified},
        },
    },
};

/// The maximum number of bytes that can be inscribed in a single inscription
/// operation, its inscription and the inscriptions of its steps together.
pub const MAX_BYTES: usize = MAX_OPERATION_DATA_SIZE;
pub type Inscription = UpperBoundedVec<u8, MAX_BYTES>;

pub(super) mod serde_inscription {
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

pub const MAX_CHANNEL_STEPS: usize = u16::MAX as usize; // StepCount = UINT16
pub const MAX_MINI_BLOCKS: usize = u16::MAX as usize; // MiniBlockCount = UINT16
// The user steps and program deposits of a mini-block are proven against their
// recent root extended with the buffer of the channel note set as it stands
// when the mini-block starts.
pub type MiniBlock = NonEmptyBoundedVec<Step, MAX_CHANNEL_STEPS>;
pub type MiniBlocks = UpperBoundedVec<MiniBlock, MAX_MINI_BLOCKS>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct UserStep {
    pub inputs: InputNullifiers,
    pub outputs: OutputCommitments,
    #[serde(with = "serde_fr")]
    pub commitment_merkle_root: Fr,
    /// The zone message of the step, signed by its holder through
    /// `user_step_id`
    #[serde(with = "serde_inscription")]
    pub inscription: Inscription,
}

impl UserStep {
    /// The user notes the step creates.
    #[must_use]
    pub fn outputs(&self) -> StepOutputs<'_> {
        StepOutputs::user(self.outputs.as_slice())
    }
}

/// The notes a step creates, in the order they are appended: the notes of the
/// program of a program step before the user notes it pays.
#[derive(Clone, Copy, Debug)]
pub struct StepOutputs<'a> {
    program_outputs: &'a [NoteCommitment],
    outputs: &'a [NoteCommitment],
}

impl<'a> StepOutputs<'a> {
    /// The user notes of a step creating no note of a program.
    #[must_use]
    pub const fn user(outputs: &'a [NoteCommitment]) -> Self {
        Self {
            program_outputs: &[],
            outputs,
        }
    }

    /// The notes of the program, then the user notes.
    #[must_use]
    pub const fn program(
        program_outputs: &'a [NoteCommitment],
        outputs: &'a [NoteCommitment],
    ) -> Self {
        Self {
            program_outputs,
            outputs,
        }
    }

    /// The notes of the program among them.
    #[must_use]
    pub const fn program_outputs(&self) -> &'a [NoteCommitment] {
        self.program_outputs
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.program_outputs.len() + self.outputs.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.program_outputs.is_empty() && self.outputs.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &'a NoteCommitment> + use<'a> {
        self.into_iter()
    }

    #[must_use]
    pub fn to_vec(&self) -> Vec<NoteCommitment> {
        self.iter().copied().collect()
    }
}

impl<'a> IntoIterator for StepOutputs<'a> {
    type Item = &'a NoteCommitment;
    type IntoIter = Chain<slice::Iter<'a, NoteCommitment>, slice::Iter<'a, NoteCommitment>>;

    fn into_iter(self) -> Self::IntoIter {
        self.program_outputs.iter().chain(self.outputs.iter())
    }
}

pub const USER_STEP_KIND: u8 = 0x00;
pub const PROGRAM_DEPOSIT_KIND: u8 = 0x01;
pub const PROGRAM_STEP_KIND: u8 = 0x02;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    User(UserStep),
    ProgramDeposit(ProgramDeposit),
    Program(ProgramStep),
}

impl From<UserStep> for Step {
    fn from(step: UserStep) -> Self {
        Self::User(step)
    }
}

impl From<ProgramDeposit> for Step {
    fn from(deposit: ProgramDeposit) -> Self {
        Self::ProgramDeposit(deposit)
    }
}

impl From<ProgramStep> for Step {
    fn from(step: ProgramStep) -> Self {
        Self::Program(step)
    }
}

impl Step {
    #[must_use]
    pub const fn inputs(&self) -> &InputNullifiers {
        match self {
            Self::User(step) => &step.inputs,
            Self::ProgramDeposit(deposit) => &deposit.inputs,
            Self::Program(step) => &step.inputs,
        }
    }

    /// The notes the step creates, in the order they are appended.
    #[must_use]
    pub fn outputs(&self) -> StepOutputs<'_> {
        match self {
            Self::User(step) => step.outputs(),
            Self::ProgramDeposit(deposit) => deposit.outputs(),
            Self::Program(step) => step.outputs(),
        }
    }

    /// The kind byte the step is encoded and identified with.
    #[must_use]
    pub const fn kind(&self) -> u8 {
        match self {
            Self::User(_) => USER_STEP_KIND,
            Self::ProgramDeposit(_) => PROGRAM_DEPOSIT_KIND,
            Self::Program(_) => PROGRAM_STEP_KIND,
        }
    }

    // Checks that `proof`, the proof of the step at `index`, is of the kind of
    // the step and that its circuit fits the step.
    fn preverify_proof(&self, proof: &StepProof, index: usize) -> Result<(), Error> {
        match (self, proof) {
            (Self::User(_) | Self::ProgramDeposit(_), StepProof::ZkTransfer(_)) => Ok(
                check_zk_transfer_bounds(self.inputs().len(), self.outputs().len())?,
            ),
            (Self::Program(step), StepProof::ProgramStep(proof)) => {
                step.preverify_proof(proof, index)
            }
            _ => Err(Error::StepProofKind { step: index }),
        }
    }

    #[must_use]
    pub const fn commitment_merkle_root(&self) -> &Fr {
        match self {
            Self::User(step) => &step.commitment_merkle_root,
            Self::ProgramDeposit(deposit) => &deposit.commitment_merkle_root,
            Self::Program(step) => &step.commitment_merkle_root,
        }
    }

    #[must_use]
    pub const fn inscription(&self) -> &Inscription {
        match self {
            Self::User(step) => &step.inscription,
            Self::ProgramDeposit(deposit) => &deposit.inscription,
            Self::Program(step) => &step.inscription,
        }
    }
}

// The kind byte, then the step.
impl BinaryEncode for Step {
    fn encoded_length(&self) -> usize {
        1 + match self {
            Self::User(step) => step.encoded_length(),
            Self::ProgramDeposit(deposit) => deposit.encoded_length(),
            Self::Program(step) => step.encoded_length(),
        }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.kind().encode_into(out);
        match self {
            Self::User(step) => step.encode_into(out),
            Self::ProgramDeposit(deposit) => deposit.encode_into(out),
            Self::Program(step) => step.encode_into(out),
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
                UserStep::decode(input, &()).map(|(rest, step)| (rest, Self::User(step)))
            }
            PROGRAM_DEPOSIT_KIND => ProgramDeposit::decode(input, &())
                .map(|(rest, deposit)| (rest, Self::ProgramDeposit(deposit))),
            PROGRAM_STEP_KIND => {
                ProgramStep::decode(input, &()).map(|(rest, step)| (rest, Self::Program(step)))
            }
            other => Err(DecodeError::unknown_discriminant::<Self>(u64::from(other))),
        }
    }
}

// The message the ZkTransfer of a user step is bound to.
#[must_use]
pub fn user_step_id(channel_id: &ChannelId, step: &UserStep) -> Fr {
    step_id(b"USER_STEP_V1", channel_id, USER_STEP_KIND, step)
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
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
    pub mini_blocks: MiniBlocks,
}

impl BinaryEncode for InscriptionOp {
    fn encoded_length(&self) -> usize {
        self.channel_id.encoded_length()
            + self.inscription.encoded_length()
            + self.parent.encoded_length()
            + self.signer.encoded_length()
            + self.mini_blocks.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.channel_id.encode_into(out);
        self.inscription.encode_into(out);
        self.parent.encode_into(out);
        self.signer.encode_into(out);
        self.mini_blocks.encode_into(out);
    }
}

impl BinaryDecode for InscriptionOp {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, channel_id) = ChannelId::decode(input, &())?;
        let (rest, inscription) = Inscription::decode(rest, &())?;
        let (rest, parent) = MsgId::decode(rest, &())?;
        let (rest, signer) = UnverifiedEd25519PublicKey::decode(rest, &())?;
        let (rest, mini_blocks) = MiniBlocks::decode(rest, &())?;
        let operation = Self {
            channel_id,
            inscription,
            parent,
            signer,
            mini_blocks,
        };
        if operation.inscriptions_len() > MAX_BYTES {
            return Err(DecodeError::invalid_value::<Self>(
                "the inscriptions of an inscribe total more than MAX_OPERATION_DATA_SIZE bytes",
            ));
        }
        Ok((rest, operation))
    }
}

impl InscriptionOp {
    #[must_use]
    pub fn id(&self) -> MsgId {
        let mut hasher = Hasher::new();
        hasher.update(self.encode().as_ref());
        MsgId(hasher.finalize().into())
    }

    /// The bytes of the inscription and of the inscriptions of the steps, which
    /// count together against `MAX_OPERATION_DATA_SIZE`.
    #[must_use]
    pub fn inscriptions_len(&self) -> usize {
        self.inscription.len()
            + self
                .steps()
                .map(|step| step.inscription().len())
                .sum::<usize>()
    }

    /// The steps of the inscription, in the order they apply.
    pub fn steps(&self) -> impl Iterator<Item = &Step> {
        self.mini_blocks.iter().flat_map(Bounded::iter)
    }

    /// The number of steps of the inscription, over all its mini-blocks.
    #[must_use]
    pub fn steps_len(&self) -> usize {
        self.mini_blocks.iter().map(MiniBlock::len).sum()
    }

    // One proof per step, each of the kind of its step and fitting its circuit.
    fn preverify_step_proofs(&self, proofs: &[StepProof]) -> Result<(), Error> {
        let steps = self.steps_len();
        if proofs.len() != steps {
            return Err(Error::StepProofCount {
                steps,
                proofs: proofs.len(),
            });
        }
        self.steps()
            .zip(proofs)
            .enumerate()
            .try_for_each(|(index, (step, proof))| step.preverify_proof(proof, index))
    }

    // The note set of the channel the inscription extends, after checking the
    // inscription may extend it. A channel being created holds no note yet.
    fn verify_channel(
        &self,
        context: &InscriptionValidationContext<'_>,
    ) -> Result<Option<NoteSetIndex>, Error> {
        if let Some(channel) = context.channels.channels.get(&self.channel_id) {
            // Check the parent corresponds to the payload
            if self.parent != channel.tip_message {
                return Err(Error::InvalidParent {
                    channel_id: self.channel_id,
                    parent: self.parent.into(),
                    actual: channel.tip_message.into(),
                });
            }

            // Check that the signer is the authorized one
            if self.signer
                != channel.accredited_keys[channel.round_robin(context.block_slot).0 as usize]
            {
                return Err(Error::UnauthorizedSigner {
                    channel_id: self.channel_id,
                    signer: format!("{signer:?}", signer = self.signer),
                });
            }

            Ok(channel_note_set(self.channel_id))
        } else if self.parent != MsgId::root() {
            // The channel doesn't exist, so the parent must be ZERO
            Err(Error::InvalidParent {
                channel_id: self.channel_id,
                parent: self.parent.into(),
                actual: MsgId::root().into(),
            })
        } else {
            // A channel being created holds no note yet, so its inscription carries no
            // step
            Ok(None)
        }
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
            mini_blocks: MiniBlocks::default(),
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample_with_step() -> Self {
        Self {
            // The steps of the spec vector.
            mini_blocks: [MiniBlock::from([
                Step::User(UserStep {
                    inputs: [NoteNullifier(Fr::from(0x23u64))].into(),
                    outputs: [NoteCommitment(Fr::from(0x24u64))].into(),
                    commitment_merkle_root: Fr::from(0x25u64),
                    inscription: b"step-inscription".into(),
                }),
                Step::ProgramDeposit(ProgramDeposit::sample()),
                Step::Program(ProgramStep::sample()),
            ])]
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
    pub tx_notes: &'a TxNoteState,
    pub programs: &'a Programs,
    pub block_slot: Slot,
    pub block_number: BlockNumber,
}

pub struct InscriptionExecutionContext<'a> {
    pub channels: Channels,
    pub note_sets: NoteSets,
    pub tx_notes: &'a mut TxNoteState,
    pub block_slot: Slot,
}

impl ProvableOperation for InscriptionOp {
    type Proof = ChannelInscribeProof;
    const CODE: u8 = 0x11;
}

impl OperationGas<MainnetGasProfile> for InscriptionOp {
    const GAS_COST: Gas = Gas::new(26);
}

impl OpGasCalculator<MainnetGasProfile> for InscriptionOp {
    // The Operation and its signature, plus each step beyond its spend: a user
    // step, a program deposit, or a program step with its program proof and an
    // opening per program note, the spend of a program step covering its program
    // outputs and its outputs.
    fn execution_gas(&self, _context: &impl OpGasContext) -> Result<Gas, GasOverflow> {
        let mut gas = Self::GAS_COST.checked_add(EXECUTION_SIGNATURE_GAS)?;
        for step in self.steps() {
            let step_gas = match step {
                Step::User(step) => EXECUTION_USER_STEP_GAS
                    .checked_add(spend_gas(step.inputs.len(), step.outputs.len())?)?,
                Step::ProgramDeposit(deposit) => EXECUTION_PROGRAM_DEPOSIT_GAS
                    .checked_add(spend_gas(deposit.inputs.len(), deposit.outputs.len())?)?,
                Step::Program(step) => {
                    let program_notes = step.program_outputs.len();
                    let outputs = program_notes
                        .checked_add(step.outputs.len())
                        .ok_or(GasOverflow)?;
                    EXECUTION_PROGRAM_STEP_GAS
                        .checked_add(spend_gas(step.inputs.len(), outputs)?)?
                        .checked_add(EXECUTION_OPENING_GAS.checked_mul(
                            Value::try_from(program_notes).map_err(|_| GasOverflow)?,
                        )?)?
                }
            };
            gas = gas.checked_add(step_gas)?;
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

        // The inscription and the inscriptions of the steps count together
        let inscriptions_len = self.operation().inscriptions_len();
        if inscriptions_len > MAX_BYTES {
            return Err(Error::InscriptionsTooLarge {
                len: inscriptions_len,
                max: MAX_BYTES,
            });
        }

        // Check the signature
        self.operation()
            .signer
            .verify(context.tx_hash_view.as_bytes(), &self.proof().signature)
            .map_err(|_error| Error::InvalidSignature)?;

        // One proof per step, of the kind of its step and fitting its circuit
        self.operation()
            .preverify_step_proofs(&self.proof().step_proofs)
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<InscriptionOp, Preverified, StandardMode>
{
    type DeferredProof = InscriptionDeferredProofs;
    type Context<'a> = InscriptionValidationContext<'a>;
    type Error = Error;

    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        let operation = self.operation();
        let note_set = operation.verify_channel(context)?;

        // The signature is cheap and checked by `preverify`, only the step proofs
        // are deferred.
        if operation.mini_blocks.is_empty() {
            return Ok(InscriptionDeferredProofs::default());
        }
        let index = note_set.ok_or(Error::NoNoteSet {
            channel_id: operation.channel_id,
        })?;
        let mut step_proofs = self.proof().step_proofs.iter();
        let verifier = operation.mini_blocks.iter().try_fold(
            StepVerifier::new(operation, context, index),
            |verifier, mini_block| verifier.verify_mini_block(mini_block, &mut step_proofs),
        )?;
        Ok(verifier.deferred)
    }
}

// The verification of the steps of an inscription, in order: the buffer of the
// channel note set extended with the notes of the steps verified so far, and
// their proofs.
struct StepVerifier<'a> {
    operation: &'a InscriptionOp,
    context: &'a InscriptionValidationContext<'a>,
    note_set: &'a NoteSet,
    buffer: TxBuffer,
    // The nullifiers the steps verified so far consume
    step_nullifiers: HashSet<NoteNullifier>,
    // The message of the program spends and the third input of the program proofs
    op_id_fr: Fr,
    // The index of the next step, over all the mini-blocks
    index: usize,
    deferred: InscriptionDeferredProofs,
}

impl<'a> StepVerifier<'a> {
    fn new(
        operation: &'a InscriptionOp,
        context: &'a InscriptionValidationContext<'a>,
        index: NoteSetIndex,
    ) -> Self {
        Self {
            operation,
            context,
            note_set: context
                .note_sets
                .get(index)
                .expect("the note set of a channel exists"),
            buffer: context.tx_notes.buffer(index).into_owned(),
            step_nullifiers: HashSet::new(),
            op_id_fr: Fr::from_le_bytes_mod_order(&operation.op_id()),
            index: 0,
            deferred: InscriptionDeferredProofs::default(),
        }
    }

    fn verify_mini_block<'p>(
        mut self,
        mini_block: &'a MiniBlock,
        step_proofs: &mut impl Iterator<Item = &'p StepProof>,
    ) -> Result<Self, Error> {
        // The user steps and program deposits of the mini-block cover the buffer as
        // it stands now.
        let covered = self.buffer.len();
        // The program deposits pending for the next program step.
        let mut program_deposits: Vec<&ProgramDeposit> = Vec::new();
        for step in mini_block.iter() {
            let proof = step_proofs
                .next()
                .expect("preverify checked one proof per step");
            self.check_step_inputs(step)?;
            match (step, proof) {
                (Step::User(user_step), StepProof::ZkTransfer(proof)) => {
                    self.verify_user_step(user_step, step, covered, proof)?;
                }
                (Step::ProgramDeposit(deposit), StepProof::ZkTransfer(proof)) => {
                    self.verify_program_deposit(deposit, step, covered, proof)?;
                    program_deposits.push(deposit);
                }
                (Step::Program(program_step), StepProof::ProgramStep(proof)) => {
                    self.verify_program_step(program_step, step, proof, &program_deposits)?;
                    program_deposits.clear();
                }
                _ => return Err(Error::StepProofKind { step: self.index }),
            }
            // The next program spends and the next mini-blocks cover the notes this
            // step creates.
            self.buffer.push(&step.outputs().to_vec());
            self.index += 1;
        }
        // Every program deposit of the mini-block was consumed.
        if let Some(deposit) = program_deposits.first() {
            return Err(Error::UnconsumedDeposit(deposit.program_id));
        }
        Ok(self)
    }

    // The step consumes unspent notes of the channel, which no earlier step
    // consumed, a program step possibly none.
    fn check_step_inputs(&mut self, step: &Step) -> Result<(), Error> {
        if !matches!(step, Step::Program(_)) || !step.inputs().is_empty() {
            self.note_set.assert_spendable(step.inputs().as_slice())?;
        }
        for note_nullifier in step.inputs().iter() {
            if !self.step_nullifiers.insert(*note_nullifier) {
                return Err(Error::StepDoubleSpend(*note_nullifier));
            }
        }
        Ok(())
    }

    // A user step balances exactly.
    fn verify_user_step(
        &mut self,
        user_step: &UserStep,
        step: &Step,
        covered: usize,
        proof: &ZkTransferProof,
    ) -> Result<(), Error> {
        let msg = user_step_id(&self.operation.channel_id, user_step);
        let transfer = self.holder_transfer(step, covered, 0, msg, proof)?;
        self.deferred.zk_transfers.push(transfer);
        Ok(())
    }

    // A program deposit pays its value to a registered program and waits for the
    // next program step.
    fn verify_program_deposit(
        &mut self,
        deposit: &ProgramDeposit,
        step: &Step,
        covered: usize,
        proof: &ZkTransferProof,
    ) -> Result<(), Error> {
        if self.context.programs.get(&deposit.program_id).is_none() {
            return Err(Error::UnknownProgram(deposit.program_id));
        }
        let msg = program_deposit_id(&self.operation.channel_id, deposit);
        let transfer = self.holder_transfer(step, covered, deposit.value, msg, proof)?;
        self.deferred.zk_transfers.push(transfer);
        Ok(())
    }

    // A program step of a registered program consumes the pending program
    // deposits, which must all pay its program.
    fn verify_program_step(
        &mut self,
        program_step: &ProgramStep,
        step: &Step,
        proof: &ProgramStepProof,
        program_deposits: &[&ProgramDeposit],
    ) -> Result<(), Error> {
        let program = self
            .context
            .programs
            .get(&program_step.program_id)
            .ok_or(Error::UnknownProgram(program_step.program_id))?;
        if let Some(deposit) = program_deposits
            .iter()
            .find(|deposit| deposit.program_id != program_step.program_id)
        {
            return Err(Error::DepositToAnotherProgram {
                step: self.index,
                program_id: deposit.program_id,
            });
        }
        let paid_value = program_deposits
            .iter()
            .try_fold(0, |paid: Value, deposit| paid.checked_add(deposit.value))
            .ok_or(Error::PaidValueOverflow { step: self.index })?;
        // Rejects a root outside the window when the spend consumes nothing
        let tx_commitment_root = self.note_set.cached_step_commitment_root(
            self.context.tx_notes.roots(),
            &self.buffer,
            &program_step.commitment_merkle_root,
            self.buffer.len(),
            self.context.block_number,
        )?;
        let nullifiers: Vec<Fr> = program_step
            .inputs
            .iter()
            .map(|nullifier| nullifier.0)
            .collect();
        // The notes of the program, then the user notes it pays.
        let outputs = step.outputs();
        let commitments: Vec<Fr> = outputs.iter().map(|commitment| commitment.0).collect();
        let outputs_of_program: Vec<bool> = (0..outputs.len())
            .map(|output| output < outputs.program_outputs().len())
            .collect();
        let public = ZkProgramSpendVerifierInput::new(&ZkProgramSpendPublic {
            inputs: &nullifiers,
            outputs: &commitments,
            outputs_of_program: &outputs_of_program,
            tx_commitment_root,
            program_id: program_step.program_id,
            paid_value,
            program_step_id: program_step.program_step_id,
            msg: self.op_id_fr,
        })?;
        self.deferred
            .program_spends
            .push(DeferredZkProgramSpend::new(
                *proof.spend_proof.as_proof(),
                public,
            ));
        self.deferred.program_proofs.push(DeferredProgramProof::new(
            *proof.program_proof.as_proof(),
            ProgramProofInput {
                program: program.clone(),
                public: [
                    program_step.program_step_id,
                    program_deposits_hash(program_deposits),
                    self.op_id_fr,
                ],
            },
        ));
        Ok(())
    }

    // The ZkTransfer a holder signed for its user step or program deposit,
    // against its root extended with the buffer before its mini-block.
    fn holder_transfer(
        &self,
        step: &Step,
        covered: usize,
        excess_value: Value,
        msg: Fr,
        proof: &ZkTransferProof,
    ) -> Result<DeferredZkTransfer, Error> {
        let tx_commitment_root = self.note_set.cached_step_commitment_root(
            self.context.tx_notes.roots(),
            &self.buffer,
            step.commitment_merkle_root(),
            covered,
            self.context.block_number,
        )?;
        Ok(defer_zk_transfer(
            proof,
            step.inputs(),
            step.outputs(),
            excess_value,
            tx_commitment_root,
            msg,
        )?)
    }
}

impl<Mode: VerificationMode> ExecutableOperation
    for SignedOperation<InscriptionOp, Verified, Mode>
{
    type Context<'a> = InscriptionExecutionContext<'a>;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // if the channel doesn't exist, create it
        (context.channels, context.note_sets) = context.channels.create_if_not_exists(
            operation.channel_id,
            context.block_slot,
            UnverifiedChannelKeys::from(operation.signer),
            context.note_sets,
        );
        let channel = context
            .channels
            .channel_state(&operation.channel_id)
            .cloned()
            .expect("the channel exists once created");

        // Apply the steps to the channel note set, in order
        if !operation.mini_blocks.is_empty() {
            let note_set = context
                .note_sets
                .channel(&operation.channel_id)
                .ok_or(Error::NoNoteSet {
                    channel_id: operation.channel_id,
                })?
                .clone();
            let index = note_set.index();
            // A program step creates its notes of the program before the user notes
            // it pays, the notes extending the buffer the next Operations of the
            // transaction are proven against.
            let note_set = operation.steps().try_fold(note_set, |note_set, step| {
                let outputs = step.outputs().to_vec();
                context.tx_notes.buffer_outputs(index, &outputs);
                note_set
                    .execute_spending(step.inputs().as_slice())?
                    .execute_adding(&outputs)
            })?;
            context.note_sets = context.note_sets.update_channel(note_set);
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
    use lb_key_management_system_keys::keys::{PK0, ZkKey, ZkPublicKey};
    use lb_program_spend::ZkProgramSpendInputsError;
    use lb_utils::bounded::BoundedError;
    use lb_zktransfer::{ZkTransferInputsError, ZkTransferVerifierInput};

    use super::*;
    use crate::{
        crypto::Hash,
        mantle::{
            TxHash,
            batch::{DeferrableProof as _, DeferredProofs},
            gas::test_utils::FixedThresholds,
            note_set::{CommitmentMmr, NoteSetError, TypedNoteSet},
            notes::{self, derive_note_nullifier},
            ops::{
                ProgramNoteOpening, ProgramProof, ZkProgramSpendProof,
                channel::{NULL_CHANNEL_ID, program_step::test_utils::prove_program_spend},
                op_proof::samples::SampleProof as _,
                transfer::test_utils::prove_transfer,
            },
            programs::{
                ProgramVerificationKey, RegisteredProgram, SpendInputFields, SpendOutputFields,
                derive_program_id, program_step_id,
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
            mini_blocks: MiniBlocks::default(),
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
                    "Item count {} exceeds maximum of {MAX_BYTES}",
                    MAX_BYTES + 1,
                )
                .as_str()
            ),
            "{err:?}",
        );
    }

    #[test]
    fn the_inscriptions_of_an_inscribe_and_its_steps_count_together() {
        let bytes = |len: usize| Inscription::try_from(vec![0u8; len]).unwrap();
        // One byte in a user step, one in a deposit, `program_step_len` in the
        // program step and the rest of the limit in the inscribe.
        let inscribed = |program_step_len: usize| {
            let user = UserStep {
                inscription: bytes(1),
                ..step(&[nullifier(2)], &[commitment(3)], root())
            };
            let deposit = ProgramDeposit {
                inscription: bytes(1),
                ..ProgramDeposit::sample()
            };
            let program = ProgramStep {
                inscription: bytes(program_step_len),
                ..ProgramStep::sample()
            };
            InscriptionOp {
                inscription: bytes(MAX_BYTES - 2),
                ..with_steps(vec![user.into(), deposit.into(), program.into()])
            }
        };

        let at_the_limit = inscribed(0);
        assert_eq!(at_the_limit.inscriptions_len(), MAX_BYTES);
        assert_eq!(
            InscriptionOp::decode(&at_the_limit.encode(), &())
                .unwrap()
                .1,
            at_the_limit
        );

        let over = inscribed(1);
        assert!(matches!(
            InscriptionOp::decode(&over.encode(), &()),
            Err(DecodeError::InvalidValue { .. })
        ));
        let steps: Vec<Step> = over.steps().cloned().collect();
        assert_eq!(
            signed(over, sample_proofs(&steps)).preverify(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view()
            }),
            Err(Error::InscriptionsTooLarge {
                len: MAX_BYTES + 1,
                max: MAX_BYTES
            })
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
                    tx_notes: &TxNoteState::new(),
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
                    tx_notes: &TxNoteState::new(),
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
                    tx_notes: &TxNoteState::new(),
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
                    tx_notes: &TxNoteState::new(),
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
                    tx_notes: &TxNoteState::new(),
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

        let mut tx_notes = TxNoteState::new();

        let (context, events) = signed_operation
            .execute(InscriptionExecutionContext {
                channels: Channels::new(),
                note_sets: NoteSets::new(),
                tx_notes: &mut tx_notes,
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
            })
        );
        assert_eq!(
            context
                .note_sets
                .channel(&channel_id)
                .cloned()
                .map(TypedNoteSet::into_inner),
            Some(NoteSet::empty())
        );
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

        let mut tx_notes = TxNoteState::new();

        let (context, _) = signed_operation
            .execute(InscriptionExecutionContext {
                channels: Channels::new(),
                note_sets: NoteSets::new(),
                tx_notes: &mut tx_notes,
                block_slot: Slot::from(7u64),
            })
            .expect("inscribing never fails");

        assert!(context.channels.channel_state(&NULL_CHANNEL_ID).is_some());
        assert_eq!(context.note_sets.channel(&NULL_CHANNEL_ID), None);
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

        let mut tx_notes = TxNoteState::new();

        let (context, events) = signed_operation
            .execute(InscriptionExecutionContext {
                channels,
                note_sets: NoteSets::new(),
                tx_notes: &mut tx_notes,
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
            })
        );
        assert_eq!(events, []);
    }

    #[test]
    fn inscription_op_execution_gas_does_not_scale_with_the_threshold() {
        for threshold in [0, 1, 3] {
            assert_eq!(
                InscriptionOp::sample().execution_gas(&FixedThresholds(threshold)),
                // 26 + 557
                Ok(Gas::new(583))
            );
        }
    }

    #[test]
    fn inscription_op_execution_gas_charges_a_spend_per_user_step() {
        let step = UserStep {
            inputs: [NoteNullifier(Fr::from(1u64))].into(),
            outputs: OutputCommitments::default(),
            commitment_merkle_root: Fr::ZERO,
            inscription: Inscription::default(),
        };
        // 583 + steps * (28 + 4177 + 1758), one input, no output, nothing buffered
        for (steps, expected) in [(0usize, 583u64), (1, 6_546), (3, 18_472)] {
            let operation = with_steps(vec![Step::User(step.clone()); steps]);
            for threshold in [0, 3] {
                assert_eq!(
                    operation.execution_gas(&FixedThresholds(threshold)),
                    Ok(Gas::new(expected))
                );
            }
        }
    }

    #[test]
    fn inscription_op_execution_gas_charges_each_deposit_and_program_step() {
        let user = Step::User(step(&[nullifier(1)], &[], Fr::ZERO));
        let deposit = Step::ProgramDeposit(ProgramDeposit::sample());
        let program = Step::Program(ProgramStep::sample());
        // A user step of one input is 28 + 4177 + 1758 = 5_963, a program deposit
        // of one input and one output 144 + 4177 + 1758 + 58 = 6_137, and a program
        // step of one input and one program output 13528 + 4177 + 1758 + 58 + 470 =
        // 19_991. The transaction pays the root extensions.
        for (steps, expected) in [
            (vec![], 583u64),
            // 583 + 19_991
            (vec![program.clone()], 20_574),
            // 583 + 5_963 + 6_137 + 19_991
            (vec![user, deposit.clone(), program.clone()], 32_674),
            // 583 + 4 * 6_137 + 2 * 19_991
            (
                vec![
                    deposit.clone(),
                    program.clone(),
                    deposit.clone(),
                    deposit.clone(),
                    deposit,
                    program,
                ],
                65_113,
            ),
        ] {
            let operation = with_steps(steps);
            assert_eq!(
                operation.execution_gas(&FixedThresholds(3)),
                Ok(Gas::new(expected))
            );
        }
    }

    #[test]
    fn inscription_program_step_spends_its_program_outputs_and_its_outputs() {
        let operation = with_steps(vec![Step::Program(ProgramStep {
            inputs: [nullifier(1), nullifier(2)].into(),
            program_outputs: [commitment(3), commitment(4)].into(),
            outputs: [commitment(5)].into(),
            ..ProgramStep::sample()
        })]);
        // 583 + 13528 + the spend of two inputs and three outputs (4177 + 2 * 1758 +
        // 3 * 58) + two openings (2 * 470)
        assert_eq!(
            operation.execution_gas(&FixedThresholds(3)),
            Ok(Gas::new(583 + 13_528 + 7_867 + 940))
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
    ) -> UserStep {
        UserStep {
            inputs: InputNullifiers::try_from_iter(inputs.iter().copied()).unwrap(),
            outputs: OutputCommitments::try_from_iter(outputs.iter().copied()).unwrap(),
            commitment_merkle_root,
            inscription: Inscription::default(),
        }
    }

    // The steps in a single mini-block, none for no step.
    fn with_steps(steps: Vec<Step>) -> InscriptionOp {
        if steps.is_empty() {
            return with_mini_blocks(vec![]);
        }
        with_mini_blocks(vec![steps])
    }

    fn with_mini_blocks(mini_blocks: Vec<Vec<Step>>) -> InscriptionOp {
        InscriptionOp {
            mini_blocks: MiniBlocks::try_from(
                mini_blocks
                    .into_iter()
                    .map(|steps| MiniBlock::try_from(steps).unwrap())
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
            ..InscriptionOp::sample()
        }
    }

    fn signer() -> Ed25519Key {
        Ed25519Key::from_bytes(&[15; 32])
    }

    // The sample channel, accrediting the sample signer, with an empty note set.
    fn empty_state() -> (Channels, NoteSets) {
        let keys = UnverifiedChannelKeys::from(signer().public_key().into_unverified());
        Channels::new().create_channel(sample_channel(), Slot::from(0u64), keys, NoteSets::new())
    }

    // The sample channel, accrediting the sample signer, owns note set 2, which
    // holds commitment(1), recorded at the end of block 0.
    fn state() -> (Channels, NoteSets) {
        let channel_id = sample_channel();
        let (channels, note_sets) = empty_state();
        let set = note_sets
            .channel(&channel_id)
            .unwrap()
            .clone()
            .execute_adding(&[commitment(1)])
            .unwrap();
        (channels, note_sets.update_channel(set).end_block(0))
    }

    fn sample_channel() -> ChannelId {
        InscriptionOp::sample().channel_id
    }

    fn sample_set() -> NoteSetIndex {
        NoteSetIndex::Channel(sample_channel())
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
            .map(|step| StepProof::sample_for(step.into()))
            .collect();
        signed(with_steps(steps), proofs)
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view(),
            })
            .expect("the sample signer signed the transaction hash")
            .verify(&InscriptionValidationContext {
                channels,
                note_sets,
                tx_notes: &TxNoteState::new(),
                programs: &Programs::new(),
                block_slot: Slot::from(0u64),
                block_number: 1,
            })
    }

    #[test]
    fn user_step_id_hashes_the_channel_and_the_encoded_user_step() {
        let channel = ChannelId::from([0x22u8; 32]);
        let user_step = UserStep {
            inscription: b"step-inscription".into(),
            ..step(&[nullifier(0x23)], &[commitment(0x24)], Fr::from(0x25u64))
        };
        let mut preimage = b"USER_STEP_V1".to_vec();
        preimage.extend_from_slice(&[0x22u8; 32]);
        preimage.extend(Step::User(user_step.clone()).encode());
        let digest: [u8; 32] = Hasher::digest(&preimage).into();
        assert_eq!(
            user_step_id(&channel, &user_step),
            Fr::from_le_bytes_mod_order(&digest)
        );
        // every field of the step is covered, its root included
        assert_ne!(
            user_step_id(
                &channel,
                &UserStep {
                    commitment_merkle_root: Fr::ONE,
                    ..user_step.clone()
                }
            ),
            user_step_id(&channel, &user_step)
        );
    }

    #[test]
    fn preverify_rejects_a_step_proof_count_other_than_the_step_count() {
        let signed_operation = signed(
            with_steps(vec![Step::User(step(
                &[nullifier(2)],
                &[commitment(3)],
                root(),
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
            with_steps(vec![Step::User(step(&[nullifier(2)], &outputs, root()))]),
            vec![StepProof::ZkTransfer(ZkTransferProof::sample())],
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
    fn verify_rejects_steps_on_a_channel_being_created_and_on_the_null_channel() {
        // Neither holds a note: the null channel has no note set, and a channel being
        // created gets its set only when the inscription executes.
        for channel_id in [NULL_CHANNEL_ID, ChannelId::from([5u8; 32])] {
            let operation = InscriptionOp {
                channel_id,
                ..with_steps(vec![Step::User(step(
                    &[nullifier(2)],
                    &[commitment(3)],
                    root(),
                ))])
            };
            let deferred = signed(
                operation,
                vec![StepProof::ZkTransfer(ZkTransferProof::sample())],
            )
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view(),
            })
            .unwrap()
            .verify(&InscriptionValidationContext {
                channels: &Channels::new(),
                note_sets: &NoteSets::new(),
                tx_notes: &TxNoteState::new(),
                programs: &Programs::new(),
                block_slot: Slot::from(0u64),
                block_number: 1,
            });
            assert_eq!(deferred.unwrap_err(), Error::NoNoteSet { channel_id });
        }
    }

    #[test]
    fn verify_defers_one_balanced_transfer_per_step_bound_to_its_user_step_id() {
        let (channels, note_sets) = state();
        let steps = vec![
            step(&[nullifier(2)], &[commitment(3)], root()),
            step(&[nullifier(4)], &[commitment(5)], root()),
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
            assert_eq!(
                deferred.inputs().msg.into_inner(),
                user_step_id(&with_steps(vec![]).channel_id, step)
            );
            assert_eq!(deferred.inputs().tx_commitment_root.into_inner(), root());
            assert_eq!(
                deferred.inputs().inputs[0].into_inner(),
                step.inputs.as_slice()[0].0
            );
        }
    }

    #[test]
    fn verify_extends_each_step_root_with_the_buffer_before_its_mini_block() {
        let (_, note_sets) = state();
        // An earlier Operation of the transaction deposited commitment(7) into the
        // channel.
        let mut tx_notes = TxNoteState::new();
        tx_notes.buffer_outputs(sample_set(), &[commitment(7)]);
        let mini_blocks = vec![
            vec![
                Step::User(step(&[nullifier(2)], &[commitment(3)], root())),
                Step::User(step(&[nullifier(4)], &[commitment(5)], root())),
            ],
            vec![Step::User(step(&[nullifier(6)], &[commitment(8)], root()))],
        ];
        let deferred = verify_in_tx(
            with_mini_blocks(mini_blocks.clone()),
            sample_proofs(&mini_blocks.concat()),
            &note_sets,
            &tx_notes,
        )
        .unwrap();
        let extended = |commitments: &[u64]| {
            commitments
                .iter()
                .fold(
                    CommitmentMmr::new().push(commitment(1)).unwrap(),
                    |mmr, n| mmr.push(commitment(*n)).unwrap(),
                )
                .frontier_root()
        };
        // The steps of a mini-block cover the same buffer, whatever their order.
        for index in 0..2 {
            assert_eq!(
                deferred.zk_transfers[index]
                    .inputs()
                    .tx_commitment_root
                    .into_inner(),
                extended(&[7])
            );
        }
        assert_eq!(
            deferred.zk_transfers[2]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            extended(&[7, 3, 5])
        );
    }

    #[test]
    fn a_second_inscription_of_the_transaction_covers_the_first_in_its_first_mini_block() {
        let (_, note_sets) = state();
        // The first inscription of the transaction added commitment(3) to the buffer.
        let mut tx_notes = TxNoteState::new();
        tx_notes.buffer_outputs(sample_set(), &[commitment(3)]);
        let steps = vec![Step::User(step(&[nullifier(4)], &[], root()))];
        let deferred = verify_in_tx(
            with_steps(steps.clone()),
            sample_proofs(&steps),
            &note_sets,
            &tx_notes,
        )
        .unwrap();
        assert_eq!(
            deferred.zk_transfers[0]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            CommitmentMmr::new()
                .push(commitment(1))
                .unwrap()
                .push(commitment(3))
                .unwrap()
                .frontier_root()
        );
    }

    #[test]
    fn program_steps_of_a_mini_block_chain_their_spends_and_keep_their_deposit_root() {
        let (_, note_sets) = state();
        let steps = vec![
            deposit(&[nullifier(2)], &[commitment(3)], root(), 5),
            Step::Program(program_step(&[], &[], &[commitment(4)], root())),
            deposit(&[nullifier(5)], &[commitment(6)], root(), 7),
            Step::Program(program_step(&[nullifier(7)], &[], &[commitment(8)], root())),
        ];
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
        // Both deposits cover what precedes the mini-block, here nothing.
        for transfer in &deferred.zk_transfers {
            assert_eq!(transfer.inputs().tx_commitment_root.into_inner(), root());
        }
        // Each spend covers the whole buffer at its position, and consumes the
        // deposits since the previous program step of its program.
        let spend = |index: usize| deferred.program_spends[index].inputs();
        assert_eq!(spend(0).tx_commitment_root.into_inner(), extended(&[3]));
        assert_eq!(spend(0).paid_value.into_inner(), Fr::from(5u64));
        assert_eq!(
            spend(1).tx_commitment_root.into_inner(),
            extended(&[3, 4, 6])
        );
        assert_eq!(spend(1).paid_value.into_inner(), Fr::from(7u64));
    }

    #[test]
    fn verify_rejects_a_deposit_left_unconsumed_at_the_end_of_its_mini_block() {
        let (_, note_sets) = state();
        let program = || Step::Program(program_step(&[], &[], &[], root()));
        for mini_blocks in [
            // consumed only in the next mini-block
            vec![
                vec![deposit(&[nullifier(2)], &[], root(), 5)],
                vec![program()],
            ],
            // after the last program step of the inscription
            vec![vec![program(), deposit(&[nullifier(2)], &[], root(), 5)]],
        ] {
            assert_eq!(
                verify_operation(
                    with_mini_blocks(mini_blocks.clone()),
                    sample_proofs(&mini_blocks.concat()),
                    &note_sets
                )
                .unwrap_err(),
                Error::UnconsumedDeposit(program_id())
            );
        }
    }

    #[test]
    fn verify_rejects_a_program_step_consuming_a_deposit_to_another_program() {
        let (_, note_sets) = state();
        let other_program = derive_program_id(&other_program_key());
        let steps = vec![
            Step::ProgramDeposit(ProgramDeposit {
                program_id: other_program,
                ..program_deposit(&[nullifier(2)], &[], root(), 5)
            }),
            Step::Program(program_step(&[], &[], &[], root())),
        ];
        assert_eq!(
            verify_operation(with_steps(steps.clone()), sample_proofs(&steps), &note_sets)
                .unwrap_err(),
            Error::DepositToAnotherProgram {
                step: 1,
                program_id: other_program
            }
        );
    }

    #[test]
    fn an_empty_mini_block_does_not_decode() {
        // One mini-block of no step
        assert!(MiniBlocks::decode(&[1, 0, 0, 0], &()).is_err());
        assert_eq!(
            MiniBlocks::decode(&[0, 0], &()).unwrap().1,
            MiniBlocks::default()
        );
    }

    #[test]
    fn verify_rejects_a_note_consumed_by_an_earlier_step() {
        let (channels, note_sets) = state();
        let steps = vec![
            Step::User(step(&[nullifier(2)], &[commitment(3)], root())),
            Step::User(step(&[nullifier(2)], &[commitment(4)], root())),
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
            .channel(&sample_channel())
            .unwrap()
            .clone()
            .execute_spending(&[nullifier(2)])
            .unwrap();
        let note_sets = note_sets.update_channel(set);
        assert_eq!(
            verify_steps(
                vec![Step::User(step(&[nullifier(2)], &[commitment(3)], root()))],
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
            Step::User(step(&[nullifier(2)], &[commitment(3)], root())),
            Step::User(step(&[nullifier(4)], &[commitment(5)], root())),
        ]);
        let signed_operation: SignedOperation<_, _, StandardMode> = signed(
            operation,
            vec![
                StepProof::ZkTransfer(ZkTransferProof::sample()),
                StepProof::ZkTransfer(ZkTransferProof::sample()),
            ],
        )
        .into_state_trusted();

        let mut tx_notes = TxNoteState::new();

        let (context, _) = signed_operation
            .execute(InscriptionExecutionContext {
                channels,
                note_sets,
                tx_notes: &mut tx_notes,
                block_slot: Slot::from(1u64),
            })
            .expect("the channel has a note set");

        let set = context.note_sets.channel(&sample_channel()).unwrap();
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
        assert_eq!(
            context.tx_notes.commitment_root(set, &root(), 1),
            Ok(expected)
        );
    }

    #[test]
    fn step_proof_is_bound_to_its_user_step_id() {
        let secret_key = Fr::from(1u64);
        let input = notes::Note::new(
            1_000,
            Fr::from(11u64),
            ZkKey::new(secret_key).to_public_key(),
        );
        let (mmr, path) = CommitmentMmr::new()
            .push_with_paths(input.commitment(), &mut [])
            .unwrap();
        let (channels, note_sets) = empty_state();
        let set = note_sets
            .channel(&sample_channel())
            .unwrap()
            .clone()
            .execute_adding(&[input.commitment()])
            .unwrap();
        let note_sets = note_sets.update_channel(set).end_block(1);
        let output = notes::Note::new(1_000, Fr::from(12u64), ZkPublicKey::new(Fr::from(2u64)));
        let step = step(
            &[derive_note_nullifier(
                &input.commitment(),
                &ZkKey::new(secret_key).nullifier_key(),
            )],
            &[output.commitment()],
            mmr.frontier_root(),
        );
        let proof = prove_transfer(
            &[(input, secret_key, path)],
            &[output],
            mmr.frontier_root(),
            user_step_id(&with_steps(vec![]).channel_id, &step),
        );

        let deferred = signed(
            with_steps(vec![Step::User(step)]),
            vec![StepProof::ZkTransfer(ZkTransferProof::new(proof))],
        )
        .into_preverified(&InscriptionPreverificationContext {
            tx_hash_view: &tx_hash_view(),
        })
        .unwrap()
        .verify(&InscriptionValidationContext {
            channels: &channels,
            note_sets: &note_sets,
            tx_notes: &TxNoteState::new(),
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
        let deposit = Step::ProgramDeposit(ProgramDeposit::sample());
        assert_eq!(deposit.encode()[0], PROGRAM_DEPOSIT_KIND);
        assert_eq!(
            Step::decode(&deposit.encode(), &()).unwrap(),
            (&[][..], deposit)
        );
        let user = Step::User(step(&[nullifier(2)], &[commitment(3)], Fr::ZERO));
        assert_eq!(user.encode()[0], USER_STEP_KIND);
        let mut bytes = user.encode();
        bytes[0] = 0x03;
        assert_eq!(
            Step::decode(&bytes, &()).unwrap_err(),
            DecodeError::unknown_discriminant::<Step>(3)
        );
    }

    fn program_key() -> ProgramVerificationKey {
        ProgramVerificationKey::try_from(&three_input_setup(1).0).unwrap()
    }

    fn program_id() -> Fr {
        derive_program_id(&program_key())
    }

    // A second registered program.
    fn other_program_key() -> ProgramVerificationKey {
        ProgramVerificationKey::try_from(&three_input_setup(2).0).unwrap()
    }

    fn programs() -> Programs {
        Programs::new()
            .register(RegisteredProgram::new(program_key()).unwrap())
            .0
            .register(RegisteredProgram::new(other_program_key()).unwrap())
            .0
    }

    fn program_deposit(
        inputs: &[NoteNullifier],
        outputs: &[NoteCommitment],
        commitment_merkle_root: Fr,
        value: u64,
    ) -> ProgramDeposit {
        ProgramDeposit {
            program_id: program_id(),
            inputs: InputNullifiers::try_from_iter(inputs.iter().copied()).unwrap(),
            outputs: OutputCommitments::try_from_iter(outputs.iter().copied()).unwrap(),
            commitment_merkle_root,
            value,
            intent: Fr::from(value),
            inscription: Inscription::default(),
        }
    }

    fn deposit(
        inputs: &[NoteNullifier],
        outputs: &[NoteCommitment],
        commitment_merkle_root: Fr,
        value: u64,
    ) -> Step {
        Step::ProgramDeposit(program_deposit(
            inputs,
            outputs,
            commitment_merkle_root,
            value,
        ))
    }

    fn program_step(
        inputs: &[NoteNullifier],
        program_outputs: &[NoteCommitment],
        outputs: &[NoteCommitment],
        commitment_merkle_root: Fr,
    ) -> ProgramStep {
        ProgramStep {
            program_id: program_id(),
            inputs: InputNullifiers::try_from_iter(inputs.iter().copied()).unwrap(),
            program_outputs: OutputCommitments::try_from_iter(program_outputs.iter().copied())
                .unwrap(),
            outputs: OutputCommitments::try_from_iter(outputs.iter().copied()).unwrap(),
            commitment_merkle_root,
            program_step_id: Fr::from(77u64),
            inscription: Inscription::default(),
        }
    }

    fn verify_operation(
        operation: InscriptionOp,
        proofs: Vec<StepProof>,
        note_sets: &NoteSets,
    ) -> Result<InscriptionDeferredProofs, Error> {
        verify_in_tx(operation, proofs, note_sets, &TxNoteState::new())
    }

    fn verify_in_tx(
        operation: InscriptionOp,
        proofs: Vec<StepProof>,
        note_sets: &NoteSets,
        tx_notes: &TxNoteState,
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
                tx_notes,
                programs: &programs(),
                block_slot: Slot::from(0u64),
                block_number: 1,
            })
    }

    fn sample_proofs(steps: &[Step]) -> Vec<StepProof> {
        steps
            .iter()
            .map(|step| StepProof::sample_for(step.into()))
            .collect()
    }

    #[test]
    fn verify_rejects_an_unregistered_program() {
        let (_, note_sets) = state();
        let unregistered = Fr::from(5u64);
        for step in [
            Step::Program(ProgramStep {
                program_id: unregistered,
                ..program_step(&[], &[], &[], root())
            }),
            Step::ProgramDeposit(ProgramDeposit {
                program_id: unregistered,
                ..program_deposit(&[nullifier(2)], &[], root(), 5)
            }),
        ] {
            assert_eq!(
                verify_operation(
                    with_steps(vec![step.clone()]),
                    sample_proofs(&[step]),
                    &note_sets
                )
                .unwrap_err(),
                Error::UnknownProgram(unregistered)
            );
        }
    }

    #[test]
    fn verify_defers_deposits_and_the_spend_with_the_summed_value() {
        let (_, note_sets) = state();
        let deposits = [
            program_deposit(&[nullifier(2)], &[commitment(3)], root(), 5),
            program_deposit(&[nullifier(4)], &[], root(), 7),
        ];
        let step = program_step(&[], &[], &[commitment(8), commitment(9)], root());
        let steps = vec![
            deposits[0].clone().into(),
            deposits[1].clone().into(),
            step.clone().into(),
        ];
        let operation = with_steps(steps.clone());
        let op_id = Fr::from_le_bytes_mod_order(&operation.op_id());
        let deferred = verify_operation(operation, sample_proofs(&steps), &note_sets).unwrap();

        assert_eq!(deferred.zk_transfers.len(), 2);
        for (deferred, deposit) in deferred.zk_transfers.iter().zip(&deposits) {
            assert_eq!(
                deferred.inputs().excess_value.into_inner(),
                Fr::from(deposit.value)
            );
            assert_eq!(
                deferred.inputs().msg.into_inner(),
                program_deposit_id(&with_steps(vec![]).channel_id, deposit)
            );
        }
        let spend = deferred.program_spends[0].inputs();
        assert_eq!(spend.paid_value.into_inner(), Fr::from(12u64));
        assert_eq!(spend.program_id.into_inner(), program_id());
        assert_eq!(spend.msg.into_inner(), op_id);
        assert_eq!(spend.program_step_id.into_inner(), step.program_step_id);
        assert_eq!(
            spend.inputs_active.map(Groth16Input::into_inner),
            [Fr::ZERO; 4]
        );
        assert_eq!(
            deferred.program_proofs[0].inputs().public,
            [
                step.program_step_id,
                program_deposits_hash(&[&deposits[0], &deposits[1]]),
                op_id
            ]
        );
        assert_eq!(
            deferred.program_proofs[0].inputs().program.key(),
            &program_key()
        );
    }

    #[test]
    fn verify_extends_deposit_roots_like_their_mini_block_and_the_spend_root_with_the_whole_buffer()
    {
        let (_, note_sets) = state();
        // An earlier Operation of the transaction deposited commitment(7) into the
        // channel.
        let mut tx_notes = TxNoteState::new();
        tx_notes.buffer_outputs(sample_set(), &[commitment(7)]);
        let user = Step::User(step(&[nullifier(9)], &[commitment(10)], root()));
        let mini_blocks = vec![
            vec![
                deposit(&[nullifier(2)], &[commitment(3)], root(), 5),
                deposit(&[nullifier(4)], &[commitment(5)], root(), 7),
                Step::Program(program_step(&[nullifier(6)], &[], &[commitment(8)], root())),
            ],
            vec![user],
        ];
        let deferred = verify_in_tx(
            with_mini_blocks(mini_blocks.clone()),
            sample_proofs(&mini_blocks.concat()),
            &note_sets,
            &tx_notes,
        )
        .unwrap();

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
            extended(&[7])
        );
        assert_eq!(
            deferred.zk_transfers[1]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            extended(&[7])
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
        let step: Step = program_step(&[], &[], &[commitment(8)], Fr::from(99u64)).into();
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
    fn verify_rejects_a_note_consumed_by_a_deposit_and_the_spend() {
        let (_, note_sets) = state();
        let steps = vec![
            deposit(&[nullifier(2)], &[], root(), 5),
            program_step(&[nullifier(2)], &[], &[commitment(8)], root()).into(),
        ];
        assert_eq!(
            verify_operation(with_steps(steps.clone()), sample_proofs(&steps), &note_sets)
                .unwrap_err(),
            Error::StepDoubleSpend(nullifier(2))
        );
    }

    #[test]
    fn verify_rejects_deposit_values_whose_sum_overflows() {
        let (_, note_sets) = state();
        let steps = vec![
            deposit(&[nullifier(2)], &[], root(), u64::MAX),
            deposit(&[nullifier(4)], &[], root(), 1),
            program_step(&[], &[], &[], root()).into(),
        ];
        assert_eq!(
            verify_operation(with_steps(steps.clone()), sample_proofs(&steps), &note_sets)
                .unwrap_err(),
            Error::PaidValueOverflow { step: 2 }
        );
    }

    #[test]
    fn preverify_checks_the_shape_and_the_openings_of_a_program_step() {
        let note = notes::Note {
            value: 10,
            nonce: Fr::from(2u64),
            public_key: *PK0,
            stake_public_key: ZkPublicKey::new(Fr::from(6u64)),
            program: program_id(),
            data: Fr::from(4u64),
        };
        let opening = ProgramNoteOpening {
            value: 10,
            nonce: Fr::from(2u64),
            stake_public_key: note.stake_public_key,
            data: Fr::from(4u64),
        };
        let step = program_step(&[], &[note.commitment()], &[commitment(9)], root());
        let proof = |openings: Vec<ProgramNoteOpening>| {
            StepProof::ProgramStep(ProgramStepProof {
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

        assert_eq!(preverify(step.clone(), proof(vec![opening])), Ok(()));
        assert_eq!(
            preverify(
                step.clone(),
                proof(vec![ProgramNoteOpening {
                    data: Fr::from(5u64),
                    ..opening
                }])
            ),
            Err(Error::ProgramNoteOpening { step: 0, output: 0 })
        );
        // the opening covers the staking key of the note
        assert_eq!(
            preverify(
                step.clone(),
                proof(vec![ProgramNoteOpening {
                    stake_public_key: note.public_key,
                    ..opening
                }])
            ),
            Err(Error::ProgramNoteOpening { step: 0, output: 0 })
        );
        assert_eq!(
            preverify(step.clone(), proof(vec![])),
            Err(Error::ProgramNoteCount {
                step: 0,
                openings: 0,
                program_outputs: 1
            })
        );
        assert_eq!(
            preverify(
                step.clone(),
                StepProof::ZkTransfer(ZkTransferProof::sample())
            ),
            Err(Error::StepProofKind { step: 0 })
        );
        assert_eq!(
            preverify(
                ProgramStep {
                    outputs: (1..=8)
                        .map(commitment)
                        .collect::<Vec<_>>()
                        .try_into()
                        .unwrap(),
                    ..step.clone()
                },
                proof(vec![opening])
            ),
            Err(Error::ZkProgramSpendInputs(
                ZkProgramSpendInputsError::TooManyOutputs(9)
            ))
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
            preverify(five_inputs, proof(vec![opening])),
            Err(Error::ZkProgramSpendInputs(
                ZkProgramSpendInputsError::TooManyInputs(5)
            ))
        );
    }

    #[test]
    fn execute_applies_every_step_and_buffers_in_order() {
        let (channels, note_sets) = state();
        let steps = vec![
            deposit(&[nullifier(2)], &[commitment(3)], root(), 5),
            deposit(&[nullifier(4)], &[commitment(5)], root(), 7),
            program_step(&[nullifier(6)], &[], &[commitment(8)], root()).into(),
            Step::User(step(&[nullifier(9)], &[commitment(10)], root())),
        ];
        let signed_operation: SignedOperation<_, _, StandardMode> =
            signed(with_steps(steps.clone()), sample_proofs(&steps)).into_state_trusted();

        let mut tx_notes = TxNoteState::new();

        let (context, _) = signed_operation
            .execute(InscriptionExecutionContext {
                channels,
                note_sets,
                tx_notes: &mut tx_notes,
                block_slot: Slot::from(1u64),
            })
            .expect("the channel has a note set");

        let set = context.note_sets.channel(&sample_channel()).unwrap();
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
        assert_eq!(
            context.tx_notes.commitment_root(set, &root(), 1),
            Ok(expected)
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the deposit, the spend and the program proof of one step"
    )]
    fn program_step_proofs_verify_end_to_end() {
        // A user note of 1,000 in the channel deposits it all to the program, whose
        // spend consumes no note and creates one program note of 1,000, open to
        // everyone.
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
        let (channels, note_sets) = empty_state();
        let set = note_sets
            .channel(&sample_channel())
            .unwrap()
            .clone()
            .execute_adding(&[user_note.commitment()])
            .unwrap();
        let note_sets = note_sets.update_channel(set).end_block(1);
        let (_, proving_key) = three_input_setup(1);
        // Staked by an operator of key 7.
        let program_note = notes::Note {
            value: 1_000,
            nonce: Fr::from(21u64),
            public_key: *PK0,
            stake_public_key: ZkKey::new(Fr::from(7u64)).to_public_key(),
            program: program_id(),
            data: Fr::from(5u64),
        };

        let deposit = ProgramDeposit {
            program_id: program_id(),
            inputs: [derive_note_nullifier(
                &user_note.commitment(),
                &ZkKey::new(secret_key).nullifier_key(),
            )]
            .into(),
            outputs: OutputCommitments::default(),
            commitment_merkle_root: root,
            value: 1_000,
            intent: Fr::from(9u64),
            inscription: Inscription::default(),
        };
        let deposit_proof = prove_transfer(
            &[(user_note, secret_key, path)],
            &[],
            root,
            program_deposit_id(&with_steps(vec![]).channel_id, &deposit),
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
            inputs: InputNullifiers::default(),
            program_outputs: [program_note.commitment()].into(),
            outputs: OutputCommitments::default(),
            commitment_merkle_root: root,
            program_step_id: program_step_id(
                &core::array::from_fn(|_| SpendInputFields::inactive()),
                &outputs,
            ),
            inscription: Inscription::default(),
        };
        let operation = with_steps(vec![deposit.clone().into(), step.clone().into()]);
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
            spend_public.program_step_id.into_inner(),
            step.program_step_id
        );
        let program_proof = three_input_prove(
            &proving_key,
            [
                step.program_step_id,
                program_deposits_hash(&[&deposit]),
                op_id,
            ],
            5,
        );
        let proofs = vec![
            StepProof::ZkTransfer(ZkTransferProof::new(deposit_proof)),
            StepProof::ProgramStep(ProgramStepProof {
                spend_proof: ZkProgramSpendProof::new(spend_proof),
                program_proof: ProgramProof::new(
                    CompressedGroth16Proof::try_from(&program_proof).unwrap(),
                ),
                program_notes: vec![ProgramNoteOpening {
                    value: 1_000,
                    nonce: Fr::from(21u64),
                    stake_public_key: program_note.stake_public_key,
                    data: Fr::from(5u64),
                }],
            }),
        ];

        let deferred = signed(operation, proofs)
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view(),
            })
            .unwrap()
            .verify(&InscriptionValidationContext {
                channels: &channels,
                note_sets: &note_sets,
                tx_notes: &TxNoteState::new(),
                programs: &programs(),
                block_slot: Slot::from(0u64),
                block_number: 2,
            })
            .unwrap();
        let mut batch = DeferredProofs::new();
        deferred.defer_into(&mut batch);
        batch
            .verify()
            .expect("the deposit, the spend and the program proof are valid");
    }
}
