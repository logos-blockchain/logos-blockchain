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
    #[cfg(feature = "dhat-heap")]
    let _dhat_drop_guard = logos_blockchain_node::profiling::setup();

    let cli_args = CliArgs::parse();

    if let Some(command) = cli_args.command {
        match command {
            #[cfg(feature = "config-gen")]
            logos_blockchain_node::config::Command::Init(init_args) => {
                return logos_blockchain_node::init::run(&init_args).await;
            }
            logos_blockchain_node::config::Command::Inscribe(inscribe_args) => {
                logos_blockchain_tui_zone::run(inscribe_args).await;
                return Ok(());
            }
        }
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
            deserialize_config_at_path::<UserConfig>(cli_args.config_path(), OnUnknownKeys::Warn)
                .inspect_err(|e| {
                eprintln!("\nExiting... {e}.\n");
            })?;
        user_config.update_from_args(cli_args)?
    };

    let app = run_node_from_config(run_config)
        .map_err(|e| eyre!("{e}"))
        .inspect_err(|e| {
            eprintln!("\nExiting... {e}.\n");
        })?;
    let services_to_start = get_services_to_start(&app).await.inspect_err(|e| {
        eprintln!("\nExiting... {e}.\n");
    })?;

    app.handle()
        .start_service_sequence(services_to_start)
        .await
        .map_err(|e| eyre!("start_service_sequence failed: {e}"))
        .inspect_err(|e| {
            eprintln!("\nExiting... {e}.\n");
        })?;

    app.wait_finished().await;
    Ok(())
}
