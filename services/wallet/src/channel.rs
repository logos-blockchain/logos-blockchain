use lb_core::{
    header::HeaderId,
    mantle::{
        Note, Value,
        channel::channel_note_set,
        gas::GasOverflow,
        note_set::NoteSetIndex,
        notes::{
            InputNullifiers, NoteCommitment, NoteNullifier, OutputCommitments, ProgramId,
            derive_note_nullifier, derive_program_output_nonce,
        },
        ops::{
            OpId as _, ProgramNoteOpening, ZkTransferProof,
            channel::{
                ChannelId,
                inscribe::{Inscription, InscriptionOp, Step, UserStep, user_step_id},
                program_step::{
                    ProgramDeposit, ProgramSpendWitness, ProgramStep, program_deposit_id,
                },
                withdraw::ChannelWithdrawOp,
            },
        },
        programs::program_step_id_of,
        transactions::builder::ZkTransferOpening,
    },
};
use lb_groth16::{Fr, fr_from_mod_bytes};
use lb_key_management_system_service::keys::{PK0, ZkKey, ZkPublicKey};
use lb_wallet::{
    KnownNote, KnownNotes, NoteSetSpend, SpentNullifier, WalletError, WalletState,
    transaction::{OpWitness, program_spend_inputs, step_witness},
};
use serde::{Deserialize, Serialize};

use crate::{WalletServiceError, random_nonce, states::ServiceState};

/// A step a holder asks its wallet to build from its notes of a channel.
#[derive(Debug)]
pub struct ChannelStepRequest {
    pub channel_id: ChannelId,
    /// The commitments the buffer of the channel's note set holds when the
    /// mini-block of the step starts, in order: the outputs of the earlier
    /// channel operations of the transaction and of the earlier mini-blocks
    /// of the inscription. Empty for a mini-block that starts on an empty
    /// buffer.
    pub appended: Vec<NoteCommitment>,
    /// The channel notes the step creates, by holder key.
    pub payments: Vec<(ZkPublicKey, Value)>,
    /// Makes the step a program deposit.
    pub program_deposit: Option<ProgramPayment>,
    pub inscription: Inscription,
    pub change_pk: ZkPublicKey,
    pub holder_pks: Vec<ZkPublicKey>,
}

#[derive(Debug)]
pub struct ProgramPayment {
    pub program_id: ProgramId,
    pub value: Value,
    pub intent: Fr,
}

/// A step with its proof and the openings of the notes it creates, the
/// change last.
#[derive(Debug)]
pub struct ChannelStep {
    pub step: Step,
    pub proof: ZkTransferProof,
    pub outputs: Vec<Note>,
}

/// A program step a sequencer asks its wallet to build from the notes of the
/// program it holds in a channel.
#[derive(Debug)]
pub struct ProgramStepRequest {
    pub channel_id: ChannelId,
    pub program_id: ProgramId,
    /// The notes of the program the step consumes, possibly none: notes the
    /// wallet holds in the channel, or notes an earlier program step it built
    /// creates in the same transaction.
    pub inputs: Vec<NoteCommitment>,
    /// The notes of the program the step creates.
    pub program_outputs: Vec<ProgramOutput>,
    /// The user notes the step pays.
    pub payouts: Vec<ProgramPayout>,
    pub inscription: Inscription,
}

/// A user note a program step pays to `public_key`.
#[derive(Debug)]
pub struct ProgramPayout {
    pub public_key: ZkPublicKey,
    /// The staking key of the note, `public_key` unless the payee named
    /// another one.
    pub stake_public_key: Option<ZkPublicKey>,
    pub value: Value,
}

/// A payout of `value` a program owes to `public_key`, a key of the wallet,
/// which adopts its note from the step paying it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedPayout {
    pub channel_id: ChannelId,
    pub public_key: ZkPublicKey,
    /// The staking key the rule of the program fixes for the payout,
    /// `public_key` unless it names another one.
    pub stake_public_key: Option<ZkPublicKey>,
    pub value: Value,
}

#[derive(Debug)]
pub struct ProgramOutput {
    pub value: Value,
    pub stake_public_key: ZkPublicKey,
    pub data: Fr,
}

/// A program step with the openings of the notes it creates.
#[derive(Debug)]
pub struct BuiltProgramStep {
    pub step: ProgramStep,
    pub program_notes: Vec<ProgramNoteOpening>,
    pub paid_notes: Vec<Note>,
}

/// The program step at `step_index` of `op`, over all its mini-blocks, with
/// the openings of the notes it creates.
#[derive(Debug)]
pub struct ProgramSpendRequest {
    pub op: InscriptionOp,
    /// The commitments the earlier operations of the transaction append to
    /// the channel's note set before the inscription, in order.
    pub appended: Vec<NoteCommitment>,
    pub step_index: usize,
    pub program_notes: Vec<ProgramNoteOpening>,
    pub paid_notes: Vec<Note>,
}

/// A withdrawal of channel notes of the wallet, each released as a ledger
/// note of the same value and key.
#[derive(Debug)]
pub struct ChannelWithdrawal {
    pub op: ChannelWithdrawOp,
    pub opening: ZkTransferOpening,
}

// A user step, or a program deposit, spending the channel notes of the
// wallet, with the opening and the witness of its ZkTransfer and the note set
// of the channel.
pub fn build_step(
    tip: HeaderId,
    request: ChannelStepRequest,
    state: &ServiceState<'_>,
) -> Result<(Step, ZkTransferOpening, OpWitness, NoteSetIndex), WalletServiceError> {
    let ChannelStepRequest {
        channel_id,
        appended,
        payments,
        program_deposit,
        inscription,
        change_pk,
        holder_pks,
    } = request;
    let note_set = channel_note_set(channel_id)
        .ok_or(WalletServiceError::ChannelWithoutNoteSet(channel_id))?;
    // A step spends at least one note, so it has to move some value.
    if payments.iter().all(|(_, value)| *value == 0)
        && program_deposit
            .as_ref()
            .is_none_or(|deposit| deposit.value == 0)
    {
        return Err(WalletServiceError::EmptyChannelStep);
    }
    let paid = payments
        .into_iter()
        .map(|(public_key, value)| Note::new(value, random_nonce(), public_key))
        .collect();
    let (inputs, opening, commitment_merkle_root) = state.note_set_spend(
        tip,
        NoteSetSpend {
            note_set,
            appended: appended.clone(),
            outputs: paid,
            excess: program_deposit.as_ref().map_or(0, |deposit| deposit.value),
            change_pk,
            change_nonce: random_nonce(),
            holder_pks,
        },
    )?;
    let outputs = commitments_of(&opening.outputs)?;
    let (step, msg) = if let Some(deposit) = program_deposit {
        let deposit = ProgramDeposit {
            program_id: deposit.program_id,
            inputs,
            outputs,
            commitment_merkle_root,
            value: deposit.value,
            intent: deposit.intent,
            inscription,
        };
        let msg = program_deposit_id(&channel_id, &deposit);
        (Step::ProgramDeposit(deposit), msg)
    } else {
        let step = UserStep {
            inputs,
            outputs,
            commitment_merkle_root,
            inscription,
        };
        let msg = user_step_id(&channel_id, &step);
        (Step::User(step), msg)
    };
    let witness = step_witness(
        &state.wallet().spend_state(tip)?,
        state.wallet().known_notes(),
        note_set,
        &appended,
        step.inputs(),
        &opening,
        msg,
    )?;
    Ok((step, opening, witness, note_set))
}

// The withdrawal of `notes` from the channel at the spend state of `tip`.
pub fn build_withdrawal(
    tip: HeaderId,
    channel_id: ChannelId,
    notes: &[NoteCommitment],
    state: &ServiceState<'_>,
) -> Result<ChannelWithdrawal, WalletServiceError> {
    let note_set = channel_note_set(channel_id)
        .ok_or(WalletServiceError::ChannelWithoutNoteSet(channel_id))?;
    let spend_state = state.wallet().spend_state(tip)?;
    // A note another withdrawal waits to spend, or a transaction or a step
    // in flight spends, is left to it.
    let inputs = notes
        .iter()
        .map(|commitment| {
            let owned = spend_state
                .notes
                .get(commitment)
                .filter(|owned| owned.note_set == note_set)
                .ok_or(WalletServiceError::NoChannelNote(*commitment, channel_id))?;
            if spend_state.is_withdrawing(&owned.nullifier)
                || state.is_pending_note(&owned.nullifier)
            {
                return Err(WalletServiceError::ChannelNoteInFlight(
                    *commitment,
                    channel_id,
                ));
            }
            Ok(owned)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let outputs: Vec<Note> = inputs
        .iter()
        .map(|owned| Note::new(owned.note.value, random_nonce(), owned.note.public_key))
        .collect();
    let op = ChannelWithdrawOp {
        channel_id,
        inputs: InputNullifiers::try_from_iter(inputs.iter().map(|owned| owned.nullifier))
            .map_err(WalletError::from)?,
        outputs: commitments_of(&outputs)?,
        commitment_merkle_root: spend_state.root_of(note_set),
    };
    Ok(ChannelWithdrawal {
        op,
        opening: ZkTransferOpening {
            inputs: inputs.iter().map(|owned| owned.note).collect(),
            outputs,
        },
    })
}

// The notes of a program in a channel that a step can consume at a tip: the
// ones the wallet holds at its spend state, and the ones it knows the opening
// of that the tip has not seen created, which an earlier step of the
// transaction creates. A note on chain too young for the spend state is
// refused until it is deep enough.
struct ProgramInputs<'a> {
    spend_state: WalletState,
    tip_state: WalletState,
    known: &'a KnownNotes,
    note_set: NoteSetIndex,
    program_id: ProgramId,
}

impl<'a> ProgramInputs<'a> {
    fn at(
        tip: HeaderId,
        note_set: NoteSetIndex,
        program_id: ProgramId,
        state: &'a ServiceState<'_>,
    ) -> Result<Self, WalletServiceError> {
        Ok(Self {
            spend_state: state.wallet().spend_state(tip)?,
            tip_state: state.wallet().wallet_state_at(tip)?,
            known: state.wallet().known_notes(),
            note_set,
            program_id,
        })
    }

    fn consumable(&self, known: &KnownNote) -> bool {
        let commitment = known.note.commitment();
        let pending = || {
            !self.tip_state.notes.contains_key(&commitment)
                && !self.tip_state.spent.contains(&SpentNullifier {
                    note_set: known.note_set,
                    nullifier: known.nullifier,
                })
        };
        known.note_set == self.note_set
            && known.note.program == self.program_id
            && (self.spend_state.notes.contains_key(&commitment) || pending())
    }

    fn of(&self, commitment: &NoteCommitment) -> Option<KnownNote> {
        self.known
            .get(commitment)
            .copied()
            .filter(|known| self.consumable(known))
    }

    fn spent_by(&self, nullifier: &NoteNullifier) -> Option<KnownNote> {
        self.known
            .values()
            .copied()
            .find(|known| known.nullifier == *nullifier && self.consumable(known))
    }
}

// A program step consuming the notes of the program the request names, with
// the notes of the program it creates as the wallet follows them, so that a
// later program step of the transaction can consume them. The step of a
// program consuming a note has the nonces of its outputs derived from its
// first input, as the ZkProgramSpend enforces.
pub fn build_program_step(
    tip: HeaderId,
    request: ProgramStepRequest,
    state: &ServiceState<'_>,
) -> Result<(BuiltProgramStep, Vec<KnownNote>), WalletServiceError> {
    let ProgramStepRequest {
        channel_id,
        program_id,
        inputs,
        program_outputs,
        payouts,
        inscription,
    } = request;
    let note_set = channel_note_set(channel_id)
        .ok_or(WalletServiceError::ChannelWithoutNoteSet(channel_id))?;
    // Anyone could lead with a note of the program staked by `PK0`.
    if program_outputs
        .iter()
        .any(|output| output.stake_public_key == *PK0)
    {
        return Err(WalletServiceError::ProgramNoteStakedByPk0);
    }
    let program_inputs = ProgramInputs::at(tip, note_set, program_id, state)?;
    let inputs = inputs
        .iter()
        .map(|commitment| {
            program_inputs
                .of(commitment)
                .ok_or(WalletServiceError::NoChannelNote(*commitment, channel_id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let first_input = inputs.first().map(|input| input.nullifier);
    let nonce = |output: usize, value: Value, public_key: &ZkPublicKey| {
        first_input.map_or_else(random_nonce, |first_input| {
            derive_program_output_nonce(&first_input, output, value, public_key)
        })
    };
    let program_notes: Vec<ProgramNoteOpening> = program_outputs
        .into_iter()
        .enumerate()
        .map(|(output, program_output)| ProgramNoteOpening {
            value: program_output.value,
            nonce: nonce(output, program_output.value, &PK0),
            stake_public_key: program_output.stake_public_key,
            data: program_output.data,
        })
        .collect();
    let paid_notes: Vec<Note> = payouts
        .into_iter()
        .enumerate()
        .map(
            |(
                payout,
                ProgramPayout {
                    public_key,
                    stake_public_key,
                    value,
                },
            )| Note {
                stake_public_key: stake_public_key.unwrap_or(public_key),
                ..Note::new(
                    value,
                    nonce(program_notes.len() + payout, value, &public_key),
                    public_key,
                )
            },
        )
        .collect();
    let program_output_notes = opened_notes(&program_notes, program_id);
    let input_notes: Vec<Note> = inputs.iter().map(|input| input.note).collect();
    let output_notes = [program_output_notes.as_slice(), paid_notes.as_slice()].concat();
    let step = ProgramStep {
        program_id,
        inputs: InputNullifiers::try_from_iter(inputs.iter().map(|input| input.nullifier))
            .map_err(WalletError::from)?,
        program_outputs: commitments_of(&program_output_notes)?,
        outputs: commitments_of(&paid_notes)?,
        commitment_merkle_root: program_inputs.spend_state.root_of(note_set),
        program_step_id: program_step_id_of(&input_notes, &output_notes),
        inscription,
    };
    Ok((
        BuiltProgramStep {
            step,
            program_notes,
            paid_notes,
        },
        program_known_notes(&program_output_notes, note_set),
    ))
}

// The ZkProgramSpend witness of the program step the request names, proven
// against its root extended with the buffer of the channel at its position
// and bound to the id of its inscription, with the notes of the program it
// creates as the wallet follows them and the note set of the channel.
pub fn program_spend_witness(
    tip: HeaderId,
    request: &ProgramSpendRequest,
    state: &ServiceState<'_>,
) -> Result<(ProgramSpendWitness, Vec<KnownNote>, NoteSetIndex), WalletServiceError> {
    let ProgramSpendRequest {
        op,
        appended,
        step_index,
        program_notes,
        paid_notes,
    } = request;
    let channel_id = op.channel_id;
    let note_set = channel_note_set(channel_id)
        .ok_or(WalletServiceError::ChannelWithoutNoteSet(channel_id))?;
    let (step, paid_value, appended) = program_step_position(op, *step_index, appended)?;

    let program_output_notes = opened_notes(program_notes, step.program_id);
    let output_notes = [program_output_notes.as_slice(), paid_notes.as_slice()].concat();
    if !output_notes
        .iter()
        .map(Note::commitment)
        .eq(step.outputs().iter().copied())
    {
        return Err(WalletServiceError::ProgramStepOpenings(*step_index));
    }

    let program_inputs = ProgramInputs::at(tip, note_set, step.program_id, state)?;
    if program_inputs.spend_state.root_of(note_set) != step.commitment_merkle_root {
        return Err(WalletError::UnknownRoot {
            op_index: *step_index,
        }
        .into());
    }
    let input_notes = step
        .inputs
        .iter()
        .map(|nullifier| {
            program_inputs
                .spent_by(nullifier)
                .map(|input| input.note)
                .ok_or(WalletServiceError::NoChannelNoteOf(*nullifier, channel_id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (tx_commitment_root, inputs) = program_spend_inputs(
        &program_inputs.spend_state,
        program_inputs.known,
        note_set,
        &appended,
        step.inputs.as_slice(),
        input_notes,
    )?;
    let witness = ProgramSpendWitness::new(
        &inputs,
        &output_notes,
        program_output_notes.len(),
        tx_commitment_root,
        step.program_id,
        paid_value,
        fr_from_mod_bytes(&op.op_id()),
    )?;
    Ok((
        witness,
        program_known_notes(&program_output_notes, note_set),
        note_set,
    ))
}

fn opened_notes(openings: &[ProgramNoteOpening], program_id: ProgramId) -> Vec<Note> {
    openings
        .iter()
        .map(|opening| opening.note(program_id))
        .collect()
}

fn commitments_of(notes: &[Note]) -> Result<OutputCommitments, WalletServiceError> {
    Ok(
        OutputCommitments::try_from_iter(notes.iter().map(Note::commitment))
            .map_err(WalletError::from)?,
    )
}

// The notes of a program as the wallet follows them: held by `PK0`, whose
// nullifier key everyone knows.
pub fn program_known_notes(notes: &[Note], note_set: NoteSetIndex) -> Vec<KnownNote> {
    let nullifier_key = ZkKey::new(Fr::from(0u64)).nullifier_key();
    notes
        .iter()
        .map(|note| KnownNote {
            note: *note,
            note_set,
            nullifier: derive_note_nullifier(&note.commitment(), &nullifier_key),
        })
        .collect()
}

// The program step at `step_index` of `op`, over all its mini-blocks, with the
// value it is paid and the buffer of the channel at its position: `appended`,
// which the earlier operations of the transaction add, then the outputs of
// the steps before it.
pub fn program_step_position<'a>(
    op: &'a InscriptionOp,
    step_index: usize,
    appended: &[NoteCommitment],
) -> Result<(&'a ProgramStep, Value, Vec<NoteCommitment>), WalletServiceError> {
    let Some(Step::Program(step)) = op.steps().nth(step_index) else {
        return Err(WalletServiceError::NotAProgramStep(step_index));
    };
    let appended = appended
        .iter()
        .chain(
            op.steps()
                .take(step_index)
                .flat_map(|step| step.outputs().iter()),
        )
        .copied()
        .collect();
    Ok((step, paid_value(op, step_index)?, appended))
}

// The value paid to the program step at `step_index` of `op`: the program
// deposits between it and the program step before it in its mini-block.
fn paid_value(op: &InscriptionOp, step_index: usize) -> Result<Value, WalletError> {
    let mut first = 0;
    let before: Vec<&Step> = op
        .mini_blocks
        .iter()
        .find_map(|mini_block| {
            let position = step_index - first;
            first += mini_block.len();
            (step_index < first).then(|| mini_block.iter().take(position).collect())
        })
        .unwrap_or_default();
    before
        .into_iter()
        .rev()
        .map_while(|step| match step {
            Step::Program(_) => None,
            Step::ProgramDeposit(deposit) => Some(deposit.value),
            Step::User(_) => Some(0),
        })
        .try_fold(0, Value::checked_add)
        .ok_or_else(|| GasOverflow.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_step_is_proven_behind_the_buffer_of_its_transaction() {
        let op = InscriptionOp::sample_with_step();
        let earlier = NoteCommitment(Fr::from(1u64));
        let (step, paid_value, appended) = program_step_position(&op, 2, &[earlier]).unwrap();
        assert_eq!(step, &ProgramStep::sample());
        // the program deposit before it pays it
        assert_eq!(paid_value, 0x38);
        // the notes of the earlier operations, then those of the earlier steps
        assert_eq!(
            appended,
            vec![
                earlier,
                NoteCommitment(Fr::from(0x24u64)),
                NoteCommitment(Fr::from(0x36u64))
            ]
        );
        assert!(matches!(
            program_step_position(&op, 1, &[]),
            Err(WalletServiceError::NotAProgramStep(1))
        ));
    }
}
