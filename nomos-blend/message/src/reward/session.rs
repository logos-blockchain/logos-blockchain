use std::ops::{Add as _, Deref};

use groth16::fr_to_bytes;
use nomos_blend_crypto::blake2b512;
use nomos_core::{crypto::ZkHash, sdp::SessionNumber};
use nomos_utils::math::{F64Ge1, NonNegativeF64};
use serde::{Deserialize, Serialize};

use crate::reward::{BlendingToken, activity, token::HammingDistance};

/// Session-specific information to compute an activity proof.
pub struct SessionInfo {
    pub(crate) session_number: SessionNumber,
    pub(crate) session_randomness: SessionRandomness,
    pub(crate) token_evaluation: BlendingTokenEvaluation,
}

impl SessionInfo {
    pub fn new(
        session_number: SessionNumber,
        pol_epoch_nonce: &ZkHash,
        num_core_nodes: u64,
        core_quota: u64,
    ) -> Result<Self, Error> {
        let session_randomness = SessionRandomness::new(session_number, pol_epoch_nonce);
        let token_evaluation = BlendingTokenEvaluation::new(core_quota, num_core_nodes)?;

        Ok(Self {
            session_number,
            session_randomness,
            token_evaluation,
        })
    }

    #[must_use]
    pub const fn session_randomness(&self) -> SessionRandomness {
        self.session_randomness
    }
}

/// Parameters to evaluate a blending token for a session.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlendingTokenEvaluation {
    token_count_byte_len: u64,
    activity_threshold: HammingDistance,
}

impl BlendingTokenEvaluation {
    pub fn new(core_quota: u64, num_core_nodes: u64) -> Result<Self, Error> {
        let expected_token_count_bit_len = token_count_bit_len(core_quota, num_core_nodes)?;
        let activity_threshold = activity_threshold(expected_token_count_bit_len, num_core_nodes)?;

        Ok(Self {
            token_count_byte_len: expected_token_count_bit_len.div_ceil(8),
            activity_threshold,
        })
    }

    /// Calculate the Hamming distance of the given blending token to the next
    /// session randomness, and return it only if it is not larger than the
    /// activity threshold.
    #[must_use]
    pub fn evaluate(
        &self,
        token: &BlendingToken,
        next_session_randomness: SessionRandomness,
    ) -> Option<HammingDistance> {
        let distance = token.hamming_distance(self.token_count_byte_len, next_session_randomness);
        (distance <= self.activity_threshold).then_some(distance)
    }
}

/// Deterministic unbiased randomness for a session.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRandomness(#[serde(with = "serde_big_array::BigArray")] [u8; 64]);

impl Deref for SessionRandomness {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<[u8; 64]> for SessionRandomness {
    fn from(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }
}

const SESSION_RANDOMNESS_TAG: [u8; 27] = *b"BLEND_SESSION_RANDOMNESS_V1";

impl SessionRandomness {
    /// Derive the session randomness from the given session number and epoch
    /// nonce.
    #[must_use]
    pub fn new(session_number: SessionNumber, epoch_nonce: &ZkHash) -> Self {
        Self(blake2b512(&[
            &SESSION_RANDOMNESS_TAG,
            &fr_to_bytes(epoch_nonce),
            &session_number.to_le_bytes(),
        ]))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the total core quota({0}) is too large to compute the Hamming distance")]
    TotalCoreQuotaTooLarge(u64),
    #[error("the network size({0}) is too large to compute the activity threshold")]
    NetworkSizeTooLarge(u64),
}

/// The number of bits that can represent the maximum number of blending
/// tokens generated during a single session.
pub fn token_count_bit_len(core_quota: u64, num_core_nodes: u64) -> Result<u64, Error> {
    let total_core_quota = core_quota
        .checked_mul(num_core_nodes)
        .ok_or(Error::TotalCoreQuotaTooLarge(u64::MAX))?;
    let total_core_quota: NonNegativeF64 = total_core_quota
        .try_into()
        .map_err(|()| Error::TotalCoreQuotaTooLarge(total_core_quota))?;
    Ok(F64Ge1::try_from(total_core_quota.add(1.0))
        .expect("must be >= 1.0")
        .log2()
        .ceil() as u64)
}

pub fn activity_threshold(
    token_count_bit_len: u64,
    num_core_nodes: u64,
) -> Result<HammingDistance, Error> {
    let network_size_bit_len = F64Ge1::try_from(
        num_core_nodes
            .checked_add(1)
            .ok_or(Error::NetworkSizeTooLarge(num_core_nodes))?,
    )
    .map_err(|()| Error::NetworkSizeTooLarge(num_core_nodes))?
    .log2()
    .ceil() as u64;

    Ok(activity::activity_threshold(
        token_count_bit_len,
        network_size_bit_len,
    ))
}

#[cfg(test)]
mod tests {
    use key_management_system_keys::keys::Ed25519Key;
    use nomos_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};

    use super::*;

    #[test]
    fn test_activity_threshold() {
        let network_size = 127;
        let token_count_bit_len = 10;
        let threshold = activity_threshold(token_count_bit_len, network_size).unwrap();
        // 10 - log2(127+1) - 1
        assert_eq!(threshold, 2.into());

        let network_size = 0;
        let token_count_bit_len = 10;
        let threshold = activity_threshold(token_count_bit_len, network_size).unwrap();
        // 10 - log2(0+1) - 1
        assert_eq!(threshold, 9.into());

        let network_size = 127;
        let token_count_bit_len = 0;
        let threshold = activity_threshold(token_count_bit_len, network_size).unwrap();
        // 0 - log2(127+1) - 1 (by saturated_sub)
        assert_eq!(threshold, 0.into());
    }

    #[test]
    fn test_token_count_bit_len() {
        let core_quota = 5;
        let num_core_nodes = 2;
        // ceil(log2(10 + 1))
        assert_eq!(token_count_bit_len(core_quota, num_core_nodes).unwrap(), 4);

        let core_quota = 0;
        // ceil(log2(0 + 1))
        assert_eq!(token_count_bit_len(core_quota, num_core_nodes).unwrap(), 0);
    }

    #[test]
    fn test_token_evaluation() {
        let evaluation = BlendingTokenEvaluation::new(2000, 2).unwrap();
        // token_count_bit_len = ceil(log2((2000*2) + 1)) = 12
        // token_count_byte_len = ceil(token_count_bit_len / 8) = 2
        assert_eq!(evaluation.token_count_byte_len, 2);
        // token_count_bit_len - ceil(log2(2+1)) - 1 = 9
        assert_eq!(evaluation.activity_threshold, 9.into());

        let maybe_distance = evaluation.evaluate(
            &BlendingToken::new(
                Ed25519Key::from_bytes(&[0; _]).public_key(),
                VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
                VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
            ),
            SessionRandomness::from([0; 64]),
        );
        assert_eq!(maybe_distance, Some(8.into()));

        let maybe_distance = evaluation.evaluate(
            &BlendingToken::new(
                Ed25519Key::from_bytes(&[1; _]).public_key(),
                VerifiedProofOfQuota::from_bytes_unchecked([1; _]),
                VerifiedProofOfSelection::from_bytes_unchecked([1; _]),
            ),
            SessionRandomness::from([0; 64]),
        );
        assert_eq!(maybe_distance, None);
    }
}
