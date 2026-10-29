use core::time::Duration;

use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use serde::{Deserialize, Serialize};

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
    /// with. Panics on the first found invalid.
    pub(super) fn validate(&self, previous: Option<&Self>) {
        self.cryptarchia
            .validate(previous.map(|previous| &previous.cryptarchia));
        self.blend.validate(&self.time.slot_duration());
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

    use lb_binary_codec::canonical::BTreeMap;
    use lb_utils::math::NonNegativeRatio;

    use super::Parameters;
    use crate::config::DeploymentSettings;

    fn parameters() -> Parameters {
        DeploymentSettings::default()
            .genesis_era_parameters()
            .clone()
    }

    /// Validates the default parameters with `change`, as a genesis era.
    fn validate(change: impl FnOnce(&mut Parameters)) {
        let mut parameters = parameters();
        change(&mut parameters);
        parameters.validate(None);
    }

    fn ratio(numerator: u32, denominator: u32) -> NonNegativeRatio {
        NonNegativeRatio::new(numerator, NonZero::new(denominator).unwrap())
    }

    #[test]
    fn values_at_the_bounds_are_accepted() {
        validate(|parameters| {
            parameters.cryptarchia.security_param = NonZero::new(30).unwrap();
            parameters.cryptarchia.uncle_reference_window_in_block = 18;
        });
        for coefficient in [ratio(1, 1000), ratio(999, 1000)] {
            validate(|parameters| parameters.cryptarchia.slot_activation_coeff = coefficient);
        }
        for degree in [3, 5] {
            validate(|parameters| {
                parameters.blend.core.target_peering_degree = NonZero::new(degree).unwrap();
            });
        }
        // At a peering degree of 4, `⌊2V/3⌋` reaches 5.
        validate(|parameters| {
            parameters.blend.core.target_peering_degree = NonZero::new(4).unwrap();
            parameters.blend.core.verification_rate_per_second = NonZero::new(8).unwrap();
        });
    }

    #[test]
    #[should_panic(expected = "slot activation coefficient 1/1 must be below 1")]
    fn a_slot_activation_coeff_of_1_is_refused() {
        validate(|parameters| parameters.cryptarchia.slot_activation_coeff = ratio(1, 1));
    }

    #[test]
    #[should_panic(expected = "slot activation coefficient 1/1001 must be below 1, and at least")]
    fn a_slot_activation_coeff_below_the_inference_precision_is_refused() {
        validate(|parameters| parameters.cryptarchia.slot_activation_coeff = ratio(1, 1001));
    }

    #[test]
    #[should_panic(expected = "s_gen")]
    fn a_genesis_density_window_without_slots_is_refused() {
        // ⌊k/4f⌋ = ⌊1/2⌋.
        validate(|parameters| {
            parameters.cryptarchia.security_param = NonZero::new(1).unwrap();
            parameters.cryptarchia.slot_activation_coeff = ratio(1, 2);
        });
    }

    #[test]
    #[should_panic(expected = "wider than ⌊0.6k⌋, 18 blocks")]
    fn an_uncle_reference_window_wider_than_three_fifths_of_k_is_refused() {
        validate(|parameters| {
            parameters.cryptarchia.security_param = NonZero::new(30).unwrap();
            parameters.cryptarchia.uncle_reference_window_in_block = 19;
        });
    }

    #[test]
    #[should_panic(expected = "wider than the 6 that a fifth of the previous era's")]
    fn an_uncle_reference_window_wider_than_a_fifth_of_the_previous_finalization_window_is_refused()
    {
        // k = 10 and f = 1/20: s = 600 slots, a fifth of which is 6 blocks of
        // the next era, at its f of 1/20.
        let mut previous = parameters();
        previous.cryptarchia.security_param = NonZero::new(10).unwrap();
        previous.cryptarchia.slot_activation_coeff = ratio(1, 20);
        previous.cryptarchia.uncle_reference_window_in_block = 6;
        let mut next = parameters();
        next.cryptarchia.security_param = NonZero::new(30).unwrap();
        next.cryptarchia.slot_activation_coeff = ratio(1, 20);
        next.cryptarchia.uncle_reference_window_in_block = 7;

        next.validate(Some(&previous));
    }

    #[test]
    #[should_panic(expected = "damping exponent")]
    fn a_blend_damping_denominator_beyond_a_u32_is_refused() {
        validate(|parameters| {
            let blend = &mut parameters.cryptarchia.pow_config.blend;
            blend.damping_num = NonZeroU32::MAX;
            blend.damping_den_offset = 1;
        });
    }

    #[test]
    #[should_panic(expected = "service type BN")]
    fn missing_service_parameters_are_refused() {
        validate(|parameters| {
            parameters.cryptarchia.sdp_config.service_params = BTreeMap::empty();
        });
    }

    #[test]
    #[should_panic(expected = "peering degree, 6, must be in 3..=5")]
    fn a_blend_peering_degree_above_5_is_refused() {
        validate(|parameters| {
            parameters.blend.core.target_peering_degree = NonZero::new(6).unwrap();
        });
    }

    #[test]
    #[should_panic(expected = "at least one message per connection per round")]
    fn a_blend_verification_rate_without_a_message_per_connection_is_refused() {
        // At a peering degree of 4, `⌊2V/3⌋` is 4.
        validate(|parameters| {
            parameters.blend.core.target_peering_degree = NonZero::new(4).unwrap();
            parameters.blend.core.verification_rate_per_second = NonZero::new(7).unwrap();
        });
    }

    #[test]
    #[should_panic(expected = "`T_E` must fit")]
    fn a_blend_edge_node_send_deadline_beyond_a_timeout_is_refused() {
        validate(|parameters| {
            parameters.blend.core.edge_node_send_deadline_in_rounds = NonZero::<u64>::MAX;
        });
    }
}
