//! Block cases: the fixed work of a block, the end-of-block work per changed
//! note set, a mixed block measured whole, and the headers releasing
//! withdrawals.
//!
//! Each block case measures, from the encoded transactions of the block, what
//! the node runs on its contents: decoding and preverifying every transaction,
//! then applying them all with the block-wide accounting (the leader reward
//! note, the execution and storage markets and the end-of-block roots of the
//! changed note sets). The proofs are deferred, and only the mixed block
//! verifies them after. Each header case measures the application of a header
//! to the ledger, its proof of leadership accepted without a verification.

use std::sync::LazyLock;

use lb_binary_codec::canonical::BinaryDecode as _;
use lb_core::{
    events::{BlockEvent, HeaderEvent, TxEvent},
    mantle::{
        Op, OpProof, SignedOps,
        batch::{DeferredProof, DeferredProofs},
        gas::MainnetGasProfile,
        ledger::verification_mode::StandardMode,
        note_set::LEDGER_SET,
        notes::NoteCommitment,
        ops::{
            StepProof,
            channel::{
                ChannelId, MsgId,
                inscribe::{ChannelStep, ChannelSteps, InscriptionOp, Step},
                withdraw::{ChannelWithdrawOp, WITHDRAW_DELAY},
            },
            sdp::SDPWithdrawOp,
        },
        transactions::states::{Preverified, Unverified},
    },
    sdp::SNAPSHOT_FINALIZATION_DELAY,
};
use lb_cryptarchia_engine::{Slot, UncleSlots};
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::ZkPublicKey;
use logos_blockchain_ledger::{Config, LedgerState};

use crate::{
    cycles::Case,
    fixture::{HeaderId, commitments, filled_state, nullifiers, signed},
    ops::{
        AcceptedLeadership, ProgramShape, channel_tip, deposit_op, encode, encode_inscription,
        fresh_commitments, fresh_nullifiers, inscription, io_dims, next_epoch, program_step,
        root_of, setup_declaration, setup_provider_key, signed_by, transfer_tx, user_step,
        zk_transfer, zk_transfer_series,
    },
};

/// What applying the contents of a block yields.
pub type AppliedBlock = (LedgerState, Vec<TxEvent>, Vec<BlockEvent>, DeferredProofs);

/// The encoded transactions of a block, the state and the config they apply
/// to, and whether the block verifies its deferred proofs.
pub struct BlockShape {
    pub name: &'static str,
    pub dims: Vec<(&'static str, u64)>,
    pub state: LedgerState,
    pub config: Config,
    pub txs: Vec<Vec<u8>>,
    pub verify: bool,
}

impl BlockShape {
    // A case measuring the block, once checked to apply, so that no run
    // measures an early rejection.
    fn into_case(self) -> Case {
        let Self {
            name,
            dims,
            state,
            config,
            txs,
            verify,
        } = self;
        if let Err(error) = execute(state.clone(), &config, &txs) {
            panic!("{name} {dims:?}: {error}");
        }
        Case {
            name,
            dims,
            run: Box::new(move |counter| {
                counter.measure(
                    || (state.clone(), txs.clone()),
                    |(state, txs)| {
                        let (state, tx_events, block_events, proofs) =
                            execute(state, &config, &txs).expect("the block applies");
                        let (proofs, verified) = if verify {
                            (None, verify_each(proofs))
                        } else {
                            (Some(proofs), Vec::new())
                        };
                        (txs, state, tx_events, block_events, proofs, verified)
                    },
                )
            }),
        }
    }
}

/// Verifies the deferred `proofs` as `DeferredProofs::verify` does, but each
/// batch and each program proof on its own: `verify` returns at the first batch
/// or program proof that fails, as each of the measured ones does, their proofs
/// being of valid points but not of their statements. A batch decompresses its
/// proofs, stopping only at an invalid point, then runs one multi-pairing
/// whatever the proofs, so that a failing batch costs what a passing one does,
/// and so does a program proof. Returns whether each verification passed: the
/// `ZkTransfer` batch, the `ZkProgramSpend` batch, then each program proof.
pub fn verify_each(proofs: DeferredProofs) -> Vec<bool> {
    let transfers = DeferredProofs::from_iter(
        proofs
            .zk_transfers()
            .iter()
            .map(|proof| DeferredProof::new(*proof.proof(), *proof.inputs())),
    );
    let spends = DeferredProofs::from_iter(
        proofs
            .program_spends()
            .iter()
            .map(|proof| DeferredProof::new(*proof.proof(), *proof.inputs())),
    );
    let programs: Vec<DeferredProofs> = proofs
        .program_proofs()
        .iter()
        .map(|proof| {
            DeferredProofs::from_iter([DeferredProof::new(*proof.proof(), proof.inputs().clone())])
        })
        .collect();
    drop(proofs);
    [transfers, spends]
        .into_iter()
        .chain(programs)
        .map(|batch| batch.verify().is_ok())
        .collect()
}

/// A header at `slot`, the state and the config it applies to, and the notes
/// it releases to the ledger set, in order.
pub struct HeaderShape {
    pub name: &'static str,
    pub dims: Vec<(&'static str, u64)>,
    pub state: LedgerState,
    pub config: Config,
    pub slot: Slot,
    pub released: Vec<NoteCommitment>,
}

impl HeaderShape {
    // A case measuring the header, once checked to apply and to release the
    // notes of its shape, its proof of leadership built by the setup.
    fn into_case(self) -> Case {
        let Self {
            name,
            dims,
            state,
            config,
            slot,
            released,
        } = self;
        let (_, events) = apply_header(state.clone(), &config, slot, &leadership())
            .unwrap_or_else(|error| panic!("{name} {dims:?}: {error}"));
        assert_eq!(released_outputs(&events), released, "{name} {dims:?}");
        Case {
            name,
            dims,
            run: Box::new(move |counter| {
                counter.measure(
                    || (state.clone(), leadership()),
                    |(state, leader)| {
                        let applied = apply_header(state, &config, slot, &leader)
                            .expect("the header applies");
                        (leader, applied)
                    },
                )
            }),
        }
    }
}

/// The proof of leadership of the measured blocks, which every check accepts.
pub fn leadership() -> AcceptedLeadership {
    AcceptedLeadership(reward_key())
}

/// Applies the header of a block at `slot` led by `leader`.
pub fn apply_header(
    state: LedgerState,
    config: &Config,
    slot: Slot,
    leader: &AcceptedLeadership,
) -> Result<(LedgerState, Vec<HeaderEvent>), String> {
    state
        .try_apply_header::<_, HeaderId>(slot, leader, &UncleSlots::default(), config)
        .map_err(|error| format!("header: {error}"))
}

// The outputs the released withdrawals of `events` add to the ledger set, in
// order.
fn released_outputs(events: &[HeaderEvent]) -> Vec<NoteCommitment> {
    events
        .iter()
        .flat_map(|event| match event {
            HeaderEvent::SdpWithdrawalReleased { outputs, .. }
            | HeaderEvent::ChannelWithdrawalReleased { outputs, .. } => outputs.as_slice(),
            HeaderEvent::SdpRewardDistributed { .. } => &[],
        })
        .copied()
        .collect()
}

fn after_header(state: LedgerState, config: &Config, slot: Slot) -> LedgerState {
    apply_header(state, config, slot, &leadership())
        .expect("the setup header applies")
        .0
}

// The key the leader of the measured blocks is paid to.
fn reward_key() -> ZkPublicKey {
    ZkPublicKey::new(Fr::from(3u64))
}

/// Decodes and preverifies every transaction of `txs`, then applies them as
/// the contents of one block.
pub fn execute(
    state: LedgerState,
    config: &Config,
    txs: &[Vec<u8>],
) -> Result<AppliedBlock, String> {
    let txs = txs
        .iter()
        .map(|bytes| {
            SignedOps::<Unverified, StandardMode>::decode_all(bytes, &())
                .map_err(|error| format!("decode: {error}"))?
                .preverify()
                .map_err(|error| format!("preverify: {error}"))
        })
        .collect::<Result<Vec<SignedOps<Preverified, StandardMode>>, String>>()?;
    state
        .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
            config,
            reward_key(),
            txs.into_iter(),
        )
        .map_err(|error| format!("apply: {error}"))
}

pub fn block_cases() -> Vec<Case> {
    block_shapes()
        .into_iter()
        .map(BlockShape::into_case)
        .chain(header_shapes().into_iter().map(HeaderShape::into_case))
        .collect()
}

/// Every measured block.
pub fn block_shapes() -> Vec<BlockShape> {
    let fixture = filled_state();
    let mut shapes = vec![BlockShape {
        name: "block/fixed",
        dims: Vec::new(),
        state: fixture.state.clone(),
        config: fixture.config.clone(),
        txs: Vec::new(),
        verify: false,
    }];
    shapes.extend([1, 2, 4, 8, 16].map(note_sets_shape));
    let mixed = mixed();
    shapes.push(BlockShape {
        name: "block/mixed_no_verify",
        dims: mixed.dims.clone(),
        state: mixed.state.clone(),
        config: mixed.config.clone(),
        txs: mixed.txs.clone(),
        verify: false,
    });
    shapes.push(mixed);
    shapes
}

/// Every measured header.
pub fn header_shapes() -> Vec<HeaderShape> {
    let mut shapes: Vec<HeaderShape> = [0, 1, 2, 4, 8]
        .map(|withdrawals| {
            withdrawals_header(
                "block/header",
                vec![("withdrawals", withdrawals as u64)],
                &vec![(1, 1); withdrawals],
            )
        })
        .into();
    shapes.extend(zk_transfer_series().map(|(inputs, outputs)| {
        withdrawals_header(
            "block/header_withdrawal",
            io_dims(inputs, outputs),
            &[(inputs, outputs)],
        )
    }));
    shapes.extend([0, 1, 2, 4].map(sdp_release_header));
    shapes
}

// The header at the due slot of channel withdrawals of the fixture channel,
// one of each number of inputs and outputs of `withdrawals`, posted in a block
// after a header at slot 1. The setup applies the header of the slot before,
// in the epoch of the due slot, so that the measured header only releases
// them.
fn withdrawals_header(
    name: &'static str,
    dims: Vec<(&'static str, u64)>,
    withdrawals: &[(usize, usize)],
) -> HeaderShape {
    let fixture = filled_state();
    let config = &fixture.config;
    let state = after_header(fixture.state.clone(), config, Slot::from(1u64));
    let root = root_of(&state, fixture.channel_set);
    let mut released = Vec::new();
    let txs: Vec<Vec<u8>> = withdrawals
        .iter()
        .enumerate()
        .map(|(index, &(inputs, outputs))| {
            let salt = 900 + index as u64;
            let op = ChannelWithdrawOp {
                channel_id: fixture.channel,
                inputs: fresh_nullifiers(inputs, salt),
                commitment_merkle_root: root,
                outputs: fresh_commitments(outputs, salt),
            };
            released.extend(op.outputs.iter().copied());
            encode(vec![Op::ChannelWithdraw(op)], |index, _, _| {
                OpProof::ZkTransfer(zk_transfer(index))
            })
        })
        .collect();
    let (state, ..) = execute(state, config, &txs).expect("the withdrawals apply");
    let due = state.slot().strict_add(Slot::from(WITHDRAW_DELAY));
    let parent = Slot::from(u64::from(due) - 1);
    assert_eq!(
        config.epoch(parent),
        config.epoch(due),
        "the due slot starts an epoch"
    );
    HeaderShape {
        name,
        dims,
        state: after_header(state, config, parent),
        config: config.clone(),
        slot: due,
        released,
    }
}

// The first header of the epoch at which the SDP withdrawals of `withdrawals`
// providers release their outputs, each provider declared and withdrawn in a
// block of the fixture epoch. The setup applies the first header of each epoch
// before, so that the measured header crosses one epoch.
fn sdp_release_header(withdrawals: usize) -> HeaderShape {
    let fixture = filled_state();
    let config = &fixture.config;
    let root = root_of(&fixture.state, LEDGER_SET);
    let mut released = Vec::new();
    let mut txs = Vec::new();
    for index in 0..withdrawals {
        let declare = setup_declaration(index, root);
        let salt = 0x3_0000 + index as u64;
        let withdraw = SDPWithdrawOp {
            declaration_id: declare.id(),
            nonce: 1,
            service_note_nullifier: nullifiers(1, salt)[0],
            outputs: fresh_commitments(1, salt),
            excess_value: 0,
        };
        released.push((declare.id(), withdraw.outputs.clone()));
        let key = setup_provider_key(index);
        txs.push(signed_by(Op::SDPDeclare(declare), &key));
        txs.push(signed_by(Op::SDPWithdraw(withdraw), &key));
    }
    // The outputs are released in the order of the declaration ids.
    released.sort_by_key(|(id, _)| *id);
    let (mut state, ..) =
        execute(fixture.state.clone(), config, &txs).expect("the withdrawals apply");
    let release = u64::from(u32::from(SNAPSHOT_FINALIZATION_DELAY)) + 1;
    for epoch in 1..release {
        state = next_epoch(state, config, epoch, &[]);
    }
    HeaderShape {
        name: "block/header_sdp_release",
        dims: vec![("withdrawals", withdrawals as u64)],
        state,
        config: config.clone(),
        slot: Slot::from(release * config.epoch_length()),
        released: released
            .iter()
            .flat_map(|(_, outputs)| outputs.iter().copied())
            .collect(),
    }
}

const CHANNELS: usize = 16;

// The fixture with `CHANNELS` more channels, created in one block so the
// roots of their sets are recorded.
static CHANNELS_STATE: LazyLock<LedgerState> = LazyLock::new(|| {
    let fixture = filled_state();
    let creations = (0..CHANNELS)
        .map(|index| {
            Op::ChannelInscribe(InscriptionOp {
                channel_id: channel(index),
                inscription: [0].into(),
                parent: MsgId::root(),
                signer: fixture.channel_key.public_key().into_unverified(),
                steps: ChannelSteps::default(),
            })
        })
        .collect();
    let (state, ..) = fixture
        .state
        .clone()
        .try_apply_block_contents::<_, HeaderId, MainnetGasProfile>(
            &fixture.config,
            reward_key(),
            std::iter::once(signed(creations, &fixture.channel_key)),
        )
        .expect("the channels are created");
    state
});

fn channel(index: usize) -> ChannelId {
    ChannelId::from([0x80 + index as u8; 32])
}

// One transaction per changed channel set, each an inscription with one user
// step.
fn note_sets_shape(changed: usize) -> BlockShape {
    let fixture = filled_state();
    let state = CHANNELS_STATE.clone();
    let channels = state.mantle_ledger().channels();
    let txs = (0..changed)
        .map(|index| {
            let channel_state = channels
                .channel_state(&channel(index))
                .expect("the channel exists");
            let set = channel_state.note_set.expect("the channel has a note set");
            let root = state
                .latest_note_sets()
                .get(set)
                .expect("the set exists")
                .commitment_root();
            let salt = 500 + index as u64;
            let op = InscriptionOp {
                channel_id: channel(index),
                inscription: [1].into(),
                parent: channel_state.tip_message,
                signer: fixture.channel_key.public_key().into_unverified(),
                steps: [Step::User(ChannelStep {
                    inputs: nullifiers(1, salt).try_into().expect("one input"),
                    outputs: commitments(1, salt).try_into().expect("one output"),
                    commitment_merkle_root: root,
                    buffered: 0,
                })]
                .into(),
            };
            encode_inscription(op, &[StepProof::User(zk_transfer(index))])
        })
        .collect();
    BlockShape {
        name: "block/note_sets",
        dims: vec![("changed", changed as u64)],
        state,
        config: fixture.config.clone(),
        txs,
        verify: false,
    }
}

const TRANSFERS: usize = 40;
const USER_INSCRIPTIONS: usize = 16;
const PROGRAM_INSCRIPTIONS: usize = 7;
const PAYLOAD: usize = 64;

// 40 transfers of two inputs and two outputs, 16 inscriptions of one user
// step, 7 inscriptions of one program step (one payment, one program input,
// two outputs, one of them opened), all on the fixture channel one after the
// other, and one deposit of one input and one output; the block verifies its
// deferred proofs.
fn mixed() -> BlockShape {
    let fixture = filled_state();
    let program = ProgramShape {
        payments: 1,
        inputs: 1,
        outputs: 2,
        openings: 1,
    };
    let mut txs: Vec<Vec<u8>> = (0..TRANSFERS)
        .map(|index| transfer_tx(2, 2, 600 + index as u64))
        .collect();
    let mut parent = channel_tip();
    for index in 0..USER_INSCRIPTIONS + PROGRAM_INSCRIPTIONS {
        let salt = 700 + index as u64;
        let (step, proof) = if index < USER_INSCRIPTIONS {
            (
                user_step(1, 1, 0, salt),
                StepProof::User(zk_transfer(index)),
            )
        } else {
            program_step(program, salt)
        };
        let op = inscription(parent, vec![step], PAYLOAD);
        parent = op.id();
        txs.push(encode_inscription(op, &[proof]));
    }
    txs.push(encode(
        vec![Op::ChannelDeposit(deposit_op(1, 1, 800))],
        |index, _, _| OpProof::ZkTransfer(zk_transfer(index)),
    ));
    let nullifiers = TRANSFERS * 2
        + USER_INSCRIPTIONS
        + PROGRAM_INSCRIPTIONS * (program.payments + program.inputs)
        + 1;
    let commitments = TRANSFERS * 2
        + USER_INSCRIPTIONS
        + PROGRAM_INSCRIPTIONS * (program.payments + program.outputs)
        + 1;
    let bytes: usize = txs.iter().map(Vec::len).sum();
    BlockShape {
        name: "block/mixed",
        dims: vec![
            ("transfers", TRANSFERS as u64),
            ("user_steps", USER_INSCRIPTIONS as u64),
            ("program_steps", PROGRAM_INSCRIPTIONS as u64),
            ("payments", (PROGRAM_INSCRIPTIONS * program.payments) as u64),
            ("deposits", 1),
            ("nullifiers", nullifiers as u64),
            ("commitments", commitments as u64),
            ("bytes", bytes as u64),
        ],
        state: fixture.state.clone(),
        config: fixture.config.clone(),
        txs,
        verify: true,
    }
}
