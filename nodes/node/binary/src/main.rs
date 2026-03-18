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
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[tokio::main]
async fn main() -> Result<()> {
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

    #[cfg(feature = "dhat-heap")]
    let dhat_profiler = dhat::Profiler::new_heap();
    #[cfg(feature = "dhat-heap")]
    println!("\n\nDHAT: Profiling enabled.\n\n");
    #[cfg(feature = "dhat-heap")]
    let dhat_heap_msg = "Run https://nnethercote.github.io/dh_view/dh_view.html to view the heap output results in \
        'dhat-heap.json'.";

    let run_config = {
        let user_config =
            deserialize_config_at_path::<UserConfig>(cli_args.config_path(), OnUnknownKeys::Warn)
                .inspect_err(|e| {
                    let _ = &e; // keep non-dhat builds warning-free
                #[cfg(feature = "dhat-heap")]
                {
                    println!("\nExiting... {e}. {dhat_heap_msg}\n");
                }
            })?;
        user_config.update_from_args(cli_args)?
    };

    let app = run_node_from_config(run_config)
        .map_err(|e| eyre!("{e}"))
        .inspect_err(|e| {
            let _ = &e; // keep non-dhat builds warning-free
            #[cfg(feature = "dhat-heap")]
            {
                println!("\nExiting... {e}. {dhat_heap_msg}\n");
            }
        })?;
    let services_to_start = get_services_to_start(&app).await.inspect_err(|e| {
        let _ = &e; // keep non-dhat builds warning-free
        #[cfg(feature = "dhat-heap")]
        {
            println!("\nExiting... {e}. {dhat_heap_msg}\n");
        }
    })?;

    drop(app.handle().start_service_sequence(services_to_start).await);

    app.wait_finished().await;
    #[cfg(feature = "dhat-heap")]
    #[expect(
        clippy::semicolon_outside_block,
        reason = "Contradicts `semicolon_if_nothing_returned` when feature is enabled"
    )]
    {
        println!("\nCtrl-C pressed, exiting... {dhat_heap_msg}\n");
        drop(dhat_profiler);
    }
    Ok(())
}
