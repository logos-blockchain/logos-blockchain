use std::num::NonZero;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    // The k parameter in the Common Prefix property.
    // Blocks deeper than k are generally considered stable and forks deeper than that
    // trigger the additional fork selection rule, which is however only expected to be used
    // during bootstrapping.
    pub security_param: NonZero<u32>,
    #[serde(skip, default = "Config::active_slot_coefficient")]
    pub active_slot_coefficient: f64,
}

impl Config {
    #[must_use]
    pub const fn new(security_param: NonZero<u32>) -> Self {
        Self {
            security_param,
            active_slot_coefficient: Self::active_slot_coefficient(),
        }
    }
    #[must_use]
    const fn active_slot_coefficient() -> f64 {
        #[cfg(not(feature = "high-active-slot-coefficient"))]
        {
            1f64
        }
        #[cfg(feature = "high-active-slot-coefficient")]
        {
            1f64 / 30f64
        }
    }

    #[must_use]
    pub fn base_period_length(&self) -> NonZero<u64> {
        NonZero::new(
            (f64::from(self.security_param.get()) / Self::active_slot_coefficient()).floor() as u64,
        )
        .expect("base_period_length with proper configuration should never be zero")
    }

    // return the number of slots required to have great confidence at least k
    // blocks have been produced
    #[must_use]
    pub fn s(&self) -> u64 {
        self.base_period_length().get().saturating_mul(3)
    }
}
