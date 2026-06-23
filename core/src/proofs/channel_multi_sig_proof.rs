use std::cmp::Ordering;

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
    #[error("Signature indices are not strictly increasing: {0:?}.")]
    IndicesNotStrictlyIncreasing(Vec<ChannelKeyIndex>),
    #[error("Too many signatures: got {actual}, maximum allowed is {maximum}.")]
    TooManySignatures { actual: usize, maximum: usize },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChannelMultiSigProof {
    // Invariant: signature indices are strictly increasing (hence ordered and
    // unique), as required by the spec. Upheld by `new` AND by the custom
    // `Deserialize` impl below, so a non-monotonic proof is unrepresentable no
    // matter how it is constructed — every consumer can rely on the invariant
    // without re-checking.
    signatures: Vec<IndexedSignature>,
}

impl<'de> Deserialize<'de> for ChannelMultiSigProof {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Deserialize the raw fields, then route through `new` so the well-formedness
        // invariant holds for serde paths too (e.g. the JSON mempool path). Without
        // this, the derived `Deserialize` would let a caller construct a proof with
        // non-monotonic / duplicate signature indices, defeating threshold checks.
        #[derive(Deserialize)]
        struct Raw {
            signatures: Vec<IndexedSignature>,
        }
        let Raw { signatures } = Raw::deserialize(deserializer)?;
        Self::new(signatures).map_err(serde::de::Error::custom)
    }
}

impl ChannelMultiSigProof {
    pub fn new(signatures: Vec<IndexedSignature>) -> Result<Self, Error> {
        Self::validate_well_formedness(&signatures)?;
        Ok(Self { signatures })
    }

    /// Validates that the proof is structurally well-formed: signature indices
    /// must be strictly increasing (so they are ordered and unique, per the
    /// `CHANNEL_CONFIG` / `CHANNEL_WITHDRAW` spec), and the count must not
    /// exceed `ChannelKeyIndex::MAX`.
    ///
    /// This validates structural correctness only. Cryptographic validity
    /// (signature verification, threshold requirements, index-to-key
    /// correspondence) must be checked separately.
    fn validate_well_formedness(signatures: &[IndexedSignature]) -> Result<(), Error> {
        if signatures
            .windows(2)
            .any(|w| w[0].channel_key_index >= w[1].channel_key_index)
        {
            return Err(Error::IndicesNotStrictlyIncreasing(
                signatures.iter().map(|s| s.channel_key_index).collect(),
            ));
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
    fn rejects_repeated_index() {
        // Same index twice (distinct sigs): not strictly increasing, so rejected.
        let signatures = vec![
            IndexedSignature::new(0, sig(1)),
            IndexedSignature::new(0, sig(2)),
        ];
        assert!(matches!(
            ChannelMultiSigProof::new(signatures),
            Err(Error::IndicesNotStrictlyIncreasing(_))
        ));
    }

    #[test]
    fn rejects_unsorted_indices() {
        // Unique but not strictly increasing (descending): rejected (we no longer
        // silently sort — the spec asserts monotonic order).
        let signatures = vec![
            IndexedSignature::new(1, sig(1)),
            IndexedSignature::new(0, sig(2)),
        ];
        assert!(matches!(
            ChannelMultiSigProof::new(signatures),
            Err(Error::IndicesNotStrictlyIncreasing(_))
        ));
    }

    #[test]
    fn accepts_strictly_increasing_indices() {
        let signatures = vec![
            IndexedSignature::new(0, sig(1)),
            IndexedSignature::new(1, sig(2)),
        ];
        let proof = ChannelMultiSigProof::new(signatures)
            .expect("strictly-increasing indices are well-formed");
        assert_eq!(proof.signatures().len(), 2);
    }

    /// Regression test for #2985: a non-monotonic proof must be unrepresentable
    /// via serde too, not just via `new`. The derived `Deserialize` would have
    /// let the JSON mempool path bypass the well-formedness check; the custom
    /// `Deserialize` routes through `new`, so deserialization fails.
    #[test]
    fn deserialize_rejects_non_monotonic_indices() {
        // Two distinct signatures sharing index 0 — not strictly increasing, so
        // `new` (and now `Deserialize`) must reject it.
        let raw = vec![
            IndexedSignature::new(0, sig(1)),
            IndexedSignature::new(0, sig(2)),
        ];
        let json = format!(
            "{{\"signatures\":{}}}",
            serde_json::to_string(&raw).expect("serialize signatures")
        );
        assert!(
            serde_json::from_str::<ChannelMultiSigProof>(&json).is_err(),
            "a non-monotonic proof must not be deserializable"
        );

        // A well-formed proof still round-trips.
        let ok = ChannelMultiSigProof::new(vec![
            IndexedSignature::new(0, sig(1)),
            IndexedSignature::new(1, sig(2)),
        ])
        .expect("distinct indices are well-formed");
        let round_tripped: ChannelMultiSigProof =
            serde_json::from_str(&serde_json::to_string(&ok).expect("serialize proof"))
                .expect("well-formed proof round-trips");
        assert_eq!(round_tripped, ok);
    }
}
