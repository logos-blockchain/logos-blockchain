use std::cmp::Ordering;

use lb_key_management_system_keys::keys::Ed25519Signature;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mantle::ops::channel::ChannelKeyIndex;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawSignature {
    pub channel_key_index: ChannelKeyIndex, /* Using ChannelKeyIndex ensures indices are
                                             * bounded, and MAX provides an upper limit for the
                                             * number of unique signatures (one per index) */
    pub signature: Ed25519Signature,
}

impl WithdrawSignature {
    #[must_use]
    pub const fn new(channel_key_index: ChannelKeyIndex, signature: Ed25519Signature) -> Self {
        Self {
            channel_key_index,
            signature,
        }
    }
}

impl From<(ChannelKeyIndex, Ed25519Signature)> for WithdrawSignature {
    fn from((index, signature): (ChannelKeyIndex, Ed25519Signature)) -> Self {
        Self::new(index, signature)
    }
}

impl PartialOrd<Self> for WithdrawSignature {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WithdrawSignature {
    fn cmp(&self, other: &Self) -> Ordering {
        self.channel_key_index
            .cmp(&other.channel_key_index)
            .then_with(|| self.signature.to_bytes().cmp(&other.signature.to_bytes()))
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Duplicate indices found: {0:?}.")]
    DuplicateIndices(Vec<ChannelKeyIndex>),
    #[error("Too many signatures: got {actual}, maximum allowed is {maximum}.")]
    TooManySignatures { actual: usize, maximum: usize },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelWithdrawProof {
    // Invariant: signatures are sorted by index (then signature) with no duplicates
    signatures: Vec<WithdrawSignature>,
}

impl ChannelWithdrawProof {
    pub fn new(signatures: Vec<WithdrawSignature>) -> Result<Self, Error> {
        let signatures = Self::normalize_signatures(signatures);
        Self::validate_well_formedness(&signatures)?;
        Ok(Self { signatures })
    }

    /// Sorts and removes duplicate signatures.
    ///
    /// This is required for the Proof to be well-formed, but it's not
    /// sufficient for the Proof to be valid.
    fn normalize_signatures(mut signatures: Vec<WithdrawSignature>) -> Vec<WithdrawSignature> {
        signatures.sort_unstable();
        signatures.dedup();
        signatures
    }

    /// Validates that the proof is structurally well-formed.
    ///
    /// Must be called after [`Self::normalize_signatures`].
    ///
    /// # Checks
    ///
    /// - No duplicate indices (each index appears at most once)
    /// - Signature count doesn't exceed `ChannelKeyIndex::MAX`
    ///
    /// # Note
    ///
    /// This validates structural correctness only. Cryptographic validity
    /// (e.g.: signature verification, threshold requirements, index-to-key
    /// correspondence) must be checked separately.
    fn validate_well_formedness(signatures: &[WithdrawSignature]) -> Result<(), Error> {
        let unique_indices = signatures
            .iter()
            .map(|signature| signature.channel_key_index)
            .collect::<Vec<_>>();
        if unique_indices.len() != signatures.len() {
            return Err(Error::DuplicateIndices(unique_indices));
        }
        let max_signatures_allowed = usize::from(ChannelKeyIndex::MAX) + 1;
        if signatures.len() > max_signatures_allowed {
            return Err(Error::TooManySignatures {
                actual: signatures.len(),
                maximum: max_signatures_allowed,
            });
        }
        Ok(())
    }

    #[must_use]
    pub const fn signatures(&self) -> &Vec<WithdrawSignature> {
        &self.signatures
    }
}

impl TryFrom<Vec<WithdrawSignature>> for ChannelWithdrawProof {
    type Error = Error;

    fn try_from(value: Vec<WithdrawSignature>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
