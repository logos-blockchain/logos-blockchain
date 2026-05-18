pub mod get_peer_id;
pub mod init;
pub mod participate;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use color_eyre::eyre::Result;
use lb_libp2p::Multiaddr;

use crate::config::{
    ApiArgs, BlendArgs, DeploymentArgs, DeploymentSettings, DeploymentType, LogArgs, NetworkArgs,
    OnUnknownKeys, RunConfig, StateArgs, UserConfig, deserialize_config_at_path, update_api,
    update_blend, update_network, update_state, update_tracing,
};

fn long_version() -> String {
    let head_commit_hash = env!("HEAD_COMMIT_HASH");
    let head_tag_name = env!("HEAD_TAG_NAME");
    let pkg_version = env!("PKG_VERSION");
    let target = env!("TARGET");
    let profile = env!("PROFILE");
    let rustc_version = env!("RUSTC_VERSION");

    let commit_line = match (head_commit_hash, head_tag_name) {
        (commit_hash, tag_name) if !commit_hash.is_empty() && !tag_name.is_empty() => {
            format!("commit:  {commit_hash} (tag {tag_name})")
        }
        (commit_hash, _) if !commit_hash.is_empty() => {
            format!("commit:  {commit_hash}")
        }
        _ => "commit:  unknown".to_owned(),
    };

    format!(
        "\
{pkg_version}
{commit_line}
target:  {target}
profile: {profile}
rustc:   {rustc_version}"
    )
}

#[derive(Parser, Debug)]
#[command(author, version, long_version = long_version(), about, long_about = None,
          args_conflicts_with_subcommands = true,
          subcommand_negates_reqs = true)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Path for a yaml-encoded network config file
    config: Option<PathBuf>,
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
    api: ApiArgs,
    #[clap(flatten)]
    deployment: DeploymentArgs,
    #[clap(flatten)]
    state: StateArgs,
}

impl CliArgs {
    #[must_use]
    pub fn config_path(&self) -> &Path {
        self.config
            .as_deref()
            .expect("config path is required when not using a subcommand")
    }

    #[must_use]
    pub const fn dry_run(&self) -> bool {
        self.check_config_only
    }

    #[must_use]
    pub const fn deployment_type(&self) -> &DeploymentType {
        self.deployment.deployment_type()
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Initialize a new user config with generated keys
    Init(InitArgs),
    /// Publish text inscriptions as zone blocks
    Inscribe(lb_tui_zone::InscribeArgs),
    /// Generate stakeholder.yaml and provider.yaml from a user config
    Participate(ParticipateArgs),
    /// Print the libp2p `PeerId` derived from the node key in a user config
    GetPeerId(GetPeerIdArgs),
}

#[derive(Parser, Debug)]
pub struct InitArgs {
    /// Trusted peers to bootstrap from (multiaddr format).
    /// If `--ibd` is set, peers whose multiaddrs include a `PeerId`
    /// are also used as IBD peers.
    #[clap(long = "initial-peers", short = 'p', num_args = 1.., value_delimiter = ',')]
    pub initial_peers: Vec<Multiaddr>,

    /// Output file path for the generated config
    #[clap(long = "output", short = 'o', default_value = "user_config.yaml")]
    pub output: PathBuf,

    /// Network listen port
    #[clap(long = "net-port", default_value = "3000")]
    pub net_port: u16,

    /// Blend listen port
    #[clap(long = "blend-port", default_value = "3400")]
    pub blend_port: u16,

    /// HTTP API listen address
    #[clap(long = "http-addr", default_value = "0.0.0.0:8080")]
    pub http_addr: SocketAddr,

    /// External address for nodes with a known public IP (disables NAT
    /// traversal). Format: /ip4/<public-ip>/udp/<port>/quic-v1
    #[clap(long = "external-address")]
    pub external_address: Option<Multiaddr>,

    #[clap(long = "state-path")]
    pub state_path: Option<PathBuf>,

    /// Enable Initial Block Download (IBD) using peers
    /// passed via `--initial-peers`/`-p`.
    #[clap(long = "ibd", default_value_t = false)]
    pub ibd: bool,

    /// Log filter directives to write into the generated config, e.g.
    /// `warn,logos_blockchain=debug,libp2p_gossipsub::behaviour=error`.
    #[clap(long = "log-filter")]
    pub log_filter: Option<String>,

    /// Path for the generated KMS keys YAML file.
    /// Defaults to 'kms.yaml' in the same directory as --output.
    #[clap(long = "kms-file")]
    pub kms_file: Option<PathBuf>,
}

impl Default for InitArgs {
    fn default() -> Self {
        Self::parse_from::<Vec<String>, String>(vec![])
    }
}

#[derive(Parser, Debug)]
pub struct ParticipateArgs {
    /// Path to the user config YAML file
    #[arg(long, default_value = "user_config.yaml")]
    pub config: PathBuf,
    /// Output directory for `participation_data.yaml`
    #[arg(long, default_value = ".")]
    pub output: PathBuf,
    /// Node's public IPv4 address, required when the blend listening address
    /// is 0.0.0.0
    #[arg(long)]
    pub external_address: Option<std::net::Ipv4Addr>,
}

#[derive(Parser, Debug)]
pub struct GetPeerIdArgs {
    /// Path to the user config YAML file
    #[arg(long, default_value = "user_config.yaml")]
    pub config: PathBuf,
}

/// Applies CLI overrides from `args` to `user_config` and returns a
/// `RunConfig` ready to start the node.
pub fn build_run_config(mut user_config: UserConfig, args: CliArgs) -> Result<RunConfig> {
    let CliArgs {
        log: log_args,
        api: api_args,
        network: network_args,
        blend: blend_args,
        deployment: deployment_args,
        state: state_args,
        ..
    } = args;
    update_tracing(&mut user_config.tracing, log_args)?;
    update_network(&mut user_config.network, network_args)?;
    update_blend(&mut user_config.blend, blend_args);
    update_api(&mut user_config.api, api_args);
    update_state(&mut user_config.state, state_args);

    let deployment_settings = match deployment_args.deployment_type() {
        DeploymentType::WellKnown(well_known_deployment) => (*well_known_deployment).into(),
        DeploymentType::Custom(custom_deployment_config_path) => {
            deserialize_config_at_path::<DeploymentSettings>(
                custom_deployment_config_path,
                OnUnknownKeys::Warn,
            )?
        }
    };

    Ok(RunConfig {
        deployment: deployment_settings,
        user: user_config,
    })
}
