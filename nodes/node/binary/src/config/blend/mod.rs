use lb_blend_service::{
    core::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pCoreBlendBackendSettings,
        dispatcher::libp2p::Libp2pBroadcastSettings,
        settings::CoreServiceSettings as BlendCoreSettings,
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

/// The three settings a Blend deployment produces, each in every era: the
/// proxy's, which picks between the two, and one for each of the services it
/// can start.
type BlendServicesSettings = (
    Eras<
        BlendSettings<
            Libp2pCoreBlendBackendSettings,
            Libp2pEdgeBlendBackendSettings,
            Libp2pBroadcastSettings,
        >,
    >,
    BlendCoreSettings<Libp2pCoreBlendBackendSettings, Libp2pBroadcastSettings>,
    Eras<BlendEdgeSettings<Libp2pEdgeBlendBackendSettings, Libp2pBroadcastSettings>>,
);

/// Blend service config: the user-provided configuration, completed with the
/// chain's eras.
pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_blend_services_settings(
        self,
        eras: &Eras<EraDefinition>,
        recovery_data: RecoveryData,
    ) -> BlendServicesSettings {
        let blend_service_settings =
            eras.map(|era| BlendSettings::from_era(self.user.clone(), &era.entry.parameters, eras));
        let blend_core_settings = BlendCoreSettings {
            eras: blend_service_settings.map(|era| era.entry.parameters.clone().into()),
            recovery_data,
        };
        let blend_edge_settings =
            blend_service_settings.map(|era| era.entry.parameters.clone().into());
        (
            blend_service_settings,
            blend_core_settings,
            blend_edge_settings,
        )
    }
}
