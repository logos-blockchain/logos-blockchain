use clap::Parser as _;
use color_eyre::eyre::{Result, eyre};
use nomos_node::{
    Config,
    config::{CliArgs, ConfigDeserializationError, deserialize_config_at_path},
    get_services_to_start, run_node_from_config,
};
use tracing::warn;

#[tokio::main]
async fn main() -> Result<()> {
    let cli_args = CliArgs::parse();
    let is_dry_run = cli_args.dry_run();
    let must_blend_service_group_start = cli_args.must_blend_service_group_start();
    let must_da_service_group_start = cli_args.must_da_service_group_start();

    // If we are dry-running the binary, fail in case unknown keys in the config are
    // found or exit successfully if deserialization succeeds.
    // In case of a non dry run, print a warning and do not fail if unknown keys are
    // found.
    let config = match (
        deserialize_config_at_path::<Config>(cli_args.config_path()),
        is_dry_run,
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
    }?.update_from_args(cli_args)?;

    let app = run_node_from_config(config).map_err(|e| eyre!("{e}"))?;
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
