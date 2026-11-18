use std::collections::{BTreeMap, HashMap};

use lb_core::mantle::{
    Note, Op, Value,
    note_set::{CommitmentMmr, NoteSetIndex},
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
use lb_zktransfer::{MAX_INPUTS, MAX_OUTPUTS, ZkTransferWitnessBuilder};

use crate::{KnownNotes, WalletError, WalletState};

#[derive(Clone, Debug, Default)]
pub struct WitnessContext {
    // channels the transaction deposits into, withdraws from or steps in
    pub channel_note_sets: HashMap<ChannelId, NoteSetIndex>,
    // value of the reward note of every CLAIM_POW_REWARD
    pub pow_epoch_reward: Value,
}

// The ZkTransfer witness of one Operation, without the secret keys, which the
// holders of `signers` (in input order) add.
#[derive(Clone, Debug)]
pub struct OpWitness {
    pub op_index: usize,
    pub witness: ZkTransferWitnessBuilder,
    pub signers: Vec<ZkPublicKey>,
}

// A note set's commitment MMR at the root the transaction names, as the
// transaction extends it, with the inputs of an Operation and their paths.
type SetBuffer = (CommitmentMmr, Vec<NoteCommitment>, Vec<MerklePath>);

// The commitments the previous Operations of the transaction append, by set.
type Appended = HashMap<NoteSetIndex, Vec<NoteCommitment>>;

#[derive(Clone, Copy)]
enum Spend<'a> {
    // inputs of `note_set` proven against a root of the set
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
// proven against the root of `state` as the Operations before it extended it.
// `state` is the one the transaction was built against (see
// `Wallet::spend_state`).
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

// The witness of a channel step spending `nullifiers` of `note_set`, proven
// against the root of `state` extended with `appended`: the commitments the
// buffer of the set holds when the mini-block of the step starts.
pub fn step_witness(
    state: &WalletState,
    known: &KnownNotes,
    note_set: NoteSetIndex,
    appended: &[NoteCommitment],
    nullifiers: &[NoteNullifier],
    opening: &ZkTransferOpening,
    msg: Fr,
) -> Result<OpWitness, WalletError> {
    let spend = Spend::InSet {
        note_set,
        nullifiers,
        commitment_merkle_root: state.root_of(note_set),
    };
    let appended = Appended::from([(note_set, appended.to_vec())]);
    op_witness(state, known, &appended, 0, opening, spend, msg)
}

// The notes a program step spends with their paths, and the root they are
// proven against: the root of `note_set` at `state` extended by `appended`,
// the commitments the steps before it create.
pub fn program_spend_inputs(
    state: &WalletState,
    known: &KnownNotes,
    note_set: NoteSetIndex,
    appended: &[NoteCommitment],
    nullifiers: &[NoteNullifier],
    inputs: Vec<Note>,
) -> Result<(Fr, Vec<(Note, MerklePath)>), WalletError> {
    let opening = ZkTransferOpening {
        inputs,
        outputs: Vec::new(),
    };
    in_set_inputs(state, known, note_set, appended, 0, &opening, nullifiers)
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
    let mut appended = Appended::new();
    let mut witnesses = Vec::new();
    for (op_index, op) in ops.iter().enumerate() {
        let (spend, appends) = op_effects(op_index, op, openings, context)?;
        if let Some(spend) = spend.filter(|_| witnessed(op_index)) {
            let opening = openings
                .get(&op_index)
                .ok_or(WalletError::MissingOpening { op_index })?;
            witnesses.push(op_witness(
                state, known, &appended, op_index, opening, spend, msg,
            )?);
        }
        for (note_set, commitment) in appends {
            appended.entry(note_set).or_default().push(commitment);
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
                note_set: NoteSetIndex::Ledger,
                nullifiers: &transfer.inputs,
                commitment_merkle_root: transfer.commitment_merkle_root,
            }),
            transfer
                .outputs
                .iter()
                .map(|commitment| (NoteSetIndex::Ledger, *commitment))
                .collect(),
        ),
        Op::ChannelDeposit(deposit) => {
            let note_set = channel_note_set(context, &deposit.channel_id)?;
            (
                Some(Spend::InSet {
                    note_set: NoteSetIndex::Ledger,
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
                note_set: NoteSetIndex::Ledger,
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
            (None, vec![(NoteSetIndex::Ledger, reward.commitment())])
        }
        Op::ChannelInscribe(inscription) if !inscription.mini_blocks.is_empty() => {
            if openings.contains_key(&op_index) {
                return Err(WalletError::UnsupportedSteps { op_index });
            }
            let note_set = channel_note_set(context, &inscription.channel_id)?;
            (
                None,
                inscription
                    .steps()
                    .flat_map(Step::outputs)
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
    appended: &Appended,
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
            if state
                .commitments
                .get(&note_set)
                .map(CommitmentMmr::frontier_root)
                != Some(commitment_merkle_root)
            {
                return Err(WalletError::UnknownRoot { op_index });
            }
            in_set_inputs(
                state,
                known,
                note_set,
                appended.get(&note_set).map_or(&[], Vec::as_slice),
                op_index,
                opening,
                nullifiers,
            )?
        }
        Spend::ServiceNote(nullifier) => {
            let [service] = opening.inputs.as_slice() else {
                return Err(WalletError::NullifierMismatch { op_index });
            };
            let service_commitment = service.commitment();
            let (mmr, path) = push(
                &CommitmentMmr::new(),
                service_commitment,
                &mut [],
                NoteSetIndex::Sdp,
            )?;
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

// The root of `note_set` at `state` extended by `appended`, and the opened
// inputs with their paths to it.
fn in_set_inputs(
    state: &WalletState,
    known: &KnownNotes,
    note_set: NoteSetIndex,
    appended: &[NoteCommitment],
    op_index: usize,
    opening: &ZkTransferOpening,
    nullifiers: &[NoteNullifier],
) -> Result<(Fr, Vec<(Note, MerklePath)>), WalletError> {
    let (mmr, commitments, paths) = set_buffer(state, note_set, appended, opening)?;
    let inputs = input_paths(state, known, op_index, opening, nullifiers, |commitment| {
        commitments
            .iter()
            .position(|buffered| buffered == commitment)
            .map(|position| paths[position].clone())
    })?;
    Ok((mmr.frontier_root(), inputs))
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

// The MMR of `note_set` at `state` extended by `appended`, with the inputs of
// `opening` it holds: the ones `state` owns, then the ones `appended` creates.
fn set_buffer(
    state: &WalletState,
    note_set: NoteSetIndex,
    appended: &[NoteCommitment],
    opening: &ZkTransferOpening,
) -> Result<SetBuffer, WalletError> {
    let inputs = opening
        .inputs
        .iter()
        .map(Note::commitment)
        .collect::<Vec<_>>();
    let (mut commitments, mut paths): (Vec<_>, Vec<_>) = inputs
        .iter()
        .filter_map(|commitment| {
            let owned = state.notes.get(commitment)?;
            (owned.note_set == note_set).then(|| (*commitment, owned.path.clone()))
        })
        .unzip();
    let mut mmr = state
        .commitments
        .get(&note_set)
        .cloned()
        .unwrap_or_default();
    for commitment in appended {
        let (next, path) = push(&mmr, *commitment, &mut paths, note_set)?;
        mmr = next;
        if inputs.contains(commitment) {
            commitments.push(*commitment);
            paths.push(path);
        }
    }
    Ok((mmr, commitments, paths))
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
            if known_nullifier(state, known, &commitment) != Some(*nullifier) {
                return Err(WalletError::NullifierMismatch { op_index });
            }
            Ok((*note, path))
        })
        .collect()
}

// The nullifier the wallet derived for the note of `commitment`, from its owned
// notes or else from the openings it knows, if it holds the note.
fn known_nullifier(
    state: &WalletState,
    known: &KnownNotes,
    commitment: &NoteCommitment,
) -> Option<NoteNullifier> {
    state
        .notes
        .get(commitment)
        .map(|owned| owned.nullifier)
        .or_else(|| known.get(commitment).map(|known_note| known_note.nullifier))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use lb_core::mantle::{
        ops::{
            sdp::{SDPDeclareOp, SDPWithdrawOp},
            transfer::TransferOp,
        },
        transactions::{
            GasPrices, TxList,
            builder::MantleTxBuilder,
            tx_list::ops::{OpsContext, OpsGasContext},
        },
    };
    use lb_cryptarchia_engine::Epoch;

    use super::*;
    use crate::{
        DEFAULT_FEE_MARGIN_PERCENT, NoteSetSpend,
        test_utils::{FORK_DIGEST, GAS_PROFILE, known, nullifier, pk},
    };

    fn free_context() -> OpsContext {
        OpsContext {
            gas_context: OpsGasContext::new(HashMap::new(), GasPrices::new(0, 0), GAS_PROFILE),
        }
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "Walks the witnesses of every operation of one transaction."
    )]
    fn op_witnesses_chain_the_buffer_and_prove_withdrawals_against_the_service_note() {
        let owned = Note::new(1_000, Fr::from(1u64), pk(1));
        let service = Note::new(500, Fr::from(2u64), pk(3));
        let exact = Note::new(600, Fr::from(5u64), pk(1));
        let change = Note::new(400, Fr::from(6u64), pk(1));
        let returned = Note::new(500, Fr::from(7u64), pk(1));
        let known = known(&[
            (owned, NoteSetIndex::Ledger, nullifier(1)),
            (service, NoteSetIndex::Sdp, nullifier(2)),
            (exact, NoteSetIndex::Ledger, nullifier(5)),
        ]);
        let state = WalletState::from_genesis(&known, &[owned], &[service], Epoch::new(0)).unwrap();
        let root = state.commitments[&NoteSetIndex::Ledger].frontier_root();

        let ops = Ops::new(
            FORK_DIGEST,
            TxList::new_unchecked(vec![
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
            ]),
        );
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
            .map(|w| w.witness.tx_commitment_root())
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
        // `exact` is leaf 1 of the extended ledger set: its level 0 sibling is
        // `owned`, on the left.
        assert_eq!(
            witnesses[1].witness.inputs()[0].path_and_selectors[0],
            (owned.commitment().0, true)
        );
        assert_eq!(
            witnesses
                .iter()
                .map(|w| w.signers.clone())
                .collect::<Vec<_>>(),
            vec![vec![pk(1)], vec![pk(1)], vec![pk(3)]]
        );
        assert!(witnesses.iter().all(|w| w.witness.msg() == Fr::from(77u64)));
    }

    #[test]
    fn step_witness_spends_the_channel_notes_against_the_root_of_their_set() {
        let channel = NoteSetIndex::Channel(ChannelId::from([2; 32]));
        let small = Note::new(300, Fr::from(1u64), pk(1));
        let large = Note::new(700, Fr::from(2u64), pk(1));
        let ledger = Note::new(900, Fr::from(3u64), pk(1));
        let known = known(&[
            (small, channel, nullifier(1)),
            (large, channel, nullifier(2)),
            (ledger, NoteSetIndex::Ledger, nullifier(3)),
        ]);
        let state = WalletState::from_genesis(&known, &[ledger], &[], Epoch::new(0))
            .unwrap()
            .apply_block(
                &known,
                &crate::WalletBlock {
                    id: [1; 32].into(),
                    parent: [0; 32].into(),
                    epoch: Epoch::new(0),
                    updates: vec![lb_core::mantle::note_set::NoteSetUpdate {
                        note_set: channel,
                        nullifiers: vec![],
                        commitments: vec![small.commitment(), large.commitment()],
                    }],
                    revealed: vec![],
                    withdrawals: vec![],
                },
            )
            .unwrap();
        let root = state.commitments[&channel].frontier_root();
        let paid = Note::new(500, Fr::from(4u64), pk(2));

        let (nullifiers, opening, spend_root) = state
            .note_set_spend(
                NoteSetSpend {
                    note_set: channel,
                    appended: vec![],
                    outputs: vec![paid],
                    excess: 100,
                    change_pk: pk(1),
                    change_nonce: Fr::from(5u64),
                    holder_pks: vec![pk(1)],
                },
                &known,
                &HashSet::new(),
            )
            .unwrap();

        // the largest note covers the payment and the excess, the rest is
        // change
        assert_eq!(nullifiers.as_slice(), [nullifier(2)]);
        assert_eq!(opening.inputs, vec![large]);
        assert_eq!(
            opening.outputs,
            vec![paid, Note::new(100, Fr::from(5u64), pk(1))]
        );
        assert_eq!(spend_root, root);

        let witness = step_witness(
            &state,
            &known,
            channel,
            &[],
            &nullifiers,
            &opening,
            Fr::from(77u64),
        )
        .unwrap();
        assert_eq!(witness.witness.tx_commitment_root(), root);
        assert_eq!(witness.signers, vec![pk(1)]);

        // the notes of the other sets do not pay for a channel step
        assert_eq!(
            state.note_set_spend(
                NoteSetSpend {
                    note_set: channel,
                    appended: vec![],
                    outputs: vec![Note::new(1_001, Fr::from(6u64), pk(2))],
                    excess: 0,
                    change_pk: pk(1),
                    change_nonce: Fr::from(7u64),
                    holder_pks: vec![pk(1)],
                },
                &known,
                &HashSet::new(),
            ),
            Err(WalletError::InsufficientFunds { available: 1_000 })
        );
    }

    // A step of a later mini-block is proven against the root extended with
    // the notes the transaction creates before it, and can spend the ones the
    // wallet knows the opening of.
    #[test]
    fn step_witness_covers_the_notes_appended_before_its_mini_block() {
        let channel = NoteSetIndex::Channel(ChannelId::from([2; 32]));
        let on_chain = Note::new(300, Fr::from(1u64), pk(1));
        // created by an earlier mini-block: one paid to the wallet, one not
        let buffered = Note::new(700, Fr::from(2u64), pk(1));
        let foreign = Note::new(900, Fr::from(3u64), pk(2));
        let known = known(&[
            (on_chain, channel, nullifier(1)),
            (buffered, channel, nullifier(2)),
        ]);
        let state = WalletState::from_genesis(&known, &[], &[], Epoch::new(0))
            .unwrap()
            .apply_block(
                &known,
                &crate::WalletBlock {
                    id: [1; 32].into(),
                    parent: [0; 32].into(),
                    epoch: Epoch::new(0),
                    updates: vec![lb_core::mantle::note_set::NoteSetUpdate {
                        note_set: channel,
                        nullifiers: vec![],
                        commitments: vec![on_chain.commitment()],
                    }],
                    revealed: vec![],
                    withdrawals: vec![],
                },
            )
            .unwrap();
        let root = state.commitments[&channel].frontier_root();
        let extended = |appended: &[NoteCommitment]| {
            appended
                .iter()
                .fold(state.commitments[&channel].clone(), |mmr, commitment| {
                    mmr.push_with_paths(*commitment, &mut []).unwrap().0
                })
                .frontier_root()
        };
        let spend = |value, appended: &[NoteCommitment]| {
            state.note_set_spend(
                NoteSetSpend {
                    note_set: channel,
                    appended: appended.to_vec(),
                    outputs: vec![Note::new(value, Fr::from(4u64), pk(2))],
                    excess: 0,
                    change_pk: pk(1),
                    change_nonce: Fr::from(5u64),
                    holder_pks: vec![pk(1)],
                },
                &known,
                &HashSet::new(),
            )
        };
        let witness_root = |appended: &[NoteCommitment],
                            nullifiers: &[NoteNullifier],
                            opening: &ZkTransferOpening| {
            step_witness(
                &state,
                &known,
                channel,
                appended,
                nullifiers,
                opening,
                Fr::from(77u64),
            )
            .unwrap()
            .witness
            .tx_commitment_root()
        };

        // the note on chain, behind a note of somebody else
        let appended = [foreign.commitment()];
        let (nullifiers, opening, named_root) = spend(300, &appended).unwrap();
        assert_eq!(opening.inputs, vec![on_chain]);
        assert_eq!(named_root, root, "the step names the root on chain");
        assert_eq!(
            witness_root(&appended, &nullifiers, &opening),
            extended(&appended)
        );

        // the note an earlier mini-block paid to the wallet, with the one on
        // chain
        let appended = [foreign.commitment(), buffered.commitment()];
        let (nullifiers, opening, named_root) = spend(1_000, &appended).unwrap();
        assert_eq!(opening.inputs, vec![buffered, on_chain]);
        assert_eq!(named_root, root);
        assert_eq!(
            witness_root(&appended, &nullifiers, &opening),
            extended(&appended)
        );

        // a known note is not spendable before the transaction creates it
        assert_eq!(
            spend(1_000, &[]),
            Err(WalletError::InsufficientFunds { available: 300 })
        );
    }

    // The inscription of the next three tests. The channel holds a note of
    // Alice (5) and one of Bob (4). In its first mini-block Alice pays Carol 2,
    // which creates `carol_note` and `alice_change`. Its second mini-block
    // comes behind those two notes.
    const ALICE: u64 = 1;
    const BOB: u64 = 2;
    const CAROL: u64 = 3;

    fn mini_block_channel() -> NoteSetIndex {
        NoteSetIndex::Channel(ChannelId::from([3; 32]))
    }

    fn alice_note() -> Note {
        Note::new(5, Fr::from(1u64), pk(ALICE))
    }

    fn bob_note() -> Note {
        Note::new(4, Fr::from(2u64), pk(BOB))
    }

    fn carol_note() -> Note {
        Note::new(2, Fr::from(3u64), pk(CAROL))
    }

    fn alice_change() -> Note {
        Note::new(3, Fr::from(4u64), pk(ALICE))
    }

    // The root of the channel on chain, extended with `appended`.
    fn mini_block_root(appended: &[NoteCommitment]) -> Fr {
        [alice_note().commitment(), bob_note().commitment()]
            .iter()
            .chain(appended)
            .fold(CommitmentMmr::new(), |mmr, commitment| {
                mmr.push(*commitment).unwrap()
            })
            .frontier_root()
    }

    // The step of the holder `pk` whose wallet knows `known`, paying `value`
    // behind `appended`: the notes it spends and creates, the root it names
    // and the root it is proven against.
    fn mini_block_step(
        holder: u64,
        known: &KnownNotes,
        appended: &[NoteCommitment],
        value: Value,
    ) -> Result<(ZkTransferOpening, Fr, Fr), WalletError> {
        let channel = mini_block_channel();
        let state = WalletState::from_genesis(known, &[], &[], Epoch::new(0))?.apply_block(
            known,
            &crate::WalletBlock {
                id: [1; 32].into(),
                parent: [0; 32].into(),
                epoch: Epoch::new(0),
                updates: vec![lb_core::mantle::note_set::NoteSetUpdate {
                    note_set: channel,
                    nullifiers: vec![],
                    commitments: vec![alice_note().commitment(), bob_note().commitment()],
                }],
                revealed: vec![],
                withdrawals: vec![],
            },
        )?;
        let (nullifiers, opening, named_root) = state.note_set_spend(
            NoteSetSpend {
                note_set: channel,
                appended: appended.to_vec(),
                outputs: vec![Note::new(value, Fr::from(8u64), pk(9))],
                excess: 0,
                change_pk: pk(holder),
                change_nonce: Fr::from(9u64),
                holder_pks: vec![pk(holder)],
            },
            known,
            &HashSet::new(),
        )?;
        let proven_root = step_witness(
            &state,
            known,
            channel,
            appended,
            &nullifiers,
            &opening,
            Fr::from(77u64),
        )
        .unwrap()
        .witness
        .tx_commitment_root();
        Ok((opening, named_root, proven_root))
    }

    // Mini-block 1: Alice spends her note on chain, proven against the root on
    // chain.
    #[test]
    fn a_first_mini_block_step_is_proven_against_the_root_on_chain() {
        let known = known(&[(alice_note(), mini_block_channel(), nullifier(ALICE))]);

        let (opening, named_root, proven_root) = mini_block_step(ALICE, &known, &[], 2).unwrap();

        assert_eq!(opening.inputs, vec![alice_note()]);
        assert_eq!(
            opening
                .outputs
                .iter()
                .map(|note| note.value)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(named_root, mini_block_root(&[]));
        assert_eq!(proven_root, mini_block_root(&[]));
    }

    // Mini-block 2, old note: Bob spends his note on chain behind the notes
    // Alice's step created, which he knows nothing of but their commitments.
    #[test]
    fn a_later_mini_block_step_spends_a_note_on_chain_behind_the_earlier_outputs() {
        let known = known(&[(bob_note(), mini_block_channel(), nullifier(BOB))]);
        let appended = [carol_note().commitment(), alice_change().commitment()];

        let (opening, named_root, proven_root) =
            mini_block_step(BOB, &known, &appended, 1).unwrap();

        assert_eq!(opening.inputs, vec![bob_note()]);
        assert_eq!(
            named_root,
            mini_block_root(&[]),
            "it names the root on chain"
        );
        assert_eq!(proven_root, mini_block_root(&appended));
        assert_ne!(proven_root, named_root);
    }

    // Mini-block 2, new note: Carol holds nothing on chain. She imported the
    // opening of the note Alice's step pays her, and spends it in the same
    // inscription.
    #[test]
    fn a_later_mini_block_step_spends_a_note_an_earlier_one_created() {
        let known = known(&[(carol_note(), mini_block_channel(), nullifier(CAROL))]);
        let appended = [carol_note().commitment(), alice_change().commitment()];

        let (opening, named_root, proven_root) =
            mini_block_step(CAROL, &known, &appended, 2).unwrap();

        assert_eq!(opening.inputs, vec![carol_note()]);
        assert_eq!(
            named_root,
            mini_block_root(&[]),
            "it names the root on chain"
        );
        assert_eq!(proven_root, mini_block_root(&appended));

        // not before the inscription creates it
        assert_eq!(
            mini_block_step(CAROL, &known, &[], 2).unwrap_err(),
            WalletError::InsufficientFunds { available: 0 }
        );
        // nor behind the outputs of somebody else alone
        assert_eq!(
            mini_block_step(CAROL, &known, &[alice_change().commitment()], 2).unwrap_err(),
            WalletError::InsufficientFunds { available: 0 }
        );
    }

    #[test]
    fn op_witness_at_needs_no_opening_for_the_other_operations() {
        let owned = Note::new(1_000, Fr::from(1u64), pk(1));
        let foreign = Note::new(70, Fr::from(2u64), pk(2));
        let known = known(&[(owned, NoteSetIndex::Ledger, nullifier(1))]);
        let state = WalletState::from_genesis(&known, &[owned], &[], Epoch::new(0)).unwrap();
        let root = state.commitments[&NoteSetIndex::Ledger].frontier_root();
        let ops = Ops::new(
            FORK_DIGEST,
            TxList::new_unchecked(vec![
                // proven by the caller, with a note the wallet does not know
                Op::Transfer(TransferOp::new(
                    [nullifier(9)].into(),
                    [foreign.commitment()].into(),
                    Fr::from(3u64),
                    0,
                )),
                Op::Transfer(TransferOp::new([nullifier(1)].into(), [].into(), root, 0)),
            ]),
        );
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
            witness.witness.tx_commitment_root(),
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
            (notes[0], NoteSetIndex::Ledger, nullifier(1)),
            (notes[1], NoteSetIndex::Ledger, nullifier(2)),
            (notes[2], NoteSetIndex::Ledger, nullifier(3)),
        ]);
        let state = WalletState::from_genesis(&known, &notes, &[], Epoch::new(0)).unwrap();
        let output = Note::new(350, Fr::from(9u64), pk(2));
        let builder = MantleTxBuilder::new().add_ledger_output(output).unwrap();

        let funded = state
            .fund_tx(
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
        let ops: Vec<Op> = funded.build(FORK_DIGEST).unwrap().iter().cloned().collect();
        assert_eq!(
            ops,
            vec![Op::Transfer(TransferOp::new(
                [nullifier(2), nullifier(3)].into(),
                [output.commitment(), change.commitment()].into(),
                state.commitments[&NoteSetIndex::Ledger].frontier_root(),
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
            .map(|(note, i)| (*note, NoteSetIndex::Ledger, nullifier(i)))
            .collect();
        let known = known(&entries);
        let state = WalletState::from_genesis(&known, &notes, &[], Epoch::new(0)).unwrap();
        let builder = MantleTxBuilder::new()
            .add_ledger_output(Note::new(450, Fr::from(9u64), pk(2)))
            .unwrap();
        assert_eq!(
            state
                .fund_tx(
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

    #[test]
    fn a_tx_funded_with_the_fee_margin_survives_one_step_of_each_gas_market() {
        let note = Note::new(10_000_000_000_000, Fr::from(1u64), pk(1));
        let known = known(&[(note, NoteSetIndex::Ledger, nullifier(1))]);
        let state = WalletState::from_genesis(&known, &[note], &[], Epoch::new(0)).unwrap();
        let builder = MantleTxBuilder::new()
            .add_ledger_output(Note::new(5, Fr::from(9u64), pk(2)))
            .unwrap();
        let context = |execution: u64, storage: u64| OpsContext {
            gas_context: OpsGasContext::new(
                HashMap::new(),
                GasPrices::new(execution, storage),
                GAS_PROFILE,
            ),
        };
        let (execution, storage) = (60_563_532, 753_662_110);
        let fund = |margin| {
            state
                .fund_tx(
                    &builder,
                    pk(1),
                    Fr::from(10u64),
                    [pk(1)],
                    &context(execution, storage),
                    &HashSet::new(),
                    margin,
                )
                .unwrap()
        };
        // Both prices one step of 9/8 higher, rounded up as the markets do.
        let raised = context((execution * 9).div_ceil(8), (storage * 9).div_ceil(8));
        let still_funded = |funded: &MantleTxBuilder| {
            funded.net_balance()
                >= i128::from(funded.minimum_gas_cost(&raised).unwrap().into_inner())
        };

        assert!(!still_funded(&fund(0)));
        assert!(still_funded(&fund(DEFAULT_FEE_MARGIN_PERCENT)));
    }
}
