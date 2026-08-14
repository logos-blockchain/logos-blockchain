//! The per-epoch Blend `PoW` difficulty, `d_blend`.
//!
//! `d_blend` is the threshold a puzzle ticket must fall below to buy
//! permissionless Blend admission. It is a per-epoch protocol value — the
//! `PoQ` circuit consumes it as a public input, so every `PoW`-branch proof in
//! an epoch must use the same one — and it tracks transaction load: heavy load
//! means real traffic already supplies an anonymity set, so `PoW` entry can be
//! rate-limited harder; thin load eases the threshold so `PoW`-backed messages
//! come in and build the anonymity set.
//!
//! *When* the value is fixed matters as much as how. Epoch `N`'s threshold is
//! frozen at the same moment as epoch `N`'s nonce — the snapshot taken during
//! epoch `N - 1` — and reads the load of epoch `N - 2`, the last epoch complete
//! at that moment. A prover may grind solutions for an epoch from the moment
//! its nonce is public, which it can only do if the threshold those solutions
//! must clear is public just as early. Fixing it at the boundary instead would
//! leave that precomputation window without one of its public inputs, and would
//! derive a value every node must agree on from blocks still shallow enough to
//! be re-orged.

use core::num::NonZeroU64;

use lb_core::mantle::ops::pow::PowTarget;
use lb_groth16::{fr_from_biguint_saturating, fr_modulus};
use num_bigint::BigUint;

use crate::{config::BlendPoWConfig, cryptarchia::tx_density::ClosedEpochLoad};

/// `BLEND_DIFFICULTY_BASE`: the threshold in effect at exactly the reference
/// load, and the value the chain starts from.
///
/// The spec states it as a fraction of the scalar field, `p / 2^n`, and reasons
/// about it in exponents throughout, so that is how the deployment carries it.
#[must_use]
pub fn base_difficulty(config: &BlendPoWConfig) -> PowTarget {
    fr_from_biguint_saturating(fr_modulus() >> config.base_difficulty_exponent)
}

/// Retarget `d_blend` for one epoch from the transaction load of a whole,
/// closed epoch.
///
/// With `load = avg_txs_per_block / T_tx` and a damping exponent
/// `alpha = a / b <= 1`, the threshold is the baseline divided down by the
/// load — smaller target, harder puzzle, as load rises:
///
/// ```text
/// d_blend = BASE / load^alpha, clamped to [previous / k, previous * k]
/// ```
///
/// An epoch that carried no transactions at all — including an epoch that
/// produced no blocks — is read as no load observed, and eases the threshold as
/// far as this epoch's clamp allows.
#[must_use]
pub fn compute_epoch_blend_difficulty(
    load: ClosedEpochLoad,
    previous_difficulty: PowTarget,
    config: &BlendPoWConfig,
) -> PowTarget {
    // The arithmetic happens on the canonical integer representatives:
    // `PowTarget` is a field element, and a field has no order and no floor
    // division, so neither the clamp nor the ratio below is field arithmetic.
    let previous = BigUint::from(previous_difficulty);
    let max_step = BigUint::from(config.max_step.get());
    // Bound the change to at most a factor of `k` in either direction. The
    // interval is never empty: `previous` is at most `p - 1`, so `low` is at
    // most `(p - 1) / 2`, strictly below the `p - 1` ceiling of `high`.
    let low = &previous / &max_step;
    let high = previous * max_step;

    // The load is kept as the exact ratio `numerator / denominator` — they are
    // equal at the reference load — and never divided out.
    let Ok(non_empty_transactions_count) = NonZeroU64::try_from(load.transactions()) else {
        return fr_from_biguint_saturating(high);
    };
    let numerator = BigUint::from(non_empty_transactions_count.get());
    let denominator = BigUint::from(config.target_transactions_per_block.get()) * load.blocks();

    // target = BASE / load^alpha
    //        = (BASE^b * denominator^a // numerator^a)^(1/b)
    // Every quantity is an integer and only the final b-th root is floored, so
    // the result is at most one unit away from the exact value. The radicand
    // reaches roughly 2^487, well past any fixed-width type.
    let (a, b) = config.damping_exponent();
    let base = BigUint::from(base_difficulty(config));
    let radicand = (base.pow(b.get()) * denominator.pow(a)) / numerator.pow(a);
    // `nth_root` is the floor of the exact root, as the spec requires: an
    // approximation that could be off by one would fork the chain.
    let target = radicand.nth_root(b.get());

    fr_from_biguint_saturating(target.clamp(low, high))
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;

    use super::*;

    /// The deployed shape: `alpha = 1/2`, `k = 2`. `T_tx` is scaled down from
    /// the deployed 512 so a test can state a load in a handful of blocks, and
    /// the baseline is taken far down the field so the expected values are
    /// small enough to reason about.
    fn config() -> BlendPoWConfig {
        BlendPoWConfig {
            base_difficulty_exponent: 234,
            target_transactions_per_block: NonZeroU64::new(10).unwrap(),
            max_step: NonZeroU64::new(2).unwrap(),
            damping_num: NonZeroU32::new(1).unwrap(),
            damping_den_offset: 1,
        }
    }

    fn as_int(target: PowTarget) -> BigUint {
        BigUint::from(target)
    }

    #[test]
    fn the_reference_load_sits_at_the_baseline() {
        // 10 transactions per block over 7 blocks is exactly `T_tx`: the load
        // is 1, so the threshold is the baseline itself.
        let config = config();
        let base = base_difficulty(&config);
        assert_eq!(
            compute_epoch_blend_difficulty(ClosedEpochLoad::new(70, 7), base, &config),
            base
        );
    }

    #[test]
    fn the_response_to_load_is_sub_linear() {
        // At alpha = 1/2, quadrupling the load halves the threshold — which is
        // still within the factor-2 clamp, so the clamp does not bind.
        let config = config();
        let base = base_difficulty(&config);
        let retargeted =
            compute_epoch_blend_difficulty(ClosedEpochLoad::new(280, 7), base, &config);
        assert_eq!(as_int(retargeted), as_int(base) / 2u8);
    }

    #[test]
    fn a_thin_epoch_eases_the_threshold() {
        // A quarter of the reference load doubles the threshold, exactly at
        // the clamp.
        let config = config();
        let base = base_difficulty(&config);
        let retargeted = compute_epoch_blend_difficulty(ClosedEpochLoad::new(20, 8), base, &config);
        assert_eq!(as_int(retargeted), as_int(base) * 2u8);
    }

    #[test]
    fn an_epoch_with_no_transactions_eases_by_the_full_step() {
        // Nothing observed: the threshold moves to the top of the clamp.
        let config = config();
        let previous = PowTarget::from(1_000u64);
        assert_eq!(
            compute_epoch_blend_difficulty(ClosedEpochLoad::new(0, 7), previous, &config),
            PowTarget::from(2_000u64)
        );
    }

    #[test]
    fn an_epoch_with_no_blocks_eases_by_the_full_step() {
        // No blocks at all is the same signal as no transactions, and must not
        // divide by the empty block count.
        let config = config();
        let previous = PowTarget::from(1_000u64);
        assert_eq!(
            compute_epoch_blend_difficulty(ClosedEpochLoad::new(0, 0), previous, &config),
            PowTarget::from(2_000u64)
        );
    }

    #[test]
    fn the_step_is_clamped_in_both_directions() {
        let config = config();
        let base = base_difficulty(&config);
        // A flood far past the clamp: the threshold tightens by the factor k
        // and no further, so the anonymity set can shrink only gradually.
        let flooded =
            compute_epoch_blend_difficulty(ClosedEpochLoad::new(1_000_000, 7), base, &config);
        assert_eq!(as_int(flooded), as_int(base) / 2u8);
        // And symmetrically on a near-empty epoch.
        let idle = compute_epoch_blend_difficulty(ClosedEpochLoad::new(1, 7), base, &config);
        assert_eq!(as_int(idle), as_int(base) * 2u8);
    }

    #[test]
    fn the_clamp_is_anchored_on_the_previous_epoch_not_on_the_baseline() {
        // From a threshold far below the baseline, even a flood moves the
        // threshold *up*: the load decides where the controller aims, the
        // clamp decides how far one epoch may travel towards it.
        let config = config();
        assert_eq!(
            compute_epoch_blend_difficulty(
                ClosedEpochLoad::new(1_000_000, 7),
                PowTarget::from(1_000u64),
                &config
            ),
            PowTarget::from(2_000u64)
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
        // alpha = 1 (offset 0): the threshold is exactly BASE / load, subject
        // to the clamp.
        let config = BlendPoWConfig {
            damping_den_offset: 0,
            max_step: NonZeroU64::new(1_000).unwrap(),
            ..config()
        };
        let base = base_difficulty(&config);
        let retargeted =
            compute_epoch_blend_difficulty(ClosedEpochLoad::new(280, 7), base, &config);
        assert_eq!(as_int(retargeted), as_int(base) / 4u8);
    }

    #[test]
    fn growth_is_capped_at_the_maximum_field_element() {
        // From the easiest possible threshold (p - 1), an empty epoch would
        // grow past the field; the cap keeps it there rather than letting the
        // conversion wrap it around to a tiny — that is, maximally hard — one.
        // `fr_from_biguint_saturating(p)` is exactly that ceiling.
        let config = config();
        let max_target = fr_from_biguint_saturating(fr_modulus());
        assert_eq!(
            compute_epoch_blend_difficulty(ClosedEpochLoad::new(0, 7), max_target, &config),
            max_target
        );
    }

    #[test]
    fn the_deployed_baseline_is_the_field_over_two_to_the_nineteen() {
        // The value the spec gives: p / 2^19.
        let config = BlendPoWConfig {
            base_difficulty_exponent: 19,
            ..config()
        };
        assert_eq!(as_int(base_difficulty(&config)), fr_modulus() >> 19u32);
    }
}
