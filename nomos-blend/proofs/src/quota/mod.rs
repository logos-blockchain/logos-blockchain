use std::sync::LazyLock;

use ::serde::{Deserialize, Serialize};
use ed25519_dalek::{PUBLIC_KEY_LENGTH, VerifyingKey};
use generic_array::{ArrayLength, GenericArray};
use groth16::{Bn254, CompressSize, fr_from_bytes, fr_from_bytes_unchecked, fr_to_bytes};
use poq::{PoQProof, PoQVerifierInput, PoQWitnessInputs, ProveError, prove, verify};
use thiserror::Error;

use crate::{
    ZkHash, ZkHashExt as _,
    quota::inputs::{
        VerifyInputs,
        prove::{Inputs, PrivateInputs, PublicInputs},
    },
};

pub mod inputs;
mod serde;
#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "unsafe-test-functions"))]
pub mod fixtures;

// Cannot depend on `key-management-system-keys` crate here due to circular
// dependency.
pub(crate) type Ed25519PublicKey = VerifyingKey;
pub(crate) const ED25519_PUBLIC_KEY_SIZE: usize = PUBLIC_KEY_LENGTH;

const KEY_NULLIFIER_SIZE: usize = size_of::<ZkHash>();
const PROOF_CIRCUIT_SIZE: usize = size_of::<PoQProof>();
pub const PROOF_OF_QUOTA_SIZE: usize = KEY_NULLIFIER_SIZE.checked_add(PROOF_CIRCUIT_SIZE).unwrap();

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid input: {0}.")]
    InvalidInput(#[from] Box<dyn core::error::Error + Send + Sync>),
    #[error("Proof generation failed: {0}.")]
    ProofGeneration(#[from] ProveError),
    #[error("Invalid proof")]
    InvalidProof,
}

/// A Proof of Quota as described in the Blend v1 spec: <https://www.notion.so/nomos-tech/Proof-of-Quota-Specification-215261aa09df81d88118ee22205cbafe?source=copy_link#26a261aa09df80f4b119f900fbb36f3f>.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProofOfQuota {
    #[serde(with = "groth16::serde::serde_fr")]
    key_nullifier: ZkHash,
    #[serde(with = "self::serde::proof::SerializablePoQProof")]
    proof: PoQProof,
}

impl ProofOfQuota {
    /// Verify a Proof of Quota with the provided inputs.
    ///
    /// The key nullifier required to verify the proof is taken from the proof
    /// itself and is not contained in the passed inputs.
    pub fn verify(self, public_inputs: &PublicInputs) -> Result<VerifiedProofOfQuota, Error> {
        let verifier_input =
            VerifyInputs::from_prove_inputs_and_nullifier(*public_inputs, self.key_nullifier);
        let is_proof_valid = matches!(verify(&self.proof, verifier_input.into()), Ok(true));
        if is_proof_valid {
            Ok(VerifiedProofOfQuota(self))
        } else {
            Err(Error::InvalidProof)
        }
    }

    #[must_use]
    pub const fn key_nullifier(&self) -> ZkHash {
        self.key_nullifier
    }
}

impl PartialEq<VerifiedProofOfQuota> for ProofOfQuota {
    fn eq(&self, other: &VerifiedProofOfQuota) -> bool {
        *self == other.0
    }
}

impl From<&ProofOfQuota> for [u8; PROOF_OF_QUOTA_SIZE] {
    fn from(proof: &ProofOfQuota) -> Self {
        let mut bytes = [0u8; PROOF_OF_QUOTA_SIZE];
        bytes[..KEY_NULLIFIER_SIZE].copy_from_slice(&fr_to_bytes(&proof.key_nullifier));
        bytes[KEY_NULLIFIER_SIZE..].copy_from_slice(&proof.proof.to_bytes());
        bytes
    }
}

impl TryFrom<[u8; PROOF_OF_QUOTA_SIZE]> for ProofOfQuota {
    type Error = Box<dyn std::error::Error>;

    fn try_from(bytes: [u8; PROOF_OF_QUOTA_SIZE]) -> Result<Self, Self::Error> {
        let (key_nullifier_bytes, proof_circuit_bytes) = bytes.split_at(KEY_NULLIFIER_SIZE);
        let key_nullifier = fr_from_bytes(key_nullifier_bytes).map_err(Box::new)?;
        let (pi_a, pi_b, pi_c) = split_proof_components::<
            <Bn254 as CompressSize>::G1CompressedSize,
            <Bn254 as CompressSize>::G2CompressedSize,
        >(proof_circuit_bytes.try_into().map_err(Box::new)?);

        Ok(Self {
            key_nullifier,
            proof: PoQProof::new(pi_a, pi_b, pi_c),
        })
    }
}

/// A verified Proof of Quota.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct VerifiedProofOfQuota(ProofOfQuota);

impl VerifiedProofOfQuota {
    /// Generate a new Proof of Quota with the provided public and private
    /// inputs, along with the secret selection randomness for the Proof of
    /// Selection associated to this Proof of Quota.
    pub fn new(
        public_inputs: &PublicInputs,
        private_inputs: PrivateInputs,
    ) -> Result<(Self, ZkHash), Error> {
        let key_index = private_inputs.key_index;
        let secret_selection_randomness_sk = private_inputs.get_secret_selection_randomness_sk();
        let witness_inputs: PoQWitnessInputs = Inputs {
            private: private_inputs,
            public: *public_inputs,
        }
        .try_into()
        .map_err(|e| Error::InvalidInput(Box::new(e)))?;
        let (proof, PoQVerifierInput { key_nullifier, .. }) =
            prove(witness_inputs).map_err(Error::ProofGeneration)?;
        let secret_selection_randomness = generate_secret_selection_randomness(
            secret_selection_randomness_sk,
            key_index,
            public_inputs.session,
        );
        Ok((
            Self(ProofOfQuota {
                key_nullifier: key_nullifier.into_inner(),
                proof,
            }),
            secret_selection_randomness,
        ))
    }

    #[must_use]
    pub const fn into_inner(self) -> ProofOfQuota {
        self.0
    }

    #[must_use]
    pub fn from_bytes_unchecked(bytes: [u8; PROOF_OF_QUOTA_SIZE]) -> Self {
        let (key_nullifier_bytes, proof_circuit_bytes) = bytes.split_at(KEY_NULLIFIER_SIZE);
        let key_nullifier = fr_from_bytes_unchecked(key_nullifier_bytes);
        let (pi_a, pi_b, pi_c) = split_proof_components::<
            <Bn254 as CompressSize>::G1CompressedSize,
            <Bn254 as CompressSize>::G2CompressedSize,
        >(proof_circuit_bytes.try_into().unwrap());

        Self(ProofOfQuota {
            key_nullifier,
            proof: PoQProof::new(pi_a, pi_b, pi_c),
        })
    }

    #[must_use]
    pub const fn key_nullifier(&self) -> ZkHash {
        self.0.key_nullifier
    }

    /// Returns an unverified Proof of Quota from a verified one without
    /// performing any checks.
    ///
    /// This is useful to use a locally-generated proof in contexts where an
    /// unverified one is expected.
    #[must_use]
    pub const fn from_proof_of_quota_unchecked(proof: ProofOfQuota) -> Self {
        Self(proof)
    }
}

impl From<VerifiedProofOfQuota> for ProofOfQuota {
    fn from(value: VerifiedProofOfQuota) -> Self {
        value.0
    }
}

impl AsRef<ProofOfQuota> for VerifiedProofOfQuota {
    fn as_ref(&self) -> &ProofOfQuota {
        &self.0
    }
}

impl PartialEq<ProofOfQuota> for VerifiedProofOfQuota {
    fn eq(&self, other: &ProofOfQuota) -> bool {
        self.0 == *other
    }
}

const DOMAIN_SEPARATION_TAG: [u8; 23] = *b"SELECTION_RANDOMNESS_V1";
static DOMAIN_SEPARATION_TAG_FR: LazyLock<ZkHash> = LazyLock::new(|| {
    fr_from_bytes(&DOMAIN_SEPARATION_TAG[..])
        .expect("DST for secret selection randomness calculation must be correct.")
});
// As per Proof of Quota v1 spec: <https://www.notion.so/nomos-tech/Proof-of-Quota-Specification-215261aa09df81d88118ee22205cbafe?source=copy_link#215261aa09df81adb8ccd1448c9afd68>.
fn generate_secret_selection_randomness(sk: ZkHash, key_index: u64, session: u64) -> ZkHash {
    [
        *DOMAIN_SEPARATION_TAG_FR,
        sk,
        key_index.into(),
        session.into(),
    ]
    .hash()
}

fn split_proof_components<G1Compressed, G2Compressed>(
    bytes: [u8; PROOF_CIRCUIT_SIZE],
) -> (
    GenericArray<u8, G1Compressed>,
    GenericArray<u8, G2Compressed>,
    GenericArray<u8, G1Compressed>,
)
where
    G1Compressed: ArrayLength,
    G2Compressed: ArrayLength,
{
    let first_point_end_index = G1Compressed::USIZE;
    let second_point_end_index = first_point_end_index
        .checked_add(G2Compressed::USIZE)
        .expect("Second index overflow");
    let third_point_end_index = second_point_end_index
        .checked_add(G1Compressed::USIZE)
        .expect("Third index overflow");

    (
        GenericArray::try_from_iter(
            bytes
                .get(..first_point_end_index)
                .expect("Input byte array is not large enough for the first G1 compressed point.")
                .iter()
                .copied(),
        )
        .unwrap(),
        GenericArray::try_from_iter(
            bytes
                .get(first_point_end_index..second_point_end_index)
                .expect("Input byte array is not large enough for the first G2 compressed point.")
                .iter()
                .copied(),
        )
        .unwrap(),
        GenericArray::try_from_iter(
            bytes
                .get(second_point_end_index..third_point_end_index)
                .expect("Input byte array is not large enough for the second G1 compressed point.")
                .iter()
                .copied(),
        )
        .unwrap(),
    )
}
