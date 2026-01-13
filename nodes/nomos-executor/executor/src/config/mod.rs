use color_eyre::eyre::Result;
use nomos_node::{
    CryptarchiaLeaderArgs, HttpArgs, LogArgs, NetworkArgs,
    config::{
        BlendArgs, TimeArgs, blend::serde::Config as BlendConfig,
        cryptarchia::serde::Config as CryptarchiaConfig, deployment::DeploymentSettings,
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
pub struct Config {
    pub network: NetworkConfig,
    pub blend: BlendConfig,
    pub deployment: DeploymentSettings,
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
    pub tracing: <nomos_node::Tracing<RuntimeServiceId> as ServiceData>::Settings,

    #[cfg(feature = "testing")]
    pub testing_http: <ApiService as ServiceData>::Settings,
}

impl Config {
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
    ) -> Result<Self> {
        #[cfg(feature = "tracing")]
        nomos_node::config::update_tracing(&mut self.tracing, log_args)?;
        update_network(&mut self.network, network_args)?;
        update_blend(&mut self.blend, blend_args)?;
        update_http(&mut self.http, http_args)?;
        update_cryptarchia_leader_consensus(&mut self.cryptarchia.leader, cryptarchia_leader_args)?;
        update_time(&mut self.time, time_args)?;
        Ok(self)
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
