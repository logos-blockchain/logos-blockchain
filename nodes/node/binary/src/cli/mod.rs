pub mod config;
pub mod get_peer_id;
pub mod keys;
pub mod participate;

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use color_eyre::eyre::Result;
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_at_path};
use libp2p::Multiaddr;

use crate::{
    cli::{
        config::migrate_0_1_2,
        keys::{AddKeyArgs, GenerateKeyArgs, RemoveKeyArgs},
    },
    config::{
        ApiArgs, BlendArgs, CryptarchiaArgs, DeploymentArgs, DeploymentSettings, DeploymentType,
        LogArgs, NetworkArgs, RunConfig, SdpArgs, StateArgs, UserConfig,
        api::serde::AxumBackendSettings, blend::serde::core::BackendConfig as BlendCoreConfig,
        network::serde::SwarmConfig, update_api, update_blend, update_cryptarchia, update_network,
        update_sdp, update_state, update_tracing,
    },
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
    /// Overrides cryptarchia config.
    #[clap(flatten)]
    cryptarchia: CryptarchiaArgs,
    /// Overrides sdp config.
    #[clap(flatten)]
    sdp: SdpArgs,
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
    InitConfig(Box<InitArgs>),
    /// Update existing user config with keys from keystore
    UpdateConfig(Box<UpdateArgs>),
    /// Migrates a new user config with generated keys
    MigrateConfig(Box<MigrateArgs>),
    /// Migrates 0.1.2 config to a new user config
    #[command(name = "migrate-from-0.1.2")]
    Migrate0_1_2(Box<migrate_0_1_2::MigrateArgs>),
    /// Generate a new key of type.
    GenerateKey(Box<GenerateKeyArgs>),
    /// Add a key of type to a keystore.
    AddKey(Box<AddKeyArgs>),
    /// Remove a key with title from a keystore.
    RemoveKey(Box<RemoveKeyArgs>),
    /// Publish text inscriptions as zone blocks
    Inscribe(lb_tui_zone::InscribeArgs),
    /// Generate stakeholder.yaml and provider.yaml from a user config
    Participate(ParticipateArgs),
    /// Print the libp2p `PeerId` derived from the node key in a user config
    GetPeerId(GetPeerIdArgs),
}

#[derive(Parser, Debug)]
pub struct InitArgs {
    /// Output file path for the generated config.
    #[clap(long = "output", short = 'o', default_value = "user_config.yaml")]
    pub output: PathBuf,

    /// Path for the generated keystore file.
    /// Defaults to 'keystore.yaml' in the same directory as --output.
    #[clap(long = "keystore", short = 'k')]
    pub keystore: Option<PathBuf>,

    #[clap(flatten)]
    pub log: LogArgs,

    #[clap(flatten)]
    pub network: NetworkArgs,

    #[clap(flatten)]
    pub blend: BlendArgs,

    #[clap(flatten)]
    pub cryptarchia: CryptarchiaArgs,

    #[clap(flatten)]
    pub sdp: SdpArgs,

    #[clap(flatten)]
    pub api: ApiArgs,

    #[clap(flatten)]
    pub state: StateArgs,
}

/// Set of arguments for use in c-bindings crate.
#[derive(Debug)]
pub struct EmbeddedInitArgs {
    /// Trusted peers to bootstrap from (multiaddr format).
    /// If `--ibd` is set, peers whose multiaddrs include a `PeerId`
    /// are also used as IBD peers.
    pub initial_peers: Vec<Multiaddr>,

    /// Output file path for the generated config
    pub output: PathBuf,

    /// Network listen port
    pub net_port: u16,

    /// Blend listen port
    pub blend_port: u16,

    /// HTTP API listen address
    pub http_addr: SocketAddr,

    /// External address for nodes with a known public IP (disables NAT
    /// traversal). Format: /ip4/<public-ip>/udp/<port>/quic-v1
    pub external_address: Option<Multiaddr>,

    pub state_path: Option<PathBuf>,

    /// Enable Initial Block Download (IBD) using peers
    /// passed via `--initial-peers`/`-p`.
    pub ibd: bool,

    /// Log filter directives to write into the generated config, e.g.
    /// `warn,logos_blockchain=debug,libp2p_gossipsub::behaviour=error`.
    pub log_filter: Option<String>,

    /// Path for the generated KMS keys YAML file.
    /// Defaults to 'kms.yaml' in the same directory as --output.
    pub kms_file: Option<PathBuf>,
}

impl From<EmbeddedInitArgs> for InitArgs {
    fn from(args: EmbeddedInitArgs) -> Self {
        let mut init_args = Self {
            output: args.output.clone(),
            keystore: args.kms_file.clone(),
            ..Default::default()
        };

        init_args.log.filter.clone_from(&args.log_filter);
        init_args.network.port = Some(args.net_port);
        init_args
            .network
            .external_address
            .clone_from(&args.external_address);
        init_args.network.initial_peers = Some(args.initial_peers.clone());

        init_args.blend.blend_addr =
            Some(BlendCoreConfig::default_listening_address(args.blend_port));

        init_args.cryptarchia.ibd = args.ibd;
        init_args.api.addr = Some(args.http_addr);
        init_args.state.path.clone_from(&args.state_path);

        init_args
    }
}

impl Default for EmbeddedInitArgs {
    fn default() -> Self {
        Self {
            initial_peers: Vec::new(),
            output: PathBuf::from("user_config.yaml"),
            net_port: SwarmConfig::default_port(),
            blend_port: BlendCoreConfig::default_port(),
            http_addr: AxumBackendSettings::default_listening_address(
                AxumBackendSettings::default_port(),
            ),
            external_address: None,
            state_path: None,
            ibd: false,
            log_filter: None,
            kms_file: None,
        }
    }
}

#[derive(Parser, Debug)]
pub struct UpdateArgs {
    /// Output file path for the generated config.
    #[clap(long = "user-config", short = 'o', default_value = "user_config.yaml")]
    pub user_config: PathBuf,

    /// Path for the keystore file.
    #[clap(long = "keystore")]
    pub keystore: PathBuf,

    /// Auto approve interactive promps.
    #[arg(short, long, default_value_t = false)]
    pub yes: bool,

    #[clap(flatten)]
    log: LogArgs,

    #[clap(flatten)]
    network: NetworkArgs,

    #[clap(flatten)]
    blend: BlendArgs,

    #[clap(flatten)]
    cryptarchia: CryptarchiaArgs,

    #[clap(flatten)]
    sdp: SdpArgs,

    #[clap(flatten)]
    api: ApiArgs,

    #[clap(flatten)]
    state: StateArgs,
}

impl UpdateArgs {
    /// Creates arguments programmatically (e.g. from the c-bindings crate),
    /// leaving all config overrides at their defaults. `auto_approve` skips
    /// interactive prompts.
    #[must_use]
    pub fn new(user_config: PathBuf, keystore: PathBuf, auto_approve: bool) -> Self {
        Self {
            user_config,
            keystore,
            yes: auto_approve,
            ..Default::default()
        }
    }
}

// Custom default implementation to require keystore path initialized from clap.
impl Default for UpdateArgs {
    fn default() -> Self {
        Self {
            user_config: "user_config.yaml".into(),
            keystore: "keystore.yaml".into(),
            yes: false,
            log: LogArgs::default(),
            network: NetworkArgs::default(),
            blend: BlendArgs::default(),
            cryptarchia: CryptarchiaArgs::default(),
            sdp: SdpArgs::default(),
            api: ApiArgs::default(),
            state: StateArgs::default(),
        }
    }
}

#[derive(Parser, Debug)]
pub struct MigrateArgs {
    /// Output file path for the generated config.
    #[clap(long = "output", short = 'o', default_value = "user_config.yaml")]
    pub output: PathBuf,

    /// Path for the keystore file.
    #[clap(long = "keystore")]
    pub keystore: PathBuf,

    #[clap(flatten)]
    log: LogArgs,

    #[clap(flatten)]
    network: NetworkArgs,

    #[clap(flatten)]
    blend: BlendArgs,

    #[clap(flatten)]
    cryptarchia: CryptarchiaArgs,

    #[clap(flatten)]
    sdp: SdpArgs,

    #[clap(flatten)]
    api: ApiArgs,

    #[clap(flatten)]
    state: StateArgs,
}

impl MigrateArgs {
    /// Creates arguments programmatically (e.g. from the c-bindings crate),
    /// leaving all config overrides at their defaults.
    #[must_use]
    pub fn new(output: PathBuf, keystore: PathBuf) -> Self {
        Self {
            output,
            keystore,
            log: LogArgs::default(),
            network: NetworkArgs::default(),
            blend: BlendArgs::default(),
            cryptarchia: CryptarchiaArgs::default(),
            sdp: SdpArgs::default(),
            api: ApiArgs::default(),
            state: StateArgs::default(),
        }
    }
}

impl From<MigrateArgs> for InitArgs {
    fn from(migrate: MigrateArgs) -> Self {
        Self {
            output: migrate.output,
            keystore: Some(migrate.keystore),
            log: migrate.log,
            network: migrate.network,
            blend: migrate.blend,
            cryptarchia: migrate.cryptarchia,
            sdp: migrate.sdp,
            api: migrate.api,
            state: migrate.state,
        }
    }
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
    /// Path to the keystore YAML file
    #[arg(long, default_value = "keystore.yaml")]
    pub keystore: PathBuf,
    /// Output directory for `participation_data.yaml`
    #[arg(long, default_value = ".")]
    pub output: PathBuf,
    /// Node's public IPv4 address, required when the blend listening address
    /// is 0.0.0.0
    #[arg(long)]
    pub external_address: Option<Ipv4Addr>,
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
        cryptarchia: cryptarchia_args,
        sdp: sdp_args,
        deployment: deployment_args,
        state: state_args,
        ..
    } = args;
    update_tracing(&mut user_config.tracing, log_args)?;
    update_network(&mut user_config.network, network_args)?;
    update_blend(&mut user_config.blend, blend_args);
    update_cryptarchia(&mut user_config.cryptarchia, cryptarchia_args);
    update_sdp(&mut user_config.sdp, sdp_args);
    update_api(&mut user_config.api, api_args);
    update_state(&mut user_config.state, state_args);

    let deployment_settings = match deployment_args.deployment_type() {
        DeploymentType::WellKnown(well_known_deployment) => (*well_known_deployment).into(),
        DeploymentType::Custom(custom_deployment_config_path) => {
            deserialize_value_at_path::<DeploymentSettings>(
                custom_deployment_config_path,
                OnUnknownKeys::Fail,
            )?
        }
    };

    Ok(RunConfig {
        deployment: deployment_settings,
        user: user_config,
    })
}
