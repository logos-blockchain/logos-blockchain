//! Micro-benchmarks of the primitives the ledger runs: trees, hashes,
//! decoding, point decompression and the Groth16 verifiers.

use std::{array, sync::LazyLock};

use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode as _};
use lb_core::{
    block::BlockNumber,
    crypto::{Digest as _, Hasher, ZkDigest as _, ZkHasher},
    mantle::{
        Op, OpProof, SignedOps,
        batch::{DeferredProofs, DeferredZkProgramSpend},
        gas::MainnetGasProfile,
        ledger::verification_mode::StandardMode,
        note_set::{LEDGER_SET, NoteSet, NoteSets, RECENT_ROOTS_WINDOW, TxRoots, eligible_leaf},
        notes::{InputNullifiers, OutputCommitments},
        ops::{
            ChannelInscribeProof, StepProof,
            channel::{
                MsgId,
                inscribe::{ChannelSteps, Inscription, InscriptionOp, Step},
                program_step::{Payment, ProgramOutputs, ProgramStep},
            },
        },
        programs::RegisteredProgram,
        traits::Hashable as _,
        transactions::{OpProofs, Ops, states::Unverified},
    },
};
use lb_groth16::{
    AdditiveGroup as _, CompressedGroth16Proof, Field as _, Fr, Groth16Input, Groth16Proof,
    Groth16VerificationKey, fr_from_mod_bytes, groth16_batch_verify,
    test_utils::{three_input_prove, three_input_setup},
};
use lb_pol::PolVerifierInput;
use lb_zktransfer::ZkTransferVerifierInput;
use rand::{Rng as _, SeedableRng as _, rngs::StdRng};

use crate::{
    cycles::{Case, Counter},
    fixture::{HeaderId, commitments, filled_state, nullifiers, program_key, signed},
};

const BATCH_SIZES: [u64; 9] = [1, 2, 4, 8, 16, 32, 64, 128, 256];
const BUFFERED: [u64; 7] = [0, 1, 2, 4, 8, 16, 32];
const HASHED_BYTES: [u64; 7] = [0, 64, 256, 1024, 4096, 16384, 65536];
const INSCRIBED_BYTES: [u64; 6] = [0, 256, 1024, 4096, 16384, 65536];
const MMR_MERGES: [u64; 6] = [0, 1, 2, 4, 8, 12];
// The runs of each side of a difference, of which the cheapest is kept.
const DIFFERENCE_RUNS: usize = 5;
// The block the ledger set is read and closed at, after the block the fixture
// closed.
const BLOCK: u64 = 1;
/// The recent roots of the ledger set when a block changing it closes: one, a
/// few hundred and the whole window a live chain holds.
pub const RECENT_ROOTS: [u64; 3] = [1, 256, RECENT_ROOTS_WINDOW];

static RECENT_ROOT_SETS: LazyLock<Vec<(NoteSets, Vec<Fr>)>> =
    LazyLock::new(|| RECENT_ROOTS.map(with_recent_roots).into());

// The note sets of the fixture after `recent_roots` blocks changing the ledger
// set, and the roots of the ledger set they superseded, oldest first. Each
// block adds one commitment, the first `RECENT_ROOTS_WINDOW - recent_roots`
// more, so that the ledger set ends at one length for every count and its root
// hashes as many peaks. The blocks are numbered from the one after the window,
// at which the roots superseded by the block the fixture closed have expired.
fn with_recent_roots(recent_roots: u64) -> (NoteSets, Vec<Fr>) {
    let mut sets = filled_state().state.latest_note_sets().clone();
    let mut roots = Vec::new();
    for block in 1..=recent_roots {
        let added = if block == 1 {
            1 + RECENT_ROOTS_WINDOW - recent_roots
        } else {
            1
        };
        let ledger = sets.get(LEDGER_SET).expect("the ledger set exists");
        roots.push(ledger.commitment_root());
        let ledger = ledger
            .clone()
            .execute_adding(&commitments(added as usize, 0x4e00 + block))
            .expect("the commitment MMR has room");
        sets = sets
            .update(LEDGER_SET, ledger)
            .end_block(RECENT_ROOTS_WINDOW + block);
    }
    (sets, roots)
}

/// The note sets after `recent_roots` blocks changing the ledger set, one of
/// [`RECENT_ROOTS`], the `recent_roots` roots of the ledger set they
/// superseded, oldest first, and the number of the last of them.
pub fn recent_roots_sets(recent_roots: u64) -> (NoteSets, Vec<Fr>, BlockNumber) {
    let position = RECENT_ROOTS
        .iter()
        .position(|count| *count == recent_roots)
        .expect("a measured count of recent roots");
    let (sets, roots) = RECENT_ROOT_SETS[position].clone();
    (sets, roots, RECENT_ROOTS_WINDOW + recent_roots)
}

pub fn micro_cases() -> Vec<Case> {
    let mut rng = StdRng::seed_from_u64(0x6d1c);
    let mut cases = Vec::new();
    cases.extend(hash_cases(&mut rng));
    cases.extend(tree_cases());
    cases.extend(decode_cases());
    cases.extend(point_cases());
    cases.push(ed25519_case());
    cases.extend(batch_cases(&mut rng));
    cases.extend(key_cases(&mut rng));
    cases.push(pol_case(&mut rng));
    cases
}

/// The `n` deferred `ZkProgramSpend`s the batch of `n` verifies: a random proof
/// each, against random full-size public inputs.
///
/// Their verifier input is only built by the ledger, so a program step
/// applied to the fixture gives one, every field of which is then replaced.
pub fn program_spends(n: usize) -> Vec<DeferredZkProgramSpend> {
    let mut rng = StdRng::seed_from_u64(0x5be4d);
    PROOFS
        .iter()
        .take(n)
        .map(|proof| {
            let mut inputs = *PROGRAM_SPEND.inputs();
            inputs.inputs = random_inputs(&mut rng);
            inputs.outputs = random_inputs(&mut rng);
            inputs.spend_notes_hash = random_input(&mut rng);
            inputs.inputs_active = random_inputs(&mut rng);
            inputs.outputs_active = random_inputs(&mut rng);
            inputs.outputs_of_program = random_inputs(&mut rng);
            inputs.tx_commitment_root = random_input(&mut rng);
            inputs.program_id = random_input(&mut rng);
            inputs.paid_value = random_input(&mut rng);
            inputs.msg = random_input(&mut rng);
            DeferredZkProgramSpend::new(compressed(proof), inputs)
        })
        .collect()
}

fn case<S, R>(
    name: &'static str,
    dims: Vec<(&'static str, u64)>,
    mut setup: impl FnMut() -> S + 'static,
    mut run: impl FnMut(S) -> R + 'static,
) -> Case {
    Case {
        name,
        dims,
        run: Box::new(move |counter: &mut Counter| counter.measure(&mut setup, &mut run)),
    }
}

// Distinct proofs of valid points, so batches never repeat a point.
static PROOFS: LazyLock<Vec<Groth16Proof>> = LazyLock::new(|| {
    let (_, proving_key) = three_input_setup(1);
    (0..BATCH_SIZES[BATCH_SIZES.len() - 1])
        .map(|seed| three_input_prove(&proving_key, [Fr::from(seed), Fr::ONE, Fr::ONE], seed))
        .collect()
});

static PROGRAM_SPEND: LazyLock<DeferredZkProgramSpend> = LazyLock::new(applied_program_spend);

fn compressed(proof: &Groth16Proof) -> CompressedGroth16Proof {
    CompressedGroth16Proof::try_from(proof).expect("a valid proof compresses")
}

fn random_fr(rng: &mut StdRng) -> Fr {
    fr_from_mod_bytes(&rng.r#gen::<[u8; 32]>())
}

fn random_input(rng: &mut StdRng) -> Groth16Input {
    random_fr(rng).into()
}

fn random_inputs<const N: usize>(rng: &mut StdRng) -> [Groth16Input; N] {
    array::from_fn(|_| random_input(rng))
}

fn ledger_set() -> &'static NoteSet {
    filled_state()
        .state
        .latest_note_sets()
        .get(LEDGER_SET)
        .expect("the ledger set exists")
}

fn hash_cases(rng: &mut StdRng) -> Vec<Case> {
    let pair = [random_fr(rng), random_fr(rng)];
    let four: [Fr; 4] = array::from_fn(|_| random_fr(rng));
    let mut cases = vec![
        case(
            "micro/poseidon2_compress",
            vec![],
            || (),
            move |()| ZkHasher::compress(&pair),
        ),
        case(
            "micro/poseidon2_digest",
            vec![("elements", 4)],
            || (),
            move |()| ZkHasher::digest(&four),
        ),
    ];
    for bytes in HASHED_BYTES {
        let message: Vec<u8> = std::iter::repeat_with(|| rng.r#gen())
            .take(bytes as usize)
            .collect();
        cases.push(case(
            "micro/blake2b",
            vec![("bytes", bytes)],
            || (),
            move |()| Hasher::digest(&message),
        ));
    }
    cases
}

fn tree_cases() -> Vec<Case> {
    let set = ledger_set();
    let nullifier = nullifiers(1, 0x1a7);
    let commitment = commitments(1, 0x33a);
    let absent = nullifier[0];
    let mut cases = vec![
        case(
            "micro/imt_insert",
            vec![],
            || ledger_set().clone(),
            move |set: NoteSet| set.execute_spending(&nullifier),
        ),
        case(
            "micro/imt_contains",
            vec![],
            || (),
            move |()| ledger_set().contains_nullifier(&absent),
        ),
        case(
            "micro/mmr_frontier_root",
            vec![],
            || (),
            |()| ledger_set().commitment_root(),
        ),
    ];

    // A push merges one peak per trailing one of the length of the MMR.
    for merges in MMR_MERGES {
        let len = set.commitments().len();
        let missing = (0..=2usize << merges)
            .find(|added| (len + added).trailing_ones() == merges as u32)
            .expect("a length has every count of trailing ones");
        let pushed = set
            .clone()
            .execute_adding(&commitments(missing, 0x3e6 + merges))
            .expect("the commitment MMR has room");
        let commitment = commitment.clone();
        cases.push(case(
            "micro/mmr_push",
            vec![("merges", merges)],
            move || pushed.clone(),
            move |set: NoteSet| set.execute_adding(&commitment),
        ));
    }

    // The ledger calls the cached variant, here with the cache of a new
    // transaction, which holds nothing.
    let root = set.commitment_root();
    let buffered_set = set
        .clone()
        .buffer_outputs(&commitments(BUFFERED[BUFFERED.len() - 1] as usize, 0xbf));
    for buffered in BUFFERED {
        let buffered_set = buffered_set.clone();
        cases.push(case(
            "micro/step_root",
            vec![("buffered", buffered)],
            TxRoots::default,
            move |tx_roots: TxRoots| {
                let root = buffered_set.cached_step_commitment_root(
                    &tx_roots,
                    &root,
                    buffered as usize,
                    BLOCK,
                );
                (tx_roots, root)
            },
        ));
    }

    // The record of the commitment root, which scans the recent roots to expire
    // the old ones, and the update of the eligible leaf are private to
    // `end_block`, measured whole on one changed set.
    for recent_roots in RECENT_ROOTS {
        let (sets, _, last_block) = recent_roots_sets(recent_roots);
        let changed_set = sets
            .get(LEDGER_SET)
            .expect("the ledger set exists")
            .clone()
            .execute_spending(&nullifiers(1, 0xeb))
            .expect("a fresh nullifier is absent")
            .execute_adding(&commitments(1, 0xeb))
            .expect("the commitment MMR has room");
        let changed = sets.update(LEDGER_SET, changed_set);
        cases.push(case(
            "micro/end_block_one_set",
            vec![("recent_roots", recent_roots)],
            move || changed.clone(),
            move |note_sets: NoteSets| note_sets.end_block(last_block + 1),
        ));
    }
    let (commitment_root, nullifier_root) = (set.commitment_root(), set.nullifier_root());
    cases.push(case(
        "micro/eligible_leaf",
        vec![],
        || (),
        move |()| eligible_leaf(&commitment_root, &nullifier_root),
    ));
    cases
}

fn inscription(payload: usize) -> Op {
    let fixture = filled_state();
    Op::ChannelInscribe(InscriptionOp {
        channel_id: fixture.channel,
        inscription: Inscription::try_new(vec![0xa5; payload]).expect("the payload fits"),
        parent: MsgId::root(),
        signer: fixture.channel_key.public_key().into_unverified(),
        steps: ChannelSteps::default(),
    })
}

fn decode_cases() -> Vec<Case> {
    INSCRIBED_BYTES
        .into_iter()
        .map(|bytes| {
            let encoded = signed(
                vec![inscription(bytes as usize)],
                &filled_state().channel_key,
            )
            .encode_to_vec();
            case(
                "micro/decode_tx",
                vec![("bytes", bytes)],
                || (),
                move |()| {
                    <SignedOps<Unverified, StandardMode> as BinaryDecode>::decode(&encoded, &())
                        .map(|(_, tx)| tx)
                },
            )
        })
        .collect()
}

fn decompress_cycles(counter: &mut Counter, proof: &CompressedGroth16Proof) -> u64 {
    std::iter::repeat_with(|| counter.measure(|| (), |()| Groth16Proof::try_from(proof)))
        .take(DIFFERENCE_RUNS)
        .min()
        .expect("runs")
}

// The cycles of decompressing `more` beyond those of `less`, where both are
// rejected at their first invalid point.
fn decompress_difference(
    name: &'static str,
    more: CompressedGroth16Proof,
    less: CompressedGroth16Proof,
) -> Case {
    Case {
        name,
        dims: vec![],
        run: Box::new(move |counter: &mut Counter| {
            decompress_cycles(counter, &more).saturating_sub(decompress_cycles(counter, &less))
        }),
    }
}

// A proof decompresses its points in order and stops at the first invalid one,
// rejected on its first coordinate being above the modulus, so one point is
// isolated as the difference of two rejected proofs.
fn point_cases() -> Vec<Case> {
    let valid = compressed(&PROOFS[1]).to_bytes();
    let invalid_from = |start: usize| {
        let mut bytes = valid;
        bytes[start..].fill(0xff);
        CompressedGroth16Proof::from_bytes(&bytes)
    };
    let (a_invalid, b_invalid, c_invalid) = (invalid_from(0), invalid_from(32), invalid_from(96));
    for proof in [&a_invalid, &b_invalid, &c_invalid] {
        assert!(
            Groth16Proof::try_from(proof).is_err(),
            "the point is invalid"
        );
    }
    let point = PROOFS[2].pi_b;
    vec![
        decompress_difference("micro/g1_decompress", b_invalid, a_invalid),
        decompress_difference("micro/g2_decompress", c_invalid, b_invalid),
        case(
            "micro/g2_subgroup",
            vec![],
            || (),
            move |()| point.is_in_correct_subgroup_assuming_on_curve(),
        ),
    ]
}

// The inscription preverification checks that the signer is not weak, then
// verifies its signature strictly.
fn ed25519_case() -> Case {
    let fixture = filled_state();
    let message = *Ops::new_unchecked(vec![inscription(1)])
        .hash()
        .as_signing_bytes();
    let signature = fixture.channel_key.sign_payload(&message);
    let signer = fixture.channel_key.public_key().into_unverified();
    case(
        "micro/ed25519_verify",
        vec![],
        || (),
        move |()| (signer.is_weak(), signer.verify(&message, &signature)),
    )
}

// The proofs of a program step are sample proofs, which the ledger defers.
fn applied_program_spend() -> DeferredZkProgramSpend {
    let fixture = filled_state();
    let channel_set = fixture
        .state
        .latest_note_sets()
        .get(fixture.channel_set)
        .expect("the channel set exists");
    let root = channel_set.commitment_root();
    let tip = fixture
        .state
        .mantle_ledger()
        .channels()
        .channel_state(&fixture.channel)
        .expect("the channel exists")
        .tip_message;
    let steps: ChannelSteps = [Step::Program(ProgramStep {
        program_id: fixture.program,
        payments: [Payment {
            inputs: [nullifiers(1, 0x9a)[0]].into(),
            outputs: OutputCommitments::default(),
            commitment_merkle_root: root,
            buffered: 0,
            paid_value: 5,
            intent: Fr::ONE,
        }]
        .into(),
        inputs: InputNullifiers::default(),
        outputs: [commitments(1, 0x9a)[0]].into(),
        program_outputs: ProgramOutputs::default(),
        commitment_merkle_root: root,
        spend_notes_hash: Fr::ZERO,
    })]
    .into();
    let step_proofs = steps
        .iter()
        .map(|step| StepProof::sample_for(step.proof_shape()))
        .collect();
    let ops = Ops::new_unchecked(vec![Op::ChannelInscribe(InscriptionOp {
        channel_id: fixture.channel,
        inscription: [1].into(),
        parent: tip,
        signer: fixture.channel_key.public_key().into_unverified(),
        steps,
    })]);
    let signature = fixture
        .channel_key
        .sign_payload(ops.hash().as_signing_bytes());
    let proofs = OpProofs::new_unchecked(vec![OpProof::ChannelInscribe(
        ChannelInscribeProof::new(signature, step_proofs),
    )]);
    let tx = SignedOps::from_parts(ops, proofs)
        .expect("the transaction builds")
        .preverify()
        .expect("the transaction preverifies");
    let (_, _, _, deferred) = fixture
        .state
        .clone()
        .try_apply_transaction::<_, HeaderId, MainnetGasProfile>(&fixture.config, &tx)
        .expect("the program step applies");
    let spend = &deferred.program_spends()[0];
    DeferredZkProgramSpend::new(*spend.proof(), *spend.inputs())
}

fn zk_transfers(
    n: usize,
    rng: &mut StdRng,
) -> Vec<(CompressedGroth16Proof, ZkTransferVerifierInput)> {
    PROOFS
        .iter()
        .take(n)
        .map(|proof| {
            (
                compressed(proof),
                ZkTransferVerifierInput {
                    inputs: random_inputs(rng),
                    outputs: random_inputs(rng),
                    excess_value: random_input(rng),
                    inputs_active: random_inputs(rng),
                    outputs_active: random_inputs(rng),
                    tx_commitment_root: random_input(rng),
                    msg: random_input(rng),
                },
            )
        })
        .collect()
}

// Random proofs fail the batch, which costs what a passing batch does. The
// fixed keys are prepared once, on their first use, outside the measures.
fn batch_cases(rng: &mut StdRng) -> Vec<Case> {
    drop(lb_zktransfer::batch_verify(&zk_transfers(1, rng)));
    drop(DeferredProofs::from_iter(program_spends(1)).verify());
    let key = three_input_setup(2).0;
    let mut cases = Vec::new();
    for n in BATCH_SIZES {
        let transfers = zk_transfers(n as usize, rng);
        cases.push(case(
            "micro/groth16_batch",
            vec![("N", 27), ("n", n)],
            || (),
            move |()| lb_zktransfer::batch_verify(&transfers),
        ));
        cases.push(case(
            "micro/groth16_batch",
            vec![("N", 37), ("n", n)],
            move || program_spends(n as usize),
            |spends| DeferredProofs::from_iter(spends).verify(),
        ));
        let prepared = key.clone().into_prepared();
        let proofs: Vec<Groth16Proof> = PROOFS.iter().take(n as usize).copied().collect();
        let inputs: Vec<Vec<Fr>> =
            std::iter::repeat_with(|| std::iter::repeat_with(|| random_fr(rng)).take(3).collect())
                .take(n as usize)
                .collect();
        cases.push(case(
            "micro/groth16_batch",
            vec![("N", 3), ("n", n)],
            || (),
            move |()| groth16_batch_verify(&prepared, &proofs, &inputs),
        ));
    }
    cases
}

// A registered program keeps its decompressed key and `e(alpha, beta)`, and
// every verification of its proofs computes the line coefficients of `-gamma`
// and `-delta`.
fn key_cases(rng: &mut StdRng) -> Vec<Case> {
    let fixture = filled_state();
    let program = fixture
        .state
        .mantle_ledger()
        .programs()
        .get(&fixture.program)
        .expect("the program is registered")
        .clone();
    let proof = compressed(&PROOFS[0]);
    let public = [random_fr(rng), random_fr(rng), random_fr(rng)];
    let key = program_key();
    let registered_key = key.clone();
    let groth16_key = key.to_groth16().expect("the test key is valid");
    let alpha_beta = groth16_key.alpha_beta();
    let (pairing_key, lines_key, prepared_key) =
        (groth16_key.clone(), groth16_key.clone(), groth16_key);
    vec![
        case(
            "micro/program_proof_verify",
            vec![],
            || (),
            move |()| program.verify(&proof, &public),
        ),
        case(
            "micro/key_decompress",
            vec![],
            || (),
            move |()| key.to_groth16(),
        ),
        case(
            "micro/program_key_register",
            vec![],
            move || registered_key.clone(),
            RegisteredProgram::new,
        ),
        case(
            "micro/key_e_alpha_beta",
            vec![],
            || (),
            move |()| pairing_key.alpha_beta(),
        ),
        case(
            "micro/key_lines",
            vec![],
            || (),
            move |()| lines_key.prepare_with_alpha_beta(alpha_beta),
        ),
        case(
            "micro/key_prepare",
            vec![],
            move || prepared_key.clone(),
            Groth16VerificationKey::into_prepared,
        ),
    ]
}

fn pol_case(rng: &mut StdRng) -> Case {
    let proof = compressed(&PROOFS[3]);
    let inputs = PolVerifierInput {
        entropy_contribution: random_input(rng),
        slot_number: random_input(rng),
        epoch_nonce: random_input(rng),
        lottery_0: random_input(rng),
        lottery_1: random_input(rng),
        eligible_aged: random_input(rng),
        eligible_latest: random_input(rng),
        msg: random_input(rng),
    };
    drop(lb_pol::verify(&proof, &inputs));
    case(
        "micro/pol_verify",
        vec![],
        || (),
        move |()| lb_pol::verify(&proof, &inputs),
    )
}
