pub const PRECISION: u64 = 1000;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Copy, Clone)]
pub struct StakeInference {
    learning_rate: f64,
    slot_activation_coefficient: f64,
    period: u64,
}

impl StakeInference {
    pub const fn new(learning_rate: f64, slot_activation_coefficient: f64, period: u64) -> Self {
        Self {
            learning_rate,
            slot_activation_coefficient,
            period,
        }
    }

    pub const fn period(&self) -> u64 {
        self.period
    }

    pub fn total_stake_inference<const PRECISION: u64>(
        &self,
        total_stake_estimate: u64,
        measured_block_density: u64,
    ) -> u64 {
        let learning_rate_with_precision: u64 =
            f64::trunc(self.learning_rate * PRECISION as f64) as u64;
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
    use std::{collections::HashMap, sync::Arc};

    use lb_core::sdp::MinStake;
    use lb_utils::math::NonNegativeRatio;

    use super::*;
    use crate::{
        Config,
        mantle::sdp::{ServiceRewardsParameters, rewards::blend},
    };

    const SECURITY_PARAM: u32 = 10;
    const LEARNING_RATE: f64 = 1f64;

    #[test]
    fn test_total_stake_inference_zero_block_density() {
        let config = config(NonNegativeRatio::new(1, 2.try_into().unwrap()));
        let inference = stake_inference_from(&config);
        let total_stake_estimate = 1000u64;
        let period_block_density = 0u64;

        let result = inference
            .total_stake_inference::<PRECISION>(total_stake_estimate, period_block_density);

        // minimum stake is 1
        assert_eq!(result, 1);
    }

    #[test]
    fn test_total_stake_inference_high_block_density() {
        let config = config(NonNegativeRatio::new(1, 2.try_into().unwrap()));
        let inference = stake_inference_from(&config);
        let total_stake_estimate = 1000u64;
        let measured_block_density = expected_density(&inference) * 2;

        let result = inference
            .total_stake_inference::<PRECISION>(total_stake_estimate, measured_block_density);

        // total stake should decrease because blocks more than expected were produced
        assert!(
            result > total_stake_estimate,
            "result({result}) must be > total_stake_estimate({total_stake_estimate})"
        );
    }

    #[test]
    fn test_total_stake_inference_exact_block_density() {
        let config = config(NonNegativeRatio::new(1, 2.try_into().unwrap()));
        let inference = stake_inference_from(&config);
        let total_stake_estimate = 1000u64;
        let measured_block_density = expected_density(&inference);

        let result = inference
            .total_stake_inference::<PRECISION>(total_stake_estimate, measured_block_density);

        // total stake shouldn't change because the measured density matches the
        // expected one
        assert_eq!(result, total_stake_estimate);
    }

    #[test]
    fn test_total_stake_inference_intermediate_block_density() {
        let config = config(NonNegativeRatio::new(1, 2.try_into().unwrap()));
        let inference = stake_inference_from(&config);
        let total_stake_estimate = 1000u64;
        let measured_block_density = expected_density(&inference) / 2;

        let result = inference
            .total_stake_inference::<PRECISION>(total_stake_estimate, measured_block_density);

        // With intermediate density, result should be between 0 and
        // total_stake_estimate
        assert!(
            result < total_stake_estimate,
            "result({result}) must be < total_stake_estimate({total_stake_estimate})"
        );
    }

    #[test]
    fn test_total_stake_inference_very_high_stake() {
        let config = config(NonNegativeRatio::new(1, 2.try_into().unwrap()));
        let inference = stake_inference_from(&config);
        let total_stake_estimate = u64::MAX; //maximum stake supported is half
        let measured_block_density = expected_density(&inference) / 2;

        let result = inference
            .total_stake_inference::<PRECISION>(total_stake_estimate, measured_block_density);

        // Should handle large numbers without overflow
        assert!(
            result <= total_stake_estimate,
            "result({result}) must be <= total_stake_estimate({total_stake_estimate})"
        );
    }

    fn stake_inference_from(config: &Config) -> StakeInference {
        StakeInference::new(
            config.consensus_config.stake_inference_learning_rate(),
            config.consensus_config.slot_activation_coeff().as_f64(),
            config.total_stake_inference_period(),
        )
    }

    fn config(slot_activation_coeff: NonNegativeRatio) -> Config {
        Config {
            epoch_config: lb_cryptarchia_engine::EpochConfig {
                epoch_stake_distribution_stabilization: 3.try_into().unwrap(),
                epoch_period_nonce_buffer: 3.try_into().unwrap(),
                epoch_period_nonce_stabilization: 4.try_into().unwrap(),
            },
            consensus_config: lb_cryptarchia_engine::Config::new(
                SECURITY_PARAM.try_into().unwrap(),
                slot_activation_coeff,
                LEARNING_RATE.try_into().unwrap(),
            ),
            // Not used in the tests
            sdp_config: crate::mantle::sdp::Config {
                service_params: Arc::new(HashMap::new()),
                service_rewards_params: ServiceRewardsParameters {
                    blend: blend::RewardsParameters {
                        rounds_per_session: 10.try_into().unwrap(),
                        message_frequency_per_round: 1.0.try_into().unwrap(),
                        num_blend_layers: 1.try_into().unwrap(),
                        data_replication_factor: 0,
                        minimum_network_size: 1.try_into().unwrap(),
                        activity_threshold_sensitivity: 1,
                    },
                },
                min_stake: MinStake {
                    threshold: 0,
                    timestamp: 0,
                },
            },
            faucet_pk: None,
        }
    }

    fn expected_density(inference: &StakeInference) -> u64 {
        (inference.period() as f64 * inference.slot_activation_coefficient).floor() as u64
    }
}
