use std::num::NonZero;

use lb_pol::LotteryConstants;
use lb_utils::math::{NonNegativeF64, NonNegativeRatio};

#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The `k` parameter in the Common Prefix property.
    /// Blocks deeper than k are generally considered stable and forks deeper
    /// than that trigger the additional fork selection rule, which is
    /// however only expected to be used during bootstrapping.
    security_param: NonZero<u32>,
    /// `f`, the rate of occupied slots
    slot_activation_coeff: NonNegativeRatio,
    stake_inference_learning_rate: NonNegativeF64,
    /// Lottery approximation constants computed from `slot_activation_coeff`
    #[cfg_attr(feature = "serde", serde(skip))]
    lottery_constants: LotteryConstants,
}

#[cfg(feature = "serde")]
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
        }

        let raw = RawConfig::deserialize(deserializer)?;

        Ok(Self {
            security_param: raw.security_param,
            slot_activation_coeff: raw.slot_activation_coeff,
            stake_inference_learning_rate: raw.stake_inference_learning_rate,
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
    ) -> Self {
        Self {
            security_param,
            slot_activation_coeff,
            stake_inference_learning_rate,
            lottery_constants: LotteryConstants::new(slot_activation_coeff),
        }
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
        NonZero::new(
            ((self.security_param.get() as f64) / self.slot_activation_coeff.as_f64()).floor()
                as u64,
        )
        .expect("base_period_length with proper configuration should never be zero")
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
        );
        assert_eq!(config.security_param(), NonZero::new(10).unwrap());
        assert_eq!(config.base_period_length(), NonZero::new(50).unwrap());
        assert_eq!(config.s_gen(), NonZero::new(12).unwrap());
        assert_eq!(
            config.stake_inference_learning_rate().mul(10.0).floor() as u64,
            1,
        );
    }
}
