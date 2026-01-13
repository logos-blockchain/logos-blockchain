use core::fmt::Debug;
use std::sync::LazyLock;

use groth16::{fr_from_bytes, fr_from_bytes_unchecked, fr_to_bytes};
use nomos_blend_crypto::pseudo_random_sized_bytes;
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ZkCompressExt as _, ZkHash, selection::inputs::VerifyInputs};

pub mod inputs;

#[cfg(test)]
mod tests;

pub const PROOF_OF_SELECTION_SIZE: usize = size_of::<ProofOfSelection>();
const DOMAIN_SEPARATION_TAG: [u8; 9] = *b"BlendNode";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Index mismatch. Expected {expected:?}, provided {provided}.")]
    IndexMismatch {
        // `Some` if the provided index is less than the membership size, `None` otherwise, since
        // we skip computing the expected index if the provided one would fail regardless of the
        // calculated value because it's too large.
        expected: Option<u64>,
        provided: u64,
    },
    #[error("Overflow when verifying PoSel.")]
    Overflow,
    #[error("Key nullifier mismatch. Expected {expected}, provided {provided}.")]
    KeyNullifierMismatch { expected: ZkHash, provided: ZkHash },
    #[error("Invalid input: {0}.")]
    InvalidInput(Box<dyn core::error::Error>),
    #[error("Proof of Selection verification failed.")]
    Verification,
    #[error("Empty membership.")]
    EmptyMembershipSet,
}

/// A Proof of Selection as described in the Blend v1 spec: <https://www.notion.so/nomos-tech/Blend-Protocol-215261aa09df81ae8857d71066a80084?source=copy_link#215261aa09df81d6bb3febd62b598138>.
#[derive(Clone, Debug, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ProofOfSelection {
    #[serde(with = "groth16::serde::serde_fr")]
    selection_randomness: ZkHash,
}

impl ProofOfSelection {
    /// Returns the index the Proof of Selection refers to, for the provided
    /// membership size.
    pub fn expected_index(&self, membership_size: usize) -> Result<usize, Error> {
        if membership_size == 0 {
            return Err(Error::EmptyMembershipSet);
        }
        // Condition 1: https://www.notion.so/nomos-tech/Blend-Protocol-215261aa09df81ae8857d71066a80084?source=copy_link#215261aa09df819991e6f9455ff7ec92
        let selection_randomness_bytes = fr_to_bytes(&self.selection_randomness);
        let pseudo_random_output: u64 = {
            let pseudo_random_output_bytes =
                pseudo_random_sized_bytes::<8>(&DOMAIN_SEPARATION_TAG, &selection_randomness_bytes);
            let pseudo_random_biguint = BigUint::from_bytes_le(&pseudo_random_output_bytes[..]);
            pseudo_random_biguint
                .try_into()
                .map_err(|_| Error::Overflow)?
        };
        (pseudo_random_output % u64::try_from(membership_size).map_err(|_| Error::Overflow)?)
            .try_into()
            .map_err(|_| Error::Overflow)
    }

    pub fn verify(
        self,
        VerifyInputs {
            expected_node_index,
            key_nullifier,
            total_membership_size,
        }: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Error> {
        if expected_node_index >= total_membership_size {
            return Err(Error::IndexMismatch {
                expected: None,
                provided: *expected_node_index,
            });
        }
        let final_index = self.expected_index(*total_membership_size as usize)?;
        if final_index != *expected_node_index as usize {
            return Err(Error::IndexMismatch {
                expected: Some(final_index as u64),
                provided: *expected_node_index,
            });
        }

        // Condition 2: https://www.notion.so/nomos-tech/Blend-Protocol-215261aa09df81ae8857d71066a80084?source=copy_link#215261aa09df814da8e8ec1f1fcf4fe6
        let calculated_key_nullifier =
            derive_key_nullifier_from_secret_selection_randomness(self.selection_randomness);
        if calculated_key_nullifier != *key_nullifier {
            return Err(Error::KeyNullifierMismatch {
                expected: calculated_key_nullifier,
                provided: *key_nullifier,
            });
        }

        Ok(VerifiedProofOfSelection(self))
    }
}

impl PartialEq<VerifiedProofOfSelection> for ProofOfSelection {
    fn eq(&self, other: &VerifiedProofOfSelection) -> bool {
        *self == other.0
    }
}

impl From<&ProofOfSelection> for [u8; PROOF_OF_SELECTION_SIZE] {
    fn from(proof: &ProofOfSelection) -> Self {
        fr_to_bytes(&proof.selection_randomness)
    }
}

impl TryFrom<[u8; PROOF_OF_SELECTION_SIZE]> for ProofOfSelection {
    type Error = Box<dyn std::error::Error>;

    fn try_from(value: [u8; PROOF_OF_SELECTION_SIZE]) -> Result<Self, Self::Error> {
        Ok(Self {
            selection_randomness: fr_from_bytes(&value).map_err(Box::new)?,
        })
    }
}

/// A verified Proof of Selection.
#[derive(Clone, Debug, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct VerifiedProofOfSelection(ProofOfSelection);

impl VerifiedProofOfSelection {
    #[must_use]
    pub const fn new(selection_randomness: ZkHash) -> Self {
        Self(ProofOfSelection {
            selection_randomness,
        })
    }

    /// Returns the index the Proof of Selection refers to, for the provided
    /// membership size.
    pub fn expected_index(&self, membership_size: usize) -> Result<usize, Error> {
        self.0.expected_index(membership_size)
    }

    #[must_use]
    pub fn from_bytes_unchecked(bytes: [u8; PROOF_OF_SELECTION_SIZE]) -> Self {
        Self(ProofOfSelection {
            selection_randomness: fr_from_bytes_unchecked(&bytes),
        })
    }

    #[must_use]
    pub const fn into_inner(self) -> ProofOfSelection {
        self.0
    }

    #[must_use]
    pub const fn from_proof_of_selection_unchecked(proof: ProofOfSelection) -> Self {
        Self(proof)
    }
}

impl From<VerifiedProofOfSelection> for ProofOfSelection {
    fn from(value: VerifiedProofOfSelection) -> Self {
        value.0
    }
}

impl AsRef<ProofOfSelection> for VerifiedProofOfSelection {
    fn as_ref(&self) -> &ProofOfSelection {
        &self.0
    }
}

impl PartialEq<ProofOfSelection> for VerifiedProofOfSelection {
    fn eq(&self, other: &ProofOfSelection) -> bool {
        self.0 == *other
    }
}

const KEY_NULLIFIER_DERIVATION_DOMAIN_SEPARATION_TAG: [u8; 16] = *b"KEY_NULLIFIER_V1";
static KEY_NULLIFIER_DERIVATION_DOMAIN_SEPARATION_TAG_FR: LazyLock<ZkHash> = LazyLock::new(|| {
    fr_from_bytes(&KEY_NULLIFIER_DERIVATION_DOMAIN_SEPARATION_TAG[..]).expect(
        "DST for key nullifier derivation from secret selection randomness must be correct.",
    )
});
// As per Proof of Quota v1 spec: <https://www.notion.so/nomos-tech/Proof-of-Quota-Specification-215261aa09df81d88118ee22205cbafe?source=copy_link#215261aa09df81adb8ccd1448c9afd68>.
#[must_use]
pub fn derive_key_nullifier_from_secret_selection_randomness(
    secret_selection_randomness: ZkHash,
) -> ZkHash {
    [
        *KEY_NULLIFIER_DERIVATION_DOMAIN_SEPARATION_TAG_FR,
        secret_selection_randomness,
    ]
    .compress()
}
