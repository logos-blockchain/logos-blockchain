use std::sync::Arc;

use lb_blend_service::{
    core::settings::CoreServiceSettings as BlendCoreSettings,
    settings::{ServiceSettings as BlendSettings, user::Config},
};
use lb_cryptarchia_engine::era::Eras;
use lb_era_parameters::EraDefinition;
use lb_services_utils::overwatch::RecoveryData;

/// Blend service config: the user-provided configuration. Each Blend service
/// builds its settings for every era from it and the chain's eras.
pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// What the Blend services are handed: the proxy, edge and broadcast
    /// services the same, the core service with its recovery data too.
    #[must_use]
    pub fn into_blend_services_settings(
        self,
        eras: &Arc<Eras<EraDefinition>>,
        recovery_data: RecoveryData,
    ) -> (BlendSettings, BlendCoreSettings) {
        let blend_settings = BlendSettings {
            user: self.user,
            eras: Arc::clone(eras),
        };
        let blend_core_settings = BlendCoreSettings {
            service: blend_settings.clone(),
            recovery_data,
        };
        (blend_settings, blend_core_settings)
    }
}
