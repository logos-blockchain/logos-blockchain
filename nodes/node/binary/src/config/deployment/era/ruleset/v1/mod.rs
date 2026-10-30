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

    use lb_utils::math::NonNegativeRatio;

    use super::{Parameters, cryptarchia::SlotActivationCoeff};
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

    fn coeff(numerator: u32, denominator: u32) -> SlotActivationCoeff {
        ratio(numerator, denominator).try_into().unwrap()
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
        });
        for coefficient in [coeff(1, 1000), coeff(999, 1000)] {
            validate(|parameters| parameters.cryptarchia.slot_activation_coeff = coefficient);
        }
        // At a peering degree of 4, `⌊2V/3⌋` reaches 5.
        validate(|parameters| {
            parameters.blend.core.verification_rate_per_second = NonZero::new(8).unwrap();
        });
    }

    #[test]
    #[should_panic(expected = "s_gen")]
    fn a_genesis_density_window_without_slots_is_refused() {
        // ⌊k/4f⌋ = ⌊1/2⌋.
        validate(|parameters| {
            parameters.cryptarchia.security_param = NonZero::new(1).unwrap();
            parameters.cryptarchia.slot_activation_coeff = coeff(1, 2);
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
        previous.cryptarchia.slot_activation_coeff = coeff(1, 20);
        previous.cryptarchia.uncle_reference_window_in_block = 6;
        let mut next = parameters();
        next.cryptarchia.security_param = NonZero::new(30).unwrap();
        next.cryptarchia.slot_activation_coeff = coeff(1, 20);
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
    #[should_panic(expected = "at least one message per connection per round")]
    fn a_blend_verification_rate_without_a_message_per_connection_is_refused() {
        // At a peering degree of 4, `⌊2V/3⌋` is 4.
        validate(|parameters| {
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
