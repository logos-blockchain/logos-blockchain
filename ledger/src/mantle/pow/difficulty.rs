use lb_core::mantle::ops::pow::PowTarget;
use lb_groth16::{Field as _, fr_to_bytes};
use num_bigint::BigUint;

pub trait PoWDifficultyConstants {
    /// Exponential moving average, a smoothed running estimate that weights
    /// recent blocks most
    const EMA_SMOOTHING_FACTOR: u64;
    const EMA_SMOOTHING_PRECISION: u64;
    const TARGET_CLAIMS_PER_BLOCK: u64;
}

pub struct PoWDifficultySettings;

// TODO: change settings when decided
impl PoWDifficultyConstants for PoWDifficultySettings {
    const EMA_SMOOTHING_FACTOR: u64 = 9;
    const EMA_SMOOTHING_PRECISION: u64 = 10;
    const TARGET_CLAIMS_PER_BLOCK: u64 = 100;
}

/// Constants for the epoch-scoped Blend `PoW` difficulty.
///
/// Same controller as [`PoWDifficultySettings`], but stepped once per epoch
/// instead of once per block, and driven by the average number of
/// transactions per block rather than by the claims accepted in a single
/// block. `TARGET_CLAIMS_PER_BLOCK` is therefore read here as the target
/// number of *transactions* per block the Blend difficulty calibrates to.
pub struct BlendPoWDifficultySettings;

// TODO: change settings when decided
impl PoWDifficultyConstants for BlendPoWDifficultySettings {
    const EMA_SMOOTHING_FACTOR: u64 = 9;
    const EMA_SMOOTHING_PRECISION: u64 = 10;
    const TARGET_CLAIMS_PER_BLOCK: u64 = 100;
}

/// The Blend `PoW` difficulty in effect until the first epoch average has
/// been observed and finalized.
// TODO: Setup value when decided.
const BLEND_POW_DIFFICULTY_GENESIS: u64 = 1_000_000;

/// The genesis Blend `PoW` difficulty, in effect for the epochs whose average
/// transactions per block cannot be observed yet.
pub fn genesis_blend_difficulty() -> PowTarget {
    PowTarget::from(BLEND_POW_DIFFICULTY_GENESIS)
}

pub fn compute_new_reward_difficulty<Constants: PoWDifficultyConstants>(
    claims_accepted_in_block: u64,
    current_block_reward_target: PowTarget,
) -> PowTarget {
    // A single block's count is the observation, i.e. the degenerate average
    // of `claims_accepted_in_block` over one block.
    retarget::<Constants>(claims_accepted_in_block, 1, current_block_reward_target)
}

/// Retarget the Blend difficulty for one epoch, from the total number of
/// transactions and of blocks observed over a whole, closed epoch.
///
/// An epoch with no blocks at all is read as zero demand, which eases the
/// difficulty by the largest step the smoothing factor allows — exactly what
/// an empty block does to the reward difficulty.
pub fn compute_new_blend_difficulty<Constants: PoWDifficultyConstants>(
    txs_in_epoch: u64,
    blocks_in_epoch: u64,
    current_blend_target: PowTarget,
) -> PowTarget {
    retarget::<Constants>(txs_in_epoch, blocks_in_epoch.max(1), current_blend_target)
}

/// The shared EMA controller.
///
/// The fresh observation is the rational `observed_count / observed_blocks`,
/// kept as a fraction throughout so that an average below one per block is
/// not truncated to zero. `observed_blocks` must be non-zero.
fn retarget<Constants: PoWDifficultyConstants>(
    observed_count: u64,
    observed_blocks: u64,
    current_block_reward_target: PowTarget,
) -> PowTarget {
    debug_assert!(
        observed_blocks > 0,
        "the observation window must span at least one block"
    );

    // (P - F): the weight of the fresh observation, with q = F / P.
    let observation_weight = Constants::EMA_SMOOTHING_PRECISION
        .checked_sub(Constants::EMA_SMOOTHING_FACTOR)
        .expect("EMA_SMOOTHING_FACTOR must not exceed EMA_SMOOTHING_PRECISION");

    // The arithmetic happens on plain integers: `PowTarget` is a field
    // element, whose division (multiplication by the modular inverse) does
    // not compute a ratio.
    let current_block_reward_target =
        BigUint::from_bytes_le(&fr_to_bytes(&current_block_reward_target));

    // Per block: normalize the count by the target that produced it, then
    // smooth (EMA, smoothing q ~ window N), reconstructing the previous
    // estimate from the previous target (assumed calibrated to T claims):
    //     demand_est = (1 - q) * (observed_count / observed_blocks
    //                             / current_target)
    //                + q * (TARGET_CLAIMS_PER_BLOCK / current_target)
    // The estimate is kept as a fraction: claims are astronomically smaller
    // than the target, so dividing first would truncate the demand to zero.
    // Both terms are scaled by `observed_blocks` so that the per-block
    // average stays exact rather than being floored to an integer.
    let demand_estimate_numerator = (BigUint::from(observation_weight) * observed_count
        + BigUint::from(Constants::EMA_SMOOTHING_FACTOR)
            * Constants::TARGET_CLAIMS_PER_BLOCK
            * observed_blocks)
        // Zero only when F == 0 (no smoothing) and nothing was observed;
        // floored to avoid dividing by zero below.
        .max(BigUint::from(1u8));
    let demand_estimate_denominator =
        current_block_reward_target * Constants::EMA_SMOOTHING_PRECISION * observed_blocks;

    // Set the next target so the smoothed demand yields T claims:
    //     new_target = TARGET_CLAIMS_PER_BLOCK / demand_est
    let new_target = BigUint::from(Constants::TARGET_CLAIMS_PER_BLOCK)
        * demand_estimate_denominator
        / demand_estimate_numerator;

    // Cap at p - 1 (the maximum field element) so converting back into the
    // field cannot reduce mod p and wrap a large target into a tiny one.
    let max_target = BigUint::from_bytes_le(&fr_to_bytes(&-PowTarget::ONE));
    PowTarget::from(new_target.min(max_target))
}

#[cfg(test)]
mod tests {
    use lb_groth16::AdditiveGroup as _;

    use super::*;

    /// `q = 9/10`, `T = 10`.
    struct TestConstants;
    impl PoWDifficultyConstants for TestConstants {
        const EMA_SMOOTHING_FACTOR: u64 = 9;
        const EMA_SMOOTHING_PRECISION: u64 = 10;
        const TARGET_CLAIMS_PER_BLOCK: u64 = 10;
    }

    #[test]
    fn on_target_claims_leave_the_target_unchanged() {
        // claims == T is the controller's fixed point.
        let target = PowTarget::from(1_000u64);
        assert_eq!(
            compute_new_reward_difficulty::<TestConstants>(10, target),
            target
        );
    }

    #[test]
    fn excess_claims_harden_the_target() {
        // d = 1000, c = 2T: new = 10·10·1000 / (1·20 + 9·10) = 100000/110
        // = 909 — a gentle ~10/11 step, damped by q.
        assert_eq!(
            compute_new_reward_difficulty::<TestConstants>(20, PowTarget::from(1_000u64)),
            PowTarget::from(909u64)
        );
    }

    #[test]
    fn missing_claims_ease_the_target() {
        // d = 1000, c = T/2: new = 100000 / (1·5 + 9·10) = 100000/95 = 1052.
        assert_eq!(
            compute_new_reward_difficulty::<TestConstants>(5, PowTarget::from(1_000u64)),
            PowTarget::from(1_052u64)
        );
    }

    #[test]
    fn empty_block_growth_is_bounded_by_the_smoothing_factor() {
        // c = 0 is the largest possible upward step: a factor of P/F = 10/9.
        // new = 100000 / (9·10) = 1111.
        assert_eq!(
            compute_new_reward_difficulty::<TestConstants>(0, PowTarget::from(1_000u64)),
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
            compute_new_reward_difficulty::<TestConstants>(0, max_target),
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
            compute_new_reward_difficulty::<TestConstants>(10, target),
            target
        );
    }

    #[test]
    fn claim_flood_drives_the_target_to_zero() {
        // Pins current behaviour: an enormous claim count floors the target
        // to zero. Zero is an absorbing state (0 stays 0 below), so whether
        // this needs a floor of 1 is a design decision left open here.
        assert_eq!(
            compute_new_reward_difficulty::<TestConstants>(u64::MAX, PowTarget::from(1_000u64)),
            PowTarget::ZERO
        );
    }

    #[test]
    fn zero_target_is_absorbing() {
        // Pins current behaviour: a zero target (e.g. the unset genesis
        // default) stays zero forever — genesis must seed a real initial
        // difficulty for the controller to operate.
        assert_eq!(
            compute_new_reward_difficulty::<TestConstants>(0, PowTarget::ZERO),
            PowTarget::ZERO
        );
    }

    #[test]
    fn no_smoothing_with_empty_block_takes_a_large_bounded_easing_step() {
        // F = 0 (q = 0, no smoothing) with an empty block makes the exact
        // formula divide by zero; the numerator floor turns it into a large
        // but finite easing step instead: new = T·P·d = 10·10·1000.
        struct NoSmoothing;
        impl PoWDifficultyConstants for NoSmoothing {
            const EMA_SMOOTHING_FACTOR: u64 = 0;
            const EMA_SMOOTHING_PRECISION: u64 = 10;
            const TARGET_CLAIMS_PER_BLOCK: u64 = 10;
        }
        assert_eq!(
            compute_new_reward_difficulty::<NoSmoothing>(0, PowTarget::from(1_000u64)),
            PowTarget::from(100_000u64)
        );
    }

    #[test]
    fn full_smoothing_freezes_the_target() {
        // F == P is q = 1: the observation has zero weight, so the target
        // never moves no matter what the block contained.
        struct FullSmoothing;
        impl PoWDifficultyConstants for FullSmoothing {
            const EMA_SMOOTHING_FACTOR: u64 = 10;
            const EMA_SMOOTHING_PRECISION: u64 = 10;
            const TARGET_CLAIMS_PER_BLOCK: u64 = 10;
        }
        let target = PowTarget::from(1_000u64);
        assert_eq!(
            compute_new_reward_difficulty::<FullSmoothing>(0, target),
            target
        );
        assert_eq!(
            compute_new_reward_difficulty::<FullSmoothing>(1_000_000, target),
            target
        );
    }

    #[test]
    #[should_panic(expected = "EMA_SMOOTHING_FACTOR must not exceed")]
    fn smoothing_factor_above_precision_is_rejected() {
        // q > 1 would make the observation weight negative; the runtime
        // check turns a silent underflow into an explicit panic.
        struct BrokenConstants;
        impl PoWDifficultyConstants for BrokenConstants {
            const EMA_SMOOTHING_FACTOR: u64 = 11;
            const EMA_SMOOTHING_PRECISION: u64 = 10;
            const TARGET_CLAIMS_PER_BLOCK: u64 = 10;
        }
        let _ = compute_new_reward_difficulty::<BrokenConstants>(10, PowTarget::from(1_000u64));
    }

    #[test]
    fn blend_difficulty_matches_the_per_block_controller_on_the_average() {
        // An epoch of 7 blocks carrying 140 transactions averages 20 per
        // block: the epoch-scoped retarget must land exactly where the
        // per-block one lands when fed that average directly.
        assert_eq!(
            compute_new_blend_difficulty::<TestConstants>(140, 7, PowTarget::from(1_000u64)),
            compute_new_reward_difficulty::<TestConstants>(20, PowTarget::from(1_000u64))
        );
    }

    #[test]
    fn blend_difficulty_keeps_fractional_averages() {
        // 3 transactions over 6 blocks is half a transaction per block —
        // truncating the average to an integer would read it as zero and
        // ease the target as if the epoch had been empty. It must instead
        // sit strictly between the on-average-zero and the one-per-block
        // results.
        let target = PowTarget::from(1_000u64);
        let half = compute_new_blend_difficulty::<TestConstants>(3, 6, target);
        assert!(half < compute_new_blend_difficulty::<TestConstants>(0, 6, target));
        assert!(half > compute_new_blend_difficulty::<TestConstants>(6, 6, target));
    }

    #[test]
    fn blend_difficulty_reads_an_empty_epoch_as_zero_demand() {
        // No blocks at all: nothing can be averaged, so the epoch counts as
        // zero demand and eases the target exactly like an empty block.
        let target = PowTarget::from(1_000u64);
        assert_eq!(
            compute_new_blend_difficulty::<TestConstants>(0, 0, target),
            compute_new_reward_difficulty::<TestConstants>(0, target)
        );
    }
}
