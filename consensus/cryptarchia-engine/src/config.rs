use std::num::NonZero;

use lb_pol::LotteryConstants;
use lb_utils::math::{NonNegativeF64, NonNegativeRatio};

/// `MAX_UNCLES`, the maximum number of uncles a block may reference.
pub const MAX_UNCLES: usize = 4;

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Config {
    /// The `k` parameter in the Common Prefix property.
    /// Blocks deeper than k are generally considered stable and forks deeper
    /// than that trigger the additional fork selection rule, which is
    /// however only expected to be used during bootstrapping.
    security_param: NonZero<u32>,
    /// `f`, the rate of occupied slots
    slot_activation_coeff: NonNegativeRatio,
    stake_inference_learning_rate: NonNegativeF64,
    /// `W`, the width of the uncle reference window in expected
    /// block-intervals.
    uncle_reference_window_in_block: NonZero<u32>,
    /// Lottery approximation constants computed from `slot_activation_coeff`
    #[serde(skip)]
    lottery_constants: LotteryConstants,
}

impl<'de> serde::Deserialize<'de> for Config {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct RawConfig {
            security_param: NonZero<u32>,
            slot_activation_coeff: NonNegativeRatio,
            stake_inference_learning_rate: NonNegativeF64,
            uncle_reference_window_in_block: NonZero<u32>,
        }

        let raw = RawConfig::deserialize(deserializer)?;

        Ok(Self {
            security_param: raw.security_param,
            slot_activation_coeff: raw.slot_activation_coeff,
            stake_inference_learning_rate: raw.stake_inference_learning_rate,
            uncle_reference_window_in_block: raw.uncle_reference_window_in_block,
            lottery_constants: LotteryConstants::new(raw.slot_activation_coeff),
        })
    }
}

impl Config {
    #[must_use]
    pub fn new(
        security_param: NonZero<u32>,
        slot_activation_coeff: NonNegativeRatio,
        stake_inference_learning_rate: NonNegativeF64,
        uncle_reference_window_in_block: NonZero<u32>,
    ) -> Self {
        Self {
            security_param,
            slot_activation_coeff,
            stake_inference_learning_rate,
            uncle_reference_window_in_block,
            lottery_constants: LotteryConstants::new(slot_activation_coeff),
        }
    }

    /// `W * f^-1`, the maximum number of slots by which the parent of a
    /// referenced uncle may precede the block referencing it.
    #[must_use]
    pub const fn uncle_reference_window_in_slot(&self) -> NonZero<u64> {
        average_slots_for_blocks(
            self.uncle_reference_window_in_block,
            self.slot_activation_coeff,
        )
    }

    #[must_use]
    pub const fn security_param(&self) -> NonZero<u32> {
        self.security_param
    }

    #[must_use]
    pub const fn slot_activation_coeff(&self) -> NonNegativeRatio {
        self.slot_activation_coeff
    }

    #[must_use]
    pub const fn lottery_constants(&self) -> &LotteryConstants {
        &self.lottery_constants
    }

    #[must_use]
    pub const fn base_period_length(&self) -> NonZero<u64> {
        base_period_length(self.security_param, self.slot_activation_coeff)
    }

    #[must_use]
    pub const fn stake_inference_learning_rate(&self) -> f64 {
        self.stake_inference_learning_rate.get()
    }

    /// sufficient time measured in slots to measure the density of block
    /// production with enough statistical significance.
    #[must_use]
    pub const fn s_gen(&self) -> NonZero<u64> {
        NonZero::new(
            ((self.security_param.get() as f64) / (4.0 * self.slot_activation_coeff.as_f64()))
                .floor() as u64,
        )
        .expect("s_gen with proper configuration should never be zero")
    }
}

#[must_use]
pub const fn base_period_length(
    security_param: NonZero<u32>,
    slot_activation_coeff: NonNegativeRatio,
) -> NonZero<u64> {
    average_slots_for_blocks(security_param, slot_activation_coeff)
}

#[must_use]
pub const fn average_slots_for_blocks(
    num_blocks: NonZero<u32>,
    slot_activation_coeff: NonNegativeRatio,
) -> NonZero<u64> {
    NonZero::new((num_blocks.get() as f64 / slot_activation_coeff.as_f64()).floor() as u64)
        .expect("base_period_length with proper configuration should never be zero")
}

/// `N_b`: the number of blocks an epoch of `epoch_length` slots is expected to
/// produce, `epoch_length * f`.
///
/// For the standard 3/3/4 phase split this works out to `10k`: the epoch spans
/// ten base periods and a base period is `k / f` slots. It is therefore never a
/// free parameter — anything that needs a per-epoch block count derives it from
/// the schedule rather than carrying its own copy.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "The u128 product is explicitly clamped to u64 before the cast."
)]
pub const fn expected_blocks_per_epoch(
    epoch_length: u64,
    slot_activation_coeff: NonNegativeRatio,
) -> NonZero<u64> {
    // Widened so the multiplication cannot wrap: `epoch_length` spans the whole
    // of `u64` and the numerator the whole of `u32`.
    let blocks = (epoch_length as u128 * slot_activation_coeff.numerator as u128)
        / slot_activation_coeff.denominator.get() as u128;
    let blocks = if blocks > u64::MAX as u128 {
        u64::MAX
    } else {
        blocks as u64
    };
    match NonZero::new(blocks) {
        Some(blocks) => blocks,
        // Only an `f` of zero gets here, a chain that produces no blocks at
        // all. One keeps arithmetic that divides by this value well-defined.
        None => NonZero::<u64>::MIN,
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Mul as _;

    use super::*;

    #[test]
    fn test_config() {
        let config = Config::new(
            NonZero::new(10).unwrap(),
            NonNegativeRatio::new(1, 5.try_into().unwrap()),
            0.1.try_into().unwrap(),
            NonZero::new(12).unwrap(),
        );
        assert_eq!(config.security_param(), NonZero::new(10).unwrap());
        assert_eq!(config.base_period_length(), NonZero::new(50).unwrap());
        assert_eq!(
            config.uncle_reference_window_in_slot(),
            NonZero::new(60).unwrap()
        );
        assert_eq!(config.s_gen(), NonZero::new(12).unwrap());
        assert_eq!(
            config.stake_inference_learning_rate().mul(10.0).floor() as u64,
            1,
        );
    }

    #[test]
    fn test_expected_blocks_per_epoch_is_ten_k() {
        // k = 10, f = 1/5: a base period is 50 slots and the standard 3/3/4
        // split makes the epoch ten of them.
        let slot_activation_coeff = NonNegativeRatio::new(1, 5.try_into().unwrap());
        let epoch_length =
            10 * base_period_length(NonZero::new(10).unwrap(), slot_activation_coeff).get();
        assert_eq!(
            expected_blocks_per_epoch(epoch_length, slot_activation_coeff),
            NonZero::new(100).unwrap(),
        );
    }
}
