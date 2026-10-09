use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io::{BufRead as _, BufReader, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use cucumber::{gherkin::Step, given, then, when};
use futures_util::future::join_all;
use hex::ToHex as _;
use lb_common_http_client::ApiBlock;
use lb_core::{
    header::HeaderId, mantle::traits::Hashable as _, proofs::leader_proof::LeaderProof as _,
};
use lb_node::config::DeploymentSettings;
use lb_testing_framework::{
    NodeHttpClient, USER_CONFIG_FILE, configs::deployment::NodeBinaryProfile,
    ensure_node_binary_built,
};
use serde_json::{Value, json};
use tokio::time::sleep;

mod ancestry;
mod brief;
mod evidence;
mod producer;
mod propagation;
mod scenario;
use ancestry::{
    ancestry_path_json, ancestry_presence, block_id_at_height, fetch_header_link,
    header_cache_failures_json, proven_below_target_slot_path, walk_between_finalized_blocks,
    walk_to_target,
};
use brief::{
    attack_branch_json, canonical_tips_aligned, capture_pre_attack_consensus,
    check_finalized_history_agreement, classify_attack_branch, classify_tip_transition,
    common_finalized_lib,
};
use evidence::{
    command_output, format_log_timestamp, identity_bool, original_node_logs, parse_log_fields,
    parse_log_timestamp, parse_usize, security_audit_source_files, sha256_file,
};
use producer::{
    AuditConsensusParameters, find_producer_opportunities, has_nonempty_submitted_body,
    parse_header_id, record_voucher_commitment, same_transaction_body, sibling_body_integrity,
    summarize_api_block, wait_for_blend_readiness,
};
use propagation::{
    ReceiverIdentity, observe_sibling_propagation, query_accepted_blocks, receiver_stages,
    record_nonempty_sibling_propagation, wait_for_selected_reception,
};
use scenario::{audit_participants, audit_sibling_count, audit_workload, startup_sibling_count};

#[cfg(test)]
mod tests;

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::{
        TARGET,
        mempool::actions::{prepare_transfer_transaction, submit_prepared_transaction_to_nodes},
        nodes::diagnostics::BlendDiagnosticEventLogger,
    },
    utils::tx_hash_to_hex,
    world::{
        CucumberWorld, SecurityAuditParticipants, SecurityAuditWorkload,
        SecurityAuditWorkloadPolicy,
    },
};

#[cfg(test)]
const SIBLING_COUNT: usize = 4;
const FINALITY_MAX_ANCESTRY_DEPTH: usize = 100_000;

const fn valid_sibling_minimum(configured: usize, minimum: usize) -> bool {
    minimum > 0 && minimum <= configured
}

#[derive(Clone)]
struct ProducerOpportunity {
    ordinary: HashMap<String, String>,
    ordinary_line: usize,
    siblings: Vec<(usize, HashMap<String, String>)>,
    timing: HashMap<String, String>,
    timing_line: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
struct BlockBodyEvidence {
    block_id: String,
    parent_block_id: String,
    slot: u64,
    body_root: String,
    voucher_commitment: String,
    transaction_hashes: Vec<String>,
}

struct AuditFailure {
    message: String,
    details: Value,
}

struct AuditLogTail {
    root: PathBuf,
    participants: SecurityAuditParticipants,
    runtime_names: BTreeMap<String, String>,
    paths: BTreeMap<String, Vec<PathBuf>>,
    offsets: HashMap<PathBuf, u64>,
}

/// State belonging to one Cucumber scenario, including its selected
/// opportunity.
pub struct AuditScenarioState {
    provenance: Value,
    readiness: Value,
    tails: AuditLogTail,
    logs: BTreeMap<String, Vec<ObservedLogLine>>,
    opportunity: Option<ProducerOpportunity>,
    nonempty: Option<NonemptySelection>,
}

struct NonemptySelection {
    ordinary_body: BlockBodyEvidence,
    pre_attack_consensus: Value,
    submitted_hashes: BTreeSet<String>,
    submitted_transactions: Vec<Value>,
    transaction_observations: Vec<Value>,
    skipped_opportunities: Vec<Value>,
    examined_count: usize,
    workload: SecurityAuditNonemptyWorkload,
    selected_at_unix_ms: u64,
    selected_at: Instant,
}

#[derive(Clone, Debug)]
struct SecurityAuditNonemptyWorkload {
    budget: usize,
    window_seconds: u64,
    pending_seconds: u64,
}

/// Scheduling another transaction and observing leadership have separate
/// bounds: running out of transactions must not end the observation window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NonemptyWorkloadAction {
    SubmitNext,
    Observe,
    Finished,
}

#[derive(Default)]
struct NonemptyOpportunityState {
    logs: Vec<ObservedLogLine>,
    examined_ids: BTreeSet<String>,
    incomplete_body_ids: BTreeSet<String>,
    skipped: Vec<Value>,
    last_body_error: Option<String>,
}

impl NonemptyOpportunityState {
    async fn select(
        &mut self,
        world: &CucumberWorld,
        producer: &str,
        configured_siblings: usize,
        submitted_hashes: &BTreeSet<String>,
    ) -> Result<Option<(ProducerOpportunity, ApiBlock)>, AuditFailure> {
        let node = world.nodes_info.get(producer).ok_or_else(|| AuditFailure {
            message: format!("{producer} was not started"),
            details: json!({}),
        })?;
        for opportunity in find_producer_opportunities(&self.logs) {
            if parse_usize(&opportunity.ordinary, "configured_siblings")
                != Some(configured_siblings)
            {
                return Err(AuditFailure {
                    message: "producer opportunity contradicts declared sibling configuration"
                        .to_owned(),
                    details: json!({"ordinary_submission": opportunity.ordinary}),
                });
            }
            let Some(ordinary_id) = opportunity.ordinary.get("ordinary_block_id").cloned() else {
                continue;
            };
            if self.examined_ids.contains(&ordinary_id) {
                continue;
            }
            let header_id = match parse_header_id(&ordinary_id) {
                Ok(id) => id,
                Err(error) => {
                    self.incomplete_body_ids.insert(ordinary_id.clone());
                    self.skipped.push(json!({"ordinary_block_id": ordinary_id, "reason": "invalid_ordinary_block_id", "detail": error}));
                    self.examined_ids.insert(ordinary_id);
                    continue;
                }
            };
            self.incomplete_body_ids.insert(ordinary_id.clone());
            let block = match node.client.block(&header_id).await {
                Ok(Some(block)) => block,
                Ok(None) => {
                    self.incomplete_body_ids.insert(ordinary_id);
                    continue;
                }
                Err(error) => {
                    self.incomplete_body_ids.insert(ordinary_id);
                    self.last_body_error = Some(error.to_string());
                    continue;
                }
            };
            self.incomplete_body_ids.remove(&ordinary_id);
            let body = summarize_api_block(&block);
            self.examined_ids.insert(ordinary_id.clone());
            if body.block_id != ordinary_id
                || Some(&body.parent_block_id) != opportunity.ordinary.get("parent_block_id")
                || Some(&body.slot.to_string()) != opportunity.ordinary.get("slot")
            {
                return Err(AuditFailure {
                    message: format!(
                        "{producer} block API identity disagrees with the ordinary submission event"
                    ),
                    details: json!({"ordinary_submission": opportunity.ordinary, "ordinary_block_api": body}),
                });
            }
            if has_nonempty_submitted_body(&body.transaction_hashes, submitted_hashes) {
                return Ok(Some((opportunity, block)));
            }
            self.skipped.push(json!({
                "ordinary_block_id": body.block_id, "parent_block_id": body.parent_block_id,
                "slot": body.slot, "transaction_count": body.transaction_hashes.len(),
                "transaction_hashes": body.transaction_hashes, "body_root": body.body_root,
                "reason": if body.transaction_hashes.is_empty() { "empty_ordinary_body" } else { "no_submitted_transaction_in_ordinary_body" },
            }));
        }
        Ok(None)
    }
}

fn nonempty_workload_action(
    now: Instant,
    deadline: Instant,
    pending_deadline: Option<Instant>,
    submitted: usize,
    budget: usize,
) -> NonemptyWorkloadAction {
    if now >= deadline {
        NonemptyWorkloadAction::Finished
    } else if submitted < budget && pending_deadline.is_none_or(|end| now >= end) {
        NonemptyWorkloadAction::SubmitNext
    } else {
        NonemptyWorkloadAction::Observe
    }
}

#[derive(Clone)]
struct ObservedLogLine {
    text: String,
    path: PathBuf,
    offset: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HeaderLink {
    id: HeaderId,
    parent: HeaderId,
    slot: u64,
}

#[derive(Default)]
struct HeaderCache {
    links: HashMap<HeaderId, HeaderLink>,
    failures: BTreeMap<String, String>,
}

struct AncestryPath {
    links: Vec<HeaderLink>,
    target_reached: bool,
    complete_below_slot: bool,
    error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AttackBranch {
    Ordinary,
    Sibling(usize),
    OtherCompeting(HeaderId),
    None,
    Unverified,
}

#[derive(Clone)]
struct AttackCandidate {
    id: HeaderId,
}

#[derive(Clone)]
struct AttackEvidence {
    ordinary_id: HeaderId,
    ordinary_id_text: String,
    parent_id: HeaderId,
    parent_id_text: String,
    slot: u64,
    body_root: String,
    transaction_hashes: Vec<String>,
    siblings: Vec<(usize, AttackCandidate)>,
}

#[derive(Clone)]
struct ConsensusState {
    tip_id: HeaderId,
    tip_id_text: String,
    tip_height: u64,
    tip_slot: u64,
    lib_id: HeaderId,
    lib_id_text: String,
    lib_slot: u64,
    tip_descends_from_lib: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinalizedHistoryRelation {
    Shared,
    Ancestral,
    Conflicting,
    Unverified,
}

#[derive(Clone, Debug)]
struct FinalizedHistoryCheck {
    relation: FinalizedHistoryRelation,
    evidence: Value,
}

impl serde::Serialize for FinalizedHistoryCheck {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&self.evidence, serializer)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TipTransition {
    Unchanged,
    Descendant,
    Reorganization,
    Unverified,
}

mod sustained;

struct FinalityNodeObservation {
    node_name: String,
    observed_at_unix_ms: u64,
    snapshot: Value,
    consensus: Option<ConsensusState>,
    tip_path_complete: bool,
    finalized_path_complete: bool,
    attack_branch: AttackBranch,
    finalized_attack_branch: AttackBranch,
    continuity: LibContinuity,
}

impl FinalityNodeObservation {
    fn tip_descends_from_lib(&self) -> bool {
        self.consensus
            .as_ref()
            .is_some_and(|state| state.tip_descends_from_lib == Some(true))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum LibContinuity {
    Verified,
    Unchanged,
    Baseline,
    RegressionOrUnrelated,
    Unverified,
}

#[then(expr = "all declared audit nodes establish Blend readiness within {int} seconds")]
async fn step_audit_blend_readiness(world: &mut CucumberWorld, seconds: u64) -> StepResult {
    if seconds == 0 || world.security_audit_state.is_some() {
        return Err(StepError::InvalidArgument {
            message: "readiness duration must be positive and declared only once".to_owned(),
        });
    }
    let count =
        audit_sibling_count(world).map_err(|message| StepError::InvalidArgument { message })?;
    let provenance = verify_runtime_configuration_with_siblings(world, "security audit", count)
        .await
        .map_err(|message| StepError::StepFail { message })?;
    let (readiness, tails) = wait_for_blend_readiness(world, Duration::from_secs(seconds))
        .await
        .map_err(|message| StepError::StepFail { message })?;
    world.security_audit_state = Some(AuditScenarioState {
        provenance,
        readiness,
        tails,
        logs: BTreeMap::new(),
        opportunity: None,
        nonempty: None,
    });
    Ok(())
}

fn unix_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn read_consensus_configuration(world: &CucumberWorld) -> Result<Value, String> {
    let participants = audit_participants(world)?;
    let mut configs = BTreeMap::new();
    let mut reference = None;
    for name in std::iter::once(&participants.producer).chain(&participants.honest_nodes) {
        let node = world
            .nodes_info
            .get(name)
            .ok_or_else(|| format!("node information for {name} is unavailable"))?;
        let path = node.runtime_dir.join("deployment.yaml");
        let contents =
            fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let deployment: DeploymentSettings = serde_yaml::from_slice(&contents)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        let parameters = deployment.genesis_era_parameters();
        let cryptarchia = &parameters.cryptarchia;
        let epoch = cryptarchia.epoch_config;
        let settings = AuditConsensusParameters {
            security_parameter_k: cryptarchia.security_param.get(),
            slot_duration_secs: parameters.time.slot_duration().as_secs(),
            uncle_reference_window_in_block: cryptarchia.uncle_reference_window_in_block,
            slot_activation_coeff_numerator: cryptarchia.slot_activation_coeff.numerator,
            slot_activation_coeff_denominator: cryptarchia.slot_activation_coeff.denominator.get(),
            epoch_stake_distribution_stabilization: epoch
                .epoch_stake_distribution_stabilization
                .get(),
            epoch_period_nonce_buffer: epoch.epoch_period_nonce_buffer.get(),
            epoch_period_nonce_stabilization: epoch.epoch_period_nonce_stabilization.get(),
            slots_per_epoch: cryptarchia.slots_per_epoch(),
        };
        if let Some(expected) = &reference {
            if expected != &settings {
                return Err(format!("consensus configuration differs for node {name}"));
            }
        } else {
            reference = Some(settings.clone());
        }
        configs.insert(name.clone(), json!({"deployment_config_path": path, "deployment_config_sha256": sha256_file(&path)?, "settings": settings}));
    }
    let settings = reference.ok_or("no declared participants")?;
    let producer_config = &configs[&participants.producer];
    Ok(json!({
        "status": "read",
        "source_node": participants.producer,
        "deployment_config_path": producer_config["deployment_config_path"],
        "deployment_config_sha256": producer_config["deployment_config_sha256"],
        "security_parameter_k": settings.security_parameter_k,
        "slot_duration_secs": settings.slot_duration_secs,
        "uncle_reference_window_in_block": settings.uncle_reference_window_in_block,
        "slot_activation_coeff_numerator": settings.slot_activation_coeff_numerator,
        "slot_activation_coeff_denominator": settings.slot_activation_coeff_denominator,
        "epoch_stake_distribution_stabilization": settings.epoch_stake_distribution_stabilization,
        "epoch_period_nonce_buffer": settings.epoch_period_nonce_buffer,
        "epoch_period_nonce_stabilization": settings.epoch_period_nonce_stabilization,
        "slots_per_epoch": settings.slots_per_epoch,
        "per_node": configs,
        "all_nodes_agree": true,
        "source": "generated node deployment YAML",
        "interpretation": "functional experiment; not a production security threshold",
    }))
}

fn consensus_configuration(world: &CucumberWorld) -> Value {
    read_consensus_configuration(world).unwrap_or_else(
        |error| json!({ "status": "unverified", "security_parameter_k": null, "error": error }),
    )
}

fn validated_node_sibling_count(world: &CucumberWorld, node_name: &str) -> Result<usize, String> {
    let participants = world
        .security_audit_participants
        .as_ref()
        .ok_or_else(|| "security audit participants were not declared".to_owned())?;
    let is_producer = participants.producer == node_name;
    if !is_producer
        && !participants
            .honest_nodes
            .iter()
            .any(|name| name == node_name)
    {
        return Err(format!(
            "started node {node_name} is not a declared audit participant"
        ));
    }
    let node = world
        .nodes_info
        .get(node_name)
        .ok_or_else(|| format!("node {node_name} was not started"))?;
    let expected = startup_sibling_count(&node.startup_user_config_overrides)?;
    let path = node.runtime_dir.join(USER_CONFIG_FILE);
    let bytes =
        fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let config: serde_yaml::Value = serde_yaml::from_slice(&bytes)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    let setting =
        &config["cryptarchia"]["leader"]["security_audit"]["sibling_blocks_per_leadership"];
    let actual = if setting.is_null() {
        0
    } else {
        setting
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| format!("invalid sibling count in {}", path.display()))?
    };
    validate_sibling_configuration(node_name, is_producer, expected, actual)?;
    Ok(actual)
}

fn validate_sibling_configuration(
    node_name: &str,
    is_producer: bool,
    expected: usize,
    actual: usize,
) -> Result<(), String> {
    if !is_producer && expected != 0 {
        return Err(format!(
            "honest node {node_name} has nonzero sibling startup configuration"
        ));
    }
    if actual != expected {
        return Err(format!(
            "generated configuration for {node_name} has sibling count {actual}, startup override requires {expected}"
        ));
    }
    Ok(())
}

/// Validate declared participants before a generic observation step starts.
pub fn verify_audit_participants_started(world: &CucumberWorld) -> StepResult {
    if world.security_audit_participants.is_some() {
        audit_participants(world).map_err(|message| StepError::StepFail { message })?;
    }
    Ok(())
}

/// Verify audit startup settings as nodes are launched and record the complete
/// configuration once all declared participants have started.
pub async fn verify_audit_node_start(world: &CucumberWorld, node_name: &str) -> StepResult {
    let Some(participants) = &world.security_audit_participants else {
        return Ok(());
    };
    validated_node_sibling_count(world, node_name)
        .map_err(|message| StepError::StepFail { message })?;
    if world.nodes_info.len() == participants.honest_nodes.len() + 1 {
        let count =
            audit_sibling_count(world).map_err(|message| StepError::StepFail { message })?;
        let provenance = verify_runtime_configuration_with_siblings(world, "security audit", count)
            .await
            .map_err(|message| StepError::StepFail { message })?;
        let path = world
            .lifecycle
            .scenario_base_dir
            .join("audit-configuration.json");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&provenance).map_err(|error| StepError::StepFail {
                message: error.to_string(),
            })?,
        )
        .map_err(|error| StepError::StepFail {
            message: format!("cannot write {}: {error}", path.display()),
        })?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "Preserves per-node configuration and binary provenance in one report"
)]
async fn verify_runtime_configuration_with_siblings(
    world: &CucumberWorld,
    scenario: &str,
    expected_producer_siblings: usize,
) -> Result<Value, String> {
    let participants = audit_participants(world)?;
    let profile =
        NodeBinaryProfile::from_string(&std::env::var("NODE_BINARY_PROFILE").unwrap_or_default());
    let binary = ensure_node_binary_built(&profile)
        .await
        .map_err(|error| format!("cannot resolve framework-selected node binary: {error}"))?
        .canonicalize()
        .map_err(|error| format!("cannot resolve selected node binary: {error}"))?;
    let binary_sha256 = sha256_file(&binary)?;
    let consensus_config = read_consensus_configuration(world)?;
    let configured_binary = std::env::var_os("LOGOS_BLOCKCHAIN_NODE_BIN")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let binary_selection_mode = binary_selection_mode(configured_binary.as_deref(), &binary);
    let commit = command_output("git", &["rev-parse", "HEAD"])?;
    let mut nodes = BTreeMap::new();

    for (node_name, expected) in
        std::iter::once((participants.producer.as_str(), expected_producer_siblings)).chain(
            participants
                .honest_nodes
                .iter()
                .map(|node| (node.as_str(), 0)),
        )
    {
        let node = world
            .nodes_info
            .get(node_name)
            .ok_or_else(|| format!("node {node_name} was not started"))?;
        let config_path = node.runtime_dir.join(USER_CONFIG_FILE);
        let config_bytes = fs::read(&config_path).map_err(|error| {
            format!(
                "cannot read generated YAML {}: {error}",
                config_path.display()
            )
        })?;
        let config: serde_yaml::Value = serde_yaml::from_slice(&config_bytes).map_err(|error| {
            format!(
                "cannot parse generated YAML {}: {error}",
                config_path.display()
            )
        })?;
        let setting =
            &config["cryptarchia"]["leader"]["security_audit"]["sibling_blocks_per_leadership"];
        let actual = validated_node_sibling_count(world, node_name)?;
        if actual != expected {
            return Err(format!(
                "generated YAML for {node_name} has sibling count {actual}, expected {expected}: {}",
                config_path.display()
            ));
        }
        let identity = world
            .cluster
            .node_peer_ids
            .get(node_name)
            .map(ToString::to_string)
            .ok_or_else(|| format!("node identity was unavailable for {node_name}"))?;
        nodes.insert(
            node_name,
            json!({
                "runtime_name": node.runtime_name,
                "node_identity": identity,
                "sibling_count": actual,
                "sibling_count_field_present_in_yaml": !setting.is_null(),
                "runtime_audit_configuration": {
                    "sibling_blocks_per_leadership": actual,
                    "source": "generated user YAML; absent setting uses the documented default of zero",
                    "independently_verified_in_running_process": false,
                },
                "user_config_path": config_path,
                "user_config_sha256": sha256_file(&config_path)?,
                "executable_path": binary,
                "executable_sha256": binary_sha256,
            }),
        );
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository_root = manifest_dir.parent().unwrap_or(manifest_dir);
    let source_files = security_audit_source_files(repository_root)?;

    Ok(json!({
        "commit_sha": commit,
        "binary_path": binary,
        "binary_sha256": binary_sha256,
        "binary_resolution": "test framework binary provider",
        "binary_selection_mode": binary_selection_mode,
        "configured_binary_override": configured_binary,
        "node_binary_profile": profile.to_string(),
        "feature_set": Value::Null,
        "feature_set_note": "Legacy field is null: executable features are not independently verified.",
        "requested_cucumber_features": Value::Null,
        "requested_cucumber_features_note": "Cargo invocation flags are not available to the running test.",
        "compiled_test_features_known_enabled": ["cucumber", "security-audit"],
        "expected_node_build_features": expected_node_build_features(&profile),
        "expected_node_build_features_note": "Expected for a framework auto-build; selected executable features have not been independently verified.",
        "binary_features_independently_verified": false,
        "observed_audit_functionality": {
            "status": "not_observed_yet",
            "complete_producer_opportunities": 0,
        },
        "scenario": scenario,
        "source_files": source_files,
        "node_configs": nodes,
        "consensus_configuration": consensus_config,
    }))
}

fn binary_selection_mode(configured: Option<&Path>, selected: &Path) -> &'static str {
    match configured {
        Some(path) if path.canonicalize().ok().as_deref() == Some(selected) => {
            "environment_override_matches_selected_path"
        }
        Some(_) => "environment_override_configured_selection_unverified",
        None => "framework_provider_unspecified",
    }
}

fn expected_node_build_features(profile: &NodeBinaryProfile) -> Vec<&'static str> {
    let mut features = vec!["testing", "security-audit"];
    if *profile == NodeBinaryProfile::TokioConsole {
        features.push("tokio-console");
    }
    features
}

fn record_observed_audit_opportunity(provenance: &mut Value, count: usize) {
    provenance["observed_audit_functionality"] = json!({
        "status": "producer_opportunity_events_observed",
        "complete_producer_opportunities": count,
        "evidence_source": "correlated producer ordinary, sibling and timing log events",
        "executable_feature_set_independently_verified": false,
    });
}

impl AuditLogTail {
    fn after_readiness(world: &CucumberWorld) -> Result<Self, String> {
        let participants = audit_participants(world)?;
        let root = &world.lifecycle.scenario_base_dir;
        let mut runtime_names = BTreeMap::new();
        let mut paths = BTreeMap::new();
        let mut offsets = HashMap::new();
        for node_name in std::iter::once(&participants.producer).chain(&participants.honest_nodes) {
            let node = world
                .nodes_info
                .get(node_name)
                .ok_or_else(|| format!("node {node_name} was not started"))?;
            let runtime_prefix = format!("{}_", node.runtime_name);
            let log_prefix = format!("__logs-{}", node.runtime_name);
            runtime_names.insert(node_name.to_owned(), node.runtime_name.clone());
            let mut files = Vec::new();
            for entry in fs::read_dir(root).map_err(|error| {
                format!(
                    "cannot inspect scenario log root {}: {error}",
                    root.display()
                )
            })? {
                let entry = entry.map_err(|error| error.to_string())?;
                if !entry
                    .file_type()
                    .map_err(|error| error.to_string())?
                    .is_dir()
                    || !entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(&runtime_prefix)
                {
                    continue;
                }
                for log in fs::read_dir(entry.path()).map_err(|error| error.to_string())? {
                    let log = log.map_err(|error| error.to_string())?;
                    if !log.file_type().map_err(|error| error.to_string())?.is_dir()
                        && log.file_name().to_string_lossy().starts_with(&log_prefix)
                    {
                        files.push(log.path());
                    }
                }
            }
            files.sort();
            if files.is_empty() {
                return Err(format!(
                    "no live log file found for {node_name} ({})",
                    node.runtime_name
                ));
            }
            for path in &files {
                let len = fs::metadata(path).map_err(|error| error.to_string())?.len();
                offsets.insert(path.clone(), len);
            }
            paths.insert(node_name.to_owned(), files);
        }
        Ok(Self {
            root: root.clone(),
            participants: participants.clone(),
            runtime_names,
            paths,
            offsets,
        })
    }

    fn read_new_lines(&mut self, node_name: &str) -> Result<Vec<ObservedLogLine>, String> {
        let mut new_lines = Vec::new();
        let runtime_name = self
            .runtime_names
            .get(node_name)
            .ok_or_else(|| format!("no runtime name recorded for {node_name}"))?;
        let runtime_prefix = format!("{runtime_name}_");
        let log_prefix = format!("__logs-{runtime_name}");
        let mut discovered = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|error| {
            format!(
                "cannot refresh scenario log root {}: {error}",
                self.root.display()
            )
        })? {
            let entry = entry.map_err(|error| error.to_string())?;
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
                || !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&runtime_prefix)
            {
                continue;
            }
            for log in fs::read_dir(entry.path()).map_err(|error| error.to_string())? {
                let log = log.map_err(|error| error.to_string())?;
                if !log.file_type().map_err(|error| error.to_string())?.is_dir()
                    && log.file_name().to_string_lossy().starts_with(&log_prefix)
                {
                    discovered.push(log.path());
                }
            }
        }
        discovered.sort();
        let paths = self.paths.entry(node_name.to_owned()).or_default();
        for path in discovered {
            if !paths.contains(&path) {
                self.offsets.insert(path.clone(), 0);
                paths.push(path);
            }
        }
        paths.sort();
        let files = self
            .paths
            .get(node_name)
            .ok_or_else(|| format!("no log paths recorded for {node_name}"))?;
        for path in files {
            let mut file = match File::open(path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!(
                        "cannot read live node log {}: {error}",
                        path.display()
                    ));
                }
            };
            let mut offset = self.offsets.get(path).copied().unwrap_or(0);
            let len = file.metadata().map_err(|error| error.to_string())?.len();
            if len < offset {
                offset = 0;
            }
            file.seek(SeekFrom::Start(offset))
                .map_err(|error| error.to_string())?;
            let mut reader = BufReader::new(file);
            let mut bytes_read = 0u64;
            loop {
                let mut line = String::new();
                let bytes_in_line = reader
                    .read_line(&mut line)
                    .map_err(|error| error.to_string())?;
                if bytes_in_line == 0 {
                    break;
                }
                let line_offset = offset.saturating_add(bytes_read);
                bytes_read = bytes_read.saturating_add(
                    u64::try_from(bytes_in_line).map_err(|error| error.to_string())?,
                );
                new_lines.push(ObservedLogLine {
                    text: line.trim_end_matches(['\r', '\n']).to_owned(),
                    path: path.clone(),
                    offset: line_offset,
                });
            }
            self.offsets
                .insert(path.clone(), offset.saturating_add(bytes_read));
        }
        Ok(new_lines)
    }
}
