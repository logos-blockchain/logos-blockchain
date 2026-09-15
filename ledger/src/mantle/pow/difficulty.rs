use lb_core::mantle::ops::pow::PowTarget;
use lb_groth16::{Field as _, fr_to_bytes};
use num_bigint::BigUint;

use crate::config::RewardPoWConfig;

pub fn compute_new_reward_difficulty(
    claims_accepted_in_block: u64,
    current_block_reward_target: PowTarget,
    config: &RewardPoWConfig,
) -> PowTarget {
    let smoothing_precision = config.ema_smoothing_precision.get();
    let smoothing_factor = config.ema_smoothing_factor;
    let target_claims_per_block = config.target_claims_per_block;

    // (P - F): the weight of the fresh observation, with q = F / P.
    let observation_weight = smoothing_precision
        .checked_sub(smoothing_factor)
        .expect("EMA_SMOOTHING_FACTOR must be below EMA_SMOOTHING_PRECISION");

    // The arithmetic happens on plain integers: `PowTarget` is a field
    // element, whose division (multiplication by the modular inverse) does
    // not compute a ratio.
    let current_block_reward_target =
        BigUint::from_bytes_le(&fr_to_bytes(&current_block_reward_target));

    // Per block: normalize the count by the target that produced it, then
    // smooth (EMA, smoothing q ~ window N), reconstructing the previous
    // estimate from the previous target (assumed calibrated to T claims):
    //     demand_est = (1 - q) * (claims_in_block / current_target)
    //                + q * (TARGET_CLAIMS_PER_BLOCK / current_target)
    // The estimate is kept as a fraction: claims are astronomically smaller
    // than the target, so dividing first would truncate the demand to zero.
    let demand_estimate_numerator = (BigUint::from(observation_weight) * claims_accepted_in_block
        + BigUint::from(smoothing_factor) * target_claims_per_block)
        // Zero only when F == 0 (no smoothing) and the block had no claims;
        // floored to avoid dividing by zero below.
        .max(BigUint::from(1u8));
    let demand_estimate_denominator = current_block_reward_target * smoothing_precision;

    // Set the next target so the smoothed demand yields T claims:
    //     new_target = TARGET_CLAIMS_PER_BLOCK / demand_est
    let new_target = BigUint::from(target_claims_per_block) * demand_estimate_denominator
        / demand_estimate_numerator;

    // Use REWARD_TARGET_FLOOR to prevent it from falling to a value it can
    // never recover from (see `RewardPoWConfig::reward_target_floor`).
    let target_floor = BigUint::from(config.reward_target_floor().get());
    // Cap at p - 1 (the maximum field element) so converting back into the
    // field cannot reduce mod p and wrap a large target into a tiny one.
    let max_target = BigUint::from_bytes_le(&fr_to_bytes(&-PowTarget::ONE));
    PowTarget::from(new_target.max(target_floor).min(max_target))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use lb_groth16::AdditiveGroup as _;

    use super::*;
    use crate::config::ModulusShift;

    /// A [`RewardPoWConfig`] with the difficulty controller set to
    /// `F/P = factor/precision` and `T = target_claims_per_block`. The other
    /// fields are unused by [`compute_new_reward_difficulty`] and take
    /// arbitrary (claiming-disabled) values.
    fn difficulty_config(
        factor: u64,
        precision: u64,
        target_claims_per_block: u64,
    ) -> RewardPoWConfig {
        RewardPoWConfig {
            reward_pool_genesis: 1_000_000_000,
            epoch_reward_genesis: 1_000_000,
            initial_difficulty: ModulusShift::new::<26>(),
            ema_smoothing_factor: factor,
            ema_smoothing_precision: NonZeroU64::new(precision)
                .expect("test precision is non-zero"),
            target_claims_per_block,
            rate_num: 0,
            rate_den: NonZeroU64::MIN,
            target_claim_per_block: NonZeroU64::MIN,
            slot_window: NonZeroU64::new(100).expect("100 is non-zero"),
        }
    }

    /// `q = 9/10`, `T = 10`.
    fn test_config() -> RewardPoWConfig {
        difficulty_config(9, 10, 10)
    }

    #[test]
    fn on_target_claims_leave_the_target_unchanged() {
        // claims == T is the controller's fixed point.
        let target = PowTarget::from(1_000u64);
        assert_eq!(
            compute_new_reward_difficulty(10, target, &test_config()),
            target
        );
    }

    #[test]
    fn excess_claims_harden_the_target() {
        // d = 1000, c = 2T: new = 10·10·1000 / (1·20 + 9·10) = 100000/110
        // = 909 — a gentle ~10/11 step, damped by q.
        assert_eq!(
            compute_new_reward_difficulty(20, PowTarget::from(1_000u64), &test_config()),
            PowTarget::from(909u64)
        );
    }

    #[test]
    fn missing_claims_ease_the_target() {
        // d = 1000, c = T/2: new = 100000 / (1·5 + 9·10) = 100000/95 = 1052.
        assert_eq!(
            compute_new_reward_difficulty(5, PowTarget::from(1_000u64), &test_config()),
            PowTarget::from(1_052u64)
        );
    }

    #[test]
    fn empty_block_growth_is_bounded_by_the_smoothing_factor() {
        // c = 0 is the largest possible upward step: a factor of P/F = 10/9.
        // new = 100000 / (9·10) = 1111.
        assert_eq!(
            compute_new_reward_difficulty(0, PowTarget::from(1_000u64), &test_config()),
            PowTarget::from(1_111u64)
        );
    }

    #[test]
    fn growth_is_capped_at_the_maximum_field_element() {
        // From the easiest possible target (p - 1), an empty block would
        // grow past the field; the cap keeps it at p - 1 instead of letting
        // the field conversion wrap it around to a tiny target.
        let max_target = -PowTarget::ONE;
        assert_eq!(
            compute_new_reward_difficulty(0, max_target, &test_config()),
            max_target
        );
    }

    #[test]
    fn realistic_magnitude_target_stays_in_range() {
        // A target around 2^250 (the realistic magnitude): the controller
        // must neither truncate the demand to zero nor wrap mod p. An
        // on-target block leaves it unchanged.
        let target = PowTarget::from(BigUint::from(1u8) << 250);
        assert_eq!(
            compute_new_reward_difficulty(10, target, &test_config()),
            target
        );
    }

    #[test]
    fn claim_flood_stops_at_the_floor() {
        // An enormous claim count would drive the target to zero.
        // The floor prevents it: ceil(9 / (10-9)) = 9
        assert_eq!(
            compute_new_reward_difficulty(u64::MAX, PowTarget::from(1_000u64), &test_config()),
            PowTarget::from(9u64)
        );
    }

    #[test]
    fn zero_target_is_lifted_to_the_floor() {
        // If `claims=0` and `current=0`, the new target is 0, which stays at 0
        // forever. The floor prevents it: ceil(9 / (10-9)) = 9
        assert_eq!(
            compute_new_reward_difficulty(0, PowTarget::ZERO, &test_config()),
            PowTarget::from(9u64)
        );
    }

    #[test]
    fn target_below_the_floor_is_lifted_to_the_floor() {
        // If `claims=0` and `current=8`, the new target is `8 * 10 / 9 = 8`,
        // which is below the floor of 9. The floor lifts it to 9 instead.
        assert_eq!(
            compute_new_reward_difficulty(0, PowTarget::from(8u64), &test_config()),
            PowTarget::from(9u64)
        );
    }

    #[test]
    fn target_at_the_floor_eases_on_its_own() {
        // If `claims=0` and `current=9` (== floor), the new target is
        // `9 * 10 / 9 = 10`. The target eases on its own, without the floor's
        // help.
        assert_eq!(
            compute_new_reward_difficulty(0, PowTarget::from(9u64), &test_config()),
            PowTarget::from(10u64)
        );
    }

    #[test]
    fn no_smoothing_with_empty_block_takes_a_large_bounded_easing_step() {
        // F = 0 (q = 0, no smoothing) with an empty block makes the exact
        // formula divide by zero; the numerator floor turns it into a large
        // but finite easing step instead: new = T·P·d = 10·10·1000.
        assert_eq!(
            compute_new_reward_difficulty(
                0,
                PowTarget::from(1_000u64),
                &difficulty_config(0, 10, 10)
            ),
            PowTarget::from(100_000u64)
        );
    }

    #[test]
    #[should_panic(expected = "reward_target_floor must be computed successfully")]
    fn smoothing_factor_equal_to_precision_is_rejected() {
        // F == P leaves P - F at zero, so the floor is undefined. Config
        // validation rejects it at load time; here it surfaces as a panic.
        let _ = compute_new_reward_difficulty(
            0,
            PowTarget::from(1_000u64),
            &difficulty_config(10, 10, 10),
        );
    }

    #[test]
    #[should_panic(expected = "EMA_SMOOTHING_FACTOR must be below")]
    fn smoothing_factor_above_precision_is_rejected() {
        // q > 1 would make the observation weight negative; the runtime
        // check turns a silent underflow into an explicit panic.
        let _ = compute_new_reward_difficulty(
            10,
            PowTarget::from(1_000u64),
            &difficulty_config(11, 10, 10),
        );
    }
}
