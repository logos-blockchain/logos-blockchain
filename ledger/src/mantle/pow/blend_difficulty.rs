//! The per-epoch Blend `PoW` difficulty, `d_blend`.
//!
//! `d_blend` is the threshold a puzzle ticket must fall below to buy
//! permissionless Blend admission. It is a per-epoch protocol value — the
//! `PoQ` circuit consumes it as a public input, so every `PoW`-branch proof in
//! an epoch must use the same one — and it tracks transaction load: heavy load
//! means real traffic already supplies an anonymity set, so `PoW` entry can be
//! rate-limited harder; thin load eases the threshold so `PoW`-backed messages
//! come in and build the anonymity set.

use lb_core::mantle::ops::pow::PowTarget;
use lb_groth16::{Field as _, fr_to_bytes};
use num_bigint::BigUint;

use crate::{config::BlendPoWConfig, cryptarchia::tx_density::ClosedEpochLoad};

pub fn compute_epoch_blend_difficulty(
    load: ClosedEpochLoad,
    previous_difficulty: PowTarget,
    config: &BlendPoWConfig,
) -> PowTarget {
    let previous_difficulty = BigUint::from(previous_difficulty);
    let numerator = BigUint::from(load.transactions());
    let max_step = BigUint::from(config.max_step.get());

    let clamp_upper_bound = previous_difficulty * max_step;

    if numerator == BigUint::ZERO {
        return clamp_upper_bound; // no load observed: as easy as this epoch's clamp allows
    }

    let clamp_lower_bound = previous_difficulty / max_step;

    let lo = previous_difficulty / config.max_step.get();

    let max_step = BigUint::from(config.max_step.get());
    // Bound the change to at most a factor of `k` in either direction.
    let low = &previous / &max_step;
    let high = previous * max_step;

    if numerator == BigUint::ZERO {
        // No load observed: as easy as this epoch's clamp allows.
        return into_target(high);
    }
    let denominator = BigUint::from(config.target_transactions_per_block.get()) * load.blocks();

    // target = BASE / load^alpha
    //        = (BASE^b * denominator^a // numerator^a)^(1/b)
    // Every quantity is an integer and only the final b-th root is floored, so
    // the rounding error is at most one unit of target.
    let (a, b) = config.damping_exponent();
    let base = BigUint::from_bytes_le(&fr_to_bytes(&config.base_difficulty));
    let radicand = (base.pow(b) * denominator.pow(a)) / numerator.pow(a);
    let target = radicand.nth_root(b);

    into_target(target.clamp(low, high))
}

/// Convert back into the field, capping at `p - 1` (the maximum field element)
/// so the conversion cannot reduce mod p and wrap a large threshold into a
/// tiny — that is, maximally hard — one.
fn into_target(value: BigUint) -> PowTarget {
    let max_target = BigUint::from_bytes_le(&fr_to_bytes(&-PowTarget::ONE));
    PowTarget::from(value.min(max_target))
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;

    use super::*;

    /// `alpha = 1/2`, `T_tx = 10`, `k = 4`, with a baseline that is a perfect
    /// square so the reference-load case is exact.
    fn config() -> BlendPoWConfig {
        BlendPoWConfig {
            base_difficulty: PowTarget::from(1_000_000u64),
            target_transactions_per_block: NonZeroU64::new(10).unwrap(),
            max_step: NonZeroU64::new(4).unwrap(),
            damping_num: NonZeroU64::new(1).unwrap(),
            damping_den_offset: 1,
        }
    }

    #[test]
    fn the_reference_load_sits_at_the_baseline() {
        // 10 transactions per block over 7 blocks is exactly T_tx: load = 1,
        // so the threshold is the baseline itself.
        let config = config();
        assert_eq!(
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(70, 7),
                config.base_difficulty,
                &config
            ),
            config.base_difficulty
        );
    }

    #[test]
    fn the_response_to_load_is_sub_linear() {
        // At alpha = 1/2, quadrupling the load only halves the threshold.
        let config = config();
        assert_eq!(
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(280, 7),
                config.base_difficulty,
                &config
            ),
            PowTarget::from(500_000u64)
        );
    }

    #[test]
    fn a_thin_epoch_eases_the_threshold() {
        // 20 transactions over 8 blocks is a quarter of the reference load,
        // which doubles the threshold — still inside the factor-4 clamp, so
        // the clamp does not bind.
        let config = config();
        assert_eq!(
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(20, 8),
                config.base_difficulty,
                &config
            ),
            PowTarget::from(2_000_000u64)
        );
    }

    #[test]
    fn an_epoch_with_no_transactions_eases_by_the_full_step() {
        // Nothing observed: the threshold moves to the top of the clamp.
        let config = config();
        assert_eq!(
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(0, 7),
                PowTarget::from(1_000u64),
                &config
            ),
            PowTarget::from(4_000u64)
        );
    }

    #[test]
    fn an_epoch_with_no_blocks_eases_by_the_full_step() {
        // No blocks at all is the same signal as no transactions, and must not
        // divide by the empty block count.
        let config = config();
        assert_eq!(
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(0, 0),
                PowTarget::from(1_000u64),
                &config
            ),
            PowTarget::from(4_000u64)
        );
    }

    #[test]
    fn the_step_is_clamped_in_both_directions() {
        let config = config();
        // A flood far past the clamp: the threshold falls by the factor k and
        // no further, so the anonymity set can shrink only gradually.
        assert_eq!(
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(1_000_000, 7),
                config.base_difficulty,
                &config
            ),
            PowTarget::from(250_000u64)
        );
        // And symmetrically upwards on a near-empty epoch.
        assert_eq!(
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(1, 7),
                config.base_difficulty,
                &config
            ),
            PowTarget::from(4_000_000u64)
        );
    }

    #[test]
    fn the_clamp_is_anchored_on_the_previous_epoch_not_on_the_baseline() {
        // From a threshold far below the baseline, even a flood moves the
        // threshold *up* — the load only decides where the controller aims,
        // while the clamp bounds how far one epoch may travel towards it.
        let config = config();
        assert_eq!(
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(1_000_000, 7),
                PowTarget::from(1_000u64),
                &config
            ),
            PowTarget::from(4_000u64)
        );
    }

    #[test]
    fn a_unit_max_step_freezes_the_threshold() {
        // k = 1 collapses the clamp onto the previous value: a deployment can
        // pin the threshold without a separate switch.
        let config = BlendPoWConfig {
            max_step: NonZeroU64::new(1).unwrap(),
            ..config()
        };
        let previous = PowTarget::from(1_234u64);
        assert_eq!(
            compute_epoch_blend_difficulty(ClosedEpochLoad::new(9_999, 7), previous, &config),
            previous
        );
        assert_eq!(
            compute_epoch_blend_difficulty(ClosedEpochLoad::new(0, 7), previous, &config),
            previous
        );
    }

    #[test]
    fn undamped_settings_track_the_load_linearly() {
        // alpha = 1 (offset 0): the threshold is exactly BASE / load.
        let config = BlendPoWConfig {
            damping_den_offset: 0,
            max_step: NonZeroU64::new(1_000).unwrap(),
            ..config()
        };
        assert_eq!(
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(280, 7),
                config.base_difficulty,
                &config
            ),
            PowTarget::from(250_000u64)
        );
    }

    #[test]
    fn growth_is_capped_at_the_maximum_field_element() {
        // From the easiest possible threshold (p - 1), an empty epoch would
        // grow past the field; the cap keeps it there instead of letting the
        // field conversion wrap it around to a tiny — maximally hard — one.
        let config = config();
        let max_target = -PowTarget::ONE;
        assert_eq!(
            compute_epoch_blend_difficulty(ClosedEpochLoad::new(0, 7), max_target, &config),
            max_target
        );
    }

    #[test]
    fn realistic_magnitude_threshold_stays_in_range() {
        // A baseline around 2^250, the realistic magnitude: the controller
        // must neither truncate the load to zero nor wrap mod p.
        let base = PowTarget::from(BigUint::from(1u8) << 250);
        let config = BlendPoWConfig {
            base_difficulty: base,
            ..config()
        };
        let retargeted =
            compute_epoch_blend_difficulty(ClosedEpochLoad::new(280, 7), base, &config);
        // Quadruple load at alpha = 1/2: half the baseline, up to the flooring
        // of the square root.
        let expected = PowTarget::from(BigUint::from(1u8) << 249);
        assert!(retargeted <= expected);
        assert!(retargeted > PowTarget::from((BigUint::from(1u8) << 249) - 2u8));
    }
}
