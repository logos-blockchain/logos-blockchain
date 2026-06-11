use core::fmt::{self, Debug, Formatter};
use std::ops::Add as _;

use lb_core::crypto::ZkHash;
use lb_cryptarchia_engine::Epoch;
use lb_groth16::{Fr, FrBytes, fr_to_bytes};
use lb_utils::math::{F64Ge1, NonNegativeF64};
use serde::{Deserialize, Serialize};

use crate::reward::{BlendingToken, activity, token::HammingDistance};

/// Epoch-specific information to compute an activity proof.
pub struct EpochInfo {
    pub(crate) epoch: Epoch,
    pub(crate) epoch_randomness: EpochRandomness,
    pub(crate) token_evaluation: BlendingTokenEvaluation,
}

impl EpochInfo {
    pub fn new(
        epoch: Epoch,
        pol_epoch_nonce: &ZkHash,
        num_core_nodes: u64,
        core_quota: u64,
        activity_threshold_sensitivity: u64,
    ) -> Result<Self, Error> {
        let epoch_randomness = (*pol_epoch_nonce).into();
        let token_evaluation = BlendingTokenEvaluation::new(
            core_quota,
            num_core_nodes,
            activity_threshold_sensitivity,
        )?;

        Ok(Self {
            epoch,
            epoch_randomness,
            token_evaluation,
        })
    }

    #[must_use]
    pub const fn epoch_randomness(&self) -> EpochRandomness {
        self.epoch_randomness
    }
}

/// Parameters to evaluate a blending token for an epoch.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlendingTokenEvaluation {
    token_count_byte_len: u64,
    activity_threshold: HammingDistance,
}

impl BlendingTokenEvaluation {
    pub fn new(
        core_quota: u64,
        num_core_nodes: u64,
        activity_threshold_sensitivity: u64,
    ) -> Result<Self, Error> {
        let expected_token_count_bit_len = token_count_bit_len(core_quota, num_core_nodes)?;
        let activity_threshold = activity_threshold(
            expected_token_count_bit_len,
            num_core_nodes,
            activity_threshold_sensitivity,
        )?;

        Ok(Self {
            token_count_byte_len: expected_token_count_bit_len.div_ceil(8),
            activity_threshold,
        })
    }

    /// Calculate the Hamming distance of the given blending token to the next
    /// epoch randomness, and return it only if it is not larger than the
    /// activity threshold.
    #[must_use]
    pub fn evaluate(
        &self,
        token: &BlendingToken,
        next_epoch_randomness: EpochRandomness,
    ) -> Option<HammingDistance> {
        let distance = token.hamming_distance(self.token_count_byte_len, next_epoch_randomness);
        tracing::trace!(
            "Evaluated blending token {:?} for activity proof. Calculated Hamming distance = {distance:?}",
            token.signing_key()
        );
        (distance <= self.activity_threshold).then_some(distance)
    }

    #[must_use]
    pub const fn activity_threshold(&self) -> HammingDistance {
        self.activity_threshold
    }
}

/// Deterministic unbiased randomness for an epoch.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochRandomness(#[serde(with = "lb_groth16::serde::serde_fr")] Fr);

impl Debug for EpochRandomness {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "EpochRandomness({})", hex::encode(self.as_bytes()))
    }
}

impl From<Fr> for EpochRandomness {
    fn from(value: Fr) -> Self {
        Self(value)
    }
}

impl EpochRandomness {
    #[must_use]
    pub fn as_bytes(&self) -> FrBytes {
        fr_to_bytes(&self.0)
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
/// tokens generated during a single epoch.
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
    activity_threshold_sensitivity: u64,
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
        activity_threshold_sensitivity,
    ))
}

#[cfg(test)]
mod tests {
    use lb_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};
    use lb_groth16::AdditiveGroup as _;
    use lb_key_management_system_keys::keys::Ed25519Key;

    use super::*;

    #[test]
    fn test_activity_threshold() {
        let network_size = 127;
        let token_count_bit_len = 10;
        let threshold = activity_threshold(token_count_bit_len, network_size, 1).unwrap();
        // 10 - log2(127+1) - 1
        assert_eq!(threshold, 2.into());

        let network_size = 0;
        let token_count_bit_len = 10;
        let threshold = activity_threshold(token_count_bit_len, network_size, 1).unwrap();
        // 10 - log2(0+1) - 1
        assert_eq!(threshold, 9.into());

        let network_size = 127;
        let token_count_bit_len = 0;
        let threshold = activity_threshold(token_count_bit_len, network_size, 1).unwrap();
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
        let evaluation = BlendingTokenEvaluation::new(2000, 2, 1).unwrap();
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
            EpochRandomness::from(Fr::ZERO),
        );
        assert_eq!(maybe_distance, Some(6.into()));

        let maybe_distance = evaluation.evaluate(
            &BlendingToken::new(
                Ed25519Key::from_bytes(&[0; _]).public_key(),
                VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
                VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
            ),
            EpochRandomness::from(ZkHash::from(100)),
        );
        assert_eq!(maybe_distance, None);
    }
}
