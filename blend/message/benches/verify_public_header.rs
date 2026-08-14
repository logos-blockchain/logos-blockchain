//! Blend public-header verification benchmarks.
//!
//! Run with `cargo bench -p logos-blockchain-blend-message --bench
//! verify_public_header`.
//!
//! Measures the cost of verifying an incoming message's public header in three
//! slices: the signature check alone (what nodes run immediately today, with
//! the `PoQ` check deferred to a later stage), the deferred Proof of Quota
//! check alone, and the complete header verification performing both, as done
//! by `EncapsulatedMessage::verify_public_header`.
//!
//! The fixture is a real single-layer message (deployment configurations use
//! `num_blend_layers: 1`): a genuine Ed25519 signature over the encapsulated
//! part and a genuine core-branch Proof of Quota bound to the header's
//! ephemeral signing key. All witness data is generated here — a fresh core
//! ZK key set, the membership Merkle tree built over it, and fresh chain
//! values — rather than reusing the reference test vectors shipped with the
//! proof crates. Proving happens once up front, so expect a short idle period
//! before the first benchmark.

use std::sync::LazyLock;

use divan::counter::ItemsCount;
use lb_blend_crypto::{ZkHash, merkle::MerkleTree};
use lb_blend_proofs::{
    quota::{
        KeyIndex, Quota, VerifiedProofOfQuota,
        inputs::prove::{
            PrivateInputs, PublicInputs,
            private::ProofOfCoreQuotaInputs,
            public::{CoreInputs, LeaderInputs, PowInputs},
        },
    },
    selection::{PROOF_OF_SELECTION_SIZE, VerifiedProofOfSelection},
};
use lb_key_management_system_keys::keys::{UnsecuredEd25519Key, UnsecuredZkKey};
use logos_blockchain_blend_message::{
    PaddedPayloadBody, PayloadType,
    crypto::proofs::{PoQVerificationInputsMinusSigningKey, RealProofsVerifier},
    encap::{
        ProofsVerifier as _, encapsulated::EncapsulatedMessage,
        validated::EncapsulatedMessageWithVerifiedPublicHeader,
    },
    input::EncapsulationInput,
};

/// Number of blending headers every wire message carries. Deployment
/// configurations currently use a single layer.
const NUM_BLEND_LAYERS: usize = 1;

/// Quota advertised by the proof fixture; the proven key index must stay below
/// it.
const QUOTA: Quota = Quota::new::<1>();

/// Index of the proven key within the advertised quota.
const KEY_INDEX: KeyIndex = KeyIndex::new::<0>();

/// Size of the freshly generated core-node key set the membership Merkle tree
/// is built over. The prover is one member among several, so its Merkle path
/// mixes left and right siblings instead of the degenerate single-leaf shape.
const CORE_NODES: u64 = 8;

/// Timing samples per benchmark (one iteration each). At this count the 95%
/// confidence interval of the median is bounded by order statistics ~469..=531
/// — roughly a third the width divan's auto-chosen default of 100 gives. How
/// much wall-clock that buys depends on the per-sample spread, which differs
/// sharply between these benchmarks: the signature check is tight, while the
/// `PoQ` checks spread over a few hundred microseconds and stay correspondingly
/// looser. Override ad hoc with `--sample-count <N>`.
const SAMPLE_COUNT: u32 = 1000;

/// A deterministic full-width (~254-bit) field element: the Poseidon image of
/// the seed, via the ZK key derivation already in scope.
fn full_width_value(seed: u64) -> ZkHash {
    UnsecuredZkKey::new(ZkHash::from(seed))
        .to_public_key()
        .into_inner()
}

/// A message whose public header carries a genuine signature and a genuine
/// core-branch `PoQ` over freshly generated witness data, plus the verifier
/// configured with the matching epoch inputs.
static FIXTURE: LazyLock<(EncapsulatedMessage, RealProofsVerifier)> = LazyLock::new(|| {
    // The ephemeral identity signing the header; the `PoQ` is bound to its
    // public key.
    let ephemeral_signing_key = UnsecuredEd25519Key::from_bytes(&[7u8; 32]);

    // A fresh core-node registry: distinct ZK keys and the membership Merkle
    // tree over their public keys, exactly as membership builds it.
    let mut zk_keys: Vec<(UnsecuredZkKey, ZkHash)> = (1..=CORE_NODES)
        .map(|seed| {
            let secret_key = UnsecuredZkKey::new(ZkHash::from(seed));
            let public_key = secret_key.to_public_key().into_inner();
            (secret_key, public_key)
        })
        .collect();
    let merkle_tree = MerkleTree::new(zk_keys.iter().map(|(_, pk)| *pk).collect())
        .expect("fresh key set is non-empty, deduplicated and within capacity");
    let (prover_secret_key, prover_public_key) = zk_keys.remove(0);

    let public_inputs = PublicInputs {
        signing_key: *ephemeral_signing_key.public_key().as_inner(),
        core: CoreInputs {
            quota: QUOTA,
            zk_root: merkle_tree.root(),
        },
        // The leader branch is inactive in a core-quota proof; its chain values
        // are ordinary public inputs and only need to be consistent between
        // proving and verification. They are full-width field elements
        // (Poseidon images of fixed seeds) because the verifier's public-input
        // MSM does measurably less work on small scalars, which would make the
        // benchmark optimistic compared to production values.
        leader: LeaderInputs {
            pol_epoch_nonce: full_width_value(1001),
            pol_ledger_aged: full_width_value(1002),
            message_quota: Quota::ONE,
            lottery_0: full_width_value(1003),
            lottery_1: full_width_value(1004),
        },
        pow: PowInputs::disabled(),
    };
    let private_inputs = ProofOfCoreQuotaInputs {
        core_sk: prover_secret_key.into_inner(),
        core_path_and_selectors: merkle_tree
            .get_proof_for_key(&prover_public_key)
            .expect("prover key is a member of the tree"),
    };
    let (proof_of_quota, _) = VerifiedProofOfQuota::new(
        &public_inputs,
        PrivateInputs::new_proof_of_core_quota_inputs(KEY_INDEX, private_inputs),
    )
    .expect("freshly generated witness is valid");

    // The `PoSel` is carried in the private header and plays no role in
    // public-header verification, so an unchecked stand-in is enough.
    let input = EncapsulationInput::try_new(
        ephemeral_signing_key,
        &UnsecuredEd25519Key::from_bytes(&[9u8; 32]).public_key(),
        proof_of_quota,
        VerifiedProofOfSelection::from_bytes_unchecked([0u8; PROOF_OF_SELECTION_SIZE]),
    )
    .expect("shared secret derivation succeeds for the fixture keys");

    let message: EncapsulatedMessage = EncapsulatedMessageWithVerifiedPublicHeader::try_new(
        &[input],
        PayloadType::BlockProposal,
        PaddedPayloadBody::try_from(b"public header verification benchmark".to_vec())
            .expect("body fits in the payload"),
        NUM_BLEND_LAYERS,
    )
    .expect("fixture message is valid")
    .into();

    let verifier = RealProofsVerifier::new(PoQVerificationInputsMinusSigningKey {
        core: public_inputs.core,
        leader: public_inputs.leader,
        pow: public_inputs.pow,
    });

    // The benches below only unwrap; fail fast here if the fixture is broken.
    message
        .clone()
        .verify_public_header(&verifier)
        .expect("fixture message must pass complete header verification");

    (message, verifier)
});

fn main() {
    divan::main();
}

/// The signature check alone: the slice of header verification nodes run
/// immediately today, with the `PoQ` check deferred.
#[divan::bench(sample_count = SAMPLE_COUNT, sample_size = 1)]
fn bench_verify_header_signature(bencher: divan::Bencher) {
    let (message, _) = &*FIXTURE;
    bencher
        .counter(ItemsCount::new(1usize))
        .with_inputs(|| message.clone())
        .bench_values(|message| divan::black_box(message.verify_header_signature()).unwrap());
}

/// The deferred `PoQ` check alone, starting from a signature-verified message.
#[divan::bench(sample_count = SAMPLE_COUNT, sample_size = 1)]
fn bench_verify_proof_of_quota(bencher: divan::Bencher) {
    let (message, verifier) = &*FIXTURE;
    let signature_verified = message
        .clone()
        .verify_header_signature()
        .expect("fixture signature is valid");
    bencher
        .counter(ItemsCount::new(1usize))
        .with_inputs(|| signature_verified.clone())
        .bench_values(|message| divan::black_box(message.verify_proof_of_quota(verifier)).unwrap());
}

/// Complete public-header verification — signature plus `PoQ` in one call: the
/// per-message cost once the `PoQ` check is no longer deferred.
#[divan::bench(sample_count = SAMPLE_COUNT, sample_size = 1)]
fn bench_verify_public_header_complete(bencher: divan::Bencher) {
    let (message, verifier) = &*FIXTURE;
    bencher
        .counter(ItemsCount::new(1usize))
        .with_inputs(|| message.clone())
        .bench_values(|message| divan::black_box(message.verify_public_header(verifier)).unwrap());
}
