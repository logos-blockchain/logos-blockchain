use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use lb_core::{
    block::genesis::{GenesisBlock, GenesisBlockBuilder},
    mantle::{
        Note,
        ops::{channel::inscribe::InscriptionOp, sdp::SDPDeclareOp},
    },
};
use lb_node::config::deployment::{DeploymentSettings, WellKnownDeployment};
use logos_blockchain_tools::{
    distribution::{self, ProviderInfo, StakeHolderInfo},
    overwrite_yaml, value_from_dotted_kv,
};
use serde_yml::Value;

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
    /// Generate a deployment config YAML from a well-known deployment or file,
    /// with optional field overrides.
    Config(ConfigArgs),

    /// Build a genesis block from component files and optionally embed it into
    /// a deployment config under `cryptarchia.genesis_block`.
    Block(BlockArgs),

    /// Calculate the distribution of notes and SDP declarations from
    /// stakeholder and provider definitions.
    Distribute(DistributeArgs),
}

// ── config subcommand
// ─────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
struct ConfigArgs {
    /// Base config source: a well-known deployment name (e.g. 'devnet') or a
    /// path to an existing YAML deployment config file.
    #[arg(long, value_name = "NAME_OR_PATH")]
    deployment: String,

    /// Override to apply on top of the base config. Each occurrence is either
    /// a dot-notation key=value pair (e.g. `cryptarchia.security_param=60`)
    /// or a path to a YAML file that is deep-merged into the config.
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
    /// `zk_id`, and `locked_note_id` fields.
    /// At least one declaration is required.
    #[arg(long, value_name = "FILE")]
    declarations: PathBuf,

    /// Existing deployment config YAML to embed the genesis block into.
    /// When provided, the block is written into `cryptarchia.genesis_block`
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

    /// Write notes output to FILE instead of stdout.
    #[arg(long, short, value_name = "FILE")]
    notes_output: Option<PathBuf>,

    /// Write declarations output to FILE instead of stdout.
    #[arg(long, short, value_name = "FILE")]
    declarations_output: Option<PathBuf>,
}

// ── entry point
// ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Config(args) => run_config(&args),
        Commands::Block(args) => run_block(&args),
        Commands::Distribute(args) => run_distribute(&args),
    }
}

// ── config implementation
// ─────────────────────────────────────────────────────

fn run_config(args: &ConfigArgs) -> Result<()> {
    let mut config = load_base_config(&args.deployment)?;

    for raw in &args.overrides {
        let patch = resolve_override(raw)?;
        config = overwrite_yaml(config, patch);
    }

    write_yaml(&config, args.output.as_deref())
}

/// Load a deployment config as a raw YAML value.
///
/// `source` is first matched against the known well-known deployment names; if
/// it does not match, it is treated as a file path.
fn load_base_config(source: &str) -> Result<Value> {
    if let Ok(well_known) = source.parse::<WellKnownDeployment>() {
        let settings = DeploymentSettings::from(well_known);
        return struct_to_yaml_value(&settings);
    }

    let path = Path::new(source);
    let content = fs::read_to_string(path)
        .with_context(|| format!("cannot read config file '{}'", path.display()))?;
    serde_yml::from_str(&content)
        .with_context(|| format!("cannot parse YAML from '{}'", path.display()))
}

/// Resolve a single `--override` argument.
///
/// If `s` contains `=`, it is parsed as a dotted `key=value` pair.
/// Otherwise it is treated as a path to a YAML file.
fn resolve_override(s: &str) -> Result<Value> {
    if s.contains('=') {
        return value_from_dotted_kv(s).map_err(|e| anyhow::anyhow!(e));
    }

    let path = Path::new(s);
    let content = fs::read_to_string(path)
        .with_context(|| format!("cannot read override file '{}'", path.display()))?;
    serde_yml::from_str(&content)
        .with_context(|| format!("cannot parse YAML from override file '{}'", path.display()))
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
            let block_value = struct_to_yaml_value(&genesis_block)?;
            let patch = wrap_as_cryptarchia_genesis_block(block_value);
            let base: Value = load_yaml_file(embed_path)?;
            overwrite_yaml(base, patch)
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
        builder = builder.add_note(note);
    }

    // Transition: WithNotes → WithNotesAndInscription → WithAll.
    let mut builder = builder
        .set_inscription(inscription)
        .add_declaration(first_decl);
    for decl in decls_iter {
        builder = builder.add_declaration(decl);
    }

    builder.build().context("failed to build genesis block")
}

/// Wrap a serialised `GenesisBlock` value in the mapping that corresponds to
/// `cryptarchia.genesis_block` in a deployment config.
fn wrap_as_cryptarchia_genesis_block(block_value: Value) -> Value {
    let mut inner = serde_yml::Mapping::new();
    inner.insert(Value::String("genesis_block".to_owned()), block_value);

    let mut outer = serde_yml::Mapping::new();
    outer.insert(
        Value::String("cryptarchia".to_owned()),
        Value::Mapping(inner),
    );

    Value::Mapping(outer)
}

// ── distribute implementation
// ─────────────────────────────────────────────────────

fn run_distribute(args: &DistributeArgs) -> Result<()> {
    let stakeholders: Vec<StakeHolderInfo> = load_yaml_file(&args.stake_holders)?;
    let providers: Vec<ProviderInfo> = load_yaml_file(&args.providers)?;

    let (utxos, declarations) = distribution::distribute(stakeholders, providers)
        .map_err(|e| anyhow::anyhow!(e))
        .context("Failed to calculate distribution")?;

    let utxos_value = struct_to_yaml_value(&utxos)?;
    let declarations_value = struct_to_yaml_value(&declarations)?;

    write_yaml(&utxos_value, args.notes_output.as_deref())?;
    write_yaml(&declarations_value, args.declarations_output.as_deref())?;

    Ok(())
}

// ── shared helpers
// ────────────────────────────────────────────────────────────

/// Serialize a value to a human-readable YAML [`Value`].
///
/// Two pitfalls make a direct `serde_yml::to_value` call unsuitable:
///
/// 1. `serde_yml::to_value` uses a *non*-human-readable serializer, so types
///    guarded by `is_human_readable()` (e.g. `HeaderId`, `MantleTx`) fall back
///    to their binary representation.
/// 2. Some types (e.g. `PoLProof`) call `serializer.serialize_bytes`
///    unconditionally; `serde_yml::to_string` rejects those with an error.
///
/// Using `serde_json` as an intermediate format avoids both problems: JSON is
/// a human-readable format (fixing pitfall 1), and its `serialize_bytes`
/// implementation emits a JSON array of integers (fixing pitfall 2). JSON is
/// a strict subset of YAML, so `serde_yml::from_str` can parse the resulting
/// JSON string transparently, and YAML's human-readable deserializer correctly
/// interprets all field formats.
fn struct_to_yaml_value<T: serde::Serialize>(value: &T) -> Result<Value> {
    let json = serde_json::to_string(value)?;
    serde_yml::from_str(&json).map_err(Into::into)
}

fn load_yaml_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let content =
        fs::read_to_string(path).with_context(|| format!("cannot read '{}'", path.display()))?;
    serde_yml::from_str(&content)
        .with_context(|| format!("cannot parse YAML from '{}'", path.display()))
}

fn write_yaml(value: &Value, output: Option<&Path>) -> Result<()> {
    let yaml = serde_yml::to_string(value)?;
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
