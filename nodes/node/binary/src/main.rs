use clap::Parser as _;
use color_eyre::eyre::{Result, eyre};
use logos_blockchain_node::{
    UserConfig,
    config::{
        CliArgs, DeploymentType, OnUnknownKeys, deployment::DeploymentSettings,
        deserialize_config_at_path,
    },
    get_services_to_start, run_node_from_config,
};

#[tokio::main]
async fn main() -> Result<()> {
    let cli_args = CliArgs::parse();

    #[cfg(feature = "config-gen")]
    if let Some(logos_blockchain_node::config::Command::Init(init_args)) = &cli_args.command {
        return logos_blockchain_node::init::run(init_args);
    }

    let is_dry_run = cli_args.dry_run();

    // If we are dry-running the binary, fail in case unknown keys in one of the
    // configs are found or exit successfully if deserializations succeed.
    if is_dry_run {
        // Check user config.
        drop(deserialize_config_at_path::<UserConfig>(
            cli_args.config_path(),
            OnUnknownKeys::Fail,
        )?);
        // If custom, check deployment config.
        if let DeploymentType::Custom(custom_deployment_config_file) = cli_args.deployment_type() {
            drop(deserialize_config_at_path::<DeploymentSettings>(
                custom_deployment_config_file,
                OnUnknownKeys::Fail,
            )?);
        }
        #[expect(
            clippy::non_ascii_literal,
            reason = "Use of green checkmark for better UX."
        )]
        {
            println!("Configs are valid! ✅");
        };
        // Early return since we are dry-running.
        return Ok(());
    }

    let run_config = {
        let user_config =
            deserialize_config_at_path::<UserConfig>(cli_args.config_path(), OnUnknownKeys::Warn)?;
        user_config.update_from_args(cli_args)?
    };

    let app = run_node_from_config(run_config).map_err(|e| eyre!("{e}"))?;
    let services_to_start = get_services_to_start(&app).await?;

    drop(app.handle().start_service_sequence(services_to_start).await);

    app.wait_finished().await;
    Ok(())
}
