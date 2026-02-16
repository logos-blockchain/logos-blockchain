use std::ops::Div as _;

/// Current learning rate as per [especification](https://nomos-tech.notion.site/Total-Stake-Inference-22d261aa09df8051a454caa46ec54b34), this is not configurable.
pub const LEARNING_RATE: u64 = 1;
pub const PRECISION: u64 = 1000;

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
        (self.security_parameter as f64)
            .div(self.slot_activation_coefficient)
            .floor() as u64
            * PERIOD_CONSTANT
    }

    pub fn total_stake_inference<const PRECISION: u64>(
        &self,
        total_stake_estimate: u64,
        measured_block_density: u64,
    ) -> u64 {
        let learning_rate_with_precision: u64 = self.learning_rate * PRECISION;
        let slot_activation_coefficient_with_precision: i128 =
            (self.slot_activation_coefficient * PRECISION as f64).trunc() as i128;
        let total_stake_estimate_with_precision: i128 =
            i128::from(total_stake_estimate) * i128::from(PRECISION);
        let measured_block_density_with_precision: i128 =
            i128::from(measured_block_density) * i128::from(PRECISION);
        let expected_density_with_precision: i128 =
            i128::from(self.period()) * slot_activation_coefficient_with_precision;
        let density_difference_with_precision: i128 =
            expected_density_with_precision - measured_block_density_with_precision;
        let slot_activation_error_with_precision: i128 = total_stake_estimate_with_precision
            * density_difference_with_precision
            / expected_density_with_precision;
        let correction: i128 = (i128::from(learning_rate_with_precision)
            * slot_activation_error_with_precision)
            / i128::from(PRECISION);
        let new_total_stake_estimate =
            (total_stake_estimate_with_precision - correction) / i128::from(PRECISION);

        tracing::debug!(
            old_total_stake = total_stake_estimate,
            new_total_stake = new_total_stake_estimate,
            measured_density = measured_block_density,
            expected_density = expected_density_with_precision / i128::from(PRECISION),
            learning_rate = self.learning_rate,
            period = self.period(),
            "TSI update"
        );

        new_total_stake_estimate
            .max(1)
            .try_into()
            .expect("After precision it should fit in a u64")
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

        let result = inference
            .total_stake_inference::<PRECISION>(total_stake_estimate, period_block_density);

        // minimum stake is 1
        assert_eq!(result, 1);
    }

    #[test]
    fn test_total_stake_inference_max_block_density() {
        let inference = StakeInference::new(1, 1.0, 10);
        let total_stake_estimate = 1000u64;
        let period = inference.period(); // 10
        let period_block_density = period;

        let result = inference
            .total_stake_inference::<PRECISION>(total_stake_estimate, period_block_density);

        assert_eq!(result, total_stake_estimate);
    }

    #[test]
    fn test_total_stake_inference_intermediate_block_density() {
        let inference = StakeInference::new(1, 1.0, 10);
        let total_stake_estimate = 1000u64;
        let period_block_density = inference.period() / 2;

        let result = inference
            .total_stake_inference::<PRECISION>(total_stake_estimate, period_block_density);

        // With intermediate density, result should be between 0 and
        // total_stake_estimate
        assert!(result < total_stake_estimate);
    }

    #[test]
    fn test_total_stake_inference_very_high_stake() {
        let inference = StakeInference::new(1, 1.0, 10);
        let total_stake_estimate = u64::MAX; //maximum stake suported is half
        let period_block_density = inference.period();

        let result = inference
            .total_stake_inference::<PRECISION>(total_stake_estimate, period_block_density);

        // Should handle large numbers without overflow
        assert!(result <= total_stake_estimate);
    }
}
