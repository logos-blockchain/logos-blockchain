use std::{cmp::Ordering, collections::HashSet};

use lb_key_management_system_keys::keys::Ed25519Signature;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mantle::ops::channel::ChannelKeyIndex;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedSignature {
    pub channel_key_index: ChannelKeyIndex, /* Using ChannelKeyIndex ensures indices are
                                             * bounded, and MAX provides an upper limit for the
                                             * number of unique signatures (one per index) */
    pub signature: Ed25519Signature,
}

impl IndexedSignature {
    #[must_use]
    pub const fn new(channel_key_index: ChannelKeyIndex, signature: Ed25519Signature) -> Self {
        Self {
            channel_key_index,
            signature,
        }
    }
}

impl From<(ChannelKeyIndex, Ed25519Signature)> for IndexedSignature {
    fn from((index, signature): (ChannelKeyIndex, Ed25519Signature)) -> Self {
        Self::new(index, signature)
    }
}

impl PartialOrd<Self> for IndexedSignature {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IndexedSignature {
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
pub struct ChannelMultiSigProof {
    // Invariant: signatures are sorted by index (then signature) with no duplicates
    signatures: Vec<IndexedSignature>,
}

impl ChannelMultiSigProof {
    pub fn new(signatures: Vec<IndexedSignature>) -> Result<Self, Error> {
        let signatures = Self::normalize_signatures(signatures);
        Self::validate_well_formedness(&signatures)?;
        Ok(Self { signatures })
    }

    /// Sorts and removes duplicate signatures.
    ///
    /// This is required for the Proof to be well-formed, but it's not
    /// sufficient for the Proof to be valid.
    fn normalize_signatures(mut signatures: Vec<IndexedSignature>) -> Vec<IndexedSignature> {
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
    fn validate_well_formedness(signatures: &[IndexedSignature]) -> Result<(), Error> {
        let mut seen = HashSet::with_capacity(signatures.len());
        for sig in signatures {
            if !seen.insert(sig.channel_key_index) {
                return Err(Error::DuplicateIndices(
                    signatures.iter().map(|s| s.channel_key_index).collect(),
                ));
            }
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
    pub const fn signatures(&self) -> &Vec<IndexedSignature> {
        &self.signatures
    }
}

impl TryFrom<Vec<IndexedSignature>> for ChannelMultiSigProof {
    type Error = Error;

    fn try_from(value: Vec<IndexedSignature>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(byte: u8) -> Ed25519Signature {
        Ed25519Signature::from_bytes(&[byte; 64])
    }

    #[test]
    fn rejects_same_index_with_different_signatures() {
        // Same index, distinct signatures: survives `dedup` (full-equality), so
        // `validate_well_formedness` must reject it.
        let signatures = vec![
            IndexedSignature::new(0, sig(1)),
            IndexedSignature::new(0, sig(2)),
        ];
        assert!(matches!(
            ChannelMultiSigProof::new(signatures),
            Err(Error::DuplicateIndices(_))
        ));
    }

    #[test]
    fn accepts_distinct_indices() {
        let signatures = vec![
            IndexedSignature::new(0, sig(1)),
            IndexedSignature::new(1, sig(2)),
        ];
        let proof =
            ChannelMultiSigProof::new(signatures).expect("distinct indices are well-formed");
        assert_eq!(proof.signatures().len(), 2);
    }

    #[test]
    fn deduplicates_identical_signatures() {
        let signatures = vec![
            IndexedSignature::new(0, sig(1)),
            IndexedSignature::new(0, sig(1)),
        ];
        let proof =
            ChannelMultiSigProof::new(signatures).expect("identical signatures are deduplicated");
        assert_eq!(proof.signatures().len(), 1);
    }
}
