use std::convert::Infallible;

pub use lb_services_utils::overwatch::recovery::operators::RecoveryBackend as SdpStateStorage;
use overwatch::services::state::ServiceState;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{Activity, Participation, SdpSettings, activity_submission::ActivityProofReport};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SdpState {
    pub participation: Option<Participation>,
    pub updated: Option<OffsetDateTime>,
    /// Activity message being tracked until it is finalized.
    pub pending_activity: Option<Activity>,
    /// The outcome of the last activity proof submitted.
    pub last_activity_proof_report: Option<ActivityProofReport>,
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
