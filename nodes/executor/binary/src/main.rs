use clap::Parser;
use color_eyre::eyre::{Result, eyre};
use lb_node::{
    CryptarchiaLeaderArgs, HttpArgs, LogArgs, NetworkArgs,
    config::{
        BlendArgs, ConfigDeserializationError, DeploymentArgs, TimeArgs,
        blend::ServiceConfig as BlendConfig, cryptarchia::ServiceConfig as CryptarchiaConfig,
        deserialize_config_at_path, mempool::ServiceConfig as MempoolConfig,
        network::ServiceConfig as NetworkConfig, time::ServiceConfig as TimeConfig,
    },
};
use lb_sdp_service::SdpSettings;
use logos_blockchain_executor::{
    LogosBlockchainExecutor, LogosBlockchainExecutorServiceSettings, RuntimeServiceId,
    config::UserConfig as ExecutorConfig,
};
use overwatch::overwatch::{Error as OverwatchError, Overwatch, OverwatchRunner};
use tracing::warn;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path for a yaml-encoded network config file
    config: std::path::PathBuf,
    /// Dry-run flag. If active, the binary will try to deserialize the config
    /// file and then exit.
    #[clap(long = "check-config", action)]
    check_config_only: bool,
    /// Overrides log config.
    #[clap(flatten)]
    log: LogArgs,
    /// Overrides network config.
    #[clap(flatten)]
    network: NetworkArgs,
    /// Overrides blend config.
    #[clap(flatten)]
    blend: BlendArgs,
    /// Overrides http config.
    #[clap(flatten)]
    http: HttpArgs,
    #[clap(flatten)]
    cryptarchia_leader: CryptarchiaLeaderArgs,
    #[clap(flatten)]
    time: TimeArgs,
    #[clap(flatten)]
    deployment: DeploymentArgs,
}

#[tokio::main]
#[expect(clippy::too_many_lines, reason = "Main function for executor binary.")]
async fn main() -> Result<()> {
    let Args {
        config,
        log: log_args,
        http: http_args,
        network: network_args,
        blend: blend_args,
        cryptarchia_leader: cryptarchia_args,
        time: time_args,
        deployment: deployment_args,
        check_config_only,
    } = Args::parse();

    let run_config = match (
        deserialize_config_at_path::<ExecutorConfig>(&config),
        check_config_only,
    ) {
        (Ok(_), true) => {
            #[expect(
                clippy::non_ascii_literal,
                reason = "Use of green checkmark for better UX."
            )]
            {
                println!("Config file is valid! ✅");
            };
            return Ok(());
        }
        (Ok(config), false) => Ok(config),
        (Err(ConfigDeserializationError::UnrecognizedFields { config, fields }), true) => {
            Err(ConfigDeserializationError::UnrecognizedFields { config, fields })
        }
        (Err(ConfigDeserializationError::UnrecognizedFields { config, fields }), false) => {
            warn!(
                "The following unrecognized fields were found in the config file: {fields:?}. They won't have any effects on the node."
            );
            Ok(config)
        }
        (Err(e), _) => Err(e),
    }?.update_from_args(
        log_args,
        network_args,
        blend_args,
        http_args,
        cryptarchia_args,
        &time_args,
        &deployment_args
    )?;

    let time_service_config = TimeConfig {
        user: run_config.user.time,
        deployment: run_config.deployment.time,
    }
    .into_time_service_settings(&run_config.deployment.cryptarchia);

    let (chain_service_config, chain_network_config, chain_leader_config) = CryptarchiaConfig {
        user: run_config.user.cryptarchia,
        deployment: run_config.deployment.cryptarchia,
    }
    .into_cryptarchia_services_settings(&run_config.deployment.blend);

    let (blend_config, blend_core_config, blend_edge_config) = BlendConfig {
        user: run_config.user.blend,
        deployment: run_config.deployment.blend,
    }
    .into();

    let mempool_service_config = MempoolConfig {
        user: run_config.user.mempool,
        deployment: run_config.deployment.mempool,
    }
    .into();

    let app = OverwatchRunner::<LogosBlockchainExecutor>::run(
        LogosBlockchainExecutorServiceSettings {
            network: NetworkConfig {
                user: run_config.user.network,
                deployment: run_config.deployment.network,
            }
            .into(),
            blend: blend_config,
            blend_core: blend_core_config,
            blend_edge: blend_edge_config,
            block_broadcast: (),
            #[cfg(feature = "tracing")]
            tracing: run_config.user.tracing,
            http: run_config.user.http,
            mempool: mempool_service_config,
            da_dispersal: run_config.user.da_dispersal,
            da_network: run_config.user.da_network,
            da_sampling: run_config.user.da_sampling,
            da_verifier: run_config.user.da_verifier,
            cryptarchia: chain_service_config,
            chain_network: chain_network_config,
            cryptarchia_leader: chain_leader_config,
            time: time_service_config,
            storage: run_config.user.storage,
            system_sig: (),
            sdp: SdpSettings { declaration: None },
            wallet: run_config.user.wallet,
            key_management: run_config.user.key_management,
            #[cfg(feature = "testing")]
            testing_http: run_config.user.testing_http,
        },
        None,
    )
    .map_err(|e| eyre!("Error encountered: {}", e))?;

    drop(
        app.handle()
            .start_service_sequence(get_services_to_start(&app).await?)
            .await,
    );
    app.wait_finished().await;
    Ok(())
}

async fn get_services_to_start(
    app: &Overwatch<RuntimeServiceId>,
) -> Result<Vec<RuntimeServiceId>, OverwatchError> {
    let mut service_ids = app.handle().retrieve_service_ids().await?;

    // Exclude core and edge blend services, which will be started
    // on demand by the blend service.
    let blend_inner_service_ids = [RuntimeServiceId::BlendCore, RuntimeServiceId::BlendEdge];
    service_ids.retain(|value| !blend_inner_service_ids.contains(value));

    Ok(service_ids)
}
