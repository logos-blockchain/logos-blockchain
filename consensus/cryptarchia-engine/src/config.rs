use std::num::NonZero;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    // The k parameter in the Common Prefix property.
    // Blocks deeper than k are generally considered stable and forks deeper than that
    // trigger the additional fork selection rule, which is however only expected to be used
    // during bootstrapping.
    security_param: NonZero<u32>,
    base_period_length: NonZero<u64>,
}

impl Config {
    #[must_use]
    pub const fn new(security_param: NonZero<u32>, active_slot_coefficient: f64) -> Self {
        Self {
            security_param,
            base_period_length: Self::compute_base_period_length(
                security_param,
                active_slot_coefficient,
            ),
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
    pub const fn base_period_length(&self) -> NonZero<u64> {
        self.base_period_length
    }

    // return the number of slots required to have great confidence at least k
    // blocks have been produced
    #[must_use]
    pub const fn s(&self) -> u64 {
        self.base_period_length().get().saturating_mul(3)
    }
}
