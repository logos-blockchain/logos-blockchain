//! End-to-end cases: one Mantle Transaction per Operation shape, from its
//! encoded bytes to the applied state, its proofs deferred.
//!
//! Each case measures what the node runs on a transaction it receives:
//! decoding it, its preverification (transaction hash, signatures, the
//! stateless checks of each Operation) and its application to the filled
//! ledger, which defers the ZK proofs. The batch verification of the deferred
//! proofs is measured by the micro cases.

use std::{
    num::NonZero,
    sync::{Arc, LazyLock},
};

use lb_binary_codec::canonical::{BinaryDecode as _, BinaryEncode as _};
use lb_blend_crypto::merkle::MerkleTree;
use lb_blend_message::reward::{BlendingToken, BlendingTokenEvaluation};
use lb_blend_proofs::{
    quota::{
        Quota, VerifiedProofOfQuota,
        inputs::prove::{
            PrivateInputs, PublicInputs,
            private::ProofOfCoreQuotaInputs,
            public::{CoreInputs, LeaderInputs, PowInputs},
        },
    },
    selection::VerifiedProofOfSelection,
};
use lb_core::{
    blend::core_quota,
    events::TxEvent,
    mantle::{
        Note, Op, OpProof, SignedOps, Value,
        batch::DeferredProofs,
        channel::{SlotTimeframe, SlotTimeout},
        gas::MainnetGasProfile,
        ledger::verification_mode::StandardMode,
        note_set::NoteSetIndex,
        notes::{InputNullifiers, NoteCommitment, OutputCommitments},
        ops::{
            ChannelInscribeProof, NoOpProof, ProgramNoteOpening, ProgramProof, ProgramStepProof,
            StepProof, ZkAndEd25519Proof, ZkProgramSpendProof, ZkTransferProof,
            channel::{
                ChannelId, MsgId, VerifiedChannelKeys,
                config::ChannelConfigOp,
                deposit::{DepositOp, Metadata},
                inscribe::{Inscription, InscriptionOp, MiniBlock, MiniBlocks, Step, UserStep},
                program_step::{ProgramDeposit, ProgramStep},
                withdraw::ChannelWithdrawOp,
            },
            pow::ClaimPowRewardOp,
            program::ProgramRegisterOp,
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
            transfer::TransferOp,
        },
        programs::ProgramVerificationKey,
        traits::Hashable as _,
        transactions::{GasPrices, OpProofs, Ops, states::Unverified},
    },
    proofs::{
        channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignatures},
        leader_proof::{LeaderProof, LeaderPublic},
    },
    sdp::{ActivityMetadata, Locator, Nonce, ProviderId, ServiceType, blend::ActivityProof},
};
use lb_cryptarchia_engine::{Epoch, Slot, UncleSlots};
use lb_groth16::{CompressedGroth16Proof, Fr};
use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey, ZkPublicKey, kdf_public_key};
use logos_blockchain_ledger::{
    EpochState, GasAndFees, Ledger, LedgerState,
    config::{EraScheduledConfig, ModulusShift},
};

use crate::{
    cycles::{Case, ROUNDS, in_turn},
    fixture::{HeaderId, commitments, filled_state, nullifiers},
};

/// What applying a transaction yields.
pub type Applied = (LedgerState, GasAndFees, Vec<TxEvent>, DeferredProofs);

/// The proofs a transaction defers, by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Deferred {
    pub zk_transfers: usize,
    pub program_spends: usize,
    pub program_proofs: usize,
}

impl Deferred {
    #[must_use]
    pub fn of(proofs: &DeferredProofs) -> Self {
        Self {
            zk_transfers: proofs.zk_transfers().len(),
            program_spends: proofs.program_spends().len(),
            program_proofs: proofs.program_proofs().len(),
        }
    }

    const fn transfers(zk_transfers: usize) -> Self {
        Self {
            zk_transfers,
            program_spends: 0,
            program_proofs: 0,
        }
    }
}

/// The encodings of one transaction, the state and the config it applies to,
/// and the proofs it defers. A transaction spending notes has one encoding per
/// round, from [`each_round`], each spending its own nullifiers; any other may
/// have a single one.
pub struct OpShape {
    pub name: &'static str,
    pub dims: Vec<(&'static str, u64)>,
    pub state: LedgerState,
    pub config: EraScheduledConfig,
    pub bytes: Vec<Vec<u8>>,
    pub deferred: Deferred,
}

impl OpShape {
    fn on_fixture(
        name: &'static str,
        dims: Vec<(&'static str, u64)>,
        bytes: Vec<Vec<u8>>,
        deferred: Deferred,
    ) -> Self {
        let fixture = filled_state();
        Self {
            name,
            dims,
            state: fixture.state.clone(),
            config: fixture.config.clone(),
            bytes,
            deferred,
        }
    }

    // A case measuring the transaction, once checked to apply and to defer
    // the proofs of its shape, so that no run measures an early rejection.
    // Each round applies its own encoding to a clone of the state, so that
    // nothing a round spends stays for the next.
    fn into_case(self) -> Case {
        let Self {
            name,
            dims,
            state,
            config,
            bytes,
            deferred,
        } = self;
        let (.., proofs) = execute(state.clone(), &config, &bytes[0])
            .unwrap_or_else(|error| panic!("{name} {dims:?}: {error}"));
        assert_eq!(Deferred::of(&proofs), deferred, "{name} {dims:?}");
        let mut next_bytes = in_turn(bytes);
        Case {
            name,
            dims,
            run: Box::new(move |counter| {
                counter.measure(
                    || (state.clone(), next_bytes()),
                    |(state, bytes)| {
                        let applied = execute(state, &config, &bytes).expect("the case applies");
                        (bytes, applied)
                    },
                )
            }),
        }
    }
}

/// Decodes, preverifies and applies the transaction `bytes` encode, as the
/// node does with a transaction it receives.
pub fn execute(
    state: LedgerState,
    config: &EraScheduledConfig,
    bytes: &[u8],
) -> Result<Applied, String> {
    let tx = SignedOps::<Unverified, StandardMode>::decode_all(bytes, &())
        .map_err(|error| format!("decode: {error}"))?
        .preverify()
        .map_err(|error| format!("preverify: {error}"))?;
    state
        .try_apply_transaction::<_, HeaderId, MainnetGasProfile>(config, &tx)
        .map_err(|error| format!("apply: {error}"))
}

pub fn op_cases() -> Vec<Case> {
    op_shapes().into_iter().map(OpShape::into_case).collect()
}

/// Every measured transaction.
pub fn op_shapes() -> Vec<OpShape> {
    let mut shapes = Vec::new();
    shapes.extend(transfers());
    shapes.extend(inscriptions());
    shapes.extend(program_inscriptions());
    shapes.extend(configs());
    shapes.extend(deposits());
    shapes.extend(withdrawals());
    shapes.extend(sdp_declares());
    shapes.push(sdp_withdraw());
    shapes.push(sdp_active());
    shapes.push(claim_pow());
    shapes.push(program_register());
    shapes
}

// Proofs made of valid curve points, as a prover makes them: the ledger does
// not check them, but they decompress as real ones.
static PROOFS: LazyLock<Vec<CompressedGroth16Proof>> = LazyLock::new(|| {
    let (_, proving_key) = lb_groth16::test_utils::three_input_setup(1);
    (0..8u64)
        .map(|seed| {
            let proof =
                lb_groth16::test_utils::three_input_prove(&proving_key, [Fr::from(seed); 3], seed);
            CompressedGroth16Proof::try_from(&proof).expect("the proof compresses")
        })
        .collect()
});

/// A proof of valid curve points, a different one for each `index` modulo 8.
pub fn proof(index: usize) -> CompressedGroth16Proof {
    PROOFS[index % PROOFS.len()]
}

/// A `ZkTransfer` of [`proof`] `index`.
pub fn zk_transfer(index: usize) -> ZkTransferProof {
    ZkTransferProof::new(proof(index))
}

/// The encoding of `ops`, each with the proof `prove` gives it from the hash
/// of the transaction.
pub fn encode(ops: Vec<Op>, prove: impl Fn(usize, &Op, &[u8]) -> OpProof) -> Vec<u8> {
    let ops = Ops::new_unchecked(ops);
    let tx_hash = ops.hash();
    let proofs = ops
        .iter()
        .enumerate()
        .map(|(index, op)| prove(index, op, tx_hash.as_signing_bytes()))
        .collect();
    SignedOps::<Unverified, StandardMode>::from_parts(ops, OpProofs::new_unchecked(proofs))
        .expect("the transaction builds")
        .encode_to_vec()
}

/// The encoding of the inscription `op` with its step proofs, signed by the
/// key of the fixture channel.
pub fn encode_inscription(op: InscriptionOp, step_proofs: &[StepProof]) -> Vec<u8> {
    let key = &filled_state().channel_key;
    encode(vec![Op::ChannelInscribe(op)], |_, _, hash| {
        OpProof::ChannelInscribe(ChannelInscribeProof::new(
            key.sign_payload(hash),
            step_proofs.to_vec(),
        ))
    })
}

/// The current commitment root of the note set `set` of `state`.
pub fn root_of(state: &LedgerState, set: NoteSetIndex) -> Fr {
    state
        .latest_note_sets()
        .get(set)
        .expect("the set exists")
        .commitment_root()
}

/// The current commitment root of the ledger set of the fixture.
pub fn ledger_root() -> Fr {
    root_of(&filled_state().state, NoteSetIndex::Ledger)
}

/// The current commitment root of the set of the fixture channel.
pub fn channel_root() -> Fr {
    let fixture = filled_state();
    root_of(&fixture.state, fixture.channel_set)
}

/// The inputs of the `count` nullifiers [`nullifiers`] gives for `salt`.
pub fn fresh_nullifiers(count: usize, salt: u64) -> InputNullifiers {
    nullifiers(count, salt)
        .try_into()
        .expect("the inputs fit a list")
}

/// The outputs of the `count` commitments [`commitments`] gives for `salt`.
pub fn fresh_commitments(count: usize, salt: u64) -> OutputCommitments {
    commitments(count, salt)
        .try_into()
        .expect("the outputs fit a list")
}

/// The salt the round `round` of a case adds to the salts of its notes, in bits
/// none of them uses.
pub const fn round_salt(round: usize) -> u64 {
    (round as u64) << 32
}

/// The encodings of a transaction, one per round, `encode` given the salt of
/// the round: each round spends its own fresh nullifiers, whose low leaves are
/// elsewhere in the tree, so that no round reads the paths an earlier one left
/// in the caches.
pub fn each_round(encode: impl Fn(u64) -> Vec<u8>) -> Vec<Vec<u8>> {
    (0..ROUNDS).map(|round| encode(round_salt(round))).collect()
}

fn transfer_op(inputs: usize, outputs: usize, salt: u64) -> TransferOp {
    TransferOp::new(
        fresh_nullifiers(inputs, salt),
        fresh_commitments(outputs, salt),
        ledger_root(),
        0,
    )
}

/// A transfer of `inputs` and `outputs` notes of the ledger set.
pub fn transfer_tx(inputs: usize, outputs: usize, salt: u64) -> Vec<u8> {
    encode(
        vec![Op::Transfer(transfer_op(inputs, outputs, salt))],
        |index, _, _| OpProof::ZkTransfer(zk_transfer(index)),
    )
}

/// The two series of a `ZkTransfer` shape: the inputs vary at one output, then
/// the outputs at one input.
pub fn zk_transfer_series() -> impl Iterator<Item = (usize, usize)> {
    (1..=4)
        .map(|inputs| (inputs, 1))
        .chain((2..=8).map(|outputs| (1, outputs)))
}

/// The dimensions of a `ZkTransfer` shape.
pub fn io_dims(inputs: usize, outputs: usize) -> Vec<(&'static str, u64)> {
    vec![("inputs", inputs as u64), ("outputs", outputs as u64)]
}

fn transfers() -> Vec<OpShape> {
    zk_transfer_series()
        .map(|(inputs, outputs)| {
            OpShape::on_fixture(
                "op/transfer",
                io_dims(inputs, outputs),
                each_round(|salt| transfer_tx(inputs, outputs, salt)),
                Deferred::transfers(1),
            )
        })
        .collect()
}

/// A user step of the fixture channel, spending `inputs` and creating
/// `outputs` fresh notes against the current root of its set.
pub fn user_step(inputs: usize, outputs: usize, salt: u64) -> Step {
    Step::User(UserStep {
        inputs: fresh_nullifiers(inputs, salt),
        outputs: fresh_commitments(outputs, salt),
        commitment_merkle_root: channel_root(),
        inscription: Inscription::default(),
    })
}

/// An inscription on the fixture channel after `parent`, its steps in a single
/// mini-block.
pub fn inscription(parent: MsgId, steps: Vec<Step>, payload: usize) -> InscriptionOp {
    let mini_blocks = if steps.is_empty() {
        vec![]
    } else {
        vec![steps]
    };
    inscription_of_mini_blocks(parent, mini_blocks, payload)
}

/// An inscription on the fixture channel after `parent`.
pub fn inscription_of_mini_blocks(
    parent: MsgId,
    mini_blocks: Vec<Vec<Step>>,
    payload: usize,
) -> InscriptionOp {
    let fixture = filled_state();
    InscriptionOp {
        channel_id: fixture.channel,
        inscription: Inscription::try_from(vec![0xa5; payload]).expect("the payload fits"),
        parent,
        signer: fixture.channel_key.public_key().into_unverified(),
        mini_blocks: MiniBlocks::try_from(
            mini_blocks
                .into_iter()
                .map(|steps| MiniBlock::try_from(steps).expect("a mini-block has a step"))
                .collect::<Vec<_>>(),
        )
        .expect("the mini-blocks fit"),
    }
}

/// The last message of the fixture channel.
pub fn channel_tip() -> MsgId {
    let fixture = filled_state();
    fixture
        .state
        .mantle_ledger()
        .channels()
        .channel_state(&fixture.channel)
        .expect("the fixture channel exists")
        .tip_message
}

/// The most user steps of one input and one output an inscription of no
/// payload carries within the Execution Gas limit, at the gas prices of the
/// node: the end of the step series.
pub const MOST_USER_STEPS: usize = 518;

/// The user steps of the step series of `op/inscribe`.
pub const USER_STEPS: [usize; 12] = [0, 1, 2, 4, 8, 16, 32, 64, 128, 256, 512, MOST_USER_STEPS];

// `buffered` is the number of commitments the last mini-block covers; the
// mini-blocks of round salt `salt` are `mini_blocks(salt)`.
fn user_inscription_shape(
    mini_blocks: impl Fn(u64) -> Vec<Vec<Step>>,
    buffered: u16,
    payload: usize,
) -> OpShape {
    let first = mini_blocks(round_salt(0));
    let user_steps = first.iter().map(Vec::len).sum::<usize>();
    let outputs: usize = first
        .iter()
        .flatten()
        .map(|step| step.outputs().len())
        .sum();
    let step_proofs: Vec<StepProof> = (0..user_steps)
        .map(|index| StepProof::ZkTransfer(zk_transfer(index)))
        .collect();
    let tip = channel_tip();
    OpShape::on_fixture(
        "op/inscribe",
        vec![
            ("user_steps", user_steps as u64),
            ("outputs", outputs as u64),
            ("buffered", u64::from(buffered)),
            ("payload", payload as u64),
        ],
        each_round(|salt| {
            let op = inscription_of_mini_blocks(tip, mini_blocks(salt), payload);
            encode_inscription(op, &step_proofs)
        }),
        Deferred::transfers(user_steps),
    )
}

// Three series, each step spending one note: the user steps of one output at
// no buffered output and no payload, in one mini-block; a step of one output
// in a second mini-block covering the outputs of the first, one step creating
// them; the payload of one step of one output. The outputs of all the steps are
// a dimension, the buffered series being the only one where they differ from
// the steps.
fn inscriptions() -> Vec<OpShape> {
    let mut shapes = Vec::new();
    for user_steps in USER_STEPS {
        shapes.push(user_inscription_shape(
            |salt| {
                let steps: Vec<Step> = (0..user_steps as u64)
                    .map(|index| user_step(1, 1, salt + 100 + index))
                    .collect();
                if steps.is_empty() {
                    vec![]
                } else {
                    vec![steps]
                }
            },
            0,
            0,
        ));
    }
    shapes.push(user_inscription_shape(
        |salt| {
            vec![vec![
                user_step(1, 8, salt + 200),
                user_step(1, 1, salt + 201),
            ]]
        },
        0,
        0,
    ));
    for buffered in [1u16, 4, 8] {
        shapes.push(user_inscription_shape(
            |salt| {
                vec![
                    vec![user_step(1, usize::from(buffered), salt + 200)],
                    vec![user_step(1, 1, salt + 201)],
                ]
            },
            buffered,
            0,
        ));
    }
    for payload in [256, 1024, 4096, 16_384] {
        shapes.push(user_inscription_shape(
            |salt| vec![vec![user_step(1, 1, salt + 300)]],
            0,
            payload,
        ));
    }
    shapes
}

/// The shape of a program step of the fixture program and of the program
/// deposits it consumes.
#[derive(Clone, Copy, Debug)]
pub struct ProgramShape {
    pub deposits: usize,
    pub inputs: usize,
    pub outputs: usize,
    pub openings: usize,
}

// The tags of the salts of the notes a program deposit and a program step
// spend and create, which keep them apart from each other and from the notes
// of the user steps of the same salt.
const DEPOSIT_NOTES: u64 = 1 << 48;
const STEP_NOTES: u64 = 2 << 48;

/// The program deposits of `shape`, then its program step, with their proofs:
/// each program deposit spends one fresh note and returns one change note;
/// `openings` of the `outputs` are program outputs, derived from their
/// openings, and the others user outputs.
pub fn program_steps(shape: ProgramShape, salt: u64) -> Vec<(Step, StepProof)> {
    let fixture = filled_state();
    let root = channel_root();
    let spent = nullifiers(shape.deposits, salt | DEPOSIT_NOTES);
    let change = commitments(shape.deposits, salt | DEPOSIT_NOTES);
    let deposits = spent
        .into_iter()
        .zip(change)
        .enumerate()
        .map(|(index, (input, output))| {
            let deposit = ProgramDeposit {
                program_id: fixture.program,
                inputs: [input].into(),
                outputs: [output].into(),
                commitment_merkle_root: root,
                value: 1,
                intent: Fr::from(index as u64),
                inscription: Inscription::default(),
            };
            (
                Step::ProgramDeposit(deposit),
                StepProof::ZkTransfer(zk_transfer(index)),
            )
        });
    let openings: Vec<ProgramNoteOpening> = (0..shape.openings)
        .map(|index| ProgramNoteOpening {
            value: 1 + index as u64,
            nonce: Fr::from(salt * 64 + index as u64 + 1),
            stake_public_key: ZkPublicKey::new(Fr::from(5u64)),
            data: Fr::from(index as u64),
        })
        .collect();
    let program_outputs: Vec<NoteCommitment> = openings
        .iter()
        .map(|opening| opening.note(fixture.program).commitment())
        .collect();
    let step = ProgramStep {
        program_id: fixture.program,
        inputs: fresh_nullifiers(shape.inputs, salt | STEP_NOTES),
        program_outputs: program_outputs.try_into().expect("the program outputs fit"),
        outputs: fresh_commitments(shape.outputs - shape.openings, salt | STEP_NOTES),
        commitment_merkle_root: root,
        program_step_id: Fr::from(7u64),
        inscription: Inscription::default(),
    };
    let proof = StepProof::ProgramStep(ProgramStepProof {
        spend_proof: ZkProgramSpendProof::new(proof(1)),
        program_proof: ProgramProof::new(proof(2)),
        program_notes: openings,
    });
    deposits.chain([(Step::Program(step), proof)]).collect()
}

/// The most program deposits of one input and one output an inscription of no
/// payload carries before a program step of one input and one output, within
/// the Execution Gas limit, at the gas prices of the node: the end of the
/// program deposit series.
pub const MOST_PROGRAM_DEPOSITS: usize = 500;

/// The program deposits of the program deposit series of
/// `op/inscribe_program`.
pub const PROGRAM_DEPOSITS: [usize; 11] =
    [0, 1, 2, 4, 8, 16, 32, 64, 128, 256, MOST_PROGRAM_DEPOSITS];

// Four series, one dimension varying at a time from no program deposit, one
// program input, one output and no opening; the openings vary at eight
// outputs, the most a step can create.
fn program_inscriptions() -> Vec<OpShape> {
    let base = ProgramShape {
        deposits: 0,
        inputs: 1,
        outputs: 1,
        openings: 0,
    };
    let tip = channel_tip();
    PROGRAM_DEPOSITS
        .map(|deposits| ProgramShape { deposits, ..base })
        .into_iter()
        .chain(
            (0..=4)
                .filter(|i| *i != 1)
                .map(|inputs| ProgramShape { inputs, ..base }),
        )
        .chain((2..=8).map(|outputs| ProgramShape { outputs, ..base }))
        .chain((1..=8).map(|openings| ProgramShape {
            outputs: 8,
            openings,
            ..base
        }))
        .map(|shape| {
            OpShape::on_fixture(
                "op/inscribe_program",
                vec![
                    ("program_deposits", shape.deposits as u64),
                    ("program_inputs", shape.inputs as u64),
                    ("program_outputs", shape.outputs as u64),
                    ("openings", shape.openings as u64),
                ],
                each_round(|salt| {
                    let (steps, proofs): (Vec<_>, Vec<_>) =
                        program_steps(shape, 1 + salt).into_iter().unzip();
                    encode_inscription(inscription(tip, steps, 0), &proofs)
                }),
                Deferred {
                    zk_transfers: shape.deposits,
                    program_spends: 1,
                    program_proofs: 1,
                },
            )
        })
        .collect()
}

// A channel of `keys`, the first `threshold` of them needed to configure it:
// it is created by a first configuration, and the measured one replaces it with
// the same keys, signed by the first `threshold`.
fn config_shape(
    name: &'static str,
    dims: Vec<(&'static str, u64)>,
    channel: ChannelId,
    keys: &[Ed25519Key],
    threshold: u16,
) -> OpShape {
    let fixture = filled_state();
    let public_keys: Vec<_> = keys.iter().map(Ed25519Key::public_key).collect();
    let config_op = |parent, posting_timeframe: u32| {
        ChannelConfigOp::new(
            channel,
            parent,
            VerifiedChannelKeys::try_from(public_keys.clone()).expect("the keys fit"),
            SlotTimeframe::from(posting_timeframe),
            SlotTimeout::from(0u32),
            NonZero::new(threshold).expect("the threshold is positive"),
        )
        .expect("the threshold is reachable")
    };
    let create = config_op(MsgId::root(), 0);
    let create_tx = encode(vec![Op::ChannelConfig(create.clone())], |_, _, _| {
        OpProof::ChannelMultiSigProof(ChannelMultiSigProof::empty())
    });
    let (state, ..) = execute(fixture.state.clone(), &fixture.config, &create_tx)
        .expect("the channel is created");
    let update = config_op(create.id(), 10);
    let bytes = encode(vec![Op::ChannelConfig(update)], |_, _, hash| {
        OpProof::ChannelMultiSigProof(ChannelMultiSigProof::new(
            IndexedSignatures::try_from_iter(
                keys.iter()
                    .take(usize::from(threshold))
                    .enumerate()
                    .map(|(index, key)| (index as u16, key.sign_payload(hash))),
            )
            .expect("the signatures fit"),
        ))
    });
    OpShape {
        name,
        dims,
        state,
        config: fixture.config.clone(),
        bytes: vec![bytes],
        deferred: Deferred::default(),
    }
}

/// The keys of the key series of `op/config_keys`, at a threshold of one.
pub const CONFIG_KEYS: [u16; 6] = [1, 64, 256, 1024, 4096, 16_384];

/// The key `index` of a channel of the key series, each distinct.
pub fn config_key(index: u16) -> Ed25519Key {
    let mut bytes = [0x5a; 32];
    bytes[..2].copy_from_slice(&index.to_le_bytes());
    Ed25519Key::from_bytes(&bytes)
}

// Two series: the threshold, every key needed; the keys at a threshold of
// one, each of which the node decodes from the transaction, decompresses and
// checks not to be of small order.
fn configs() -> Vec<OpShape> {
    let thresholds = [1u16, 2, 4, 8].map(|threshold| {
        let keys: Vec<Ed25519Key> = (0..threshold)
            .map(|index| Ed25519Key::from_bytes(&[0x50 + index as u8; 32]))
            .collect();
        config_shape(
            "op/config",
            vec![("threshold", u64::from(threshold))],
            ChannelId::from([0x40 + threshold as u8; 32]),
            &keys,
            threshold,
        )
    });
    let key_counts = CONFIG_KEYS.map(|count| {
        let keys: Vec<Ed25519Key> = (0..count).map(config_key).collect();
        config_shape(
            "op/config_keys",
            vec![("keys", u64::from(count)), ("threshold", 1)],
            ChannelId::from([0x80; 32]),
            &keys,
            1,
        )
    });
    thresholds.into_iter().chain(key_counts).collect()
}

/// A deposit of `inputs` notes of the ledger set into `outputs` notes of the
/// fixture channel.
pub fn deposit_op(inputs: usize, outputs: usize, salt: u64) -> DepositOp {
    DepositOp {
        channel_id: filled_state().channel,
        inputs: fresh_nullifiers(inputs, salt),
        commitment_merkle_root: ledger_root(),
        outputs: fresh_commitments(outputs, salt),
        excess_value: 0,
        metadata: Metadata::empty(),
    }
}

fn deposits() -> Vec<OpShape> {
    zk_transfer_series()
        .map(|(inputs, outputs)| {
            OpShape::on_fixture(
                "op/deposit",
                io_dims(inputs, outputs),
                each_round(|salt| {
                    let op = Op::ChannelDeposit(deposit_op(inputs, outputs, salt));
                    encode(vec![op], |index, _, _| {
                        OpProof::ZkTransfer(zk_transfer(index))
                    })
                }),
                Deferred::transfers(1),
            )
        })
        .collect()
}

fn withdrawals() -> Vec<OpShape> {
    let root = channel_root();
    zk_transfer_series()
        .map(|(inputs, outputs)| {
            OpShape::on_fixture(
                "op/withdraw",
                io_dims(inputs, outputs),
                each_round(|salt| {
                    let op = Op::ChannelWithdraw(ChannelWithdrawOp {
                        channel_id: filled_state().channel,
                        inputs: fresh_nullifiers(inputs, salt),
                        commitment_merkle_root: root,
                        outputs: fresh_commitments(outputs, salt),
                    });
                    encode(vec![op], |index, _, _| {
                        OpProof::ZkTransfer(zk_transfer(index))
                    })
                }),
                Deferred::transfers(1),
            )
        })
        .collect()
}

/// The key of the provider of [`declaration`].
pub fn provider_key() -> Ed25519Key {
    Ed25519Key::from_bytes(&[0x61; 32])
}

/// A Blend declaration staking one fresh note of the ledger set of root
/// `commitment_merkle_root`, owned by `zk_id`.
pub fn declaration(zk_id: ZkPublicKey, commitment_merkle_root: Fr) -> SDPDeclareOp {
    declaration_of(&provider_key(), zk_id, commitment_merkle_root, 400)
}

/// A Blend declaration of the provider of `key`, staking the fresh note of
/// salt `salt` of the ledger set of root `commitment_merkle_root`, owned by
/// `zk_id`.
pub fn declaration_of(
    key: &Ed25519Key,
    zk_id: ZkPublicKey,
    commitment_merkle_root: Fr,
    salt: u64,
) -> SDPDeclareOp {
    SDPDeclareOp {
        service_type: ServiceType::BlendNetwork,
        locators: "/ip4/1.1.1.1/udp/3000"
            .parse::<Locator>()
            .expect("the locator is valid")
            .into(),
        provider_id: ProviderId(key.public_key()),
        zk_id,
        note_pk: zk_id,
        note_stake_pk: zk_id,
        inputs: fresh_nullifiers(1, salt),
        commitment_merkle_root,
        amount: 1,
    }
}

/// The key of the provider `index` a setup declares, none of them that of
/// [`provider_key`].
pub fn setup_provider_key(index: usize) -> Ed25519Key {
    let mut bytes = [0x71; 32];
    bytes[..2].copy_from_slice(&u16::try_from(index).expect("the index fits").to_le_bytes());
    Ed25519Key::from_bytes(&bytes)
}

/// The declaration of the provider `index` a setup declares, staking a note of
/// the ledger set of root `commitment_merkle_root`: its key, its zk id and its
/// note are its own.
pub fn setup_declaration(index: usize, commitment_merkle_root: Fr) -> SDPDeclareOp {
    declaration_of(
        &setup_provider_key(index),
        ZkPublicKey::new(Fr::from(0x1_0000 + index as u64)),
        commitment_merkle_root,
        0x2_0000 + index as u64,
    )
}

/// The encoding of `op`, with its `ZkTransfer` and signed by the provider.
pub fn provider_signed(op: Op) -> Vec<u8> {
    signed_by(op, &provider_key())
}

/// The encoding of `op`, with its `ZkTransfer` and signed by `key`.
pub fn signed_by(op: Op, key: &Ed25519Key) -> Vec<u8> {
    encode(vec![op], |index, _, hash| {
        OpProof::ZkAndEd25519Sigs(ZkAndEd25519Proof::new(
            zk_transfer(index),
            key.sign_payload(hash),
        ))
    })
}

// The declarations of the service a declaration is checked to differ from.
const DECLARATIONS: [usize; 4] = [0, 16, 256, 1024];

// The declarations a setup block holds, within the Execution Gas limit of a
// block.
const DECLARATIONS_PER_BLOCK: usize = 128;

// The fixture, then after the blocks declaring the providers between two
// counts of `DECLARATIONS`, each built on the state the one before left: one
// state for each count, built once.
static DECLARED: LazyLock<Vec<LedgerState>> = LazyLock::new(|| {
    let fixture = filled_state();
    let mut states = vec![fixture.state.clone()];
    for counts in DECLARATIONS.windows(2) {
        let mut state = states.last().expect("the fixture").clone();
        for start in (counts[0]..counts[1]).step_by(DECLARATIONS_PER_BLOCK) {
            let root = root_of(&state, NoteSetIndex::Ledger);
            let txs: Vec<Vec<u8>> = (start..counts[1].min(start + DECLARATIONS_PER_BLOCK))
                .map(|index| {
                    signed_by(
                        Op::SDPDeclare(setup_declaration(index, root)),
                        &setup_provider_key(index),
                    )
                })
                .collect();
            (state, ..) = crate::block::execute(state, &fixture.config, &txs)
                .expect("the declarations apply");
        }
        states.push(state);
    }
    states
});

// A declaration among those of `DECLARATIONS` other providers of the service,
// every one of which it is checked to differ from.
fn sdp_declares() -> Vec<OpShape> {
    let fixture = filled_state();
    DECLARATIONS
        .iter()
        .zip(DECLARED.iter())
        .map(|(declarations, state)| {
            let root = root_of(state, NoteSetIndex::Ledger);
            OpShape {
                name: "op/sdp_declare",
                dims: vec![("declarations", *declarations as u64)],
                state: state.clone(),
                config: fixture.config.clone(),
                bytes: each_round(|salt| {
                    provider_signed(Op::SDPDeclare(declaration_of(
                        &provider_key(),
                        ZkPublicKey::new(Fr::from(0x62u64)),
                        root,
                        400 + salt,
                    )))
                }),
                deferred: Deferred::transfers(1),
            }
        })
        .collect()
}

// The withdrawal of a declaration the setup makes.
fn sdp_withdraw() -> OpShape {
    let fixture = filled_state();
    let declare = declaration(ZkPublicKey::new(Fr::from(0x62u64)), ledger_root());
    let (state, ..) = execute(
        fixture.state.clone(),
        &fixture.config,
        &provider_signed(Op::SDPDeclare(declare.clone())),
    )
    .expect("the declaration applies");
    OpShape {
        name: "op/sdp_withdraw",
        dims: Vec::new(),
        state,
        config: fixture.config.clone(),
        bytes: each_round(|salt| {
            provider_signed(Op::SDPWithdraw(SDPWithdrawOp {
                declaration_id: declare.id(),
                service_note_nullifier: nullifiers(1, 401 + salt)[0],
                outputs: fresh_commitments(1, 401 + salt),
                excess_value: 0,
            }))
        }),
        deferred: Deferred::transfers(1),
    }
}

// The block a claim anchors to.
const ANCHOR: HeaderId = [0xc1; 32];

// The fixture has a reward difficulty of 2^-26, which a claim would grind for
// minutes to meet: the claim applies on the fixture note sets under a ledger
// whose genesis difficulty, which is its minimum, is 2^-1, and which saw the
// anchor block.
fn claim_pow() -> OpShape {
    let fixture = filled_state();
    let mut parameters = crate::fixture::parameters(&fixture.config).clone();
    parameters.pow_config.reward.minimum_difficulty = ModulusShift::new::<1>();
    let genesis = Note::new(1, Fr::from(1u64), ZkPublicKey::new(Fr::from(2u64)));
    let state = LedgerState::from_notes([genesis], &parameters, 1_000_000_000)
        .with_gas_prices(&GasPrices::new(0, 0))
        .with_latest_note_sets(fixture.state.latest_note_sets().clone());
    let config = crate::fixture::single_era(parameters);
    let ledger = Ledger::new(ANCHOR, state, Arc::new(config.clone()));
    let state = ledger.state(&ANCHOR).expect("the anchor state").clone();
    let difficulty = state.mantle_ledger().pow.reward_difficulty();
    let claim = (0u64..1024)
        .map(|nonce| ClaimPowRewardOp {
            block_hash: ANCHOR,
            public_key: ZkPublicKey::new(Fr::from(0x63u64)),
            nonce: Fr::from(nonce),
        })
        .find(|claim| claim.get_puzzle_ticket().as_fr() < &difficulty)
        .expect("half of the tickets meet the difficulty");
    OpShape {
        name: "op/claim_pow",
        dims: Vec::new(),
        state,
        config,
        bytes: vec![encode(vec![Op::ClaimPowReward(claim)], |_, _, _| {
            OpProof::None(NoOpProof)
        })],
        deferred: Deferred::default(),
    }
}

// A key the fixture has not registered.
fn program_register() -> OpShape {
    let verification_key =
        ProgramVerificationKey::try_from(&lb_groth16::test_utils::three_input_setup(2).0)
            .expect("the test key is valid");
    OpShape::on_fixture(
        "op/program_register",
        Vec::new(),
        vec![encode(
            vec![Op::ProgramRegister(ProgramRegisterOp { verification_key })],
            |_, _, _| OpProof::None(NoOpProof),
        )],
        Deferred::default(),
    )
}

/// A leadership proof every check accepts, for the headers the setups apply and
/// the measured headers: the verification of a proof of leadership is measured
/// by `micro/pol_verify`. It holds the commitment of the reward note; the value
/// of the note is read by the block contents, which take it on their own.
pub struct AcceptedLeadership(pub NoteCommitment);

impl LeaderProof for AcceptedLeadership {
    fn verify(&self, _public_inputs: &LeaderPublic) -> bool {
        true
    }

    fn verify_genesis(&self) -> bool {
        true
    }

    fn entropy(&self) -> Fr {
        Fr::from(0u64)
    }

    fn reward_commitment(&self) -> NoteCommitment {
        self.0
    }

    fn reward_value(&self) -> Value {
        0
    }
}

/// `state` after a header at the first slot of `epoch` and, when `txs` holds
/// any, a block of them.
pub fn next_epoch(
    state: LedgerState,
    config: &EraScheduledConfig,
    epoch: u64,
    txs: &[Vec<u8>],
) -> LedgerState {
    let leader = crate::block::leadership();
    let (state, _) = state
        .try_apply_header::<_, HeaderId>(
            Slot::from(epoch * crate::fixture::parameters(config).epoch_length()),
            &leader,
            &UncleSlots::default(),
            config,
        )
        .expect("the header applies");
    if txs.is_empty() {
        return state;
    }
    crate::block::execute(state, config, txs)
        .expect("the block applies")
        .0
}

// The activity of the only provider of the snapshot `target` holds, for its
// epoch, submitted during `current`: a real proof of quota and selection,
// the release index ground until its blending token counts.
fn activity_proof(
    zk_key: &ZkKey,
    target: &EpochState,
    current: &EpochState,
    config: &EraScheduledConfig,
) -> ActivityProof {
    let blend = &crate::fixture::parameters(config)
        .sdp_config
        .service_rewards_params
        .blend;
    let zk_id = kdf_public_key(zk_key.as_fr());
    let tree = MerkleTree::new(vec![zk_id.into_inner()]).expect("one provider");
    let core_path_and_selectors = tree
        .get_proof_for_key(&zk_id.into_inner())
        .expect("the provider is in the tree");
    let quota = core_quota(
        blend.rounds_per_epoch,
        blend.message_frequency_per_round,
        blend.num_blend_layers,
        1,
    );
    let layers = blend.num_blend_layers.get();
    let message_quota = Quota::try_new(layers + layers * blend.data_replication_factor)
        .expect("the leader quota fits the circuit");
    let public_inputs = PublicInputs {
        signing_key: ephemeral_key().public_key().into_inner(),
        core: CoreInputs {
            zk_root: tree.root(),
            quota,
        },
        leader: LeaderInputs {
            pol_eligible_aged: target.eligible_root(),
            pol_epoch_nonce: *target.nonce(),
            message_quota,
            lottery_0: target.lottery_0,
            lottery_1: target.lottery_1,
        },
        pow: PowInputs {
            pow_blend_difficulty: target.blend_pow_difficulty,
            pow_quota: message_quota,
        },
    };
    let evaluation = BlendingTokenEvaluation::new(quota, 1, blend.activity_threshold_sensitivity)
        .expect("the evaluation parameters are valid");
    public_inputs
        .core
        .quota
        .values_range()
        .find_map(|message_release_index| {
            let private_inputs = PrivateInputs::new_proof_of_core_quota_inputs(
                message_release_index,
                ProofOfCoreQuotaInputs {
                    core_path_and_selectors,
                    core_sk: *zk_key.as_fr(),
                },
            );
            let (proof_of_quota, randomness) =
                VerifiedProofOfQuota::new(&public_inputs, private_inputs).ok()?;
            let proof_of_selection = VerifiedProofOfSelection::new(randomness);
            let token = BlendingToken::new(
                ephemeral_key().public_key(),
                proof_of_quota,
                proof_of_selection,
            );
            evaluation
                .evaluate(&token, (*current.nonce()).into())
                .map(|_| ActivityProof {
                    epoch: target.epoch(),
                    signing_key: ephemeral_key().public_key(),
                    proof_of_quota: proof_of_quota.into(),
                    proof_of_selection: proof_of_selection.into(),
                })
        })
        .expect("a release index yields a counted token")
}

fn ephemeral_key() -> Ed25519Key {
    Ed25519Key::from_bytes(&[0x64; 32])
}

// The activity of a provider declared at epoch 3, for epoch 5, submitted at
// the first slot of epoch 6, as in the ledger: the declaration is in the
// snapshot of epoch 5 and the rewards of epoch 6 target it.
fn sdp_active() -> OpShape {
    let fixture = filled_state();
    let config = &fixture.config;
    let zk_key = ZkKey::new(Fr::from(0x65u64));
    let mut state = fixture.state.clone();
    for epoch in 1..=2 {
        state = next_epoch(state, config, epoch, &[]);
    }
    let declare = declaration(
        kdf_public_key(zk_key.as_fr()),
        root_of(&state, NoteSetIndex::Ledger),
    );
    state = next_epoch(
        state,
        config,
        3,
        &[provider_signed(Op::SDPDeclare(declare.clone()))],
    );
    for epoch in 4..=5 {
        state = next_epoch(state, config, epoch, &[]);
    }
    let target = state.epoch_state().clone();
    let state = next_epoch(state, config, 6, &[]);
    let active = SDPActiveOp {
        declaration_id: declare.id(),
        nonce: Nonce::new(Epoch::new(3), 1),
        metadata: ActivityMetadata::Blend(Box::new(activity_proof(
            &zk_key,
            &target,
            state.epoch_state(),
            config,
        ))),
    };
    OpShape {
        name: "op/sdp_active",
        dims: Vec::new(),
        state,
        config: config.clone(),
        bytes: vec![encode(vec![Op::SDPActive(active)], |_, _, hash| {
            OpProof::Ed25519Sig(provider_key().sign_payload(hash))
        })],
        deferred: Deferred::default(),
    }
}
