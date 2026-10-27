//! End-to-end cases: one Mantle Transaction per Operation shape, from its
//! encoded bytes to the applied state, its proofs deferred.
//!
//! Each case measures what the node runs on a transaction it receives:
//! decoding it, its preverification (transaction hash, signatures, the
//! stateless checks of each Operation) and its application to the filled
//! ledger, which defers the ZK proofs. The batch verification of the deferred
//! proofs is measured by the micro cases.

use std::sync::LazyLock;

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
        Note, Op, OpProof, SignedOps,
        batch::DeferredProofs,
        channel::{SlotTimeframe, SlotTimeout},
        gas::MainnetGasProfile,
        ledger::verification_mode::StandardMode,
        note_set::{LEDGER_SET, NoteSetIndex},
        notes::{InputNullifiers, NoteCommitment, OutputCommitments},
        ops::{
            ChannelInscribeProof, NoOpProof, ProgramNoteOpening, ProgramProof, ProgramStepProof,
            StepProof, ZkAndEd25519Proof, ZkProgramSpendProof, ZkTransferProof,
            channel::{
                ChannelId, MsgId, VerifiedChannelKeys,
                config::ChannelConfigOp,
                deposit::{DepositOp, Metadata},
                inscribe::{ChannelStep, ChannelSteps, Inscription, InscriptionOp, Step},
                program_step::{Payment, ProgramOutputs, ProgramStep},
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
    sdp::{ActivityMetadata, Locator, ProviderId, ServiceType, blend::ActivityProof},
};
use lb_cryptarchia_engine::{Slot, UncleSlots};
use lb_groth16::{CompressedGroth16Proof, Fr};
use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey, ZkPublicKey};
use logos_blockchain_ledger::{
    Config, EpochState, GasAndFees, Ledger, LedgerState, config::ModulusShift,
};

use crate::{
    cycles::Case,
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

/// One encoded transaction, the state and the config it applies to, and the
/// proofs it defers.
pub struct OpShape {
    pub name: &'static str,
    pub dims: Vec<(&'static str, u64)>,
    pub state: LedgerState,
    pub config: Config,
    pub bytes: Vec<u8>,
    pub deferred: Deferred,
}

impl OpShape {
    fn on_fixture(
        name: &'static str,
        dims: Vec<(&'static str, u64)>,
        bytes: Vec<u8>,
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
    fn into_case(self) -> Case {
        let Self {
            name,
            dims,
            state,
            config,
            bytes,
            deferred,
        } = self;
        let (.., proofs) = execute(state.clone(), &config, &bytes)
            .unwrap_or_else(|error| panic!("{name} {dims:?}: {error}"));
        assert_eq!(Deferred::of(&proofs), deferred, "{name} {dims:?}");
        Case {
            name,
            dims,
            run: Box::new(move |counter| {
                counter.measure(
                    || (state.clone(), bytes.clone()),
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
pub fn execute(state: LedgerState, config: &Config, bytes: &[u8]) -> Result<Applied, String> {
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
    root_of(&filled_state().state, LEDGER_SET)
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
                transfer_tx(inputs, outputs, 0),
                Deferred::transfers(1),
            )
        })
        .collect()
}

/// A user step of the fixture channel, spending `inputs` and creating
/// `outputs` fresh notes against the current root of its set.
pub fn user_step(inputs: usize, outputs: usize, buffered: u16, salt: u64) -> Step {
    Step::User(ChannelStep {
        inputs: fresh_nullifiers(inputs, salt),
        outputs: fresh_commitments(outputs, salt),
        commitment_merkle_root: channel_root(),
        buffered,
    })
}

/// An inscription on the fixture channel after `parent`.
pub fn inscription(parent: MsgId, steps: Vec<Step>, payload: usize) -> InscriptionOp {
    let fixture = filled_state();
    InscriptionOp {
        channel_id: fixture.channel,
        inscription: Inscription::try_from(vec![0xa5; payload]).expect("the payload fits"),
        parent,
        signer: fixture.channel_key.public_key().into_unverified(),
        steps: ChannelSteps::try_from(steps).expect("the steps fit"),
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

fn user_inscription_shape(steps: Vec<Step>, buffered: u16, payload: usize) -> OpShape {
    let user_steps = steps.len();
    let outputs: usize = steps.iter().map(|step| step.outputs().len()).sum();
    let step_proofs: Vec<StepProof> = (0..user_steps)
        .map(|index| StepProof::User(zk_transfer(index)))
        .collect();
    let op = inscription(channel_tip(), steps, payload);
    OpShape::on_fixture(
        "op/inscribe",
        vec![
            ("user_steps", user_steps as u64),
            ("outputs", outputs as u64),
            ("buffered", u64::from(buffered)),
            ("payload", payload as u64),
        ],
        encode_inscription(op, &step_proofs),
        Deferred::transfers(user_steps),
    )
}

// Three series, each step spending one note: the user steps of one output at
// no buffered output and no payload; the buffered outputs of a second step of
// one output, the first creating the eight outputs it covers; the payload of
// one step of one output. The outputs of all the steps are a dimension, the
// buffered series being the only one where they differ from the steps.
fn inscriptions() -> Vec<OpShape> {
    let mut shapes = Vec::new();
    for user_steps in [0, 1, 2, 4, 8] {
        let steps = (0..user_steps)
            .map(|index| user_step(1, 1, 0, 100 + index))
            .collect();
        shapes.push(user_inscription_shape(steps, 0, 0));
    }
    for buffered in [0, 1, 4, 8] {
        let steps = vec![user_step(1, 8, 0, 200), user_step(1, 1, buffered, 201)];
        shapes.push(user_inscription_shape(steps, buffered, 0));
    }
    for payload in [256, 1024, 4096, 16_384] {
        shapes.push(user_inscription_shape(
            vec![user_step(1, 1, 0, 300)],
            0,
            payload,
        ));
    }
    shapes
}

/// The shape of a program step of the fixture program.
#[derive(Clone, Copy, Debug)]
pub struct ProgramShape {
    pub payments: usize,
    pub inputs: usize,
    pub outputs: usize,
    pub openings: usize,
}

/// A program step of `shape` and its proof: each payment spends one fresh
/// note and returns one change note; the first `openings` outputs are notes
/// of the program, derived from their openings.
pub fn program_step(shape: ProgramShape, salt: u64) -> (Step, StepProof) {
    let fixture = filled_state();
    let root = channel_root();
    let payments = (0..shape.payments)
        .map(|index| Payment {
            inputs: fresh_nullifiers(1, salt * 64 + index as u64),
            outputs: fresh_commitments(1, salt * 64 + index as u64),
            commitment_merkle_root: root,
            buffered: 0,
            paid_value: 1,
            intent: Fr::from(index as u64),
        })
        .collect::<Vec<_>>();
    let openings: Vec<ProgramNoteOpening> = (0..shape.openings)
        .map(|index| ProgramNoteOpening {
            value: 1 + index as u64,
            nonce: Fr::from(salt * 64 + index as u64 + 1),
            public_key: ZkPublicKey::new(Fr::from(5u64)),
            data: Fr::from(index as u64),
        })
        .collect();
    let program_notes = openings
        .iter()
        .map(|opening| opening.note(fixture.program).commitment());
    let user_notes = commitments(shape.outputs - shape.openings, salt * 64 + 32);
    let outputs: Vec<NoteCommitment> = program_notes.chain(user_notes).collect();
    let flags = u8::try_from((1u16 << shape.openings) - 1).expect("at most eight outputs");
    let step = ProgramStep {
        program_id: fixture.program,
        payments: payments.try_into().expect("the payments fit"),
        inputs: fresh_nullifiers(shape.inputs, salt * 64 + 48),
        outputs: outputs.try_into().expect("the outputs fit"),
        program_outputs: ProgramOutputs::from(flags),
        commitment_merkle_root: root,
        spend_notes_hash: Fr::from(7u64),
    };
    let proof = StepProof::Program(ProgramStepProof {
        payment_proofs: (0..shape.payments).map(zk_transfer).collect(),
        spend_proof: ZkProgramSpendProof::new(proof(1)),
        program_proof: ProgramProof::new(proof(2)),
        program_notes: openings,
    });
    (Step::Program(step), proof)
}

// Four series, one dimension varying at a time from no payment, one program
// input, one output and no opening; the openings vary at eight outputs, the
// most a step can open.
fn program_inscriptions() -> Vec<OpShape> {
    let base = ProgramShape {
        payments: 0,
        inputs: 1,
        outputs: 1,
        openings: 0,
    };
    let shapes = [0, 1, 2, 4, 8]
        .map(|payments| ProgramShape { payments, ..base })
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
        }));
    shapes
        .map(|shape| {
            let (step, proof) = program_step(shape, 1);
            let op = inscription(channel_tip(), vec![step], 0);
            OpShape::on_fixture(
                "op/inscribe_program",
                vec![
                    ("payments", shape.payments as u64),
                    ("program_inputs", shape.inputs as u64),
                    ("program_outputs", shape.outputs as u64),
                    ("openings", shape.openings as u64),
                ],
                encode_inscription(op, &[proof]),
                Deferred {
                    zk_transfers: shape.payments,
                    program_spends: 1,
                    program_proofs: 1,
                },
            )
        })
        .collect()
}

// A channel of `threshold` accredited keys, all needed to configure it: it is
// created by a first configuration, and the measured one replaces it, signed
// by every key.
fn config_shape(threshold: u16) -> OpShape {
    let fixture = filled_state();
    let keys: Vec<Ed25519Key> = (0..threshold)
        .map(|index| Ed25519Key::from_bytes(&[0x50 + index as u8; 32]))
        .collect();
    let channel = ChannelId::from([0x40 + threshold as u8; 32]);
    let config_op = |parent, posting_timeframe: u32| ChannelConfigOp {
        channel,
        parent,
        keys: VerifiedChannelKeys::try_from(
            keys.iter().map(Ed25519Key::public_key).collect::<Vec<_>>(),
        )
        .expect("the keys fit"),
        posting_timeframe: SlotTimeframe::from(posting_timeframe),
        posting_timeout: SlotTimeout::from(0u32),
        configuration_threshold: threshold,
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
                    .enumerate()
                    .map(|(index, key)| (index as u16, key.sign_payload(hash))),
            )
            .expect("the signatures fit"),
        ))
    });
    OpShape {
        name: "op/config",
        dims: vec![("threshold", u64::from(threshold))],
        state,
        config: fixture.config.clone(),
        bytes,
        deferred: Deferred::default(),
    }
}

fn configs() -> Vec<OpShape> {
    [1, 2, 4, 8].map(config_shape).into()
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
            let op = Op::ChannelDeposit(deposit_op(inputs, outputs, 0));
            OpShape::on_fixture(
                "op/deposit",
                io_dims(inputs, outputs),
                encode(vec![op], |index, _, _| {
                    OpProof::ZkTransfer(zk_transfer(index))
                }),
                Deferred::transfers(1),
            )
        })
        .collect()
}

fn withdrawals() -> Vec<OpShape> {
    zk_transfer_series()
        .map(|(inputs, outputs)| {
            let op = Op::ChannelWithdraw(ChannelWithdrawOp {
                channel_id: filled_state().channel,
                inputs: fresh_nullifiers(inputs, 0),
                commitment_merkle_root: channel_root(),
                outputs: fresh_commitments(outputs, 0),
            });
            OpShape::on_fixture(
                "op/withdraw",
                io_dims(inputs, outputs),
                encode(vec![op], |index, _, _| {
                    OpProof::ZkTransfer(zk_transfer(index))
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

// The fixture, then after each block declaring the providers between two
// counts of `DECLARATIONS`: one state for each count, built once.
static DECLARED: LazyLock<Vec<LedgerState>> = LazyLock::new(|| {
    let fixture = filled_state();
    let mut states = vec![fixture.state.clone()];
    for counts in DECLARATIONS.windows(2) {
        let state = states.last().expect("the fixture").clone();
        let root = root_of(&state, LEDGER_SET);
        let txs: Vec<Vec<u8>> = (counts[0]..counts[1])
            .map(|index| {
                signed_by(
                    Op::SDPDeclare(setup_declaration(index, root)),
                    &setup_provider_key(index),
                )
            })
            .collect();
        let (state, ..) =
            crate::block::execute(state, &fixture.config, &txs).expect("the declarations apply");
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
            let op = Op::SDPDeclare(declaration(
                ZkPublicKey::new(Fr::from(0x62u64)),
                root_of(state, LEDGER_SET),
            ));
            OpShape {
                name: "op/sdp_declare",
                dims: vec![("declarations", *declarations as u64)],
                state: state.clone(),
                config: fixture.config.clone(),
                bytes: provider_signed(op),
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
    let withdraw = SDPWithdrawOp {
        declaration_id: declare.id(),
        nonce: 1,
        service_note_nullifier: nullifiers(1, 401)[0],
        outputs: fresh_commitments(1, 401),
        excess_value: 0,
    };
    OpShape {
        name: "op/sdp_withdraw",
        dims: Vec::new(),
        state,
        config: fixture.config.clone(),
        bytes: provider_signed(Op::SDPWithdraw(withdraw)),
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
    let mut config = fixture.config.clone();
    config.pow_config.reward.minimum_difficulty = ModulusShift::new::<1>();
    let genesis = Note::new(1, Fr::from(1u64), ZkPublicKey::new(Fr::from(2u64)));
    let state = LedgerState::from_notes([genesis], &config)
        .with_gas_prices(&GasPrices::new(0, 0))
        .with_latest_note_sets(fixture.state.latest_note_sets().clone());
    let ledger = Ledger::new(ANCHOR, state, config.clone());
    let state = ledger.state(&ANCHOR).expect("the anchor state").clone();
    let difficulty = state.mantle_ledger().pow.reward_difficulty();
    let claim = (0u64..1024)
        .map(|nonce| ClaimPowRewardOp {
            epoch_nonce: *state.epoch_state().nonce(),
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
        bytes: encode(vec![Op::ClaimPowReward(claim)], |_, _, _| {
            OpProof::None(NoOpProof)
        }),
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
        encode(
            vec![Op::ProgramRegister(ProgramRegisterOp { verification_key })],
            |_, _, _| OpProof::None(NoOpProof),
        ),
        Deferred::default(),
    )
}

/// A leadership proof every check accepts, for the headers the setups apply and
/// the measured headers: the verification of a proof of leadership is measured
/// by `micro/pol_verify`.
pub struct AcceptedLeadership(pub ZkPublicKey);

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

    fn reward_key(&self) -> &ZkPublicKey {
        &self.0
    }
}

/// `state` after a header at the first slot of `epoch` and, when `txs` holds
/// any, a block of them.
pub fn next_epoch(state: LedgerState, config: &Config, epoch: u64, txs: &[Vec<u8>]) -> LedgerState {
    let leader = AcceptedLeadership(ZkPublicKey::new(Fr::from(3u64)));
    let (state, _) = state
        .try_apply_header::<_, HeaderId>(
            Slot::from(epoch * config.epoch_length()),
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
    config: &Config,
) -> ActivityProof {
    let blend = &config.sdp_config.service_rewards_params.blend;
    let zk_id = zk_key.to_public_key();
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
    let declare = declaration(zk_key.to_public_key(), root_of(&state, LEDGER_SET));
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
        nonce: 1,
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
        bytes: encode(vec![Op::SDPActive(active)], |_, _, hash| {
            OpProof::Ed25519Sig(provider_key().sign_payload(hash))
        }),
        deferred: Deferred::default(),
    }
}
