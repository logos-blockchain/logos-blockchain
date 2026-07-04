use std::cmp::Ordering;

use lb_core_macros::NomCodec;
use lb_key_management_system_keys::keys::Ed25519Signature;
use lb_utils::bounded_vec::UpperBoundedVec;
use nom::{
    Err,
    error::{Error as NomError, ErrorKind},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mantle::{
    nom::{NomDecode, NomEncode},
    ops::channel::ChannelKeyIndex,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, NomCodec)]
pub struct IndexedSignature {
    pub signature: Ed25519Signature,
    pub channel_key_index: ChannelKeyIndex, /* Using ChannelKeyIndex ensures indices are
                                             * bounded, and MAX provides an upper limit for the
                                             * number of unique signatures (one per index) */
}

impl IndexedSignature {
    #[must_use]
    pub const fn new(channel_key_index: ChannelKeyIndex, signature: Ed25519Signature) -> Self {
        Self {
            signature,
            channel_key_index,
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

pub const MAX_SIGNATURES: usize = u16::MAX as usize;
pub type IndexedSignatures = UpperBoundedVec<IndexedSignature, MAX_SIGNATURES>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// Serde goes through `ChannelMultiSigProofRepr` via `try_from`/`into`: `Deserialize`
// routes through `new`, so the well-formedness invariant (strictly-increasing
// indices) is upheld on every serde path too — a non-monotonic proof is
// unrepresentable no matter how it is constructed, and no consumer needs to
// re-check. The repr keeps the `{ "signatures": [..] }` wire form.
#[serde(
    try_from = "ChannelMultiSigProofRepr",
    into = "ChannelMultiSigProofRepr"
)]
pub struct ChannelMultiSigProof {
    // Invariant: signature indices are strictly increasing (hence ordered and
    // unique), as required by the spec.
    signatures: IndexedSignatures,
}

impl NomEncode for ChannelMultiSigProof {
    fn encode(&self) -> Vec<u8> {
        self.signatures.encode()
    }
}

impl NomDecode for ChannelMultiSigProof {
    fn decode(bytes: &[u8]) -> nom::IResult<&[u8], Self> {
        let (remaining_bytes, inner) = IndexedSignatures::decode(bytes)?;
        let proof = Self::try_new(inner)
            .map_err(|_| Err::Error(NomError::new(bytes, ErrorKind::MapRes)))?;
        Ok((remaining_bytes, proof))
    }
}

/// Serde wire representation of [`ChannelMultiSigProof`] — a struct with a
/// `signatures` field. Kept separate so the public type's (de)serialization
/// is forced through `new` (via the `TryFrom`/`From` impls below) while
/// preserving the `{ "signatures": [..] }` JSON shape.
#[derive(Serialize, Deserialize)]
struct ChannelMultiSigProofRepr {
    signatures: IndexedSignatures,
}

impl TryFrom<ChannelMultiSigProofRepr> for ChannelMultiSigProof {
    type Error = Error;

    fn try_from(repr: ChannelMultiSigProofRepr) -> Result<Self, Self::Error> {
        Self::try_new(repr.signatures)
    }
}

impl From<ChannelMultiSigProof> for ChannelMultiSigProofRepr {
    fn from(proof: ChannelMultiSigProof) -> Self {
        Self {
            signatures: proof.signatures,
        }
    }
}

impl ChannelMultiSigProof {
    pub fn try_new(signatures: IndexedSignatures) -> Result<Self, Error> {
        let signatures = Self::validate_well_formedness(signatures)?;
        Ok(Self { signatures })
    }

    /// Validates that the proof is structurally well-formed: signature indices
    /// must be strictly increasing (so they are ordered and unique, per the
    /// `CHANNEL_CONFIG` / `CHANNEL_WITHDRAW` spec).
    ///
    /// This validates structural correctness only. Cryptographic validity
    /// (signature verification, threshold requirements, index-to-key
    /// correspondence) must be checked separately.
    fn validate_well_formedness(signatures: IndexedSignatures) -> Result<IndexedSignatures, Error> {
        if signatures
            .windows(2)
            .any(|w| w[0].channel_key_index >= w[1].channel_key_index)
        {
            return Err(Error::IndicesNotStrictlyIncreasing(
                signatures.iter().map(|s| s.channel_key_index).collect(),
            ));
        }
        Ok(signatures)
    }

    #[must_use]
    pub fn signatures(&self) -> &[IndexedSignature] {
        self.signatures.as_slice()
    }
}

pub mod codec {
    use lb_key_management_system_keys::keys::ED25519_SIGNATURE_SIZE;

    use crate::mantle::ops::channel::ChannelKeyIndex;

    #[must_use]
    pub const fn calculate_channel_multi_sig_proof_byte_size(threshold: ChannelKeyIndex) -> usize {
        // Encoding: u16 signature count + N * (Ed25519 sig + u16 key index)
        2 + (threshold as usize) * (ED25519_SIGNATURE_SIZE + 2)
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
        let signatures = [
            IndexedSignature::new(0, sig(1)),
            IndexedSignature::new(0, sig(2)),
        ];
        assert!(matches!(
            ChannelMultiSigProof::try_new(signatures.into()),
            Err(Error::IndicesNotStrictlyIncreasing(_))
        ));
    }

    #[test]
    fn rejects_unsorted_indices() {
        // Unique but not strictly increasing (descending): rejected (we no longer
        // silently sort — the spec asserts monotonic order).
        let signatures = [
            IndexedSignature::new(1, sig(1)),
            IndexedSignature::new(0, sig(2)),
        ];
        assert!(matches!(
            ChannelMultiSigProof::try_new(signatures.into()),
            Err(Error::IndicesNotStrictlyIncreasing(_))
        ));
    }

    #[test]
    fn accepts_strictly_increasing_indices() {
        let signatures = [
            IndexedSignature::new(0, sig(1)),
            IndexedSignature::new(1, sig(2)),
        ];
        let proof = ChannelMultiSigProof::try_new(signatures.into())
            .expect("strictly-increasing indices are well-formed");
        assert_eq!(proof.signatures().len(), 2);
    }

    /// Regression test for #2985: a non-monotonic proof must be unrepresentable
    /// via serde too, not just via `new`. A derived `Deserialize` would have
    /// let the JSON mempool path bypass the well-formedness check; routing
    /// serde through `new` (via `#[serde(try_from)]`) makes deserialization
    /// fail.
    #[test]
    fn deserialize_rejects_non_monotonic_indices() {
        // Two distinct signatures sharing index 0 — not strictly increasing, so
        // `new` (and now `Deserialize`) must reject it.
        let raw = [
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

        // A well-formed proof still round-trips, and keeps the `{ "signatures": [..] }`
        // JSON shape.
        let ok = ChannelMultiSigProof::try_new(
            [
                IndexedSignature::new(0, sig(1)),
                IndexedSignature::new(1, sig(2)),
            ]
            .into(),
        )
        .expect("distinct indices are well-formed");
        let serialized = serde_json::to_string(&ok).expect("serialize proof");
        assert!(
            serialized.starts_with("{\"signatures\":"),
            "expected the `{{ signatures: [..] }}` shape, got {serialized}"
        );
        let round_tripped: ChannelMultiSigProof =
            serde_json::from_str(&serialized).expect("well-formed proof round-trips");
        assert_eq!(round_tripped, ok);
    }

    /// The nom decoder must uphold the same well-formedness invariant as `new`:
    /// a wire-encoded vector with a repeated index is not strictly increasing,
    /// so `decode` must fail (the `try_new` inside `decode` maps to a nom
    /// error).
    #[test]
    fn decode_rejects_repeated_index() {
        // Encode a raw vector (bypassing `ChannelMultiSigProof`) with the same
        // index twice, then decode it back through the proof's nom path.
        let raw: IndexedSignatures = [
            IndexedSignature::new(0, sig(1)),
            IndexedSignature::new(0, sig(2)),
        ]
        .into();
        let bytes = raw.encode();
        assert!(
            ChannelMultiSigProof::decode(&bytes).is_err(),
            "decoding a proof with a repeated index must fail"
        );
    }

    /// Companion to the above: a wire-encoded vector whose indices are unique
    /// but out of order (descending) is also not strictly increasing, so the
    /// nom decoder must reject it.
    #[test]
    fn decode_rejects_out_of_order_indices() {
        let raw: IndexedSignatures = [
            IndexedSignature::new(1, sig(1)),
            IndexedSignature::new(0, sig(2)),
        ]
        .into();
        let bytes = raw.encode();
        assert!(
            ChannelMultiSigProof::decode(&bytes).is_err(),
            "decoding a proof with out-of-order indices must fail"
        );
    }
}
