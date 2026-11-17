use std::num::{NonZero, NonZeroU64};

use lb_pol::LotteryConstants;
use lb_utils::math::{NonNegativeF64, NonNegativeRatio, ProperFraction};

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Config {
    /// The `k` parameter in the Common Prefix property.
    /// Blocks deeper than k are generally considered stable and forks deeper
    /// than that trigger the additional fork selection rule, which is
    /// however only expected to be used during bootstrapping.
    security_param: NonZero<u32>,
    /// `f`, the rate of occupied slots
    slot_activation_coeff: ProperFraction,
    stake_inference_learning_rate: NonNegativeF64,
    /// `W`, the width of the uncle reference window in expected
    /// block-intervals.
    uncle_reference_window_in_block: u32,
    /// The most uncles a block may reference.
    max_uncles: u8,
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
            slot_activation_coeff: ProperFraction,
            stake_inference_learning_rate: NonNegativeF64,
            uncle_reference_window_in_block: u32,
            max_uncles: u8,
        }

        let raw = RawConfig::deserialize(deserializer)?;

        Ok(Self {
            security_param: raw.security_param,
            slot_activation_coeff: raw.slot_activation_coeff,
            stake_inference_learning_rate: raw.stake_inference_learning_rate,
            uncle_reference_window_in_block: raw.uncle_reference_window_in_block,
            max_uncles: raw.max_uncles,
            lottery_constants: LotteryConstants::new(raw.slot_activation_coeff),
        })
    }
}

impl Config {
    #[must_use]
    pub fn new(
        security_param: NonZero<u32>,
        slot_activation_coeff: ProperFraction,
        stake_inference_learning_rate: NonNegativeF64,
        uncle_reference_window_in_block: u32,
        max_uncles: u8,
    ) -> Self {
        Self {
            security_param,
            slot_activation_coeff,
            stake_inference_learning_rate,
            uncle_reference_window_in_block,
            max_uncles,
            lottery_constants: LotteryConstants::new(slot_activation_coeff),
        }
    }

    /// The most uncles a block may reference.
    #[must_use]
    pub const fn max_uncles(&self) -> u8 {
        self.max_uncles
    }

    /// `W * f^-1`, the maximum number of slots by which the parent of a
    /// referenced uncle may precede the block referencing it.
    ///
    /// Zero when `W` is zero, which disables uncle references.
    #[must_use]
    pub const fn uncle_reference_window_in_slot(&self) -> u64 {
        match NonZero::new(self.uncle_reference_window_in_block) {
            Some(window) => average_slots_for_blocks(window, self.slot_activation_coeff).get(),
            None => 0,
        }
    }

    #[must_use]
    pub const fn security_param(&self) -> NonZero<u32> {
        self.security_param
    }

    #[must_use]
    pub const fn slot_activation_coeff(&self) -> ProperFraction {
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

    /// `s = 3⌊k/f⌋`: sufficient slots such that `k` blocks have been produced
    /// with high probability.
    #[must_use]
    pub const fn slot_security_param(&self) -> NonZero<u64> {
        self.base_period_length()
            .saturating_mul(NonZero::new(3).expect("3 is non-zero"))
    }

    #[must_use]
    pub const fn stake_inference_learning_rate(&self) -> f64 {
        self.stake_inference_learning_rate.get()
    }

    /// sufficient time measured in slots to measure the density of block
    /// production with enough statistical significance.
    #[must_use]
    pub const fn s_gen(&self) -> NonZero<u64> {
        NonZero::new(genesis_density_window(
            self.security_param,
            self.slot_activation_coeff,
        ))
        .expect("s_gen with proper configuration should never be zero")
    }
}

/// `⌊0.6k⌋`, the upper bound on uncle reference window (`W` blocks).
///
/// Keeping the uncle reference window within the finalization window ensures
/// the parent of any uncle a block may reference is still above LIB.
#[must_use]
pub fn max_allowed_uncle_reference_window_in_block(security_param: NonZero<u32>) -> u32 {
    // Widen `k` to `u64` first, because `6k` can overflow `u32`.
    let k = NonZeroU64::from(security_param);
    u32::try_from(k.get() * 6 / 10).expect("floor(0.6k) <= k, so it fits in u32")
}

/// `⌊k/4f⌋`: the slots [`Config::s_gen`] measures block density over, which a
/// configuration must keep above zero.
///
/// Computed in integers, as `⌊k · f_den / (4 · f_num)⌋`: a double can fall just
/// short of the whole number the quotient equals, and its floor then loses a
/// slot.
#[must_use]
pub const fn genesis_density_window(
    security_param: NonZero<u32>,
    slot_activation_coeff: ProperFraction,
) -> u64 {
    let NonNegativeRatio {
        numerator,
        denominator,
    } = slot_activation_coeff.get();
    (security_param.get() as u64 * denominator.get() as u64) / (4 * numerator as u64)
}

#[must_use]
pub const fn base_period_length(
    security_param: NonZero<u32>,
    slot_activation_coeff: ProperFraction,
) -> NonZero<u64> {
    average_slots_for_blocks(security_param, slot_activation_coeff)
}

/// `⌊n/f⌋`: the slots in which `n` blocks are expected.
///
/// Computed in integers, as `⌊n · f_den / f_num⌋`: a double can fall just
/// short of the whole number the quotient equals, and its floor then loses a
/// slot.
#[must_use]
pub const fn average_slots_for_blocks(
    num_blocks: NonZero<u32>,
    slot_activation_coeff: ProperFraction,
) -> NonZero<u64> {
    let NonNegativeRatio {
        numerator,
        denominator,
    } = slot_activation_coeff.get();
    // Below 1, `f` spreads each block over at least one slot.
    NonZero::new((num_blocks.get() as u64 * denominator.get() as u64) / numerator as u64)
        .expect("a block takes at least one slot when `f` is below 1")
}

/// `N_b`: the number of blocks an epoch of `epoch_length` slots is expected to
/// produce, `epoch_length * f`.
///
/// For the standard 3/3/4 phase split this works out to `10k`: the epoch spans
/// ten base periods and a base period is `k / f` slots. It is therefore never a
/// free parameter — anything that needs a per-epoch block count derives it from
/// the schedule rather than carrying its own copy.
///
/// # Panics
///
/// If an epoch is too short to expect a block, since such a chain produces no
/// blocks at all.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "Below 1, `f` keeps the quotient below `epoch_length`, a `u64`."
)]
pub const fn expected_blocks_per_epoch(
    epoch_length: u64,
    slot_activation_coeff: ProperFraction,
) -> NonZero<u64> {
    let NonNegativeRatio {
        numerator,
        denominator,
    } = slot_activation_coeff.get();
    // Widened so the multiplication cannot wrap: `epoch_length` spans the whole
    // of `u64` and the numerator the whole of `u32`.
    let blocks = (epoch_length as u128 * numerator as u128) / denominator.get() as u128;
    NonZero::new(blocks as u64)
        .expect("expected_blocks_per_epoch with proper configuration should never be zero")
}

#[cfg(test)]
mod tests {
    use std::ops::Mul as _;

    use super::*;

    #[test]
    fn test_config() {
        let config = Config::new(
            NonZero::new(10).unwrap(),
            ProperFraction::new(1, 5.try_into().unwrap()).unwrap(),
            0.1.try_into().unwrap(),
            12,
            4,
        );
        assert_eq!(config.security_param(), NonZero::new(10).unwrap());
        assert_eq!(config.base_period_length(), NonZero::new(50).unwrap());
        assert_eq!(config.slot_security_param(), NonZero::new(150).unwrap());
        assert_eq!(config.uncle_reference_window_in_slot(), 60);
        assert_eq!(config.s_gen(), NonZero::new(12).unwrap());
        assert_eq!(
            config.stake_inference_learning_rate().mul(10.0).floor() as u64,
            1,
        );
    }

    /// `k = 12` and `f = 1/75`: `k/f` is 900 and `k/4f` is 225, which a double
    /// makes 899.9999999999999 and 224.99999999999997, and their floors 899 and
    /// 224.
    #[test]
    fn the_base_period_and_s_gen_are_exact() {
        let config = Config::new(
            NonZero::new(12).unwrap(),
            ProperFraction::new(1, 75.try_into().unwrap()).unwrap(),
            0.1.try_into().unwrap(),
            12,
            4,
        );
        assert_eq!(config.base_period_length(), NonZero::new(900).unwrap());
        assert_eq!(config.s_gen(), NonZero::new(225).unwrap());
    }

    #[test]
    fn test_max_allowed_uncle_reference_window_in_block() {
        for (k, expected) in [
            (1, 0),
            (2, 1),
            (3, 1),
            (4, 2),
            (5, 3),
            (10, 6),
            (30, 18),
            (120, 72),
            (u32::MAX, 2_576_980_377),
        ] {
            assert_eq!(
                max_allowed_uncle_reference_window_in_block(NonZero::new(k).unwrap()),
                expected,
                "k={k}"
            );
        }
    }

    #[test]
    fn test_zero_uncle_reference_window() {
        let config = Config::new(
            NonZero::new(10).unwrap(),
            ProperFraction::new(1, 5.try_into().unwrap()).unwrap(),
            0.1.try_into().unwrap(),
            0,
            4,
        );
        assert_eq!(config.uncle_reference_window_in_slot(), 0);
    }

    #[test]
    fn test_expected_blocks_per_epoch_is_ten_k() {
        // k = 10, f = 1/5: a base period is 50 slots and the standard 3/3/4
        // split makes the epoch ten of them.
        let slot_activation_coeff = ProperFraction::new(1, 5.try_into().unwrap()).unwrap();
        let epoch_length =
            10 * base_period_length(NonZero::new(10).unwrap(), slot_activation_coeff).get();
        assert_eq!(
            expected_blocks_per_epoch(epoch_length, slot_activation_coeff),
            NonZero::new(100).unwrap(),
        );
    }
}
