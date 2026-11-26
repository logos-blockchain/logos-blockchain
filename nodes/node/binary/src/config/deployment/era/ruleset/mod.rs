use core::{num::NonZero, time::Duration};

use lb_core::sdp::{InactivityPeriod, MinStake, ServiceType};
use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use serde::{Deserialize, Serialize};

use crate::config::deployment::{InvalidDeployment, ensure};

pub mod v1;
pub mod v2;

/// The rules an era runs under, named by their version, with the values of
/// their parameters in the layout that version defines.
///
/// An era that only changes values keeps its ruleset. A change of behaviour,
/// parameters or encoding needs a new one.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "A schedule holds a handful of eras, and the legacy one keeps its parameters in both layouts."
)]
pub enum EraRuleset {
    /// The rules of releases 0.3.1 to 0.3.6: only for the genesis era of the
    /// chain they started.
    V1(v1::Parameters),
    /// The rules of release 0.4.
    V2(v2::Parameters),
}

impl EraRuleset {
    /// The ruleset's number, written ahead of its parameters in the era's
    /// digest. Only used for encoding the ruleset.
    #[must_use]
    pub(super) const fn version(&self) -> u16 {
        match self {
            Self::V1(_) => v1::VERSION,
            Self::V2(_) => 2,
        }
    }

    /// The era's parameters in the layout of the latest ruleset, which the
    /// node reads them in: those of an earlier ruleset converted to it, with
    /// the values that ruleset fixed in code.
    #[must_use]
    pub const fn parameters(&self) -> &v2::Parameters {
        match self {
            Self::V1(parameters) => parameters.as_current(),
            Self::V2(parameters) => parameters,
        }
    }

    /// Whether the era runs the rules of releases 0.3.1 to 0.3.6, whose
    /// transactions carry no fork digest and whose fork digest hashes the
    /// chain ID.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        matches!(self, Self::V1(_))
    }

    /// Checks the era's parameters, alone and against those of the era before
    /// it, `previous`, if there is one, refusing the first value found
    /// invalid.
    pub(super) fn validate(&self, previous: Option<&Self>) -> Result<(), InvalidDeployment> {
        // The releases that ran the first ruleset ran a single era, and no
        // release goes back to an earlier ruleset.
        ensure!(
            previous.is_none_or(|previous| previous.version() <= self.version())
                && (previous.is_none() || !self.is_legacy()),
            "An era of ruleset {} cannot follow another era{}.",
            self.version(),
            previous.map_or_else(String::new, |previous| format!(
                ", of ruleset {}",
                previous.version()
            ))
        );
        self.parameters().validate(previous.map(Self::parameters))
    }

    /// Whether an era of this ruleset refuses the transactions of the eras
    /// before it, coming after an era of the ruleset `previous`. An era whose
    /// ruleset changes how transactions are encoded or validated must.
    #[must_use]
    pub(crate) const fn refuses_earlier_transactions(&self, previous: &Self) -> bool {
        match (previous, self) {
            (Self::V1(_), Self::V1(_)) | (Self::V2(_), Self::V2(_)) => false,
            // The transactions of the first ruleset carry no fork digest, and
            // an era of the first ruleset follows no other.
            (Self::V1(_), Self::V2(_)) | (Self::V2(_), Self::V1(_)) => true,
        }
    }

    /// How long each slot of the era lasts.
    #[must_use]
    pub const fn slot_duration(&self) -> Duration {
        let parameters = self.parameters();
        parameters.time.slot_duration()
    }

    /// The number of slots in each epoch of the era.
    #[must_use]
    pub const fn epoch_length(&self) -> NonZero<u64> {
        let parameters = self.parameters();
        NonZero::new(parameters.cryptarchia.slots_per_epoch())
            .expect("an epoch has at least one slot: its phases and base period are not zero")
    }

    #[must_use]
    pub fn blend_payload_body_size(&self) -> u16 {
        let parameters = self.parameters();
        u16::try_from(parameters.cryptarchia.max_proposal_size())
            .expect("The era's checks keep its largest proposal within a Blend payload.")
    }

    /// For how many of its slots the network keeps serving the era's protocols
    /// once the next era starts: Blend's epoch transition period, which the
    /// era's messages still in flight need to arrive.
    #[must_use]
    pub const fn era_transition_slots(&self) -> u64 {
        let parameters = self.parameters();
        parameters
            .blend
            .epoch_transition_slots(parameters.cryptarchia.average_slots_per_block())
    }

    /// The least a node must stake to declare for a service in the era.
    #[must_use]
    pub const fn min_stake(&self) -> MinStake {
        let parameters = self.parameters();
        parameters.cryptarchia.sdp_config.min_stake
    }

    /// How long a node of `service` can go without proving activity in the
    /// era, or `None` if the era has no parameters for `service`.
    #[must_use]
    pub fn inactivity_period(&self, service: ServiceType) -> Option<InactivityPeriod> {
        let parameters = self.parameters();
        parameters
            .cryptarchia
            .sdp_config
            .service_params_by_type()
            .find(|(service_type, _)| *service_type == service)
            .map(|(_, service_parameters)| service_parameters.inactivity_period)
    }

    /// The parameters Blend rewards are computed with in the era.
    #[must_use]
    pub fn blend_reward_params(&self) -> RewardsParameters {
        let parameters = self.parameters();
        parameters.blend_reward_params()
    }
}
