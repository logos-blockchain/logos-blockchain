use lb_core::{
    header::HeaderId,
    mantle::{
        Note, Value,
        channel::channel_note_set,
        gas::GasOverflow,
        note_set::NoteSetIndex,
        notes::{
            InputNullifiers, NoteCommitment, NoteNullifier, OutputCommitments, ProgramId,
            derive_note_nullifier,
        },
        ops::{
            ProgramNoteOpening, ZkTransferProof,
            channel::{
                ChannelId,
                inscribe::{Inscription, InscriptionOp, Step, UserStep, user_step_id},
                program_step::{ProgramDeposit, ProgramStep, program_deposit_id},
                withdraw::ChannelWithdrawOp,
            },
        },
        transactions::builder::ZkTransferOpening,
    },
};
use lb_groth16::Fr;
use lb_key_management_system_service::keys::{ZkKey, ZkPublicKey};
use lb_wallet::{
    KnownNote, KnownNotes, NoteSetSpend, SpentNullifier, WalletError, WalletState,
    transaction::{OpWitness, step_witness},
};

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
    let outputs = OutputCommitments::try_from_iter(opening.outputs.iter().map(Note::commitment))
        .map_err(WalletError::from)?;
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
        outputs: OutputCommitments::try_from_iter(outputs.iter().map(Note::commitment))
            .map_err(WalletError::from)?,
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

// The note of `program_id` of `commitment` in `note_set`: one the wallet holds
// at `spend_state`, or else one it knows the opening of that `tip_state` has
// not seen created, which an earlier step of the transaction creates. A note
// on chain too young for `spend_state` is refused until it is deep enough.
pub fn program_input_of(
    spend_state: &WalletState,
    tip_state: &WalletState,
    known: &KnownNotes,
    note_set: NoteSetIndex,
    program_id: ProgramId,
    commitment: &NoteCommitment,
) -> Option<(Note, NoteNullifier)> {
    spend_state
        .notes
        .get(commitment)
        .map(|owned| (owned.note, owned.note_set, owned.nullifier))
        .or_else(|| {
            known
                .get(commitment)
                .map(|known| (known.note, known.note_set, known.nullifier))
                .filter(|(_, note_set, nullifier)| {
                    !tip_state.notes.contains_key(commitment)
                        && !tip_state.spent.contains(&SpentNullifier {
                            note_set: *note_set,
                            nullifier: *nullifier,
                        })
                })
        })
        .filter(|(note, set, _)| *set == note_set && note.program == program_id)
        .map(|(note, _, nullifier)| (note, nullifier))
}

// The note of `program_id` that `nullifier` spends in `note_set`, as
// `program_input_of`.
pub fn program_input_spent_by(
    spend_state: &WalletState,
    tip_state: &WalletState,
    known: &KnownNotes,
    note_set: NoteSetIndex,
    program_id: ProgramId,
    nullifier: &NoteNullifier,
) -> Option<Note> {
    spend_state
        .notes
        .values()
        .find(|owned| owned.note_set == note_set && owned.nullifier == *nullifier)
        .map(|owned| owned.note.commitment())
        .or_else(|| {
            known
                .values()
                .find(|known| known.note_set == note_set && known.nullifier == *nullifier)
                .map(|known| known.note.commitment())
        })
        .and_then(|commitment| {
            program_input_of(
                spend_state,
                tip_state,
                known,
                note_set,
                program_id,
                &commitment,
            )
        })
        .map(|(note, _)| note)
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
// value the program deposits before it in its mini-block pay and the buffer of
// the channel at its position: `appended`, which the earlier operations of the
// transaction add, then the outputs of the steps before it.
pub fn program_step_position(
    op: &InscriptionOp,
    step_index: usize,
    mut appended: Vec<NoteCommitment>,
) -> Result<(&ProgramStep, Value, Vec<NoteCommitment>), WalletServiceError> {
    let mut index = 0;
    for mini_block in op.mini_blocks.iter() {
        let mut paid_value: Value = 0;
        for step in mini_block.iter() {
            if index == step_index {
                let Step::Program(step) = step else {
                    return Err(WalletServiceError::NotAProgramStep(step_index));
                };
                return Ok((step, paid_value, appended));
            }
            match step {
                Step::ProgramDeposit(deposit) => {
                    paid_value = paid_value
                        .checked_add(deposit.value)
                        .ok_or_else(|| WalletError::from(GasOverflow))?;
                }
                Step::Program(_) => paid_value = 0,
                Step::User(_) => {}
            }
            appended.extend(step.outputs().iter().copied());
            index += 1;
        }
    }
    Err(WalletServiceError::NotAProgramStep(step_index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_step_is_proven_behind_the_buffer_of_its_transaction() {
        let op = InscriptionOp::sample_with_step();
        let earlier = NoteCommitment(Fr::from(1u64));
        let (step, paid_value, appended) = program_step_position(&op, 2, vec![earlier]).unwrap();
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
            program_step_position(&op, 1, Vec::new()),
            Err(WalletServiceError::NotAProgramStep(1))
        ));
    }
}
