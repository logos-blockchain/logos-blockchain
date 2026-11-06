use core::time::Duration;

use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use serde::{Deserialize, Serialize};

use crate::config::deployment::InvalidDeployment;

pub mod blend;
pub mod cryptarchia;
pub mod time;

use self::{
    blend::Settings as BlendDeploymentSettings,
    cryptarchia::Settings as CryptarchiaDeploymentSettings,
    time::Settings as TimeDeploymentSettings,
};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Parameters {
    pub blend: BlendDeploymentSettings,
    pub cryptarchia: CryptarchiaDeploymentSettings,
    pub time: TimeDeploymentSettings,
}

impl Parameters {
    /// Checks the values of the era's parameters, each alone and together, and
    /// against those of the era before it, `previous`, if there is one: the
    /// values the specifications rule out, and those the node could not run
    /// with. Refuses the first found invalid.
    pub(super) fn validate(&self, previous: Option<&Self>) -> Result<(), InvalidDeployment> {
        self.cryptarchia
            .validate(previous.map(|previous| &previous.cryptarchia))?;
        self.blend.validate(
            self.cryptarchia.slots_per_epoch(),
            &self.time.slot_duration(),
        )
    }

    #[must_use]
    pub const fn blend_round_duration(&self) -> Duration {
        self.blend.round_duration(&self.time.slot_duration())
    }

    #[must_use]
    pub fn blend_reward_params(&self) -> RewardsParameters {
        self.blend.rewards_params(&self.cryptarchia, &self.time)
    }
}

#[cfg(test)]
mod tests {
    use core::num::{NonZero, NonZeroU32};

    use lb_core::mantle::gas::Gas;
    use lb_utils::math::NonNegativeRatio;

    use super::{Parameters, cryptarchia::SlotActivationCoeff};
    use crate::config::{DeploymentSettings, deployment::InvalidDeployment};

    fn parameters() -> Parameters {
        DeploymentSettings::default()
            .genesis_era_parameters()
            .clone()
    }

    /// Validates the default parameters with `change`, as a genesis era.
    fn validate(change: impl FnOnce(&mut Parameters)) -> Result<(), InvalidDeployment> {
        let mut parameters = parameters();
        change(&mut parameters);
        parameters.validate(None)
    }

    /// Why the default parameters with `change` are refused, as a genesis era.
    fn rejection(change: impl FnOnce(&mut Parameters)) -> String {
        validate(change).unwrap_err().to_string()
    }

    fn ratio(numerator: u32, denominator: u32) -> NonNegativeRatio {
        NonNegativeRatio::new(numerator, NonZero::new(denominator).unwrap())
    }

    fn coeff(numerator: u32, denominator: u32) -> SlotActivationCoeff {
        ratio(numerator, denominator).try_into().unwrap()
    }

    /// The default parameters with `k`, `f = 1/f_inverse` and `W` replaced.
    fn parameters_with(security_param: u32, f_inverse: u32, window: u32) -> Parameters {
        let mut parameters = parameters();
        let cryptarchia = &mut parameters.cryptarchia;
        cryptarchia.security_param = NonZero::new(security_param).unwrap();
        cryptarchia.slot_activation_coeff = coeff(1, f_inverse);
        cryptarchia.uncle_reference_window_in_block = window;
        parameters
    }

    #[test]
    fn the_slot_activation_coeff_is_below_1_and_at_least_the_inference_precision() {
        for (numerator, denominator) in [(0, 1), (1, 1), (3, 2), (1, 1001)] {
            assert!(SlotActivationCoeff::try_from(ratio(numerator, denominator)).is_err());
        }
        for (numerator, denominator) in [(1, 1000), (999, 1000)] {
            assert!(SlotActivationCoeff::try_from(ratio(numerator, denominator)).is_ok());
        }
    }

    #[test]
    fn values_at_the_bounds_are_accepted() {
        validate(|parameters| {
            parameters.cryptarchia.security_param = NonZero::new(30).unwrap();
            parameters.cryptarchia.uncle_reference_window_in_block = 18;
        })
        .unwrap();
        for coefficient in [coeff(1, 1000), coeff(999, 1000)] {
            validate(|parameters| parameters.cryptarchia.slot_activation_coeff = coefficient)
                .unwrap();
        }
        for degree in [3, 5] {
            validate(|parameters| {
                parameters.blend.core.target_peering_degree = NonZero::new(degree).unwrap();
            })
            .unwrap();
        }
        // An Operation may cost as much as a block consumes.
        validate(|parameters| {
            let max_gas_per_block = parameters.cryptarchia.execution_market.max_gas_per_block;
            parameters.cryptarchia.gas_profile.sdp_active = Gas::from(max_gas_per_block);
        })
        .unwrap();
        // 6,000 rounds shared by 2 nodes, 349 blending operations per message:
        // 1,047,000 of the 1,048,575 a `Quota` holds.
        validate(|parameters| {
            parameters.blend.common.num_blend_layers = NonZero::new(349).unwrap();
        })
        .unwrap();
        // At a peering degree of 4, `⌊2V/3⌋` reaches 5.
        validate(|parameters| {
            parameters.blend.core.target_peering_degree = NonZero::new(4).unwrap();
            parameters.blend.core.verification_rate_per_second = NonZero::new(8).unwrap();
        })
        .unwrap();
        // k = 10 and f = 1/20: s = 600 slots, a fifth of which is 120 slots, 6
        // blocks of the next era at its f of 1/20.
        parameters_with(30, 20, 6)
            .validate(Some(&parameters_with(10, 20, 6)))
            .unwrap();
    }

    #[test]
    fn a_core_quota_past_the_poq_circuit_is_refused() {
        // 6,000 rounds shared by 2 nodes, 350 blending operations per message.
        let reason = rejection(|parameters| {
            parameters.blend.common.num_blend_layers = NonZero::new(350).unwrap();
        });
        assert!(
            reason.contains("value 1050000 does not fit in 20 bits"),
            "{reason}"
        );
    }

    #[test]
    fn a_genesis_density_window_without_slots_is_refused() {
        // ⌊k/4f⌋ = ⌊1/2⌋.
        let reason = rejection(|parameters| {
            parameters.cryptarchia.security_param = NonZero::new(1).unwrap();
            parameters.cryptarchia.slot_activation_coeff = coeff(1, 2);
        });
        assert!(reason.contains("s_gen"), "{reason}");
    }

    #[test]
    fn an_uncle_reference_window_wider_than_three_fifths_of_k_is_refused() {
        let reason = rejection(|parameters| {
            parameters.cryptarchia.security_param = NonZero::new(30).unwrap();
            parameters.cryptarchia.uncle_reference_window_in_block = 19;
        });
        assert!(reason.contains("of 19 blocks is wider than"), "{reason}");
    }

    #[test]
    fn an_uncle_reference_window_wider_than_a_fifth_of_the_previous_finalization_window_is_refused()
    {
        // k = 10 and f = 1/20: s = 600 slots, a fifth of which is 120 slots, 6
        // blocks of the next era at its f of 1/20. Its 7 blocks span 140.
        let reason = parameters_with(30, 20, 7)
            .validate(Some(&parameters_with(10, 20, 6)))
            .unwrap_err()
            .to_string();
        assert!(
            reason.contains(
                "window of 140 slots is wider than a fifth of the previous era's finalization \
                 window, 600 slots"
            ),
            "{reason}"
        );
    }

    #[test]
    fn the_same_uncle_reference_window_is_refused_at_a_lower_f() {
        // The 6 blocks that span a fifth of the previous era's 600 slots at an
        // f of 1/20 span 240 at the next era's f of 1/40.
        let reason = parameters_with(30, 40, 6)
            .validate(Some(&parameters_with(10, 20, 6)))
            .unwrap_err()
            .to_string();
        assert!(
            reason.contains(
                "window of 240 slots is wider than a fifth of the previous era's finalization \
                 window, 600 slots"
            ),
            "{reason}"
        );
    }

    #[test]
    fn an_operation_costing_more_than_a_block_consumes_is_refused() {
        let reason = rejection(|parameters| {
            let max_gas_per_block = parameters.cryptarchia.execution_market.max_gas_per_block;
            parameters.cryptarchia.gas_profile.sdp_active =
                Gas::new(u64::from(max_gas_per_block) + 1);
        });
        assert!(
            reason.contains("The execution gas of an SDP activity"),
            "{reason}"
        );
    }

    #[test]
    fn a_blend_damping_denominator_beyond_a_u32_is_refused() {
        let reason = rejection(|parameters| {
            let blend = &mut parameters.cryptarchia.pow_config.blend;
            blend.damping_num = NonZeroU32::MAX;
            blend.damping_den_offset = 1;
        });
        assert!(reason.contains("damping exponent"), "{reason}");
    }

    #[test]
    fn a_blend_peering_degree_above_5_is_refused() {
        let reason = rejection(|parameters| {
            parameters.blend.core.target_peering_degree = NonZero::new(6).unwrap();
        });
        assert!(
            reason.contains("peering degree, 6, must be in 3..=5"),
            "{reason}"
        );
    }

    #[test]
    fn a_blend_verification_rate_without_a_message_per_connection_is_refused() {
        // At a peering degree of 4, `⌊2V/3⌋` is 4.
        let reason = rejection(|parameters| {
            parameters.blend.core.target_peering_degree = NonZero::new(4).unwrap();
            parameters.blend.core.verification_rate_per_second = NonZero::new(7).unwrap();
        });
        assert!(
            reason.contains("at least one message per connection per round"),
            "{reason}"
        );
    }

    #[test]
    fn a_blend_edge_node_send_deadline_beyond_a_timeout_is_refused() {
        let reason = rejection(|parameters| {
            parameters.blend.core.edge_node_send_deadline_in_rounds = NonZero::<u64>::MAX;
        });
        assert!(reason.contains("`T_E` must fit"), "{reason}");
    }
}
