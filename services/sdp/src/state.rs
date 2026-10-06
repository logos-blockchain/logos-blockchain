use std::convert::Infallible;

pub use lb_services_utils::overwatch::recovery::operators::RecoveryBackend as SdpStateStorage;
use overwatch::services::state::ServiceState;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{Activity, Participation, SdpSettings, activity_submission::ActivityProofReport};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(from = "backwards_compatibility::BackwardsCompatibleSdpState")]
pub struct SdpState {
    pub participation: Option<Participation>,
    pub updated: Option<OffsetDateTime>,
    /// Activity message being tracked until it is finalized.
    pub pending_activity: Option<Activity>,
    /// The outcome of the last activity proof submitted.
    pub last_activity_proof_report: Option<ActivityProofReport>,
}

mod backwards_compatibility {
    use lb_core::sdp::DeclarationId;
    use serde::Deserialize;
    use time::OffsetDateTime;

    use crate::{
        Activity, Participation, activity_submission::ActivityProofReport, state::SdpState,
    };

    #[derive(Deserialize)]
    pub struct BackwardsCompatibleSdpState {
        participation: Option<Participation>,
        declaration_id: Option<DeclarationId>,
        updated: Option<OffsetDateTime>,
        #[serde(default)]
        pending_activity: Option<Activity>,
        #[serde(default)]
        last_activity_proof_report: Option<ActivityProofReport>,
    }

    impl From<BackwardsCompatibleSdpState> for SdpState {
        fn from(backwards_compatible_sdp_state: BackwardsCompatibleSdpState) -> Self {
            let participation = backwards_compatible_sdp_state.participation.or_else(|| {
                backwards_compatible_sdp_state
                    .declaration_id
                    .map(Participation::Live)
            });
            Self {
                participation,
                updated: backwards_compatible_sdp_state.updated,
                pending_activity: backwards_compatible_sdp_state.pending_activity,
                last_activity_proof_report: backwards_compatible_sdp_state
                    .last_activity_proof_report,
            }
        }
    }
}

impl SdpState {
    #[must_use]
    pub fn new(
        participation: Option<Participation>,
        pending_activity: Option<Activity>,
        last_activity_proof_report: Option<ActivityProofReport>,
    ) -> Self {
        Self {
            updated: participation.map(|_| OffsetDateTime::now_utc()),
            participation,
            pending_activity,
            last_activity_proof_report,
        }
    }
}

impl ServiceState for SdpState {
    type Settings = SdpSettings;
    type Error = Infallible;

    fn from_settings(settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self {
            participation: settings.declaration_id().map(Participation::Live),
            updated: None,
            pending_activity: None,
            last_activity_proof_report: None,
        })
    }
}
