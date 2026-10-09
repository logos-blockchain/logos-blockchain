use core::fmt::Debug;
use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use lb_core::{
    block::genesis::{GenesisBlock, GenesisBlockBuilder},
    crypto::ZkHasher,
    mantle::{
        Note,
        ops::{channel::inscribe::InscriptionOp, sdp::SDPDeclareOp},
    },
};
use lb_node::config::deployment::DeploymentSettings;
use lb_sdp_service::DeclarationConfig;
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_from_reader};
use logos_blockchain_tools::{
    apply_dotted_kv,
    genesis::{
        distribution::{self, Faucet, StakeHolderInfo},
        inscription::{self, InscribeParams},
    },
    overwrite_yaml, set_at_path,
};
use serde_yaml::Value;

// ── CLI definition
// ────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Generate deployment configs and genesis blocks for Logos Blockchain nodes"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Orchestrate the full genesis ceremony: inscribe, distribute, and build
    /// the final deployment configuration.
    Ceremony(CeremonyArgs),

    /// Generate a deployment config YAML from a well-known deployment or file,
    /// with optional field overrides.
    Config(ConfigArgs),

    /// Build a genesis block from component files and optionally embed it into
    /// a deployment config under `genesis_block`.
    Block(BlockArgs),

    /// Calculate the distribution of notes and SDP declarations from
    /// stakeholder and provider definitions.
    Distribute(DistributeArgs),

    /// Generate a genesis `InscriptionOp` using entropy sources.
    Inscribe(InscribeArgs),
}

// ── ceremony subcommand
// ──────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct CeremonyArgs {
    /// Genesis parameters for the `InscriptionOp`.
    #[arg(long, value_name = "FILE")]
    pub inscription_params: PathBuf,

    /// Stakeholder definitions for note distribution.
    #[arg(long, value_name = "FILE")]
    pub stake_holders: PathBuf,

    /// Provider definitions for SDP declarations.
    #[arg(long, value_name = "FILE")]
    pub providers: PathBuf,

    /// Faucet definition for stake distribution.
    #[arg(long, value_name = "FILE")]
    pub faucet: PathBuf,

    /// The genesis template: era zero's ruleset and its parameters. The
    /// ceremony generates the rest of the deployment config from it.
    /// Without it, the default deployment's era zero is used.
    #[arg(long, value_name = "FILE")]
    pub template: Option<PathBuf>,

    /// Optional overrides for the generated deployment config, applied before
    /// its generated values are set.
    #[arg(long = "override", value_name = "KEY=VALUE|FILE", num_args = 1)]
    pub overrides: Vec<String>,

    /// Write the final deployment config to FILE instead of stdout.
    #[arg(long, short, value_name = "FILE")]
    pub output: Option<PathBuf>,
}

// ── config subcommand
// ─────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
struct ConfigArgs {
    /// The path to a custom deployment config.
    #[arg(long = "deployment", value_name = "FILE")]
    pub custom_deployment_path: Option<PathBuf>,

    /// Override to apply on top of the base config. Each occurrence is either
    /// a dot-notation key=value pair, where a number indexes a list or names
    /// an integer key (e.g. the first epoch in
    /// `eras.0.cryptarchia.security_param=60`), or a path to a YAML file that
    /// is deep-merged into the config.
    /// Repeated flags are applied left-to-right.
    #[arg(long = "override", value_name = "KEY=VALUE|FILE", num_args = 1)]
    overrides: Vec<String>,

    /// Write output to FILE instead of stdout.
    #[arg(long, short, value_name = "FILE")]
    output: Option<PathBuf>,
}

// ── block subcommand
// ──────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
struct BlockArgs {
    /// YAML file containing the list of genesis notes.
    /// Each entry must have `value` (u64) and `pk` (hex-encoded `ZkPublicKey`).
    /// At least one note is required.
    ///
    /// Example:
    ///   - value: 100000 pk: eb3158fd...
    #[arg(long, value_name = "FILE")]
    notes: PathBuf,

    /// YAML file containing the genesis `InscriptionOp`.
    /// Must have `channel_id`, `inscription`, `parent`, and `signer` fields.
    ///
    /// Example:
    /// ```yaml
    ///   channel_id: '0000...0000'
    ///   inscription: [103, 101, 110, 101, 115, 105, 115]
    ///   parent: '0000...0000'
    ///   signer: '0000...0000'
    /// ```
    #[arg(long, value_name = "FILE")]
    inscription: PathBuf,

    /// YAML file containing the list of `SDPDeclareOps`.
    /// Each entry must have `service_type`, `locators`, `provider_id`,
    /// `zk_id`, and `service_note_id` fields.
    /// At least one declaration is required.
    #[arg(long, value_name = "FILE")]
    declarations: PathBuf,

    /// Existing deployment config YAML to embed the genesis block into.
    /// When provided, the block is written into `genesis_block`
    /// and the merged config is written to --output. Without this flag,
    /// only the serialized genesis block is written.
    #[arg(long, value_name = "FILE")]
    embed_in: Option<PathBuf>,

    /// Write output to FILE instead of stdout.
    #[arg(long, short, value_name = "FILE")]
    output: Option<PathBuf>,
}

// ── distribute subcommand
// ──────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
struct DistributeArgs {
    /// YAML file containing stakeholder info.
    #[arg(long, value_name = "FILE")]
    stake_holders: PathBuf,

    /// YAML file containing provider info.
    #[arg(long, value_name = "FILE")]
    providers: PathBuf,

    /// YAML file containing faucet info.
    #[arg(long, value_name = "FILE")]
    faucet: PathBuf,

    /// Write notes output to FILE instead of stdout.
    #[arg(long, short, value_name = "FILE")]
    notes_output: Option<PathBuf>,

    /// Write declarations output to FILE instead of stdout.
    #[arg(long, short, value_name = "FILE")]
    declarations_output: Option<PathBuf>,
}

// ── inscribe subcommand
// ──────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
struct InscribeArgs {
    /// YAML file containing genesis parameters (`chain_id`, `genesis_time`, and
    /// `entropy_sources`). `entropy_sources` should be a list of hex-encoded
    /// 32-byte strings.

    #[arg(long, value_name = "FILE")]
    params: PathBuf,

    /// Write the serialized `InscriptionOp` to FILE instead of stdout.
    #[arg(long, short, value_name = "FILE")]
    output: Option<PathBuf>,
}

// ── entry point
// ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Ceremony(args) => run_ceremony(&args),
        Commands::Config(args) => run_config(&args),
        Commands::Block(args) => run_block(&args),
        Commands::Distribute(args) => run_distribute(&args),
        Commands::Inscribe(args) => run_inscribe(&args),
    }
}

/// Where a deployment config keeps its genesis block.
const GENESIS_BLOCK_PATH: &str = "genesis_block";

/// Where a deployment config keeps the faucet key of the era starting at
/// genesis, which is the era a genesis ceremony configures.
const GENESIS_ERA_FAUCET_PK_PATH: &str = "eras.0.cryptarchia.faucet_pk";

// ── ceremony implementation
// ─────────────────────────────────────────────────────

fn run_ceremony(args: &CeremonyArgs) -> Result<()> {
    let inscribe_params: InscribeParams = load_yaml_file(&args.inscription_params)?;
    let inscription_op = inscription::inscribe::<ZkHasher>(
        inscribe_params.chain_id,
        inscribe_params.genesis_time,
        inscribe_params.entropy_sources,
    );

    let stakeholders: Vec<StakeHolderInfo> = load_yaml_file(&args.stake_holders)?;
    let providers: Vec<DeclarationConfig> = load_yaml_file(&args.providers)?;
    let faucet: Faucet = load_yaml_file(&args.faucet)?;
    let (transfer_op, declarations) = distribution::distribute(stakeholders, providers, &faucet)
        .map_err(|e| anyhow::anyhow!(e))
        .context("Failed to calculate distribution during ceremony")?;
    let notes: Vec<Note> = transfer_op.notes().collect();

    let mut config_value = load_genesis_template(args.template.as_ref())?;
    for raw in &args.overrides {
        apply_override(&mut config_value, raw)?;
    }

    if notes.is_empty() {
        bail!("Ceremony failed: distribution resulted in zero notes");
    }
    if declarations.is_empty() {
        bail!("Ceremony failed: distribution resulted in zero declarations");
    }
    let genesis_block = build_genesis_block(notes, inscription_op, declarations)?;

    set_at_path(
        &mut config_value,
        GENESIS_BLOCK_PATH,
        struct_to_yaml_value(&genesis_block)?,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    set_at_path(
        &mut config_value,
        GENESIS_ERA_FAUCET_PK_PATH,
        struct_to_yaml_value(&faucet.zk_id)?,
    )
    .map_err(|e| anyhow::anyhow!(e))?;

    ensure_valid_deployment_settings(&config_value)?;

    write_yaml(&config_value, args.output.as_deref())
}

// ── config implementation
// ─────────────────────────────────────────────────────

fn run_config(args: &ConfigArgs) -> Result<()> {
    let mut config = load_base_config(args.custom_deployment_path.as_ref())?;

    for raw in &args.overrides {
        apply_override(&mut config, raw)?;
    }

    ensure_valid_deployment_settings(&config)?;

    write_yaml(&config, args.output.as_deref())
}

/// Load a deployment config as a raw YAML value.
///
/// If `path` is `None`, returns the default config as a YAML value. Otherwise,
/// loads the YAML file at `path` and returns it as a `Value`.
fn load_base_config(path: Option<&PathBuf>) -> Result<Value> {
    let Some(path) = path else {
        let default_config = DeploymentSettings::default();
        return struct_to_yaml_value(&default_config);
    };

    let content = fs::read_to_string(path)
        .with_context(|| format!("cannot read config file '{}'", path.display()))?;
    serde_yaml::from_str(&content)
        .with_context(|| format!("cannot parse YAML from '{}'", path.display()))
}

/// Load a genesis template, and assemble from it the deployment config the
/// ceremony completes: the template's ruleset (`!V1`), with its parameters,
/// becomes era zero.
///
/// If `path` is `None`, returns the default deployment config, whose genesis
/// block the ceremony replaces.
fn load_genesis_template(path: Option<&PathBuf>) -> Result<Value> {
    let Some(path) = path else {
        return struct_to_yaml_value(&DeploymentSettings::default());
    };

    let content = fs::read_to_string(path)
        .with_context(|| format!("cannot read genesis template '{}'", path.display()))?;
    let template: Value = serde_yaml::from_str(&content)
        .with_context(|| format!("cannot parse YAML from '{}'", path.display()))?;
    if !matches!(&template, Value::Tagged(era) if era.value.is_mapping()) {
        bail!(
            "genesis template '{}' is not a mapping tagged with its version",
            path.display()
        );
    }

    let mut eras = serde_yaml::Mapping::new();
    eras.insert(Value::from(0u64), template);
    let mut config = serde_yaml::Mapping::new();
    config.insert(Value::from("eras"), Value::Mapping(eras));
    Ok(Value::Mapping(config))
}

/// Apply a single `--override` argument to `config`.
///
/// If `s` contains `=`, it is applied as a dotted `key=value` pair.
/// Otherwise it is treated as a path to a YAML file, deep-merged into `config`.
fn apply_override(config: &mut Value, s: &str) -> Result<()> {
    if s.contains('=') {
        return apply_dotted_kv(config, s).map_err(|e| anyhow::anyhow!(e));
    }

    let path = Path::new(s);
    let content = fs::read_to_string(path)
        .with_context(|| format!("cannot read override file '{}'", path.display()))?;
    let patch = serde_yaml::from_str(&content)
        .with_context(|| format!("cannot parse YAML from override file '{}'", path.display()))?;
    *config = overwrite_yaml(std::mem::take(config), patch);
    Ok(())
}

// ── block implementation
// ──────────────────────────────────────────────────────

fn run_block(args: &BlockArgs) -> Result<()> {
    let notes: Vec<Note> = load_yaml_file(&args.notes)?;
    let inscription: InscriptionOp = load_yaml_file(&args.inscription)?;
    let declarations: Vec<SDPDeclareOp> = load_yaml_file(&args.declarations)?;

    if notes.is_empty() {
        bail!("notes file must contain at least one Note");
    }
    if declarations.is_empty() {
        bail!("declarations file must contain at least one SDPDeclareOp");
    }

    let genesis_block = build_genesis_block(notes, inscription, declarations)?;

    let result = match args.embed_in {
        Some(ref embed_path) => {
            let mut base: Value = load_yaml_file(embed_path)?;
            set_at_path(
                &mut base,
                GENESIS_BLOCK_PATH,
                struct_to_yaml_value(&genesis_block)?,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            base
        }
        None => struct_to_yaml_value(&genesis_block)?,
    };

    write_yaml(&result, args.output.as_deref())
}

/// Drive the [`GenesisBlockBuilder`] typestate machine with the supplied
/// components and return the finished [`GenesisBlock`].
fn build_genesis_block(
    notes: Vec<Note>,
    inscription: InscriptionOp,
    declarations: Vec<SDPDeclareOp>,
) -> Result<GenesisBlock> {
    let mut notes_iter = notes.into_iter();
    let mut decls_iter = declarations.into_iter();

    // Non-emptiness is checked by the caller, so these unwraps are safe.
    let first_note = notes_iter.next().unwrap();
    let first_decl = decls_iter.next().unwrap();

    // Accumulate additional notes into WithNotes state.
    let mut builder = GenesisBlockBuilder::new().add_note(first_note);
    for note in notes_iter {
        builder = builder
            .try_add_note(note)
            .context("failed to append note to genesis transfer")?;
    }

    // Transition: WithNotes → WithNotesAndInscription → WithAll.
    let mut builder = builder
        .set_inscription(inscription)
        .add_declaration(first_decl);
    for decl in decls_iter {
        builder = builder.add_declaration(decl)?;
    }

    builder.build().context("failed to build genesis block")
}

// ── distribute implementation
// ─────────────────────────────────────────────────────

fn run_distribute(args: &DistributeArgs) -> Result<()> {
    let stakeholders: Vec<StakeHolderInfo> = load_yaml_file(&args.stake_holders)?;
    let providers: Vec<DeclarationConfig> = load_yaml_file(&args.providers)?;
    let faucet: Faucet = load_yaml_file(&args.faucet)?;

    let (transfer_op, declarations) = distribution::distribute(stakeholders, providers, &faucet)
        .map_err(|e| anyhow::anyhow!(e))
        .context("Failed to calculate distribution")?;
    let notes: Vec<Note> = transfer_op.notes().collect();

    let notes_value = struct_to_yaml_value(&notes)?;
    let declarations_value = struct_to_yaml_value(&declarations)?;

    write_yaml(&notes_value, args.notes_output.as_deref())?;
    write_yaml(&declarations_value, args.declarations_output.as_deref())?;

    Ok(())
}

// ── inscribe implementation
// ─────────────────────────────────────────────────────

fn run_inscribe(args: &InscribeArgs) -> Result<()> {
    let params: InscribeParams = load_yaml_file(&args.params)?;

    let op = inscription::inscribe::<ZkHasher>(
        params.chain_id,
        params.genesis_time,
        params.entropy_sources,
    );

    let op_value = struct_to_yaml_value(&op)?;
    write_yaml(&op_value, args.output.as_deref())
}

// ── shared helpers
// ────────────────────────────────────────────────────────────

/// Serialize a value to a human-readable YAML [`Value`].
///
/// Two pitfalls make a direct `serde_yaml::to_value` call unsuitable:
///
/// 1. `serde_yaml::to_value` uses a *non*-human-readable serializer, so types
///    guarded by `is_human_readable()` (e.g. `HeaderId`, `MantleTx`) fall back
///    to their binary representation.
/// 2. Some types (e.g. `PoLProof`) call `serializer.serialize_bytes`
///    unconditionally; `serde_yaml::to_string` rejects those with an error.
///
/// Using `serde_yaml::to_string` as an intermediate format avoids both
/// problems: YAML is a human-readable format (fixing pitfall 1).
/// Regarding (fixing pitfall 2): The error doesn't appear when using templates
/// from the `deployment/ceremony/genesis/<env>` directories, but if it happens,
/// settings override code should be refactored to use concrete genesis related
/// types instead of operating at YAML level.
fn struct_to_yaml_value<T: serde::Serialize>(value: &T) -> Result<Value> {
    let yaml_string = serde_yaml::to_string(value)?;
    serde_yaml::from_str(&yaml_string).map_err(Into::into)
}

fn ensure_valid_deployment_settings(value: &Value) -> Result<()> {
    let yaml = serde_yaml::to_string(value)?;
    drop(
        deserialize_value_from_reader::<DeploymentSettings, _>(
            yaml.as_bytes(),
            OnUnknownKeys::Fail,
        )
        .context("generated config is not a valid DeploymentSettings value")?,
    );
    Ok(())
}

fn load_yaml_file<T>(path: &Path) -> Result<T>
where
    T: serde::de::DeserializeOwned + Send + Sync + Debug + 'static,
{
    let content =
        fs::read_to_string(path).with_context(|| format!("cannot read '{}'", path.display()))?;
    deserialize_value_from_reader(content.as_bytes(), OnUnknownKeys::Fail)
        .with_context(|| format!("cannot parse YAML from '{}'", path.display()))
}

fn write_yaml(value: &Value, output: Option<&Path>) -> Result<()> {
    let yaml = serde_yaml::to_string(value)?;
    output.map_or_else(
        || {
            io::stdout()
                .write_all(yaml.as_bytes())
                .context("cannot write to stdout")
        },
        |path| {
            fs::write(path, yaml.as_bytes())
                .with_context(|| format!("cannot write to '{}'", path.display()))
        },
    )
}
