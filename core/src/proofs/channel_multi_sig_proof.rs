use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
use lb_key_management_system_keys::keys::Ed25519Signature;
use lb_utils::bounded::UpperBoundedBTreeMap;
use serde::{Deserialize, Serialize};

use crate::mantle::ops::channel::ChannelKeyIndex;

/// A signature together with the index of the channel key that made it, as a
/// key holder hands it over before the signatures are gathered into a
/// [`ChannelMultiSigProof`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedSignature {
    pub signature: Ed25519Signature,
    pub channel_key_index: ChannelKeyIndex,
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

impl From<IndexedSignature> for (ChannelKeyIndex, Ed25519Signature) {
    fn from(
        IndexedSignature {
            signature,
            channel_key_index,
        }: IndexedSignature,
    ) -> Self {
        (channel_key_index, signature)
    }
}

pub const MAX_SIGNATURES: usize = u16::MAX as usize;

/// Signatures keyed by the index of the channel key that made them.
///
/// Being a map, it holds at most one signature per key and lists them in
/// ascending index order, as the spec requires of a proof. Its canonical
/// decoding rejects a repeated or out-of-order index, so each proof has a
/// single encoding.
pub type IndexedSignatures =
    UpperBoundedBTreeMap<ChannelKeyIndex, Ed25519Signature, MAX_SIGNATURES>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMultiSigProof {
    signatures: IndexedSignatures,
}

impl ChannelMultiSigProof {
    #[must_use]
    pub const fn new(signatures: IndexedSignatures) -> Self {
        Self { signatures }
    }

    /// The proof without signatures, which configures a channel that no key
    /// has claimed yet.
    #[must_use]
    pub const fn empty() -> Self {
        Self::new(IndexedSignatures::empty())
    }

    #[must_use]
    pub const fn signatures(&self) -> &IndexedSignatures {
        &self.signatures
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample_with_signatures(signature_count: u16) -> Self {
        let signatures = IndexedSignatures::try_from_iter((0..signature_count).map(|index| {
            let [low, _] = index.to_le_bytes();
            (index, Ed25519Signature::from_bytes(&[low; 64]))
        }))
        .expect("`signature_count` is bounded by `MAX_SIGNATURES`");

        Self::new(signatures)
    }
}

impl BinaryEncode for ChannelMultiSigProof {
    fn encoded_length(&self) -> usize {
        self.signatures.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.signatures.encode_into(out);
    }
}

impl BinaryDecode for ChannelMultiSigProof {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, signatures) = IndexedSignatures::decode(input, &((), ()))?;
        Ok((rest, Self::new(signatures)))
    }
}

pub mod codec {
    use lb_key_management_system_keys::keys::ED25519_SIGNATURE_SIZE;

    use crate::mantle::ops::channel::ChannelKeyIndex;

    #[must_use]
    pub const fn calculate_channel_multi_sig_proof_byte_size(threshold: ChannelKeyIndex) -> usize {
        // Encoding: u16 signature count + N * (u16 key index + Ed25519 sig)
        2 + (threshold as usize) * (2 + ED25519_SIGNATURE_SIZE)
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub mod sample {
    use crate::{
        mantle::ops::op_proof::samples::SampleProof,
        proofs::channel_multi_sig_proof::ChannelMultiSigProof,
    };

    impl SampleProof for ChannelMultiSigProof {
        fn sample() -> Self {
            Self::sample_with_signatures(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(byte: u8) -> Ed25519Signature {
        Ed25519Signature::from_bytes(&[byte; 64])
    }

    /// Encodes `entries` as a proof would, keeping their order and repeats,
    /// so the bytes can describe a proof that cannot be built.
    fn encode_entries(entries: &[(ChannelKeyIndex, u8)]) -> Vec<u8> {
        let count = u16::try_from(entries.len()).expect("a handful of entries");
        let mut bytes = count.to_le_bytes().to_vec();
        for (index, byte) in entries {
            bytes.extend_from_slice(&index.to_le_bytes());
            bytes.extend_from_slice(&[*byte; 64]);
        }
        bytes
    }

    #[test]
    fn encodes_signatures_in_ascending_index_order() {
        let proof = ChannelMultiSigProof::new(
            IndexedSignatures::try_from([(7, sig(2)), (0, sig(1))]).unwrap(),
        );

        assert_eq!(proof.encode_to_vec(), encode_entries(&[(0, 1), (7, 2)]));
    }

    #[test]
    fn decode_rejects_repeated_index() {
        let bytes = encode_entries(&[(0, 1), (0, 2)]);

        assert!(matches!(
            ChannelMultiSigProof::decode(&bytes, &()),
            Err(DecodeError::DuplicateItem { index: 1, .. })
        ));
    }

    #[test]
    fn decode_rejects_out_of_order_indices() {
        let bytes = encode_entries(&[(1, 1), (0, 2)]);

        assert!(matches!(
            ChannelMultiSigProof::decode(&bytes, &()),
            Err(DecodeError::OutOfOrderItem { index: 1, .. })
        ));
    }

    /// Regression test for #2985: a proof read through serde must be as
    /// well-formed as one built in code. The signatures are a map keyed by
    /// index, so they come back in ascending index order whatever order the
    /// input lists them in.
    #[test]
    fn deserialize_orders_signatures_by_index() {
        let proof = ChannelMultiSigProof::new(
            IndexedSignatures::try_from([(0, sig(1)), (7, sig(2))]).unwrap(),
        );
        let signature_json = |byte| serde_json::to_string(&sig(byte)).unwrap();

        let reversed = format!(
            "{{\"signatures\":{{\"7\":{},\"0\":{}}}}}",
            signature_json(2),
            signature_json(1)
        );
        assert_eq!(
            serde_json::from_str::<ChannelMultiSigProof>(&reversed).unwrap(),
            proof
        );
        assert_eq!(
            serde_json::to_string(&proof).unwrap(),
            format!(
                "{{\"signatures\":{{\"0\":{},\"7\":{}}}}}",
                signature_json(1),
                signature_json(2)
            )
        );
    }
}
