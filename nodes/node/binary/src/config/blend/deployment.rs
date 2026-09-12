use core::{num::NonZeroU64, time::Duration};

pub use lb_blend_service::settings::MinimumNetworkSize;
use lb_blend_service::settings::max_data_message_delay_in_rounds;
use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use lb_libp2p::protocol_name::StreamProtocol;
use lb_utils::math::{NonNegativeF64, PositiveF64};
use serde::{Deserialize, Serialize};

use crate::config::{
    cryptarchia::deployment::Settings as CryptarchiaDeploymentSettings,
    time::deployment::Settings as TimeDeploymentSettings,
};

/// The values `blend-protocol.md` fixes for the whole network.
///
/// §Global Parameters, §Minimal Network Size and §Transition Period. A
/// deployment that departs from them still runs, but it does not provide the
/// anonymity the specification analyses, so the departure has to be
/// acknowledged in the deployment settings
/// ([`CommonSettings::acknowledged_spec_deviations`]).
pub mod spec {
    /// `∆max`: the maximal delay, in rounds, between two release rounds.
    pub const MAXIMUM_RELEASE_DELAY_IN_ROUNDS: u64 = 3;
    /// The smallest `∆max` for which the release delay is random at all: with
    /// `∆max = 1` every round is a release round and the delaying step of
    /// §Delaying does nothing.
    pub const MINIMUM_RANDOM_RELEASE_DELAY_IN_ROUNDS: u64 = 2;
    /// `ß_max`: the number of blending operations of a single message, which
    /// is also the number of encapsulation layers the wire format carries.
    pub const NUM_BLEND_LAYERS: u64 = 3;
    /// The minimal number of core nodes below which Blend must not be used.
    pub const MINIMUM_NETWORK_SIZE: u64 = 32;
    /// `⌊F_1⌋^W = 3·μ`: the multiplier of `μ` giving the minimum number of
    /// messages expected on a connection per observation window.
    pub const MINIMUM_MESSAGES_COEFFICIENT: u64 = 3;
    /// `T = 2·T_M`: the transition period is twice the message traversal
    /// time, "to provide an additional safety buffer".
    pub const TRANSITION_PERIOD_FACTOR: u64 = 2;
    /// `T = 30` rounds: the transition period the spec's values give.
    pub const TRANSITION_PERIOD_IN_ROUNDS: u64 = 30;
}

/// A way in which a deployment departs from `blend-protocol.md`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Violation {
    /// The release delay is not random; the spec's delaying step is inert.
    /// This is an internal inconsistency, not a tunable, so it is never
    /// acknowledgeable.
    #[error(
        "blend.core.scheduler.delayer.maximum_release_delay_in_rounds is {actual}; the release delay is only random for values >= {}",
        spec::MINIMUM_RANDOM_RELEASE_DELAY_IN_ROUNDS
    )]
    ReleaseDelayNotRandom { actual: u64 },
    #[error(
        "blend.core.scheduler.delayer.maximum_release_delay_in_rounds is {actual}; the spec fixes ∆max = {}",
        spec::MAXIMUM_RELEASE_DELAY_IN_ROUNDS
    )]
    ReleaseDelayDeviatesFromSpec { actual: u64 },
    #[error(
        "blend.common.num_blend_layers is {actual}; the spec fixes ß_max = {}",
        spec::NUM_BLEND_LAYERS
    )]
    BlendLayersDeviateFromSpec { actual: u64 },
    #[error(
        "blend.common.minimum_network_size is {actual}; the spec fixes {}",
        spec::MINIMUM_NETWORK_SIZE
    )]
    MinimumNetworkSizeDeviatesFromSpec { actual: u64 },
    #[error(
        "blend.core.minimum_messages_coefficient is {actual}; the spec's lower bound is {}·μ",
        spec::MINIMUM_MESSAGES_COEFFICIENT
    )]
    MinimumMessagesCoefficientDeviatesFromSpec { actual: u64 },
}

impl Violation {
    /// Whether the deployment may run with this violation once it declares
    /// `acknowledged_spec_deviations: true`.
    #[must_use]
    pub const fn is_acknowledgeable(&self) -> bool {
        !matches!(self, Self::ReleaseDelayNotRandom { .. })
    }
}

/// The reason a Blend deployment is refused.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Blend deployment settings are inconsistent: {}", format_violations(.0))]
    Inconsistent(Vec<Violation>),
    #[error(
        "Blend deployment settings deviate from blend-protocol.md and the deviation is not acknowledged (set blend.common.acknowledged_spec_deviations: true to run without the spec's anonymity guarantee): {}",
        format_violations(.0)
    )]
    UnacknowledgedDeviation(Vec<Violation>),
}

fn format_violations(violations: &[Violation]) -> String {
    violations
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Deployment-specific Blend settings.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub common: CommonSettings,
    pub core: CoreSettings,
}

impl Settings {
    #[must_use]
    pub const fn round_duration(&self, slot_duration: &Duration) -> Duration {
        *slot_duration
    }

    /// Number of rounds per epoch, calculated as the number of slots per
    /// epoch, correctly scaled to account for the slot/round ratio.
    #[must_use]
    pub fn rounds_per_epoch(&self, slots_per_epoch: u64, slot_duration: &Duration) -> NonZeroU64 {
        ((slots_per_epoch * slot_duration.as_secs()) / self.round_duration(slot_duration).as_secs())
            .try_into()
            .expect("There must be at least one round per epoch.")
    }

    /// Number of rounds per observation window.
    ///
    /// The Blend spec defines this as `10 * ∆max`, where `∆max` is the maximal
    /// delay time between two release rounds.
    #[must_use]
    pub const fn rounds_per_observation_window(&self) -> NonZeroU64 {
        // TODO: Is `10` fixed or can it be derived from some other value?
        NonZeroU64::new(
            10 * self
                .core
                .scheduler
                .delayer
                .maximum_release_delay_in_rounds
                .get(),
        )
        .unwrap()
    }

    /// `T_M = ß · (∆max + η)`: the rounds a message needs to cross the network,
    /// the same figure the Blend service uses as its delivery deadline.
    #[must_use]
    pub const fn message_traversal_time_in_rounds(&self) -> NonZeroU64 {
        max_data_message_delay_in_rounds(
            self.common.num_blend_layers,
            self.core.scheduler.delayer.maximum_release_delay_in_rounds,
        )
    }

    /// Duration of the epoch transition period, `T = 2 · T_M`.
    ///
    /// `blend-protocol.md` §Transition Period derives `T` from the message
    /// traversal time, not from the block time: past-epoch messages must be
    /// able to finish crossing the network before the old connections close,
    /// so `T >= T_M` holds by construction here.
    #[must_use]
    pub const fn epoch_transition(&self, slot_duration: &Duration) -> Duration {
        Duration::from_secs(
            self.round_duration(slot_duration).as_secs()
                * spec::TRANSITION_PERIOD_FACTOR
                * self.message_traversal_time_in_rounds().get(),
        )
    }

    /// Every way these settings depart from `blend-protocol.md`, in a stable
    /// order. Empty for a conforming deployment.
    #[must_use]
    pub fn violations(&self) -> Vec<Violation> {
        let mut violations = Vec::new();

        let delay = self
            .core
            .scheduler
            .delayer
            .maximum_release_delay_in_rounds
            .get();
        if delay < spec::MINIMUM_RANDOM_RELEASE_DELAY_IN_ROUNDS {
            violations.push(Violation::ReleaseDelayNotRandom { actual: delay });
        }
        if delay != spec::MAXIMUM_RELEASE_DELAY_IN_ROUNDS {
            violations.push(Violation::ReleaseDelayDeviatesFromSpec { actual: delay });
        }
        let layers = self.common.num_blend_layers.get();
        if layers != spec::NUM_BLEND_LAYERS {
            violations.push(Violation::BlendLayersDeviateFromSpec { actual: layers });
        }
        let minimum_network_size = self.common.minimum_network_size.into_inner();
        if minimum_network_size < spec::MINIMUM_NETWORK_SIZE {
            violations.push(Violation::MinimumNetworkSizeDeviatesFromSpec {
                actual: minimum_network_size,
            });
        }
        let coefficient = self.core.minimum_messages_coefficient.get();
        if coefficient != spec::MINIMUM_MESSAGES_COEFFICIENT {
            violations.push(Violation::MinimumMessagesCoefficientDeviatesFromSpec {
                actual: coefficient,
            });
        }

        violations
    }

    /// Refuse settings that are inconsistent, or that deviate from the spec
    /// without `acknowledged_spec_deviations: true`.
    ///
    /// # Errors
    ///
    /// [`Error::Inconsistent`] for a non-random release delay;
    /// [`Error::UnacknowledgedDeviation`] for any other departure from the
    /// spec's values that the deployment has not acknowledged.
    pub fn validate(&self) -> Result<(), Error> {
        let (acknowledgeable, inconsistent): (Vec<_>, Vec<_>) = self
            .violations()
            .into_iter()
            .partition(Violation::is_acknowledgeable);
        if !inconsistent.is_empty() {
            return Err(Error::Inconsistent(inconsistent));
        }
        if !acknowledgeable.is_empty() && !self.common.acknowledged_spec_deviations {
            return Err(Error::UnacknowledgedDeviation(acknowledgeable));
        }
        Ok(())
    }

    #[must_use]
    pub fn rewards_params(
        &self,
        cryptarchia_deployment: &CryptarchiaDeploymentSettings,
        time_deployment: &TimeDeploymentSettings,
    ) -> RewardsParameters {
        RewardsParameters {
            activity_threshold_sensitivity: self.core.activity_threshold_sensitivity,
            data_replication_factor: self.common.data_replication_factor,
            message_frequency_per_round: self.core.scheduler.cover.message_frequency_per_round,
            minimum_network_size: self.common.minimum_network_size.into(),
            num_blend_layers: self.common.num_blend_layers,
            rounds_per_epoch: self.rounds_per_epoch(
                cryptarchia_deployment.slots_per_epoch(),
                &time_deployment.slot_duration,
            ),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CommonSettings {
    /// `ß_c`: expected number of blending operations for each locally generated
    /// message.
    pub num_blend_layers: NonZeroU64,
    pub minimum_network_size: MinimumNetworkSize,
    pub protocol_name: StreamProtocol,
    pub data_replication_factor: u64,
    /// Whether the operator of this deployment accepts that its values depart
    /// from `blend-protocol.md` (see [`Violation`]). A deployment that departs
    /// without saying so is refused at load time. Absent means `false`.
    #[serde(default)]
    pub acknowledged_spec_deviations: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CoreSettings {
    pub scheduler: SchedulerSettings,
    // TODO: Can we derive this?
    pub minimum_messages_coefficient: NonZeroU64,
    pub normalization_constant: NonNegativeF64,
    pub activity_threshold_sensitivity: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SchedulerSettings {
    pub cover: CoverTrafficSettings,
    pub delayer: MessageDelayerSettings,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CoverTrafficSettings {
    /// `F_c`: frequency at which cover messages are generated per round.
    pub message_frequency_per_round: PositiveF64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MessageDelayerSettings {
    /// ∆max: maximal delay time between two release rounds.
    pub maximum_release_delay_in_rounds: NonZeroU64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOT: Duration = Duration::from_secs(1);

    fn settings(
        num_blend_layers: u64,
        minimum_network_size: u64,
        maximum_release_delay_in_rounds: u64,
        minimum_messages_coefficient: u64,
        acknowledged_spec_deviations: bool,
    ) -> Settings {
        Settings {
            common: CommonSettings {
                num_blend_layers: NonZeroU64::new(num_blend_layers).unwrap(),
                minimum_network_size: MinimumNetworkSize::try_new(minimum_network_size).unwrap(),
                protocol_name: StreamProtocol::new("/blend/test"),
                data_replication_factor: 0,
                acknowledged_spec_deviations,
            },
            core: CoreSettings {
                scheduler: SchedulerSettings {
                    cover: CoverTrafficSettings {
                        message_frequency_per_round: PositiveF64::try_from(1.0).unwrap(),
                    },
                    delayer: MessageDelayerSettings {
                        maximum_release_delay_in_rounds: NonZeroU64::new(
                            maximum_release_delay_in_rounds,
                        )
                        .unwrap(),
                    },
                },
                minimum_messages_coefficient: NonZeroU64::new(minimum_messages_coefficient)
                    .unwrap(),
                normalization_constant: NonNegativeF64::try_from(1.03).unwrap(),
                activity_threshold_sensitivity: 1,
            },
        }
    }

    fn spec_settings() -> Settings {
        settings(
            spec::NUM_BLEND_LAYERS,
            spec::MINIMUM_NETWORK_SIZE,
            spec::MAXIMUM_RELEASE_DELAY_IN_ROUNDS,
            spec::MINIMUM_MESSAGES_COEFFICIENT,
            false,
        )
    }

    /// The values every template under `deployment/ceremony/genesis/` ships.
    fn shipped_settings(acknowledged: bool) -> Settings {
        settings(1, 2, 2, 1, acknowledged)
    }

    #[test]
    fn spec_values_conform_and_give_the_spec_transition_period() {
        let settings = spec_settings();
        assert_eq!(settings.message_traversal_time_in_rounds().get(), 15);
        assert_eq!(
            settings.epoch_transition(&SLOT),
            Duration::from_secs(spec::TRANSITION_PERIOD_IN_ROUNDS)
        );
        assert!(settings.violations().is_empty());
        settings.validate().unwrap();
    }

    #[test]
    fn the_transition_period_is_never_shorter_than_the_traversal_time() {
        for (layers, delay) in [(1, 2), (3, 3), (5, 7), (1, 100)] {
            let settings = settings(layers, 32, delay, 3, true);
            let traversal = Duration::from_secs(settings.message_traversal_time_in_rounds().get());
            assert!(settings.epoch_transition(&SLOT) >= traversal);
        }
    }

    #[test]
    fn shipped_values_need_the_acknowledgement() {
        let expected = vec![
            Violation::ReleaseDelayDeviatesFromSpec { actual: 2 },
            Violation::BlendLayersDeviateFromSpec { actual: 1 },
            Violation::MinimumNetworkSizeDeviatesFromSpec { actual: 2 },
            Violation::MinimumMessagesCoefficientDeviatesFromSpec { actual: 1 },
        ];
        let unacknowledged = shipped_settings(false);
        assert_eq!(unacknowledged.violations(), expected);
        match unacknowledged.validate() {
            Err(Error::UnacknowledgedDeviation(violations)) => assert_eq!(violations, expected),
            other => panic!("expected UnacknowledgedDeviation, got {other:?}"),
        }

        let acknowledged = shipped_settings(true);
        assert_eq!(acknowledged.message_traversal_time_in_rounds().get(), 4);
        acknowledged.validate().unwrap();
    }

    #[test]
    fn a_non_random_release_delay_is_refused_even_when_acknowledged() {
        let settings = settings(1, 2, 1, 1, true);
        match settings.validate() {
            Err(Error::Inconsistent(violations)) => {
                assert_eq!(
                    violations,
                    vec![Violation::ReleaseDelayNotRandom { actual: 1 }]
                );
            }
            other => panic!("expected Inconsistent, got {other:?}"),
        }
    }

    #[test]
    fn the_acknowledgement_defaults_to_false_when_absent() {
        let yaml = "\
num_blend_layers: 3
minimum_network_size: 32
protocol_name: /blend/test
data_replication_factor: 0
";
        let common: CommonSettings = serde_yaml::from_str(yaml).unwrap();
        assert!(!common.acknowledged_spec_deviations);
    }

    #[test]
    fn a_minimum_network_size_below_two_is_rejected_at_deserialization() {
        let yaml = "\
num_blend_layers: 3
minimum_network_size: 1
protocol_name: /blend/test
data_replication_factor: 0
";
        assert!(serde_yaml::from_str::<CommonSettings>(yaml).is_err());
    }
}
