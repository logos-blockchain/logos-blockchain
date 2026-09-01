use std::convert::Infallible;

use lb_core::sdp::DeclarationId;
pub use lb_services_utils::overwatch::recovery::operators::RecoveryBackend as SdpStateStorage;
use overwatch::services::state::ServiceState;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{Activity, SdpSettings};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SdpState {
    pub declaration_id: Option<DeclarationId>,
    pub updated: Option<OffsetDateTime>,
    /// Activity message being tracked until it is finalized.
    #[serde(default)]
    pub pending_activity: Option<Activity>,
}

impl SdpState {
    #[must_use]
    pub fn new(declaration_id: Option<DeclarationId>, pending_activity: Option<Activity>) -> Self {
        Self {
            updated: declaration_id.map(|_| OffsetDateTime::now_utc()),
            declaration_id,
            pending_activity,
        }
    }
}

impl ServiceState for SdpState {
    type Error = Infallible;
    type Settings = SdpSettings;

    fn from_settings(settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self {
            declaration_id: settings.declaration_id,
            updated: None,
            pending_activity: None,
        })
    }
}
