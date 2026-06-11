use lb_groth16::{Fr, serde::serde_fr};
use lb_log_targets::proofs;
use lb_mmr::MerklePath;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::error;

use crate::{
    mantle::ops::leader_claim::{VoucherNullifier, VoucherSecret},
    proofs::merkle::mmr_path_to_witness,
};

const LOG_TARGET: &str = proofs::LEADER_CLAIM;

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct Groth16LeaderClaimProof {
    #[serde(with = "proof_serde")]
    proof: lb_poc::PoCProof,
    voucher_nf: VoucherNullifier,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Proof of claim failed: {0}")]
    PoCProofFailed(#[from] lb_poc::ProveError),
}

impl Groth16LeaderClaimProof {
    pub fn prove(witness: LeaderClaimPrivate) -> Result<Self, Error> {
        let start_t = std::time::Instant::now();
        let (proof, voucher_nf) = Self::generate_proof(witness)?;
        tracing::debug!(target: LOG_TARGET, "PoC groth16 prover time: {:.2?}", start_t.elapsed());

        Ok(Self {
            proof,
            voucher_nf: voucher_nf.into(),
        })
    }

    fn generate_proof(private: LeaderClaimPrivate) -> Result<(lb_poc::PoCProof, Fr), Error> {
        let (proof, verif_inputs) =
            lb_poc::prove(private.input.into()).map_err(Error::PoCProofFailed)?;
        Ok((proof, verif_inputs.voucher_nullifier.into_inner()))
    }

    #[must_use]
    pub const fn proof(&self) -> &lb_poc::PoCProof {
        &self.proof
    }

    #[must_use]
    pub const fn new(proof: lb_poc::PoCProof, voucher_nf: VoucherNullifier) -> Self {
        Self { proof, voucher_nf }
    }
}

pub trait LeaderClaimProof {
    /// Verify the proof against the public inputs.
    fn verify(&self, public_inputs: &LeaderClaimPublic) -> bool;

    fn voucher_nf(&self) -> &VoucherNullifier;
}

impl LeaderClaimProof for Groth16LeaderClaimProof {
    fn verify(&self, public_inputs: &LeaderClaimPublic) -> bool {
        lb_poc::verify(
            &self.proof,
            &lb_poc::PoCVerifierInput::new(
                (*self.voucher_nf()).into(),
                public_inputs.voucher_root,
                public_inputs.mantle_tx_hash,
            ),
        )
        .unwrap_or_else(|e| {
            error!(target: LOG_TARGET, "Error verifying LeaderClaimProof: {e:?}");
            false
        })
    }

    fn voucher_nf(&self) -> &VoucherNullifier {
        &self.voucher_nf
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderClaimPublic {
    #[serde(with = "serde_fr")]
    pub voucher_root: Fr,
    #[serde(with = "serde_fr")]
    pub mantle_tx_hash: Fr,
}

impl LeaderClaimPublic {
    #[must_use]
    pub const fn new(voucher_root: Fr, mantle_tx_hash: Fr) -> Self {
        Self {
            voucher_root,
            mantle_tx_hash,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LeaderClaimPrivate {
    input: lb_poc::PoCWitnessInputsData,
}

impl LeaderClaimPrivate {
    #[must_use]
    pub fn new(
        public: LeaderClaimPublic,
        voucher_path: &MerklePath,
        secret_voucher: VoucherSecret,
    ) -> Self {
        let chain = lb_poc::PoCChainInputsData {
            voucher_root: public.voucher_root,
            mantle_tx_hash: public.mantle_tx_hash,
        };
        let (voucher_merkle_path, voucher_merkle_path_selectors) =
            mmr_path_to_witness(voucher_path);
        let wallet = lb_poc::PoCWalletInputsData {
            secret_voucher: secret_voucher.into(),
            voucher_merkle_path_and_selectors: core::array::from_fn(|i| {
                (voucher_merkle_path[i], voucher_merkle_path_selectors[i])
            }),
        };
        let input = lb_poc::PoCWitnessInputsData::from_chain_and_wallet_data(chain, wallet);
        Self { input }
    }

    #[must_use]
    pub const fn input(&self) -> &lb_poc::PoCWitnessInputsData {
        &self.input
    }
}

impl From<LeaderClaimPrivate> for lb_poc::PoCWitnessInputsData {
    fn from(value: LeaderClaimPrivate) -> Self {
        value.input
    }
}

mod proof_serde {
    use serde::{Deserializer, Serializer};

    // Hex string for human-readable formats; a fixed-size 128-byte array
    // (no length prefix) for binary formats like bincode.
    pub fn serialize<S>(item: &lb_poc::PoCProof, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        lb_utils::serde::serialize_bytes_array(item.to_bytes(), serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<lb_poc::PoCProof, D::Error>
    where
        D: Deserializer<'de>,
    {
        let proof_array = lb_utils::serde::deserialize_bytes_array::<128, D>(deserializer)?;
        Ok(lb_poc::PoCProof::from_bytes(&proof_array))
    }
}
