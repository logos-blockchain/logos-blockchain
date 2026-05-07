use std::convert::Infallible;

use lb_core::sdp::DeclarationId;
use overwatch::services::state::ServiceState;
use serde::{Deserialize, Serialize};

use crate::SdpSettings;

pub use lb_services_utils::overwatch::recovery::operators::RecoveryBackend as SdpStateStorage;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SdpState {
    pub declaration_id: Option<DeclarationId>,
}

impl ServiceState for SdpState {
    type Error = Infallible;
    type Settings = SdpSettings;

    fn from_settings(settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self {
            declaration_id: settings.declaration_id,
        })
    }
}
