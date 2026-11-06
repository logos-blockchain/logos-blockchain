use std::{collections::HashSet, sync::Arc};

use ark_ff::PrimeField as _;
use lb_binary_codec::canonical::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError};
use lb_cryptarchia_engine::Slot;
use lb_groth16::{Fr, serde::serde_fr};
#[cfg(any(test, feature = "test-utils"))]
use lb_key_management_system_keys::keys::Ed25519Key;
use lb_key_management_system_keys::keys::UnverifiedEd25519PublicKey;
use lb_program_spend::{
    ZkProgramSpendInputsError, ZkProgramSpendPublic, ZkProgramSpendVerifierInput,
};
use lb_utils::bounded::{Bounded, NonEmptyBoundedVec, UpperBoundedVec};
use lb_zktransfer::{MAX_INPUTS, MAX_OUTPUTS, ZkTransferInputsError, ZkTransferVerifierInput};
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
        channel::{ChannelState, Channels, Error},
        gas::{
            Gas, GasOverflow, MainnetGasProfile, OpGasCalculator, OperationGas, ThresholdSource,
        },
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        note_set::{NoteSet, NoteSets, TxRoots},
        notes::{InputNullifiers, NoteCommitment, OutputCommitments},
        ops::{
            ChannelInscribeProof, OpId, SignedOperation, StepProof, StepProofShape,
            ZkTransferProof,
            channel::{
                UnverifiedChannelKeys,
                program_step::{
                    ProgramDeposit, ProgramStep, program_deposit_id, program_deposits_hash,
                },
            },
            transfer::TransferOp,
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

pub const EXECUTION_PROGRAM_SPEND_GAS: Gas = Gas::new(619);
pub const EXECUTION_PROGRAM_PROOF_GAS: Gas = Gas::new(2451);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ChannelStep {
    pub inputs: InputNullifiers,
    pub outputs: OutputCommitments,
    #[serde(with = "serde_fr")]
    pub commitment_merkle_root: Fr,
    /// The zone message of the step, signed by its holder through
    /// `user_step_id`
    #[serde(with = "serde_inscription")]
    pub inscription: Inscription,
}

pub const USER_STEP_KIND: u8 = 0x00;
pub const PROGRAM_DEPOSIT_KIND: u8 = 0x01;
pub const PROGRAM_STEP_KIND: u8 = 0x02;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    User(ChannelStep),
    ProgramDeposit(ProgramDeposit),
    Program(ProgramStep),
}

impl From<ChannelStep> for Step {
    fn from(step: ChannelStep) -> Self {
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
    // A ZkTransfer for a user step or a program deposit.
    #[must_use]
    pub const fn proof_shape(&self) -> StepProofShape {
        match self {
            Self::User(_) | Self::ProgramDeposit(_) => StepProofShape::User,
            Self::Program(step) => StepProofShape::Program {
                program_notes: step.program_outputs.len(),
            },
        }
    }

    #[must_use]
    pub const fn inputs(&self) -> &InputNullifiers {
        match self {
            Self::User(step) => &step.inputs,
            Self::ProgramDeposit(deposit) => &deposit.inputs,
            Self::Program(step) => &step.inputs,
        }
    }

    /// The notes the step creates, in the order they are appended: the notes of
    /// the program of a program step before the user notes it pays.
    pub fn created(&self) -> impl Iterator<Item = &NoteCommitment> {
        let (program_outputs, outputs) = match self {
            Self::User(step) => (&[][..], &step.outputs),
            Self::ProgramDeposit(deposit) => (&[][..], &deposit.outputs),
            Self::Program(step) => (step.program_outputs.as_slice(), &step.outputs),
        };
        program_outputs.iter().chain(outputs.iter())
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
        match self {
            Self::User(step) => {
                USER_STEP_KIND.encode_into(out);
                step.encode_into(out);
            }
            Self::ProgramDeposit(deposit) => {
                PROGRAM_DEPOSIT_KIND.encode_into(out);
                deposit.encode_into(out);
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
pub fn user_step_id(channel_id: &ChannelId, step: &ChannelStep) -> Fr {
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

    /// The steps of the inscription, each with whether it starts a mini-block.
    pub fn steps_with_mini_block_starts(&self) -> impl Iterator<Item = (bool, &Step)> {
        self.mini_blocks.iter().flat_map(|mini_block| {
            mini_block
                .iter()
                .enumerate()
                .map(|(position, step)| (position == 0, step))
        })
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
                Step::User(ChannelStep {
                    inputs: [crate::mantle::notes::NoteNullifier(Fr::from(0x23u64))].into(),
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
    const GAS_COST: Gas = Gas::new(59);
}

impl OpGasCalculator<MainnetGasProfile> for InscriptionOp {
    // EXECUTION_CHANNEL_INSCRIBE_GAS, plus EXECUTION_TRANSFER_GAS for each user
    // step and each deposit, plus EXECUTION_PROGRAM_SPEND_GAS and
    // EXECUTION_PROGRAM_PROOF_GAS for each program step.
    fn execution_gas(&self, _thresholds: &impl ThresholdSource) -> Result<Gas, GasOverflow> {
        self.steps().try_fold(Self::GAS_COST, |gas, step| {
            let step_gas = match step {
                Step::User(_) | Step::ProgramDeposit(_) => {
                    <TransferOp as OperationGas<MainnetGasProfile>>::GAS_COST
                }
                Step::Program(_) => {
                    EXECUTION_PROGRAM_SPEND_GAS.checked_add(EXECUTION_PROGRAM_PROOF_GAS)?
                }
            };
            gas.checked_add(step_gas)
        })
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

        // One proof per step
        let steps = self.operation().steps().count();
        if self.proof().step_proofs.len() != steps {
            return Err(Error::StepProofCount {
                steps,
                proofs: self.proof().step_proofs.len(),
            });
        }

        // Ensure each step proof is of the kind of its step, and the ZkTransfer circuit
        // fits the inputs and the outputs of each user step and deposit
        for (index, (step, proof)) in self
            .operation()
            .steps()
            .zip(&self.proof().step_proofs)
            .enumerate()
        {
            match (step, proof) {
                (Step::User(_) | Step::ProgramDeposit(_), StepProof::User(_)) => {
                    if step.inputs().len() > MAX_INPUTS {
                        return Err(
                            ZkTransferInputsError::TooManyInputs(step.inputs().len()).into()
                        );
                    }
                    if step.created().count() > MAX_OUTPUTS {
                        return Err(
                            ZkTransferInputsError::TooManyOutputs(step.created().count()).into(),
                        );
                    }
                }
                (Step::Program(step), StepProof::Program(proof)) => {
                    // The ZkProgramSpend circuit fits the inputs and both output lists
                    // together
                    if step.inputs.len() > lb_program_spend::MAX_INPUTS {
                        return Err(
                            ZkProgramSpendInputsError::TooManyInputs(step.inputs.len()).into()
                        );
                    }
                    let outputs = step.program_outputs.len() + step.outputs.len();
                    if outputs > lb_program_spend::MAX_OUTPUTS {
                        return Err(ZkProgramSpendInputsError::TooManyOutputs(outputs).into());
                    }
                    if proof.program_notes.len() != step.program_outputs.len() {
                        return Err(Error::ProgramNoteCount {
                            step: index,
                            openings: proof.program_notes.len(),
                            program_outputs: step.program_outputs.len(),
                        });
                    }
                    // Every note of the program the step creates is opened, for the
                    // availability of its state; the check reads the step alone, so a copy
                    // failing it is dropped when it deserializes.
                    for ((output, commitment), opening) in step
                        .program_outputs
                        .iter()
                        .enumerate()
                        .zip(&proof.program_notes)
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
        reason = "the channel checks, then the steps of the three kinds in order"
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

            // A channel being created holds no note yet, so its inscription carries no
            // step
            None
        };

        // The signature is cheap and checked by `preverify`, only the step proofs
        // are deferred.
        if operation.mini_blocks.is_empty() {
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
        let mut step_proofs = self.proof().step_proofs.iter();
        let mut index = 0;
        for mini_block in operation.mini_blocks.iter() {
            // The user steps and program deposits of the mini-block cover the buffer as
            // it stands now.
            let covered = note_set.tx_commitment_buffer_len();
            // The program deposits pending for the next program step.
            let mut program_deposits: Vec<&ProgramDeposit> = Vec::new();
            for step in mini_block.iter() {
                let proof = step_proofs.next().ok_or_else(|| Error::StepProofCount {
                    steps: operation.steps().count(),
                    proofs: self.proof().step_proofs.len(),
                })?;

                // The step consumes unspent notes of the channel, which no earlier step
                // consumed, a program step possibly none.
                if !matches!(step, Step::Program(_)) || !step.inputs().is_empty() {
                    note_set.assert_spendable(step.inputs().as_slice())?;
                }
                for note_nullifier in step.inputs().iter() {
                    if !step_nullifiers.insert(*note_nullifier) {
                        return Err(Error::StepDoubleSpend(*note_nullifier));
                    }
                }

                match (step, proof) {
                    // A user step balances exactly.
                    (Step::User(user_step), StepProof::User(proof)) => {
                        deferred.zk_transfers.push(holder_transfer(
                            &note_set,
                            tx_roots,
                            step,
                            covered,
                            context.block_number,
                            0,
                            user_step_id(&operation.channel_id, user_step),
                            proof,
                        )?);
                    }
                    // A program deposit pays its value to a registered program and waits
                    // for the next program step.
                    (Step::ProgramDeposit(deposit), StepProof::User(proof)) => {
                        if context.programs.get(&deposit.program_id).is_none() {
                            return Err(Error::UnknownProgram(deposit.program_id));
                        }
                        deferred.zk_transfers.push(holder_transfer(
                            &note_set,
                            tx_roots,
                            step,
                            covered,
                            context.block_number,
                            deposit.value,
                            program_deposit_id(&operation.channel_id, deposit),
                            proof,
                        )?);
                        program_deposits.push(deposit);
                    }
                    // A program step of a registered program consumes the pending program
                    // deposits, which must all pay its program.
                    (Step::Program(program_step), StepProof::Program(proof)) => {
                        let program = context
                            .programs
                            .get(&program_step.program_id)
                            .ok_or(Error::UnknownProgram(program_step.program_id))?;
                        if let Some(deposit) = program_deposits
                            .iter()
                            .find(|deposit| deposit.program_id != program_step.program_id)
                        {
                            return Err(Error::DepositToAnotherProgram {
                                step: index,
                                program_id: deposit.program_id,
                            });
                        }
                        let paid_value = program_deposits
                            .iter()
                            .try_fold(0, |paid: Value, deposit| paid.checked_add(deposit.value))
                            .ok_or(Error::PaidValueOverflow { step: index })?;
                        // Rejects a root outside the window when the spend consumes nothing
                        let tx_commitment_root = note_set.cached_tx_commitment_root(
                            tx_roots,
                            &program_step.commitment_merkle_root,
                            context.block_number,
                        )?;
                        let nullifiers: Vec<Fr> = program_step
                            .inputs
                            .iter()
                            .map(|nullifier| nullifier.0)
                            .collect();
                        // The notes of the program, then the user notes it pays.
                        let commitments: Vec<Fr> =
                            step.created().map(|commitment| commitment.0).collect();
                        let outputs_of_program: Vec<bool> = (0..commitments.len())
                            .map(|output| output < program_step.program_outputs.len())
                            .collect();
                        let public = ZkProgramSpendVerifierInput::new(&ZkProgramSpendPublic {
                            inputs: &nullifiers,
                            outputs: &commitments,
                            outputs_of_program: &outputs_of_program,
                            tx_commitment_root,
                            program_id: program_step.program_id,
                            paid_value,
                            program_step_id: program_step.program_step_id,
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
                                public: [
                                    program_step.program_step_id,
                                    program_deposits_hash(&program_deposits),
                                    op_id_fr,
                                ],
                            },
                        ));
                        program_deposits.clear();
                    }
                    _ => return Err(Error::StepProofKind { step: index }),
                }
                // The next program spends and the next mini-blocks cover the notes this
                // step creates.
                note_set = note_set.buffer_outputs(&step.created().copied().collect::<Vec<_>>());
                index += 1;
            }
            // Every program deposit of the mini-block was consumed.
            if let Some(deposit) = program_deposits.first() {
                return Err(Error::UnconsumedDeposit(deposit.program_id));
            }
        }
        Ok(deferred)
    }
}

// The ZkTransfer a holder signed for its user step or program deposit, against
// its root extended with the buffer before its mini-block.
#[expect(clippy::too_many_arguments, reason = "the public values of the proof")]
fn holder_transfer(
    note_set: &NoteSet,
    tx_roots: &TxRoots,
    step: &Step,
    covered: usize,
    block_number: BlockNumber,
    excess_value: Value,
    msg: Fr,
    proof: &ZkTransferProof,
) -> Result<DeferredZkTransfer, Error> {
    let tx_commitment_root = note_set.cached_step_commitment_root(
        tx_roots,
        step.commitment_merkle_root(),
        covered,
        block_number,
    )?;
    let nullifiers: Vec<Fr> = step.inputs().iter().map(|nullifier| nullifier.0).collect();
    let commitments: Vec<Fr> = step.created().map(|commitment| commitment.0).collect();
    let public = ZkTransferVerifierInput::new(
        &nullifiers,
        &commitments,
        excess_value,
        tx_commitment_root,
        msg,
    )?;
    Ok(DeferredZkTransfer::new(*proof.as_proof(), public))
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
        if !operation.mini_blocks.is_empty() {
            let index = channel.note_set.ok_or(Error::NoNoteSet {
                channel_id: operation.channel_id,
            })?;
            let mut note_set = context
                .note_sets
                .get(index)
                .expect("the note set of a channel exists")
                .clone();
            for step in operation.steps() {
                // A program step creates its notes of the program before the user
                // notes it pays.
                let created: Vec<_> = step.created().copied().collect();
                note_set = note_set
                    .execute_spending(step.inputs().as_slice())?
                    .execute_adding(&created)?
                    // The notes extend the buffer the next Operations of the
                    // transaction are proven against.
                    .buffer_outputs(&created);
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
    use lb_key_management_system_keys::keys::{PK0, ZkKey, ZkPublicKey};
    use lb_utils::bounded::BoundedError;

    use super::*;
    use crate::{
        crypto::Hash,
        mantle::{
            TxHash,
            batch::{DeferrableProof as _, DeferredProofs},
            gas::test_utils::FixedThresholds,
            note_set::{CommitmentMmr, NoteSetError},
            notes::{self, NoteNullifier, derive_note_nullifier},
            ops::{
                ProgramNoteOpening, ProgramProof, ProgramStepProof, ZkProgramSpendProof,
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
            let user = ChannelStep {
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
                Ok(Gas::new(59))
            );
        }
    }

    #[test]
    fn inscription_op_execution_gas_charges_a_transfer_per_step() {
        let step = ChannelStep {
            inputs: [NoteNullifier(Fr::from(1u64))].into(),
            outputs: OutputCommitments::default(),
            commitment_merkle_root: Fr::ZERO,
            inscription: Inscription::default(),
        };
        for (steps, expected) in [(0usize, 59u64), (1, 678), (3, 1_916)] {
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
        for (steps, expected) in [
            (vec![], 59u64),
            // 3_129
            (vec![program.clone()], 59 + 619 + 2_451),
            // 4_367, the spec vector inscription
            (
                vec![user, deposit.clone(), program.clone()],
                59 + 619 + 619 + 619 + 2_451,
            ),
            // 8_675
            (
                vec![
                    deposit.clone(),
                    program.clone(),
                    deposit.clone(),
                    deposit.clone(),
                    deposit,
                    program,
                ],
                59 + 4 * 619 + 2 * (619 + 2_451),
            ),
        ] {
            let operation = with_steps(steps);
            assert_eq!(
                operation.execution_gas(&FixedThresholds(3)),
                Ok(Gas::new(expected))
            );
        }
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
    ) -> ChannelStep {
        ChannelStep {
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
    fn user_step_id_hashes_the_channel_and_the_encoded_user_step() {
        let channel = ChannelId::from([0x22u8; 32]);
        let user_step = ChannelStep {
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
                &ChannelStep {
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
        let set = note_sets
            .get(2)
            .unwrap()
            .clone()
            .buffer_outputs(&[commitment(7)]);
        let note_sets = note_sets.update(2, set);
        let mini_blocks = vec![
            vec![
                Step::User(step(&[nullifier(2)], &[commitment(3)], root())),
                Step::User(step(&[nullifier(4)], &[commitment(5)], root())),
            ],
            vec![Step::User(step(&[nullifier(6)], &[commitment(8)], root()))],
        ];
        let deferred = verify_operation(
            with_mini_blocks(mini_blocks.clone()),
            sample_proofs(&mini_blocks.concat()),
            &note_sets,
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
        let set = note_sets
            .get(2)
            .unwrap()
            .clone()
            .buffer_outputs(&[commitment(3)]);
        let note_sets = note_sets.update(2, set);
        let steps = vec![Step::User(step(&[nullifier(4)], &[], root()))];
        let deferred =
            verify_operation(with_steps(steps.clone()), sample_proofs(&steps), &note_sets).unwrap();
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
            .get(2)
            .unwrap()
            .clone()
            .execute_spending(&[nullifier(2)])
            .unwrap();
        let note_sets = note_sets.update(2, set);
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
        let (channels, note_sets) = state();
        let set = NoteSet::empty()
            .execute_adding(&[input.commitment()])
            .unwrap();
        let note_sets = note_sets.update(2, set).end_block(1);
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
        let set = note_sets
            .get(2)
            .unwrap()
            .clone()
            .buffer_outputs(&[commitment(7)]);
        let note_sets = note_sets.update(2, set);
        let user = Step::User(step(&[nullifier(9)], &[commitment(10)], root()));
        let mini_blocks = vec![
            vec![
                deposit(&[nullifier(2)], &[commitment(3)], root(), 5),
                deposit(&[nullifier(4)], &[commitment(5)], root(), 7),
                Step::Program(program_step(&[nullifier(6)], &[], &[commitment(8)], root())),
            ],
            vec![user],
        ];
        let deferred = verify_operation(
            with_mini_blocks(mini_blocks.clone()),
            sample_proofs(&mini_blocks.concat()),
            &note_sets,
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
            StepProof::Program(ProgramStepProof {
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
            preverify(step.clone(), StepProof::User(ZkTransferProof::sample())),
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
        let (channels, note_sets) = state();
        let set = NoteSet::empty()
            .execute_adding(&[user_note.commitment()])
            .unwrap();
        let note_sets = note_sets.update(2, set).end_block(1);
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
            StepProof::User(ZkTransferProof::new(deposit_proof)),
            StepProof::Program(ProgramStepProof {
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
