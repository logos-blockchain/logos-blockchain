use lb_blend_service::{
    core::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pCoreBlendBackendSettings,
        dispatcher::libp2p::Libp2pBroadcastSettings,
        settings::StartingBlendConfig as BlendCoreSettings,
    },
    edge::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pEdgeBlendBackendSettings,
        settings::StartingBlendConfig as BlendEdgeSettings,
    },
    settings::{Settings as BlendSettings, user::Config},
};
use lb_cryptarchia_engine::era::Eras;
use lb_era_parameters::EraDefinition;
use lb_services_utils::overwatch::RecoveryData;

/// The three settings a Blend deployment produces: the proxy's, which picks
/// between the two, and one for each of the services it can start.
type BlendServicesSettings = (
    BlendSettings<
        Libp2pCoreBlendBackendSettings,
        Libp2pEdgeBlendBackendSettings,
        Libp2pBroadcastSettings,
    >,
    BlendCoreSettings<Libp2pCoreBlendBackendSettings, Libp2pBroadcastSettings>,
    BlendEdgeSettings<Libp2pEdgeBlendBackendSettings, Libp2pBroadcastSettings>,
);

/// Blend service config: the user-provided configuration, completed with the
/// era in force.
pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_blend_services_settings(
        self,
        era: &EraDefinition,
        eras: &Eras<EraDefinition>,
        recovery_data: RecoveryData,
    ) -> BlendServicesSettings {
        let blend_service_settings = BlendSettings::from_era(self.user, era, eras, recovery_data);
        let blend_core_settings: BlendCoreSettings<_, _> = blend_service_settings.clone().into();
        let blend_edge_settings: BlendEdgeSettings<_, _> = blend_service_settings.clone().into();
        (
            blend_service_settings,
            blend_core_settings,
            blend_edge_settings,
        )
    }
}
