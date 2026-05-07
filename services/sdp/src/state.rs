use std::convert::Infallible;

use lb_core::sdp::DeclarationId;
pub use lb_services_utils::overwatch::recovery::operators::RecoveryBackend as SdpStateStorage;
use overwatch::services::state::ServiceState;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::SdpSettings;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SdpState {
    pub declaration_id: Option<DeclarationId>,
    pub updated: Option<OffsetDateTime>,
}

impl ServiceState for SdpState {
    type Error = Infallible;
    type Settings = SdpSettings;

    fn from_settings(settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self {
            declaration_id: settings.declaration_id,
            updated: None,
        })
    }
}

impl From<Option<DeclarationId>> for SdpState {
    fn from(declaration: Option<DeclarationId>) -> Self {
        Self {
            updated: declaration.map(|_| OffsetDateTime::now_utc()),
            declaration_id: declaration,
        }
    }
}
