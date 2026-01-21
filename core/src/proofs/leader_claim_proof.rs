use ark_ff::{Field as _, PrimeField as _};
use generic_array::GenericArray;
use lb_groth16::{Fr, fr_from_bytes, serde::serde_fr};
use lb_poseidon2::{Digest as _, Poseidon2Bn254Hasher, ZkHash};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const POC_PROOF_DEV_MODE: &str = "POC_PROOF_DEV_MODE";

use crate::{
    mantle::{
        ledger::Utxo,
        ops::{
            channel::Ed25519PublicKey,
            leader_claim::{VoucherCm, VoucherNullifier},
        },
    },
    utils::merkle::{MerkleNode, MerklePath},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Groth16LeaderClaimProof {
    #[serde(with = "proof_serde")]
    proof: lb_poc::PoCProof,
    voucher_nf: VoucherNullifier,
    #[cfg(feature = "poc-dev-mode")]
    public: LeaderPublic,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Proof of claim failed: {0}")]
    PoCProofFailed(#[from] lb_poc::ProveError),
}

impl Groth16LeaderClaimProof {
    pub fn prove(witness: LeaderClaimPrivate) -> Result<Self, Error> {
        let start_t = std::time::Instant::now();
        #[cfg(feature = "poc-dev-mode")]
        let public = witness.public;
        let (proof, voucher_nf) = Self::generate_proof(witness)?;
        tracing::debug!("groth16 prover time: {:.2?}", start_t.elapsed(),);

        Ok(Self {
            proof,
            voucher_nf: VoucherNullifier::from(voucher_nf),
            #[cfg(feature = "pol-dev-mode")]
            public,
        })
    }

    #[must_use]
    pub fn genesis() -> Self {
        Self {
            proof: lb_poc::PoCProof::from_bytes(&[0u8; 128]),
            voucher_nf: VoucherNullifier::default(),
            #[cfg(feature = "poc-dev-mode")]
            public: LeaderClaimPublic::new(Fr::ZERO, Fr::ZERO),
        }
    }

    fn generate_proof(private: LeaderClaimPrivate) -> Result<(lb_poc::PoCProof, Fr), Error> {
        if cfg!(feature = "poc-dev-mode") && std::env::var(POC_PROOF_DEV_MODE).is_ok() {
            tracing::warn!(
                "Proofs are being generated in dev mode. This should never be used in production."
            );
            let proof = lb_groth16::CompressedGroth16Proof::new(
                GenericArray::default(),
                GenericArray::default(),
                GenericArray::default(),
            );

            return Ok((proof, Fr::ZERO));
        }
        let (proof, verif_inputs) =
            lb_poc::prove(&private.input.into()).map_err(Error::PoCProofFailed)?;
        Ok((proof, verif_inputs.voucher_nullifier.into_inner()))
    }

    #[must_use]
    pub const fn proof(&self) -> &lb_pol::PoLProof {
        &self.proof
    }
}

pub trait LeaderClaimProof {
    /// Verify the proof against the public inputs.
    fn verify(&self, public_inputs: &LeaderClaimPublic) -> bool;

    fn verify_genesis(&self) -> bool;

    fn voucher_nf(&self) -> &VoucherNullifier;
}

impl LeaderClaimProof for Groth16LeaderClaimProof {
    fn verify(&self, public_inputs: &LeaderClaimPublic) -> bool {
        #[cfg(feature = "poc-dev-mode")]
        if std::env::var(POC_PROOF_DEV_MODE).is_ok() {
            tracing::warn!(
                "Proofs are being verified in dev mode. This should never be used in production."
            );
            return &self.public == public_inputs;
        }

        lb_poc::verify(
            &self.proof,
            &lb_poc::PoCVerifierInput::new(
                self.voucher_nf().into(),
                public_inputs.voucher_root,
                public_inputs.mantle_tx_hash,
            ),
        )
        .is_ok()
    }

    fn verify_genesis(&self) -> bool {
        self.proof == lb_poc::PoCProof::from_bytes(&[0u8; 128])
            && self.voucher_nf == VoucherNullifier::from(Fr::ZERO)
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
    #[cfg(feature = "poc-dev-mode")]
    public: LeaderClaimPublic,
}

impl LeaderClaimPrivate {
    #[must_use]
    pub fn new(
        public: LeaderClaimPublic,
        voucher_path: &MerklePath<Fr>,
        secret_voucher: Fr,
    ) -> Self {
        let chain = lb_poc::PoCChainInputsData {
            voucher_root: public.voucher_root,
            mantle_tx_hash: public.mantle_tx_hash,
        };
        let wallet = lb_poc::PoCWalletInputsData {
            secret_voucher: secret_voucher,
            voucher_merkle_path: voucher_path.iter().map(|n| *n.item()).collect(),
            voucher_merkle_path_selectors: voucher_path
                .iter()
                .rev() // PoL circuit expects the reverse order for selectors
                .map(|n| matches!(n, MerkleNode::Right(_)))
                .collect(),
        };
        let input = lb_poc::PoCWitnessInputsData::from_chain_and_wallet_data(chain, wallet);
        Self {
            input,
            #[cfg(feature = "poc-dev-mode")]
            public,
        }
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
