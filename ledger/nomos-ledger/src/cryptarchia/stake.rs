use std::ops::{Div as _, Mul as _};

/// Current learning rate as per [especification](https://nomos-tech.notion.site/Total-Stake-Inference-22d261aa09df8051a454caa46ec54b34), this is not configurable.
pub const LEARNING_RATE: u64 = 1;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Copy, Clone)]
pub struct StakeInference {
    learning_rate: u64,
    slot_activation_coefficient: f64,
    security_parameter: u64,
}

impl StakeInference {
    pub const fn new(
        learning_rate: u64,
        slot_activation_coefficient: f64,
        security_parameter: u64,
    ) -> Self {
        Self {
            learning_rate,
            slot_activation_coefficient,
            security_parameter,
        }
    }

    pub fn period(&self) -> u64 {
        const PERIOD_CONSTANT: u64 = 6;
        ((self.security_parameter * PERIOD_CONSTANT) as f64)
            .div(self.slot_activation_coefficient)
            .floor() as u64
    }

    pub fn total_stake_inference(
        &self,
        total_stake_estimate: u64,
        period_block_density: u64,
    ) -> u64 {
        let slot_activation_error: u64 =
            1 - period_block_density / (self.period() * self.slot_activation_coefficient as u64);
        let coefficient = self.learning_rate.mul(total_stake_estimate);
        total_stake_estimate - coefficient * slot_activation_error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_period_calculation_with_different_security_params() {
        let inference1 = StakeInference::new(1, 1.0, 5);
        assert_eq!(inference1.period(), 30);

        let inference2 = StakeInference::new(1, 1.0, 20);
        assert_eq!(inference2.period(), 120);
    }

    #[test]
    fn test_period_calculation_with_fractional_results() {
        let inference = StakeInference::new(1, 1.0, 7);
        assert_eq!(inference.period(), 42); // 7 * 6 / 1

        let inference2 = StakeInference::new(1, 0.9, 10);
        assert_eq!(inference2.period(), 66); // 10 * 6 / 0.9
    }

    #[test]
    fn test_total_stake_inference_zero_block_density() {
        let inference = StakeInference::new(1, 1.0, 10);
        let total_stake_estimate = 1000u64;
        let period_block_density = 0u64;

        let result = inference.total_stake_inference(total_stake_estimate, period_block_density);

        assert_eq!(result, 0);
    }

    #[test]
    fn test_total_stake_inference_max_block_density() {
        let inference = StakeInference::new(1, 1.0, 10);
        let total_stake_estimate = 1000u64;
        let period = inference.period(); // 10
        let period_block_density = period;

        let result = inference.total_stake_inference(total_stake_estimate, period_block_density);

        assert_eq!(result, total_stake_estimate);
    }

    #[test]
    fn test_total_stake_inference_intermediate_block_density() {
        let inference = StakeInference::new(1, 1.0, 10);
        let total_stake_estimate = 1000u64;
        let period_block_density = inference.period() / 2;

        let result = inference.total_stake_inference(total_stake_estimate, period_block_density);

        // With intermediate density, result should be between 0 and
        // total_stake_estimate
        assert!(result < total_stake_estimate);
    }

    #[test]
    fn test_total_stake_inference_very_high_stake() {
        let inference = StakeInference::new(1, 1.0, 10);
        let total_stake_estimate = u64::MAX;
        let period_block_density = inference.period();

        let result = inference.total_stake_inference(total_stake_estimate, period_block_density);

        // Should handle large numbers without overflow
        assert!(result <= total_stake_estimate);
    }
}
