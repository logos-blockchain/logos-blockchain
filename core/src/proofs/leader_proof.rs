use core::fmt::Debug;
use std::sync::LazyLock;

use ark_ff::{AdditiveGroup as _, PrimeField as _};
use lb_binary_codec::{
    bincode::BoundedSerializeOp,
    canonical::{BinaryDecode, BinaryEncode, DecodeError},
};
use lb_dynamic_merkle::{MerkleNode, MerklePath};
use lb_groth16::{COMPRESSED_PROOF_SIZE, FR_BYTES_SIZE, Fr, fr_from_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_log_targets::proofs;
use lb_poseidon2::{Digest as _, Poseidon2Bn254Hasher};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::error;

use crate::{
    crypto::ZkHasher,
    mantle::{
        note_set::{NoteSetIndex, NoteSets},
        notes::{self, NoteNullifier},
    },
};

const LOG_TARGET: &str = proofs::LEADER;

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Groth16LeaderProof {
    #[serde(with = "proof_serde")]
    proof: lb_pol::PoLProof,
    #[serde(with = "serde_fr")]
    entropy_contribution: Fr,
    reward_key: ZkPublicKey,
}

const GROTH16_LEADER_PROOF_BINCODE_SIZE: usize =
    COMPRESSED_PROOF_SIZE + FR_BYTES_SIZE + FR_BYTES_SIZE;

impl BoundedSerializeOp for Groth16LeaderProof {
    type Bytes = [u8; GROTH16_LEADER_PROOF_BINCODE_SIZE];
}

impl Debug for Groth16LeaderProof {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Groth16LeaderProof")
            .field(
                "proof",
                &format_args!("{} bytes", size_of::<lb_pol::PoLProof>()),
            )
            .field("entropy_contribution", &self.entropy_contribution)
            .field("reward_key", &self.reward_key)
            .finish()
    }
}

impl BinaryEncode for Groth16LeaderProof {
    fn encoded_length(&self) -> usize {
        Self::CANONICAL_ENCODED_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.proof.to_bytes());
        self.entropy_contribution.encode_into(out);
        self.reward_key.encode_into(out);
    }
}

impl BinaryDecode for Groth16LeaderProof {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, proof_bytes) = <[u8; COMPRESSED_PROOF_SIZE]>::decode(input, &())?;
        let (rest, entropy_contribution) = Fr::decode(rest, &())?;
        let (rest, reward_key) = ZkPublicKey::decode(rest, &())?;
        Ok((
            rest,
            Self {
                proof: lb_pol::PoLProof::from_bytes(&proof_bytes),
                entropy_contribution,
                reward_key,
            },
        ))
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Proof of leadership failed: {0}")]
    PoLProofFailed(#[from] lb_pol::ProveError),
}

impl Groth16LeaderProof {
    /// The fixed-size canonical representation of a proof of leadership.
    pub const CANONICAL_ENCODED_SIZE: usize = COMPRESSED_PROOF_SIZE + FR_BYTES_SIZE + FR_BYTES_SIZE;

    pub fn prove(witness: LeaderPrivate, reward_key: ZkPublicKey) -> Result<Self, Error> {
        let start_t = std::time::Instant::now();
        let (proof, entropy_contribution) = Self::generate_proof(witness)?;
        tracing::debug!(target: LOG_TARGET, "groth16 prover time: {:.2?}", start_t.elapsed(),);

        Ok(Self {
            proof,
            entropy_contribution,
            reward_key,
        })
    }

    #[must_use]
    pub fn genesis() -> Self {
        Self {
            proof: lb_pol::PoLProof::from_bytes(&[0u8; 128]),
            entropy_contribution: Fr::ZERO,
            reward_key: ZkPublicKey::zero(),
        }
    }

    fn generate_proof(private: LeaderPrivate) -> Result<(lb_pol::PoLProof, Fr), Error> {
        let (proof, verif_inputs) =
            lb_pol::prove(private.input.into()).map_err(Error::PoLProofFailed)?;
        Ok((proof, verif_inputs.entropy_contribution.into_inner()))
    }

    #[must_use]
    pub const fn proof(&self) -> &lb_pol::PoLProof {
        &self.proof
    }

    /// Construct a proof directly from its parts.
    ///
    /// Crate-internal: the resulting proof is not necessarily valid; it is used
    /// to build deterministic reference test vectors and codec fixtures with
    /// distinct per-field values (so a field-transposition bug in another
    /// implementation cannot be masked by shared values).
    #[must_use]
    pub(crate) const fn from_parts(
        proof: lb_pol::PoLProof,
        entropy_contribution: Fr,
        reward_key: ZkPublicKey,
    ) -> Self {
        Self {
            proof,
            entropy_contribution,
            reward_key,
        }
    }
}

pub trait LeaderProof {
    /// Verify the proof against the public inputs.
    fn verify(&self, public_inputs: &LeaderPublic) -> bool;

    fn verify_genesis(&self) -> bool;

    /// Get the entropy used in the proof.
    fn entropy(&self) -> Fr;

    fn reward_key(&self) -> &ZkPublicKey;
}

#[expect(
    clippy::multiple_inherent_impl,
    reason = "the former `LeaderProof` impl, kept in place for a minimal diff"
)]
impl Groth16LeaderProof {
    #[must_use]
    pub fn verify(&self, public_inputs: &LeaderPublic, msg: Fr) -> bool {
        lb_pol::verify(
            &self.proof,
            &lb_pol::PolVerifierInput::new(
                self.entropy(),
                public_inputs.slot,
                public_inputs.epoch_nonce,
                public_inputs.aged_root,
                public_inputs.latest_root,
                public_inputs.lottery_0,
                public_inputs.lottery_1,
                msg,
            ),
        )
        .unwrap_or_else(|e| {
            error!(target: LOG_TARGET, "LeaderProof verification failed: {e:?}");
            false
        })
    }

    #[must_use]
    pub fn verify_genesis(&self) -> bool {
        let expected_genesis = Self::genesis();
        self.proof == expected_genesis.proof
            && self.entropy_contribution == expected_genesis.entropy_contribution
            && self.reward_key == expected_genesis.reward_key
    }

    #[must_use]
    pub const fn entropy(&self) -> Fr {
        self.entropy_contribution
    }

    #[must_use]
    pub const fn reward_key(&self) -> &ZkPublicKey {
        &self.reward_key
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderPublic {
    pub slot: u64,
    #[serde(with = "serde_fr")]
    pub epoch_nonce: Fr,
    #[serde(with = "serde_fr")]
    pub lottery_0: Fr,
    #[serde(with = "serde_fr")]
    pub lottery_1: Fr,
    #[serde(with = "serde_fr")]
    pub aged_root: Fr,
    #[serde(with = "serde_fr")]
    pub latest_root: Fr,
}

/// Check if the given note is owned by the leader and wins the lottery with
/// the given public inputs.
#[must_use]
pub fn check_winning(
    note: &notes::Note,
    public_inputs: LeaderPublic,
    public_key: &ZkPublicKey,
    secret_key: Fr,
) -> bool {
    note.public_key == *public_key
        && public_inputs.check_winning(note.value, note.commitment().0, secret_key)
}

impl LeaderPublic {
    #[must_use]
    pub const fn new(
        aged_root: Fr,
        latest_root: Fr,
        epoch_nonce: Fr,
        slot: u64,
        lottery_0: Fr,
        lottery_1: Fr,
    ) -> Self {
        Self {
            slot,
            epoch_nonce,
            lottery_0,
            lottery_1,
            aged_root,
            latest_root,
        }
    }

    #[must_use]
    pub fn check_winning(&self, value: u64, note_id: Fr, sk: Fr) -> bool {
        let threshold =
            Self::phi_approx(&Fr::from(value), &(self.lottery_0, self.lottery_1)).into_bigint();
        let ticket = Self::ticket(note_id, sk, self.epoch_nonce, Fr::from(self.slot)).into_bigint();
        ticket < threshold
    }

    fn phi_approx(stake: &Fr, approx: &(Fr, Fr)) -> Fr {
        // stake * (t0 + t1 * stake)
        *stake * (approx.0 + (approx.1 * *stake))
    }

    fn ticket(note_id: Fr, sk: Fr, epoch_nonce: Fr, slot: Fr) -> Fr {
        Poseidon2Bn254Hasher::digest(&[*LEAD_V1, epoch_nonce, slot, note_id, sk])
    }
}

static LEAD_V1: LazyLock<Fr> =
    LazyLock::new(|| fr_from_bytes(b"LEAD_V1").expect("BigUint should load from constant string"));

#[derive(Debug, Clone)]
pub struct LeaderPrivate {
    input: lb_pol::PolWitnessInputsData,
}

impl LeaderPrivate {
    #[must_use]
    pub const fn new(public: LeaderPublic, wallet: lb_pol::PolWalletInputsData, msg: Fr) -> Self {
        let chain = lb_pol::PolChainInputsData {
            slot_number: public.slot,
            epoch_nonce: public.epoch_nonce,
            lottery_0: public.lottery_0,
            lottery_1: public.lottery_1,
            eligible_aged: public.aged_root,
            eligible_latest: public.latest_root,
            msg,
        };
        let input = lb_pol::PolWitnessInputsData::from_chain_and_wallet_data(chain, wallet);
        Self { input }
    }

    #[must_use]
    pub const fn input(&self) -> &lb_pol::PolWitnessInputsData {
        &self.input
    }
}

impl From<LeaderPrivate> for lb_pol::PolWitnessInputsData {
    fn from(value: LeaderPrivate) -> Self {
        value.input
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum LeaderWitnessError {
    #[error("Note set {0} is unknown")]
    UnknownNoteSet(NoteSetIndex),
    #[error("The aged path does not lead to the aged commitment root of note set {0}")]
    AgedPathMismatch(NoteSetIndex),
    #[error("The note is spent in the latest note set")]
    Spent,
}

fn nodes_and_selectors(
    path: &MerklePath<Fr, { lb_pol::NOTE_SET_TREE_DEPTH }>,
) -> (
    [Fr; lb_pol::NOTE_SET_TREE_DEPTH],
    [bool; lb_pol::NOTE_SET_TREE_DEPTH],
) {
    (
        core::array::from_fn(|level| *path[level].item()),
        core::array::from_fn(|level| matches!(path[level], MerkleNode::Left(_))),
    )
}

// The wallet half of a PoL witness, secret key left 0 for the KMS to fill.
pub fn pol_wallet_inputs(
    note: &notes::Note,
    note_set: NoteSetIndex,
    nullifier: &NoteNullifier,
    commitment_aged_path: &lb_mmr::MerklePath,
    aged: &NoteSets,
    latest: &NoteSets,
) -> Result<lb_pol::PolWalletInputsData, LeaderWitnessError> {
    let unknown = LeaderWitnessError::UnknownNoteSet(note_set);
    let aged_set = aged.get(note_set).ok_or(unknown)?;
    let latest_set = latest.get(note_set).ok_or(unknown)?;

    let mismatch = LeaderWitnessError::AgedPathMismatch(note_set);
    if !commitment_aged_path.verify::<ZkHasher>(note.commitment().0, aged_set.commitment_root()) {
        return Err(mismatch);
    }
    let commitment_aged_selectors =
        core::array::from_fn(|level| (commitment_aged_path.leaf_index() >> level) & 1 == 1);
    let commitment_aged_path = commitment_aged_path
        .siblings()
        .try_into()
        .map_err(|_| mismatch)?;

    let (low_index, low) = latest_set
        .nullifiers()
        .low_leaf(&nullifier.0)
        .ok_or(LeaderWitnessError::Spent)?;
    let low_path = latest_set
        .nullifiers()
        .path(low_index)
        .expect("the low leaf is in the tree");
    let (low_leaf_path, low_leaf_selectors) = nodes_and_selectors(&low_path);

    // Both eligible trees hold the set at the same index, so the selectors agree.
    let (set_aged_path, set_selectors) =
        nodes_and_selectors(&aged.eligible_path(note_set).ok_or(unknown)?);
    let (set_latest_path, _) = nodes_and_selectors(&latest.eligible_path(note_set).ok_or(unknown)?);

    Ok(lb_pol::PolWalletInputsData {
        note_value: note.value,
        note_nonce: note.nonce,
        set_selectors,
        commitment_aged_path,
        commitment_aged_selectors,
        nullifier_root_aged: aged_set.nullifier_root(),
        set_aged_path,
        low_leaf_nullifier: low.nullifier,
        low_leaf_next_nullifier: low.next_nullifier,
        low_leaf_next_index: low.next_index,
        low_leaf_path,
        low_leaf_selectors,
        commitment_root_latest: latest_set.commitment_root(),
        set_latest_path,
        secret_key: Fr::ZERO,
    })
}

mod proof_serde {
    use lb_groth16::COMPRESSED_PROOF_SIZE;
    use serde::{Deserializer, Serializer};

    // Hex string for human-readable formats; a fixed-size 128-byte array
    // (no length prefix) for binary formats like bincode.
    pub fn serialize<S>(item: &lb_pol::PoLProof, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        lb_utils::serde::serialize_bytes_array(item.to_bytes(), serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<lb_pol::PoLProof, D::Error>
    where
        D: Deserializer<'de>,
    {
        let proof_array =
            lb_utils::serde::deserialize_bytes_array::<{ COMPRESSED_PROOF_SIZE }, D>(deserializer)?;
        Ok(lb_pol::PoLProof::from_bytes(&proof_array))
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use lb_key_management_system_keys::keys::UnsecuredZkKey;
    use lb_pol::LotteryConstants;
    use lb_utils::math::NonNegativeRatio;

    use super::*;
    use crate::mantle::{
        note_set::{CommitmentMmr, LEDGER_SET},
        notes::{Note, derive_note_nullifier},
    };

    #[must_use]
    pub fn note_set_leader_inputs(
        secret_key: Fr,
        value: u64,
        nonce: Fr,
        msg: Fr,
    ) -> (LeaderPublic, LeaderPrivate) {
        let public_key = UnsecuredZkKey::new(secret_key).to_public_key();
        let note = Note::new(value, nonce, public_key);
        let commitment = note.commitment();

        let (mmr, commitment_path) = CommitmentMmr::new()
            .push_with_paths(commitment, &mut [])
            .expect("the commitment MMR is empty");

        let sets = NoteSets::new();
        let ledger = sets
            .get(LEDGER_SET)
            .expect("the ledger set exists")
            .clone()
            .execute_adding(&[commitment])
            .expect("the commitment MMR is not full");
        let sets = sets.update(LEDGER_SET, ledger).end_block(1);
        let set = sets.get(LEDGER_SET).expect("the ledger set exists");
        assert_eq!(set.commitment_root(), mmr.frontier_root());

        let nullifier = derive_note_nullifier(&commitment, &secret_key);

        let eligible = sets.eligible_root();
        let (lottery_0, lottery_1) =
            LotteryConstants::new(NonNegativeRatio::new(1, 10.try_into().unwrap()))
                .compute_lottery_values(value);
        let public = (0..10_000u64)
            .map(|epoch_nonce| {
                LeaderPublic::new(
                    eligible,
                    eligible,
                    Fr::from(epoch_nonce),
                    0,
                    lottery_0,
                    lottery_1,
                )
            })
            .find(|public| public.check_winning(value, commitment.0, secret_key))
            .expect("a winning epoch nonce below 10000");

        let wallet = lb_pol::PolWalletInputsData {
            secret_key,
            ..pol_wallet_inputs(
                &note,
                LEDGER_SET,
                &nullifier,
                &commitment_path,
                &sets,
                &sets,
            )
            .expect("the note is in the ledger set and unspent")
        };
        (public, LeaderPrivate::new(public, wallet, msg))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use ark_ff::Field as _;
    use lb_key_management_system_keys::keys::UnsecuredZkKey;
    use lb_pol::LotteryConstants;
    use lb_utils::math::NonNegativeRatio;
    use rand::RngCore as _;

    use super::*;
    use crate::mantle::{
        note_set::{CommitmentMmr, LEDGER_SET, NoteSet},
        notes::{NoteCommitment, derive_note_nullifier},
    };

    /// Compute the Hoeffding sample size:
    ///     n >= (1 / (2 * eps^2)) * ln(2/alpha)
    /// <https://en.wikipedia.org/wiki/Hoeffding's_inequality>
    fn hoeffding_sample_size(eps: f64, alpha: f64) -> usize {
        assert!(eps > 0.0 && eps < 1.0, "eps must be in (0,1)");
        assert!(alpha > 0.0 && alpha < 1.0, "alpha must be in (0,1)");
        let n = (1.0 / (2.0 * eps * eps)) * (2.0 / alpha).ln();
        n.ceil() as usize
    }

    /// Runs the generator `n` times and returns the observed success rate.
    fn empirical_rate(n: usize, f: impl Fn() -> bool) -> f64 {
        let mut k: usize = 0;
        for _ in 0..n {
            if f() {
                k += 1;
            }
        }
        k as f64 / n as f64
    }

    fn check_prob(target: f64, f: impl Fn() -> bool) {
        let eps = if target < 0.1 {
            0.01 // tight tolerance for low target (±1%p)
        } else {
            0.08 // loose tolerance for high target (±8%p)
        };

        let n = hoeffding_sample_size(
            eps,
            // fails with probability at most 1e-6 if the observed rate is within EPS of target
            1e-6,
        );
        println!("Sampling n = {n}");

        let observed = empirical_rate(n, f);

        assert!(
            (observed - target).abs() <= eps,
            "Rate out of tolerance: observed={observed:.6}, target={target:.6}, eps={eps:.6}, n={n}"
        );
    }

    fn rand_inputs(lottery_constants: &LotteryConstants) -> (LeaderPublic, Fr, Fr) {
        let (lottery_0, lottery_1) = lottery_constants.compute_lottery_values(1);
        let mut rng = rand::thread_rng();
        let public = LeaderPublic::new(
            Fr::ZERO,
            Fr::ZERO,
            Fr::ZERO,
            rng.next_u64(),
            lottery_0,
            lottery_1,
        );
        let note = Fr::from(rng.next_u64()); // note value
        let sk = Fr::from(rng.next_u64()); // secret key
        (public, note, sk)
    }

    #[test]
    fn test_genesis_verification() {
        let genesis_proof = Groth16LeaderProof::genesis();
        assert!(genesis_proof.verify_genesis());
    }

    /// Check that ticket is derived correctly with known values.
    ///
    /// NOTE: This test must be updated if the ticket derivation changes.
    #[test]
    fn test_ticket_derivation() {
        let ticket = LeaderPublic::ticket(
            fr_from_bytes(b"node_id").unwrap(),
            fr_from_bytes(b"sk").unwrap(),
            fr_from_bytes(b"epoch_nonce").unwrap(),
            fr_from_bytes(b"slot").unwrap(),
        );
        assert_eq!(
            ticket,
            Fr::from_str(
                "10938646954300723195015130306902300454523545182210299629143086933853387042384"
            )
            .unwrap()
        );
    }

    #[test]
    fn test_check_winning() {
        let slot_activation_coeff = NonNegativeRatio::new(1, 10.try_into().unwrap());
        let constants = LotteryConstants::new(slot_activation_coeff);

        // winning rate of all the stake should be ~ active slot coeff
        check_prob(slot_activation_coeff.as_f64(), || {
            let (public, note_id, sk) = rand_inputs(&constants);
            public.check_winning(1, note_id, sk)
        });
    }

    #[test]
    fn test_prove_and_verify_with_note_set_witness() {
        let msg = Fr::from(99u64);
        let (public, private) =
            test_utils::note_set_leader_inputs(Fr::from(7u64), 1000, Fr::from(11u64), msg);
        let proof = Groth16LeaderProof::prove(private, ZkPublicKey::zero()).unwrap();
        assert!(proof.verify(&public, msg));

        let wrong_latest = LeaderPublic {
            latest_root: Fr::ONE,
            ..public
        };
        assert!(!proof.verify(&wrong_latest, msg));
        assert!(!proof.verify(&public, msg + Fr::ONE));
    }

    #[test]
    fn pol_wallet_inputs_reads_the_aged_and_latest_note_sets() {
        let secret_key = Fr::from(7u64);
        let note = notes::Note::new(
            1000,
            Fr::from(11u64),
            UnsecuredZkKey::new(secret_key).to_public_key(),
        );
        let commitment = note.commitment();
        let (_, path) = CommitmentMmr::new()
            .push_with_paths(commitment, &mut [])
            .unwrap();
        let sets = NoteSets::new()
            .update(
                LEDGER_SET,
                NoteSet::empty().execute_adding(&[commitment]).unwrap(),
            )
            .end_block(1);
        let nullifier = derive_note_nullifier(&commitment, &secret_key);

        let wallet = pol_wallet_inputs(&note, LEDGER_SET, &nullifier, &path, &sets, &sets).unwrap();
        assert_eq!(wallet.secret_key, Fr::ZERO);
        assert_eq!(
            (wallet.note_value, wallet.note_nonce),
            (1000, Fr::from(11u64))
        );
        assert_eq!(
            wallet.commitment_root_latest,
            sets.get(LEDGER_SET).unwrap().commitment_root()
        );
        assert_eq!(
            wallet.nullifier_root_aged,
            sets.get(LEDGER_SET).unwrap().nullifier_root()
        );
        assert_eq!(
            (
                wallet.low_leaf_nullifier,
                wallet.low_leaf_next_nullifier,
                wallet.low_leaf_next_index
            ),
            (Fr::ZERO, Fr::ZERO, 0)
        );
        assert_eq!(wallet.set_selectors, [false; 40]);

        let spent_set = sets
            .get(LEDGER_SET)
            .unwrap()
            .clone()
            .execute_spending(&[nullifier])
            .unwrap();
        let latest = sets.clone().update(LEDGER_SET, spent_set).end_block(2);
        assert_eq!(
            pol_wallet_inputs(&note, LEDGER_SET, &nullifier, &path, &sets, &latest).unwrap_err(),
            LeaderWitnessError::Spent
        );

        let (_, foreign) = CommitmentMmr::new()
            .push(NoteCommitment(Fr::from(5u64)))
            .unwrap()
            .push_with_paths(commitment, &mut [])
            .unwrap();
        assert_eq!(
            pol_wallet_inputs(&note, LEDGER_SET, &nullifier, &foreign, &sets, &sets).unwrap_err(),
            LeaderWitnessError::AgedPathMismatch(LEDGER_SET)
        );
        assert_eq!(
            pol_wallet_inputs(&note, 7, &nullifier, &path, &sets, &sets).unwrap_err(),
            LeaderWitnessError::UnknownNoteSet(7)
        );
    }

    #[test]
    fn check_winning_requires_the_key_of_the_note() {
        let secret_key = Fr::from(7u64);
        let public_key = UnsecuredZkKey::new(secret_key).to_public_key();
        let note = notes::Note::new(1000, Fr::from(11u64), public_key);
        let public = LeaderPublic::new(
            Fr::ONE,
            Fr::ONE,
            Fr::from(3u64),
            5,
            Fr::from(2u64),
            Fr::ZERO,
        );
        assert_eq!(
            check_winning(&note, public, &public_key, secret_key),
            public.check_winning(1000, note.commitment().0, secret_key)
        );
        assert!(!check_winning(
            &note,
            public,
            &ZkPublicKey::new(Fr::ONE),
            secret_key
        ));
    }
}
