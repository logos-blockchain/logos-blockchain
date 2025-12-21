use blake2::{
    Blake2bVar,
    digest::{Update as _, VariableOutput as _},
};
use key_management_system_keys::keys::Ed25519PublicKey;
use nomos_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};
use nomos_core::codec::SerializeOp as _;
use serde::{Deserialize, Serialize};

use crate::reward::session::SessionRandomness;

/// A blending token consisting of a proof of quota and a proof of selection.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlendingToken {
    signing_key: Ed25519PublicKey,
    proof_of_quota: VerifiedProofOfQuota,
    proof_of_selection: VerifiedProofOfSelection,
}

impl BlendingToken {
    #[must_use]
    pub const fn new(
        signing_key: Ed25519PublicKey,
        proof_of_quota: VerifiedProofOfQuota,
        proof_of_selection: VerifiedProofOfSelection,
    ) -> Self {
        Self {
            signing_key,
            proof_of_quota,
            proof_of_selection,
        }
    }

    /// Computes the Hamming distance between this blending token and the next
    /// session randomness.
    #[must_use]
    pub fn hamming_distance(
        &self,
        token_count_byte_len: u64,
        next_session_randomness: SessionRandomness,
    ) -> HammingDistance {
        let token = self
            .to_bytes()
            .expect("BlendingToken should be serializable");
        let token_hash = hash(&token, token_count_byte_len as usize);
        let session_randomness_hash = hash(&next_session_randomness, token_count_byte_len as usize);

        HammingDistance::new(&token_hash, &session_randomness_hash)
    }

    pub(crate) const fn signing_key(&self) -> &Ed25519PublicKey {
        &self.signing_key
    }

    pub(crate) const fn proof_of_quota(&self) -> &VerifiedProofOfQuota {
        &self.proof_of_quota
    }

    pub(crate) const fn proof_of_selection(&self) -> &VerifiedProofOfSelection {
        &self.proof_of_selection
    }
}

/// Compute blake-2b hash of `input`, producing `output_size` bytes.
///
/// If `output_size` is greater than the maximum supported size, it will be
/// reduced to that maximum.
/// If `output_size` is zero, an empty vector will be returned.
fn hash(input: &[u8], output_size: usize) -> Vec<u8> {
    let output_size = output_size.min(Blake2bVar::MAX_OUTPUT_SIZE);
    let mut hasher = Blake2bVar::new(output_size).expect("output size should be valid");
    hasher.update(input);
    let mut output = vec![0u8; output_size];
    hasher.finalize_variable(&mut output).unwrap();
    output
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HammingDistance(u64);

impl HammingDistance {
    pub const MAX: Self = Self(u64::MAX);

    /// Computes the Hamming distance between two byte slices.
    /// (i.e. the number of differing bits)
    ///
    /// If the slices have different lengths, the extra bytes in the longer
    /// slice are silently ignored.
    #[must_use]
    pub fn new(a: &[u8], b: &[u8]) -> Self {
        Self(
            a.iter()
                .zip(b)
                .map(|(x, y)| u64::from((x ^ y).count_ones()))
                .sum(),
        )
    }
}

impl From<u64> for HammingDistance {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use key_management_system_keys::keys::Ed25519Key;
    use nomos_blend_proofs::{quota::PROOF_OF_QUOTA_SIZE, selection::PROOF_OF_SELECTION_SIZE};

    use super::*;

    #[test]
    fn test_hamming_distance() {
        assert_eq!(
            HammingDistance::new(&[0b1010_1010, 0b1100_1100], &[0b1010_1010, 0b1100_1100]),
            0.into(),
        );
        assert_eq!(
            HammingDistance::new(&[0b1010_1010, 0b1100_1100], &[0b0101_0101, 0b0011_0011]),
            16.into()
        );
        assert_eq!(
            HammingDistance::new(&[0b1111_1111, 0b1111_1111], &[0b0000_0000]),
            8.into()
        );
        assert_eq!(HammingDistance::new(&[], &[]), 0.into());
    }

    #[test]
    fn test_hash() {
        let input = b"test data";

        // Check if the output length matches the requested size.
        let output = hash(input, 3);
        assert_eq!(output.len(), 3);

        // Check if `hash` is deterministic.
        assert_eq!(output, hash(input, 3));

        // An empty output if the request size is zero.
        assert!(hash(input, 0).is_empty());

        // Output shouldn't be longer than the maximum size.
        let output = hash(input, Blake2bVar::MAX_OUTPUT_SIZE.checked_add(1).unwrap());
        assert_eq!(output.len(), Blake2bVar::MAX_OUTPUT_SIZE);
    }

    #[test]
    fn test_blending_token_hamming_distance() {
        let token = blending_token(1, 1, 2);
        assert_eq!(token.hamming_distance(1, [3u8; 64].into()), 2.into());
    }

    fn blending_token(
        signing_key: u8,
        proof_of_quota: u8,
        proof_of_selection: u8,
    ) -> BlendingToken {
        BlendingToken {
            signing_key: Ed25519Key::from_bytes(&[signing_key; _]).public_key(),
            proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked(
                [proof_of_quota; PROOF_OF_QUOTA_SIZE],
            ),
            proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked(
                [proof_of_selection; PROOF_OF_SELECTION_SIZE],
            ),
        }
    }
}
