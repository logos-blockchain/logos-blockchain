use color_eyre::eyre::Result;
use lb_node::{
    CryptarchiaLeaderArgs, HttpArgs, LogArgs, NetworkArgs,
    config::{
        BlendArgs, ConfigDeserializationError, DeploymentArgs, DeploymentType, TimeArgs,
        blend::serde::Config as BlendConfig, cryptarchia::serde::Config as CryptarchiaConfig,
        deployment::DeploymentSettings, deserialize_config_at_path,
        mempool::serde::Config as MempoolConfig, network::serde::Config as NetworkConfig,
        time::serde::Config as TimeConfig, update_blend, update_cryptarchia_leader_consensus,
        update_network, update_time,
    },
    generic_services::SdpService,
};
use overwatch::services::ServiceData;
use serde::Deserialize;

use crate::{
    ApiService, DaDispersalService, DaNetworkService, DaSamplingService, DaVerifierService,
    KeyManagementService, RuntimeServiceId, StorageService, WalletService,
};

#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "testing", derive(serde::Serialize))]
pub struct UserConfig {
    pub network: NetworkConfig,
    pub blend: BlendConfig,
    pub cryptarchia: CryptarchiaConfig,
    pub time: TimeConfig,
    pub mempool: MempoolConfig,

    pub da_dispersal: <DaDispersalService as ServiceData>::Settings,
    pub da_network: <DaNetworkService as ServiceData>::Settings,
    pub sdp: <SdpService<RuntimeServiceId> as ServiceData>::Settings,
    pub da_verifier: <DaVerifierService as ServiceData>::Settings,
    pub da_sampling: <DaSamplingService as ServiceData>::Settings,
    pub http: <ApiService as ServiceData>::Settings,
    pub storage: <StorageService as ServiceData>::Settings,
    pub wallet: <WalletService as ServiceData>::Settings,
    pub key_management: <KeyManagementService as ServiceData>::Settings,

    #[cfg(feature = "tracing")]
    pub tracing: <lb_node::Tracing<RuntimeServiceId> as ServiceData>::Settings,

    #[cfg(feature = "testing")]
    pub testing_http: <ApiService as ServiceData>::Settings,
}

impl UserConfig {
    #[expect(
        clippy::too_many_arguments,
        reason = "TODO: Refactor this at some point."
    )]
    pub fn update_from_args(
        mut self,
        #[cfg_attr(
            not(feature = "tracing"),
            expect(
                unused_variables,
                reason = "`log_args` is only used to update tracing configs when the `tracing` feature is enabled."
            )
        )]
        log_args: LogArgs,
        network_args: NetworkArgs,
        blend_args: BlendArgs,
        http_args: HttpArgs,
        cryptarchia_leader_args: CryptarchiaLeaderArgs,
        time_args: &TimeArgs,
        deployment_args: &DeploymentArgs,
    ) -> Result<RunConfig> {
        #[cfg(feature = "tracing")]
        lb_node::config::update_tracing(&mut self.tracing, log_args)?;
        update_network(&mut self.network, network_args)?;
        update_blend(&mut self.blend, blend_args)?;
        update_http(&mut self.http, http_args)?;
        update_cryptarchia_leader_consensus(&mut self.cryptarchia.leader, cryptarchia_leader_args)?;
        update_time(&mut self.time, time_args)?;

        let deployment_settings = match deployment_args.deployment_type() {
            DeploymentType::WellKnown(well_known_deployment) => {
                Ok::<_, ConfigDeserializationError<Self>>((*well_known_deployment).into())
            }
            DeploymentType::Custom(custom_deployment_config_path) => {
                let deployment_settings = deserialize_config_at_path::<DeploymentSettings>(
                    custom_deployment_config_path,
                )?;
                Ok(deployment_settings)
            }
        }?;

        Ok(RunConfig::new(self, deployment_settings))
    }
}

pub fn update_http(
    http: &mut <ApiService as ServiceData>::Settings,
    http_args: HttpArgs,
) -> Result<()> {
    let HttpArgs {
        http_addr,
        cors_origins,
    } = http_args;

    if let Some(addr) = http_addr {
        http.backend_settings.address = addr;
    }

    if let Some(cors) = cors_origins {
        http.backend_settings.cors_origins = cors;
    }

    Ok(())
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "testing", derive(serde::Serialize))]
pub struct RunConfig {
    #[serde(flatten)]
    user: UserConfig,
    deployment: DeploymentSettings,
}

impl RunConfig {
    #[must_use]
    pub const fn new(user: UserConfig, deployment: DeploymentSettings) -> Self {
        Self { user, deployment }
    }

    #[must_use]
    pub fn into_components(self) -> (UserConfig, DeploymentSettings) {
        (self.user, self.deployment)
    }
}

impl From<RunConfig> for UserConfig {
    fn from(value: RunConfig) -> Self {
        value.user
    }
}

impl AsRef<UserConfig> for RunConfig {
    fn as_ref(&self) -> &UserConfig {
        &self.user
    }
}
