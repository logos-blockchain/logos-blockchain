//! Block cases: the fixed work of a block, the end-of-block work per changed
//! note set, and a mixed block measured whole.
//!
//! Each case measures, from the encoded transactions of the block, what the
//! node runs on its contents: decoding and preverifying every transaction,
//! then applying them all with the block-wide accounting (the leader reward
//! note, the execution and storage markets and the end-of-block roots of the
//! changed note sets). The proofs are deferred.

use std::sync::LazyLock;

use lb_binary_codec::canonical::BinaryDecode as _;
use lb_core::{
    events::{BlockEvent, TxEvent},
    mantle::{
        Op, OpProof, SignedOps,
        batch::DeferredProofs,
        gas::MainnetGasProfile,
        ledger::verification_mode::StandardMode,
        ops::{
            StepProof,
            channel::{
                ChannelId, MsgId,
                inscribe::{ChannelStep, ChannelSteps, InscriptionOp, Step},
            },
        },
        transactions::states::{Preverified, Unverified},
    },
};
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::ZkPublicKey;
use logos_blockchain_ledger::{Config, LedgerState};

use crate::{
    cycles::Case,
    fixture::{HeaderId, commitments, filled_state, nullifiers, signed},
    ops::{
        ProgramShape, channel_tip, deposit_op, encode, encode_inscription, inscription,
        program_step, transfer_tx, user_step, zk_transfer,
    },
};

/// What applying the contents of a block yields.
pub type AppliedBlock = (LedgerState, Vec<TxEvent>, Vec<BlockEvent>, DeferredProofs);

/// The encoded transactions of a block and the state and the config they
/// apply to.
pub struct BlockShape {
    pub name: &'static str,
    pub dims: Vec<(&'static str, u64)>,
    pub state: LedgerState,
    pub config: Config,
    pub txs: Vec<Vec<u8>>,
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
                        let applied = execute(state, &config, &txs).expect("the block applies");
                        (txs, applied)
                    },
                )
            }),
        }
    }
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
    }];
    shapes.extend([1, 2, 4, 8, 16].map(note_sets_shape));
    shapes.push(mixed());
    shapes
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
    }
}

const TRANSFERS: usize = 40;
const USER_INSCRIPTIONS: usize = 16;
const PROGRAM_INSCRIPTIONS: usize = 7;
const PAYLOAD: usize = 64;

// 40 transfers of two inputs and two outputs, 16 inscriptions of one user
// step, 7 inscriptions of one program step (one payment, one program input,
// two outputs, one of them opened), all on the fixture channel one after the
// other, and one deposit of one input and one output.
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
    }
}
