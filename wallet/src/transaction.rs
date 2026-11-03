use std::collections::{BTreeMap, HashMap};

use lb_core::mantle::{
    Note, Op, Value,
    note_set::{CommitmentMmr, LEDGER_SET, NoteSetIndex, SDP_SET},
    notes::{NoteCommitment, NoteNullifier, derive_note_nonce},
    ops::{
        OpId as _,
        channel::{ChannelId, inscribe::Step},
        transfer::transfer_witness,
    },
    transactions::{Ops, builder::ZkTransferOpening},
};
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_mmr::{MerklePath, PushWithPathsError};
use lb_utils::bounded::BoundedError;
use lb_zktransfer::{MAX_INPUTS, MAX_OUTPUTS, ZkTransferWitnessInputsData};

use crate::{KnownNotes, WalletError, WalletState};

#[derive(Clone, Debug, Default)]
pub struct WitnessContext {
    // channels the transaction deposits into, withdraws from or steps in
    pub channel_note_sets: HashMap<ChannelId, NoteSetIndex>,
    // value of the reward note of every CLAIM_POW_REWARD
    pub pow_epoch_reward: Value,
}

// The ZkTransfer witness of one Operation, its secret keys left 0 for the
// holders of `signers` (in input order) to fill.
#[derive(Clone, Debug)]
pub struct OpWitness {
    pub op_index: usize,
    pub witness: ZkTransferWitnessInputsData,
    pub signers: Vec<ZkPublicKey>,
}

// A note set's commitment MMR as the transaction extends it, with the
// commitments the wallet can spend and their paths.
type SetBuffer = (CommitmentMmr, Vec<NoteCommitment>, Vec<MerklePath>);

#[derive(Clone, Copy)]
enum Spend<'a> {
    // inputs of `note_set` proven against its current root
    InSet {
        note_set: NoteSetIndex,
        nullifiers: &'a [NoteNullifier],
        commitment_merkle_root: Fr,
    },
    // a service note proven against the root of its own commitment
    ServiceNote(&'a NoteNullifier),
}

// What an Operation spends and the commitments it appends to the buffers.
type Effects<'a> = (Option<Spend<'a>>, Vec<(NoteSetIndex, NoteCommitment)>);

// The witnesses of the Operations of `ops` that carry a ZkTransfer, each
// proven against its set as the Operations before it extended it.
pub fn op_witnesses(
    state: &WalletState,
    known: &KnownNotes,
    ops: &Ops,
    openings: &BTreeMap<usize, ZkTransferOpening>,
    context: &WitnessContext,
    msg: Fr,
) -> Result<Vec<OpWitness>, WalletError> {
    witnesses_of(state, known, ops, openings, context, msg, |_| true)
}

// The witness of the Operation at `op_index` alone: the other Operations only
// extend the buffers, so they need no opening.
pub fn op_witness_at(
    state: &WalletState,
    known: &KnownNotes,
    ops: &Ops,
    openings: &BTreeMap<usize, ZkTransferOpening>,
    context: &WitnessContext,
    msg: Fr,
    op_index: usize,
) -> Result<OpWitness, WalletError> {
    witnesses_of(state, known, ops, openings, context, msg, |index| {
        index == op_index
    })?
    .pop()
    .ok_or(WalletError::MissingOpening { op_index })
}

fn witnesses_of(
    state: &WalletState,
    known: &KnownNotes,
    ops: &Ops,
    openings: &BTreeMap<usize, ZkTransferOpening>,
    context: &WitnessContext,
    msg: Fr,
    witnessed: impl Fn(usize) -> bool,
) -> Result<Vec<OpWitness>, WalletError> {
    let mut buffers = HashMap::new();
    let mut witnesses = Vec::new();
    for (op_index, op) in ops.iter().enumerate() {
        let (spend, appends) = op_effects(op_index, op, openings, context)?;
        if let Some(spend) = spend.filter(|_| witnessed(op_index)) {
            let opening = openings
                .get(&op_index)
                .ok_or(WalletError::MissingOpening { op_index })?;
            witnesses.push(op_witness(
                state,
                known,
                &mut buffers,
                op_index,
                opening,
                spend,
                msg,
            )?);
        }
        for (note_set, commitment) in appends {
            let (mmr, commitments, paths) = set_buffer(&mut buffers, state, note_set);
            let (next, path) = push(mmr, commitment, paths, note_set)?;
            *mmr = next;
            commitments.push(commitment);
            paths.push(path);
        }
    }
    Ok(witnesses)
}

fn op_effects<'a>(
    op_index: usize,
    op: &'a Op,
    openings: &BTreeMap<usize, ZkTransferOpening>,
    context: &WitnessContext,
) -> Result<Effects<'a>, WalletError> {
    Ok(match op {
        Op::Transfer(transfer) => (
            Some(Spend::InSet {
                note_set: LEDGER_SET,
                nullifiers: &transfer.inputs,
                commitment_merkle_root: transfer.commitment_merkle_root,
            }),
            transfer
                .outputs
                .iter()
                .map(|commitment| (LEDGER_SET, *commitment))
                .collect(),
        ),
        Op::ChannelDeposit(deposit) => {
            let note_set = channel_note_set(context, &deposit.channel_id)?;
            (
                Some(Spend::InSet {
                    note_set: LEDGER_SET,
                    nullifiers: &deposit.inputs,
                    commitment_merkle_root: deposit.commitment_merkle_root,
                }),
                deposit
                    .outputs
                    .iter()
                    .map(|commitment| (note_set, *commitment))
                    .collect(),
            )
        }
        // the outputs wait for the release
        Op::ChannelWithdraw(withdraw) => (
            Some(Spend::InSet {
                note_set: channel_note_set(context, &withdraw.channel_id)?,
                nullifiers: &withdraw.inputs,
                commitment_merkle_root: withdraw.commitment_merkle_root,
            }),
            Vec::new(),
        ),
        // the SDP set has no buffer
        Op::SDPDeclare(declare) => (
            Some(Spend::InSet {
                note_set: LEDGER_SET,
                nullifiers: &declare.inputs,
                commitment_merkle_root: declare.commitment_merkle_root,
            }),
            Vec::new(),
        ),
        Op::SDPWithdraw(withdraw) => (
            Some(Spend::ServiceNote(&withdraw.service_note_nullifier)),
            Vec::new(),
        ),
        Op::ClaimPowReward(claim) => {
            let value = context.pow_epoch_reward;
            let reward = Note::new(
                value,
                derive_note_nonce(&claim.op_id(), 0, value, &claim.public_key),
                claim.public_key,
            );
            (None, vec![(LEDGER_SET, reward.commitment())])
        }
        Op::ChannelInscribe(inscription) if !inscription.steps.is_empty() => {
            if openings.contains_key(&op_index) {
                return Err(WalletError::UnsupportedSteps { op_index });
            }
            let note_set = channel_note_set(context, &inscription.channel_id)?;
            (
                None,
                inscription
                    .steps
                    .iter()
                    // The change of the payments of a program step comes before its outputs
                    .flat_map(|step| {
                        let payments = match step {
                            Step::User(_) => &[][..],
                            Step::Program(step) => step.payments.as_slice(),
                        };
                        payments
                            .iter()
                            .flat_map(|payment| payment.outputs.iter())
                            .chain(step.outputs().iter())
                    })
                    .map(|commitment| (note_set, *commitment))
                    .collect(),
            )
        }
        Op::ChannelInscribe(_)
        | Op::ChannelConfig(_)
        | Op::SDPActive(_)
        | Op::ProgramRegister(_) => (None, Vec::new()),
    })
}

fn op_witness(
    state: &WalletState,
    known: &KnownNotes,
    buffers: &mut HashMap<NoteSetIndex, SetBuffer>,
    op_index: usize,
    opening: &ZkTransferOpening,
    spend: Spend,
    msg: Fr,
) -> Result<OpWitness, WalletError> {
    let (tx_commitment_root, inputs) = match spend {
        Spend::InSet {
            note_set,
            nullifiers,
            commitment_merkle_root,
        } => {
            // the wallet only proves against its own tip
            if state
                .commitments
                .get(&note_set)
                .map(CommitmentMmr::frontier_root)
                != Some(commitment_merkle_root)
            {
                return Err(WalletError::UnknownRoot { op_index });
            }
            let (mmr, commitments, paths) = &*set_buffer(buffers, state, note_set);
            let inputs = input_paths(state, known, op_index, opening, nullifiers, |commitment| {
                commitments
                    .iter()
                    .position(|buffered| buffered == commitment)
                    .map(|position| paths[position].clone())
            })?;
            (mmr.frontier_root(), inputs)
        }
        Spend::ServiceNote(nullifier) => {
            let [service] = opening.inputs.as_slice() else {
                return Err(WalletError::NullifierMismatch { op_index });
            };
            let service_commitment = service.commitment();
            let (mmr, path) = push(&CommitmentMmr::new(), service_commitment, &mut [], SDP_SET)?;
            let inputs = input_paths(
                state,
                known,
                op_index,
                opening,
                core::slice::from_ref(nullifier),
                |commitment| (*commitment == service_commitment).then(|| path.clone()),
            )?;
            (mmr.frontier_root(), inputs)
        }
    };
    if inputs.len() > MAX_INPUTS {
        return Err(BoundedError::TooManyItems {
            count: inputs.len(),
            max: MAX_INPUTS,
        }
        .into());
    }
    if opening.outputs.len() > MAX_OUTPUTS {
        return Err(BoundedError::TooManyItems {
            count: opening.outputs.len(),
            max: MAX_OUTPUTS,
        }
        .into());
    }
    Ok(OpWitness {
        op_index,
        witness: transfer_witness(&inputs, &opening.outputs, tx_commitment_root, msg),
        signers: inputs.iter().map(|(note, _)| note.public_key).collect(),
    })
}

fn channel_note_set(
    context: &WitnessContext,
    channel_id: &ChannelId,
) -> Result<NoteSetIndex, WalletError> {
    context
        .channel_note_sets
        .get(channel_id)
        .copied()
        .ok_or(WalletError::UnknownChannel(*channel_id))
}

fn set_buffer<'a>(
    buffers: &'a mut HashMap<NoteSetIndex, SetBuffer>,
    state: &WalletState,
    note_set: NoteSetIndex,
) -> &'a mut SetBuffer {
    buffers.entry(note_set).or_insert_with(|| {
        let (commitments, paths) = state
            .notes
            .iter()
            .filter(|(_, owned)| owned.note_set == note_set)
            .map(|(commitment, owned)| (*commitment, owned.path.clone()))
            .unzip();
        (
            state
                .commitments
                .get(&note_set)
                .cloned()
                .unwrap_or_default(),
            commitments,
            paths,
        )
    })
}

fn push(
    mmr: &CommitmentMmr,
    commitment: NoteCommitment,
    paths: &mut [MerklePath],
    note_set: NoteSetIndex,
) -> Result<(CommitmentMmr, MerklePath), WalletError> {
    mmr.push_with_paths(commitment, paths)
        .map_err(|error| match error {
            PushWithPathsError::MmrFull(_) => WalletError::MmrFull(note_set),
            PushWithPathsError::InvalidTrackedPath(error) => error.into(),
        })
}

// The opened inputs with their paths, each checked against the nullifier the
// Operation names for it.
fn input_paths(
    state: &WalletState,
    known: &KnownNotes,
    op_index: usize,
    opening: &ZkTransferOpening,
    nullifiers: &[NoteNullifier],
    path_of: impl Fn(&NoteCommitment) -> Option<MerklePath>,
) -> Result<Vec<(Note, MerklePath)>, WalletError> {
    if opening.inputs.len() != nullifiers.len() {
        return Err(WalletError::NullifierMismatch { op_index });
    }
    opening
        .inputs
        .iter()
        .zip(nullifiers)
        .map(|(note, nullifier)| {
            let commitment = note.commitment();
            let path = path_of(&commitment).ok_or(WalletError::MissingInputPath { op_index })?;
            let derived = state
                .notes
                .get(&commitment)
                .map(|owned| owned.nullifier)
                .or_else(|| {
                    known
                        .get(&commitment)
                        .map(|known_note| known_note.nullifier)
                });
            if derived != Some(*nullifier) {
                return Err(WalletError::NullifierMismatch { op_index });
            }
            Ok((*note, path))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use lb_core::mantle::{
        gas::MainnetGasProfile,
        ops::{
            sdp::{SDPDeclareOp, SDPWithdrawOp},
            transfer::TransferOp,
        },
        transactions::{
            GasPrices,
            builder::MantleTxBuilder,
            tx_list::ops::{OpsContext, OpsGasContext},
        },
    };
    use lb_cryptarchia_engine::Epoch;

    use super::*;
    use crate::test_utils::{known, nullifier, pk};

    fn free_context() -> OpsContext {
        OpsContext {
            gas_context: OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0)),
        }
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "Test function.")]
    fn op_witnesses_chain_the_buffer_and_prove_withdrawals_against_the_service_note() {
        let owned = Note::new(1_000, Fr::from(1u64), pk(1));
        let service = Note::new(500, Fr::from(2u64), pk(3));
        let exact = Note::new(600, Fr::from(5u64), pk(1));
        let change = Note::new(400, Fr::from(6u64), pk(1));
        let returned = Note::new(500, Fr::from(7u64), pk(1));
        let known = known(&[
            (owned, LEDGER_SET, nullifier(1)),
            (service, SDP_SET, nullifier(2)),
            (exact, LEDGER_SET, nullifier(5)),
        ]);
        let state = WalletState::from_genesis(&known, &[owned], &[service], Epoch::new(0)).unwrap();
        let root = state.commitments[&LEDGER_SET].frontier_root();

        let ops = Ops::new_unchecked(vec![
            Op::Transfer(TransferOp::new(
                [nullifier(1)].into(),
                [exact.commitment(), change.commitment()].into(),
                root,
                0,
            )),
            Op::SDPDeclare(SDPDeclareOp {
                inputs: [nullifier(5)].into(),
                commitment_merkle_root: root,
                amount: 600,
                ..SDPDeclareOp::sample()
            }),
            Op::SDPWithdraw(SDPWithdrawOp {
                service_note_nullifier: nullifier(2),
                outputs: [returned.commitment()].into(),
                excess_value: 0,
                ..SDPWithdrawOp::sample()
            }),
        ]);
        let openings = BTreeMap::from([
            (
                0,
                ZkTransferOpening {
                    inputs: vec![owned],
                    outputs: vec![exact, change],
                },
            ),
            (
                1,
                ZkTransferOpening {
                    inputs: vec![exact],
                    outputs: vec![],
                },
            ),
            (
                2,
                ZkTransferOpening {
                    inputs: vec![service],
                    outputs: vec![returned],
                },
            ),
        ]);

        let witnesses = op_witnesses(
            &state,
            &known,
            &ops,
            &openings,
            &WitnessContext::default(),
            Fr::from(77u64),
        )
        .unwrap();
        let roots: Vec<Fr> = witnesses
            .iter()
            .map(|w| w.witness.chain.tx_commitment_root)
            .collect();
        assert_eq!(
            roots,
            vec![
                root,
                CommitmentMmr::new()
                    .push(owned.commitment())
                    .unwrap()
                    .push(exact.commitment())
                    .unwrap()
                    .push(change.commitment())
                    .unwrap()
                    .frontier_root(),
                CommitmentMmr::new()
                    .push(service.commitment())
                    .unwrap()
                    .frontier_root(),
            ]
        );
        // `exact` is leaf 1 of the extended ledger set: its level 0 sibling is `owned`,
        // on the left.
        assert_eq!(
            witnesses[1].witness.wallet.inputs[0].path_and_selectors[0],
            (owned.commitment().0, true)
        );
        assert_eq!(
            witnesses
                .iter()
                .map(|w| w.signers.clone())
                .collect::<Vec<_>>(),
            vec![vec![pk(1)], vec![pk(1)], vec![pk(3)]]
        );
        assert!(
            witnesses
                .iter()
                .all(|w| w.witness.chain.msg == Fr::from(77u64))
        );
    }

    #[test]
    fn op_witness_at_needs_no_opening_for_the_other_operations() {
        let owned = Note::new(1_000, Fr::from(1u64), pk(1));
        let foreign = Note::new(70, Fr::from(2u64), pk(2));
        let known = known(&[(owned, LEDGER_SET, nullifier(1))]);
        let state = WalletState::from_genesis(&known, &[owned], &[], Epoch::new(0)).unwrap();
        let root = state.commitments[&LEDGER_SET].frontier_root();
        let ops = Ops::new_unchecked(vec![
            // proven by the caller, with a note the wallet does not know
            Op::Transfer(TransferOp::new(
                [nullifier(9)].into(),
                [foreign.commitment()].into(),
                Fr::from(3u64),
                0,
            )),
            Op::Transfer(TransferOp::new([nullifier(1)].into(), [].into(), root, 0)),
        ]);
        let openings = BTreeMap::from([(
            1,
            ZkTransferOpening {
                inputs: vec![owned],
                outputs: vec![],
            },
        )]);

        let witness = op_witness_at(
            &state,
            &known,
            &ops,
            &openings,
            &WitnessContext::default(),
            Fr::from(77u64),
            1,
        )
        .unwrap();
        assert_eq!(witness.op_index, 1);
        assert_eq!(
            witness.witness.chain.tx_commitment_root,
            CommitmentMmr::new()
                .push(owned.commitment())
                .unwrap()
                .push(foreign.commitment())
                .unwrap()
                .frontier_root()
        );
        assert_eq!(
            op_witnesses(
                &state,
                &known,
                &ops,
                &openings,
                &WitnessContext::default(),
                Fr::from(77u64),
            )
            .err(),
            Some(WalletError::MissingOpening { op_index: 0 })
        );
    }

    #[test]
    fn fund_tx_adds_the_largest_notes_and_a_change_note() {
        let notes = [
            Note::new(100, Fr::from(1u64), pk(1)),
            Note::new(300, Fr::from(2u64), pk(1)),
            Note::new(200, Fr::from(3u64), pk(1)),
        ];
        let known = known(&[
            (notes[0], LEDGER_SET, nullifier(1)),
            (notes[1], LEDGER_SET, nullifier(2)),
            (notes[2], LEDGER_SET, nullifier(3)),
        ]);
        let state = WalletState::from_genesis(&known, &notes, &[], Epoch::new(0)).unwrap();
        let output = Note::new(350, Fr::from(9u64), pk(2));
        let builder = MantleTxBuilder::new().add_ledger_output(output).unwrap();

        let funded = state
            .fund_tx::<MainnetGasProfile>(
                &builder,
                pk(1),
                Fr::from(10u64),
                [pk(1)],
                &free_context(),
                &HashSet::new(),
                0,
            )
            .unwrap();
        assert_eq!(
            funded
                .funding_inputs()
                .iter()
                .map(|f| (f.note.value, f.nullifier))
                .collect::<Vec<_>>(),
            vec![(300, nullifier(2)), (200, nullifier(3))]
        );
        let change = Note::new(150, Fr::from(10u64), pk(1));
        let ops: Vec<Op> = funded.build().unwrap().iter().cloned().collect();
        assert_eq!(
            ops,
            vec![Op::Transfer(TransferOp::new(
                [nullifier(2), nullifier(3)].into(),
                [output.commitment(), change.commitment()].into(),
                state.commitments[&LEDGER_SET].frontier_root(),
                0
            ))]
        );
    }

    #[test]
    fn fund_tx_spends_at_most_four_notes() {
        let notes: Vec<Note> = (1..=5)
            .map(|i| Note::new(100, Fr::from(i), pk(1)))
            .collect();
        let entries: Vec<_> = notes
            .iter()
            .zip(1..)
            .map(|(note, i)| (*note, LEDGER_SET, nullifier(i)))
            .collect();
        let known = known(&entries);
        let state = WalletState::from_genesis(&known, &notes, &[], Epoch::new(0)).unwrap();
        let builder = MantleTxBuilder::new()
            .add_ledger_output(Note::new(450, Fr::from(9u64), pk(2)))
            .unwrap();
        assert_eq!(
            state
                .fund_tx::<MainnetGasProfile>(
                    &builder,
                    pk(1),
                    Fr::from(10u64),
                    [pk(1)],
                    &free_context(),
                    &HashSet::new(),
                    0
                )
                .err(),
            Some(WalletError::InsufficientFunds { available: 400 })
        );
    }
}
