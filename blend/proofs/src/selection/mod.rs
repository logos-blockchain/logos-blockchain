use core::fmt::Debug;
use std::sync::LazyLock;

use lb_blend_crypto::pseudo_random_sized_bytes;
use lb_codec::{BinaryDecode, BinaryEncode, DecodeError};
use lb_groth16::{fr_from_bytes, fr_from_bytes_unchecked, fr_to_bytes};
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

/// A Proof of Selection as described in the Blend spec: <https://lip.logos.co/blockchain/raw/blend-protocol.html#proof-of-selection>.
#[derive(Clone, Debug, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ProofOfSelection {
    #[serde(with = "lb_groth16::serde::serde_fr")]
    selection_randomness: ZkHash,
}

impl ProofOfSelection {
    /// Returns the index the Proof of Selection refers to, for the provided
    /// membership size.
    pub fn expected_index(&self, membership_size: usize) -> Result<usize, Error> {
        if membership_size == 0 {
            return Err(Error::EmptyMembershipSet);
        }
        // Condition 1: https://lip.logos.co/blockchain/raw/blend-protocol.html#proof-of-selection
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

        // Condition 2: https://lip.logos.co/blockchain/raw/blend-protocol.html#proof-of-selection
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

impl BinaryEncode for ProofOfSelection {
    fn encoded_length(&self) -> usize {
        PROOF_OF_SELECTION_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&<[u8; _]>::from(self));
    }
}

impl BinaryDecode for ProofOfSelection {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, value) = <[u8; _]>::decode(input, &())?;
        let proof = Self::try_from(value)
            .map_err(|_| DecodeError::invalid_value::<Self>("not a valid proof of selection"))?;
        Ok((rest, proof))
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

impl BinaryEncode for VerifiedProofOfSelection {
    fn encoded_length(&self) -> usize {
        self.0.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.0.encode_into(out);
    }
}

const KEY_NULLIFIER_DERIVATION_DOMAIN_SEPARATION_TAG: [u8; 16] = *b"KEY_NULLIFIER_V1";
static KEY_NULLIFIER_DERIVATION_DOMAIN_SEPARATION_TAG_FR: LazyLock<ZkHash> = LazyLock::new(|| {
    fr_from_bytes(&KEY_NULLIFIER_DERIVATION_DOMAIN_SEPARATION_TAG[..]).expect(
        "DST for key nullifier derivation from secret selection randomness must be correct.",
    )
});
// As per Proof of Quota spec: <https://lip.logos.co/blockchain/raw/proof-of-quota.html#constraints>.
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
