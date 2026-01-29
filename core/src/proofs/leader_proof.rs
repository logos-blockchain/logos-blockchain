use std::sync::LazyLock;

use ark_ff::{Field as _, PrimeField as _};
#[cfg(feature = "pol-dev-mode")]
use generic_array::GenericArray;
use lb_groth16::{Fr, fr_from_bytes, serde::serde_fr};
use lb_poseidon2::{Digest as _, Poseidon2Bn254Hasher};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const POL_PROOF_DEV_MODE: &str = "POL_PROOF_DEV_MODE";

/// Macro to conditionally execute code based on `PoL` dev mode.
///
/// This macro checks both the `pol-dev-mode` feature flag (compile-time) and
/// the `POL_PROOF_DEV_MODE` environment variable (runtime). The dev code path
/// is only taken when both conditions are met.
///
/// When the `pol-dev-mode` feature is disabled, the dev code is completely
/// eliminated at compile time.
///
/// # Example
/// ```ignore
/// let result = if_pol_dev_mode!(
///     // Dev mode code
///     compute_dev_result(),
///     // Normal mode code
///     compute_normal_result()
/// );
/// ```
#[macro_export]
macro_rules! if_pol_dev_mode {
    ($dev:expr, $normal:expr) => {{
        #[cfg(feature = "pol-dev-mode")]
        {
            if std::env::var($crate::proofs::leader_proof::POL_PROOF_DEV_MODE).is_ok() {
                $dev
            } else {
                $normal
            }
        }
        #[cfg(not(feature = "pol-dev-mode"))]
        {
            $normal
        }
    }};
}

use crate::{
    mantle::{
        ledger::Utxo,
        ops::{channel::Ed25519PublicKey, leader_claim::VoucherCm},
    },
    proofs::merkle::merkle_path_to_witness,
    utils::merkle::MerklePath,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Groth16LeaderProof {
    #[serde(with = "proof_serde")]
    proof: lb_pol::PoLProof,
    #[serde(with = "serde_fr")]
    entropy_contribution: Fr,
    leader_key: Ed25519PublicKey,
    voucher_cm: VoucherCm,
    #[cfg(feature = "pol-dev-mode")]
    public: LeaderPublic,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Proof of leadership failed: {0}")]
    PoLProofFailed(#[from] lb_pol::ProveError),
}

impl Groth16LeaderProof {
    pub fn prove(witness: LeaderPrivate, voucher_cm: VoucherCm) -> Result<Self, Error> {
        let start_t = std::time::Instant::now();
        #[cfg(feature = "pol-dev-mode")]
        let public = witness.public;
        let leader_key = witness.pk;
        let (proof, entropy_contribution) = Self::generate_proof(witness)?;
        tracing::debug!("groth16 prover time: {:.2?}", start_t.elapsed(),);

        Ok(Self {
            proof,
            entropy_contribution,
            leader_key,
            voucher_cm,
            #[cfg(feature = "pol-dev-mode")]
            public,
        })
    }

    #[must_use]
    pub fn genesis() -> Self {
        Self {
            proof: lb_pol::PoLProof::from_bytes(&[0u8; 128]),
            entropy_contribution: Fr::ZERO,
            leader_key: Ed25519PublicKey::from_bytes(&[0u8; 32]).unwrap(),
            voucher_cm: VoucherCm::default(),
            #[cfg(feature = "pol-dev-mode")]
            public: LeaderPublic::new(Fr::ZERO, Fr::ZERO, Fr::ZERO, 0, 0),
        }
    }

    fn generate_proof(private: LeaderPrivate) -> Result<(lb_pol::PoLProof, Fr), Error> {
        if_pol_dev_mode!(
            {
                tracing::warn!(
                    "Proofs are being generated in dev mode. This should never be used in production."
                );
                let proof = lb_groth16::CompressedGroth16Proof::new(
                    GenericArray::default(),
                    GenericArray::default(),
                    GenericArray::default(),
                );
                Ok((proof, Fr::ZERO))
            },
            {
                let (proof, verif_inputs) =
                    lb_pol::prove(&private.input.into()).map_err(Error::PoLProofFailed)?;
                Ok((proof, verif_inputs.entropy_contribution.into_inner()))
            }
        )
    }

    #[must_use]
    pub const fn proof(&self) -> &lb_pol::PoLProof {
        &self.proof
    }
}

pub trait LeaderProof {
    /// Verify the proof against the public inputs.
    fn verify(&self, public_inputs: &LeaderPublic) -> bool;

    fn verify_genesis(&self) -> bool;

    /// Get the entropy used in the proof.
    fn entropy(&self) -> Fr;

    fn leader_key(&self) -> &Ed25519PublicKey;

    fn voucher_cm(&self) -> &VoucherCm;
}

impl LeaderProof for Groth16LeaderProof {
    fn verify(&self, public_inputs: &LeaderPublic) -> bool {
        if_pol_dev_mode!(
            {
                tracing::warn!(
                    "Proofs are being verified in dev mode. This should never be used in production."
                );
                &self.public == public_inputs
            },
            {
                let leader_pk = ed25519_pk_to_fr_tuple(self.leader_key());
                lb_pol::verify(
                    &self.proof,
                    &lb_pol::PolVerifierInput::new(
                        self.entropy(),
                        public_inputs.slot,
                        public_inputs.epoch_nonce,
                        public_inputs.aged_root,
                        public_inputs.latest_root,
                        public_inputs.total_stake,
                        leader_pk,
                    ),
                )
                .is_ok()
            }
        )
    }

    fn verify_genesis(&self) -> bool {
        let expected_genesis = Self::genesis();
        self.proof == expected_genesis.proof
            && self.entropy_contribution == expected_genesis.entropy_contribution
            && self.leader_key == expected_genesis.leader_key
            && self.voucher_cm == expected_genesis.voucher_cm
    }

    fn entropy(&self) -> Fr {
        self.entropy_contribution
    }

    fn leader_key(&self) -> &Ed25519PublicKey {
        &self.leader_key
    }

    fn voucher_cm(&self) -> &VoucherCm {
        &self.voucher_cm
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderPublic {
    pub slot: u64,
    #[serde(with = "serde_fr")]
    pub epoch_nonce: Fr,
    pub total_stake: u64,
    #[serde(with = "serde_fr")]
    pub aged_root: Fr,
    #[serde(with = "serde_fr")]
    pub latest_root: Fr,
}

impl LeaderPublic {
    #[must_use]
    pub const fn new(
        aged_root: Fr,
        latest_root: Fr,
        epoch_nonce: Fr,
        slot: u64,
        total_stake: u64,
    ) -> Self {
        Self {
            slot,
            epoch_nonce,
            total_stake,
            aged_root,
            latest_root,
        }
    }

    #[must_use]
    pub fn check_winning(&self, value: u64, note_id: Fr, sk: Fr) -> bool {
        let (t0, t1) = self.scaled_phi_approx();
        let threshold =
            Self::phi_approx(&Fr::from(value), &(Fr::from(t0), Fr::from(t1))).into_bigint();
        let ticket = Self::ticket(note_id, sk, self.epoch_nonce, Fr::from(self.slot)).into_bigint();
        ticket < threshold
    }

    #[must_use]
    #[cfg(feature = "pol-dev-mode")]
    pub fn check_winning_dev(
        &self,
        value: u64,
        note_id: Fr,
        sk: Fr,
        active_slot_coeff: f64,
    ) -> bool {
        let (t0, t1) = self.scaled_phi_approx_dev(active_slot_coeff);
        let threshold =
            Self::phi_approx(&Fr::from(value), &(Fr::from(t0), Fr::from(t1))).into_bigint();
        let ticket = Self::ticket(note_id, sk, self.epoch_nonce, Fr::from(self.slot)).into_bigint();
        ticket < threshold
    }

    fn scaled_phi_approx(&self) -> (BigUint, BigUint) {
        let t0 = &*lb_pol::T0_CONSTANT / &BigUint::from(self.total_stake);
        let total_stake_sq = &BigUint::from(self.total_stake) * &BigUint::from(self.total_stake);
        let t1 = &*lb_pol::P - (&*lb_pol::T1_CONSTANT / &total_stake_sq);
        (t0, t1)
    }

    #[cfg(feature = "pol-dev-mode")]
    fn scaled_phi_approx_dev(&self, active_slot_coeff: f64) -> (BigUint, BigUint) {
        let total_stake = BigUint::from(self.total_stake);
        let total_stake_sq = &total_stake * &total_stake;
        let double_total_stake_sq = &total_stake_sq * 2u64;

        let precision = 1_000_000_000_000_000_000u128;
        let order = lb_pol::P.clone();
        let neg_f_ln =
            BigUint::from((-(1.0 - active_slot_coeff).ln() * precision as f64).round() as u128);
        let neg_f_ln_sq = &neg_f_ln * &neg_f_ln;

        let t0 = (&order * &neg_f_ln) / (&total_stake * precision);
        let t1 =
            ((&order * &neg_f_ln_sq) / (&double_total_stake_sq * precision * precision)) % &order;
        (t0, &order - t1)
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
    pk: Ed25519PublicKey,
    #[cfg(feature = "pol-dev-mode")]
    public: LeaderPublic,
}

impl LeaderPrivate {
    #[must_use]
    pub fn new(
        public: LeaderPublic,
        note: Utxo,
        aged_path: &MerklePath<Fr>,
        latest_path: &MerklePath<Fr>,
        secret_key: Fr,
        leader_pk: &Ed25519PublicKey,
    ) -> Self {
        let public_key = *leader_pk;
        let leader_pk = ed25519_pk_to_fr_tuple(leader_pk);
        let chain = lb_pol::PolChainInputsData {
            slot_number: public.slot,
            epoch_nonce: public.epoch_nonce,
            total_stake: public.total_stake,
            aged_root: public.aged_root,
            latest_root: public.latest_root,
            leader_pk,
        };
        let (aged_path, aged_selector) = merkle_path_to_witness(aged_path);
        let (latest_path, latest_selector) = merkle_path_to_witness(latest_path);
        let wallet = lb_pol::PolWalletInputsData {
            note_value: note.note.value,
            transaction_hash: *note.tx_hash.as_ref(),
            output_number: note.output_index as u64,
            aged_path,
            aged_selector,
            latest_path,
            latest_selector,
            secret_key,
        };
        let input = lb_pol::PolWitnessInputsData::from_chain_and_wallet_data(chain, wallet);
        Self {
            input,
            pk: public_key,
            #[cfg(feature = "pol-dev-mode")]
            public,
        }
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

mod proof_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(item: &lb_pol::PoLProof, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&item.to_bytes())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<lb_pol::PoLProof, D::Error>
    where
        D: Deserializer<'de>,
    {
        let proof_bytes: Vec<u8> = Deserialize::deserialize(deserializer)?;
        let proof_array: [u8; 128] = proof_bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("Expected exactly 128 bytes"))?;
        Ok(lb_pol::PoLProof::from_bytes(&proof_array))
    }
}

fn ed25519_pk_to_fr_tuple(pk: &Ed25519PublicKey) -> (Fr, Fr) {
    let pk_bytes = pk.as_bytes();
    // Convert each half of the public key to Fr so that they alwasy fit
    (
        fr_from_bytes(&pk_bytes[0..16]).unwrap(),
        fr_from_bytes(&pk_bytes[16..32]).unwrap(),
    )
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use rand::RngCore as _;

    use super::*;

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
        const EPS: f64 = 0.01; // tolerance band (±2 percentage points)
        const ALPHA: f64 = 1e-6; // fails with probability at most ALPHA if the observed rate is within EPS of
        // target

        let n = hoeffding_sample_size(EPS, ALPHA);
        println!("Sampling n = {n}");

        let observed = empirical_rate(n, f);

        assert!(
            (observed - target).abs() <= EPS,
            "Rate out of tolerance: observed={observed:.6}, target={target:.6}, eps={EPS:.6}, n={n}"
        );
    }

    fn rand_inputs() -> (LeaderPublic, Fr, Fr) {
        let mut rng = rand::thread_rng();
        let public = LeaderPublic::new(
            Fr::ZERO,
            Fr::ZERO,
            Fr::ZERO,
            rng.next_u64(),
            1, // total stake
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

    #[cfg(feature = "pol-dev-mode")]
    #[test]
    fn test_check_winning_dev() {
        // winning rate of all the stake should be ~ active slot coeff
        check_prob(1.0 / 30.0, || {
            let (public, note_id, sk) = rand_inputs();
            public.check_winning_dev(1, note_id, sk, 1.0 / 30.0)
        });
        check_prob(0.05, || {
            let (public, note_id, sk) = rand_inputs();
            public.check_winning_dev(1, note_id, sk, 0.05)
        });
    }

    #[test]
    fn test_check_winning() {
        // winning rate of all the stake should be ~ active slot coeff
        check_prob(1.0 / 30.0, || {
            let (public, note_id, sk) = rand_inputs();
            public.check_winning(1, note_id, sk)
        });
    }
}
