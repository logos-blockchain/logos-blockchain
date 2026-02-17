use std::num::NonZero;

use lb_utils::math::NonNegativeF64;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    // The k parameter in the Common Prefix property.
    // Blocks deeper than k are generally considered stable and forks deeper than that
    // trigger the additional fork selection rule, which is however only expected to be used
    // during bootstrapping.
    security_param: NonZero<u32>,
    base_period_length: NonZero<u64>,
    /// sufficient time measured in slots to measure the density of block
    /// production with enough statistical significance.
    s_gen: NonZero<u64>,
    stake_inference_learning_rate: NonNegativeF64,
}

impl Config {
    #[must_use]
    pub const fn new(
        security_param: NonZero<u32>,
        active_slot_coefficient: f64,
        stake_inference_learning_rate: NonNegativeF64,
    ) -> Self {
        Self {
            security_param,
            base_period_length: Self::compute_base_period_length(
                security_param,
                active_slot_coefficient,
            ),
            s_gen: Self::compute_s_gen(security_param, active_slot_coefficient),
            stake_inference_learning_rate,
        }
    }

    #[must_use]
    pub const fn security_param(&self) -> NonZero<u32> {
        self.security_param
    }

    #[must_use]
    const fn compute_base_period_length(
        security_param: NonZero<u32>,
        active_slot_coefficient: f64,
    ) -> NonZero<u64> {
        NonZero::new(((security_param.get() as f64) / active_slot_coefficient).floor() as u64)
            .expect("base_period_length with proper configuration should never be zero")
    }

    #[must_use]
    const fn compute_s_gen(
        security_param: NonZero<u32>,
        active_slot_coefficient: f64,
    ) -> NonZero<u64> {
        NonZero::new(
            ((security_param.get() as f64) / (4.0 * active_slot_coefficient)).floor() as u64,
        )
        .expect("s_gen with proper configuration should never be zero")
    }

    #[must_use]
    pub const fn base_period_length(&self) -> NonZero<u64> {
        self.base_period_length
    }

    #[must_use]
    pub const fn stake_inference_learning_rate(&self) -> f64 {
        self.stake_inference_learning_rate.get()
    }

    #[must_use]
    pub const fn s_gen(&self) -> NonZero<u64> {
        self.s_gen
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Mul as _;

    use super::*;

    #[test]
    fn test_config() {
        let config = Config::new(NonZero::new(10).unwrap(), 0.2, 0.1.try_into().unwrap());
        assert_eq!(config.security_param(), NonZero::new(10).unwrap());
        assert_eq!(config.base_period_length(), NonZero::new(50).unwrap());
        assert_eq!(config.s_gen(), NonZero::new(12).unwrap());
        assert_eq!(
            config.stake_inference_learning_rate().mul(10.0).floor() as u64,
            1,
        );
    }
}
