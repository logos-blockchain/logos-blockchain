use clap::Parser as _;
use color_eyre::eyre::{Result, eyre};
use lb_log_targets::node;
use logos_blockchain_node::{
    UserConfig,
    cli::{CliArgs, Command, build_run_config},
    get_services_to_start,
    panic::log_and_exit_hook,
    run_node_from_config,
};

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(feature = "dhat-heap")]
    let _dhat_drop_guard = logos_blockchain_node::global_allocators::dhat_heap::setup();

    let cli_args = CliArgs::parse();

    if let Some(command) = cli_args.command {
        match command {
            Command::InitConfig(init_args) => {
                return logos_blockchain_node::cli::config::init::run(*init_args);
            }
            Command::GenerateKey(generate_args) => {
                return logos_blockchain_node::cli::keys::run_generate_key(*generate_args);
            }
            Command::AddKey(add_args) => {
                return logos_blockchain_node::cli::keys::run_add_key(*add_args);
            }
            Command::RemoveKey(remove_args) => {
                return logos_blockchain_node::cli::keys::run_remove_key(*remove_args);
            }
            Command::Participate(participate_args) => {
                return logos_blockchain_node::cli::participate::run(&participate_args);
            }
            Command::GetPeerId(get_peer_id_args) => {
                return logos_blockchain_node::cli::get_peer_id::run(&get_peer_id_args);
            }
        }
    }

    let is_dry_run = cli_args.dry_run();
    let show_config = cli_args.show_config();

    let run_config = {
        let user_config = UserConfig::load(cli_args.user_config_path()).inspect_err(|e| {
            eprintln!("\nExiting... {e}.\n");
        })?;
        build_run_config(user_config, cli_args)?
    };

    if is_dry_run {
        #[expect(
            clippy::non_ascii_literal,
            reason = "Use of green checkmark for better UX."
        )]
        {
            println!("Configs are valid! ✅");
        };
        return Ok(());
    }
    if show_config {
        print!("{}", serde_yaml::to_string(&run_config.user.redacted()?)?);
        return Ok(());
    }

    let chain_id = run_config.deployment.chain_id();
    // Only single-era schedules are supported for now, so the node follows the
    // fork of the genesis era.
    let fork_digest = run_config.deployment.genesis_fork_digest();

    // A panic in any service leaves the node in an unknown state, so the
    // standalone binary logs it and exits. The hook is process-wide, which is
    // why it is installed here rather than in `run_node_from_config`: that
    // function is also how the node is started when embedded as a library,
    // where ending the host process is not the node's call to make.
    std::panic::set_hook(Box::new(log_and_exit_hook));

    let app = run_node_from_config(run_config, None)
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
    tracing::info!(
        target: node::ROOT,
        "Running chain {chain_id} on fork {fork_digest}."
    );

    // An error here means Overwatch shut down because a service panicked.
    app.wait_finished().await.map_err(|panic| eyre!("{panic}"))
}
