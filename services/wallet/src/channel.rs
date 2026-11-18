use lb_core::{
    header::HeaderId,
    mantle::{
        Note, Value,
        channel::channel_note_set,
        note_set::NoteSetIndex,
        notes::{InputNullifiers, NoteCommitment, OutputCommitments, ProgramId},
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
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_wallet::{
    NoteSetSpend, WalletError,
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
    /// The notes of the program the step consumes, possibly none.
    pub inputs: Vec<NoteCommitment>,
    /// The notes of the program the step creates.
    pub program_outputs: Vec<ProgramOutput>,
    /// The user notes the step pays, by holder key.
    pub payments: Vec<(ZkPublicKey, Value)>,
    pub inscription: Inscription,
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
