use clap::Parser as _;
use color_eyre::eyre::{Result, eyre};
use logos_blockchain_node::{
    UserConfig,
    config::{
        CliArgs, ConfigDeserializationError, DeploymentType, deployment::DeploymentSettings,
        deserialize_config_at_path,
    },
    get_services_to_start, run_node_from_config,
};
use tracing::warn;

#[tokio::main]
async fn main() -> Result<()> {
    let cli_args = CliArgs::parse();
    let is_dry_run = cli_args.dry_run();
    let must_blend_service_group_start = cli_args.must_blend_service_group_start();
    let must_da_service_group_start = cli_args.must_da_service_group_start();

    // If we are dry-running the binary, fail in case unknown keys in one of the
    // configs are found or exit successfully if deserializations succeed.
    if is_dry_run {
        // Check user config.
        drop(deserialize_config_at_path::<UserConfig>(
            cli_args.config_path(),
        )?);
        // If custom, check deployment config.
        if let DeploymentType::Custom(custom_deployment_config_file) = cli_args.deployment_type() {
            drop(deserialize_config_at_path::<DeploymentSettings>(
                custom_deployment_config_file,
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
        let user_config = match deserialize_config_at_path::<UserConfig>(cli_args.config_path()) {
            Ok(config) => config,
            Err(ConfigDeserializationError::UnrecognizedFields { fields, config }) => {
                warn!(
                    "The following unrecognized fields were found in the user config file: {fields:?}. They won't have any effects on the node."
                );
                config
            }
            Err(e) => return Err(eyre!(e)),
        };
        user_config.update_from_args(cli_args)?
    };

    let app = run_node_from_config(run_config).map_err(|e| eyre!("{e}"))?;
    let services_to_start = get_services_to_start(
        &app,
        must_blend_service_group_start,
        must_da_service_group_start,
    )
    .await?;

    drop(app.handle().start_service_sequence(services_to_start).await);

    app.wait_finished().await;
    Ok(())
}
