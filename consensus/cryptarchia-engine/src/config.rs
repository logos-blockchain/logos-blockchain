use std::num::NonZero;

use lb_utils::math::NonNegativeRatio;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The `k` parameter in the Common Prefix property.
    /// Blocks deeper than k are generally considered stable and forks deeper
    /// than that trigger the additional fork selection rule, which is
    /// however only expected to be used during bootstrapping.
    security_param: NonZero<u32>,
    /// `f`, the rate of occupied slots
    slot_activation_coeff: NonNegativeRatio,
}

impl Config {
    #[must_use]
    pub const fn new(
        security_param: NonZero<u32>,
        slot_activation_coeff: NonNegativeRatio,
    ) -> Self {
        Self {
            security_param,
            slot_activation_coeff,
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
    pub const fn base_period_length(&self) -> NonZero<u64> {
        NonZero::new(
            ((self.security_param.get() as f64) / self.slot_activation_coeff.as_f64()).floor()
                as u64,
        )
        .expect("base_period_length with proper configuration should never be zero")
    }

    // return the number of slots required to have great confidence at least k
    // blocks have been produced
    #[must_use]
    pub const fn s(&self) -> u64 {
        self.base_period_length().get().saturating_mul(3)
    }
}
