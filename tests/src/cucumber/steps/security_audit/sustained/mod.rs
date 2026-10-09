use std::time::Instant as StdInstant;

use cucumber::when;
use tokio::time::{Instant as TokioInstant, timeout_at};

mod active;

use super::*;

#[derive(Default, Debug)]
struct FinalizedSafety {
    conflict_observed: bool,
    ancestry_unverified: bool,
}

impl FinalizedSafety {
    fn record_pair_reports(&mut self, reports: &[FinalizedHistoryCheck]) {
        for report in reports {
            match report.relation {
                FinalizedHistoryRelation::Conflicting => self.conflict_observed = true,
                FinalizedHistoryRelation::Unverified => self.ancestry_unverified = true,
                FinalizedHistoryRelation::Shared | FinalizedHistoryRelation::Ancestral => {}
            }
        }
    }
}

const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct NodeTip {
    id: HeaderId,
    id_text: String,
    height: u64,
    slot: u64,
    observed_at_unix_ms: u64,
}

#[derive(Default)]
struct ActiveChainState {
    header_caches: BTreeMap<String, HeaderCache>,
    last_verified_libs: BTreeMap<String, (HeaderId, u64)>,
    last_tips: BTreeMap<String, NodeTip>,
    continuity_regressions: Vec<Value>,
    continuity_unverified: Vec<Value>,
    ancestry_gaps: Vec<Value>,
    tip_transitions: Vec<Value>,
    tip_reorganizations: Vec<Value>,
    tip_reorganizations_unverified: Vec<Value>,
    last_tip_progress: BTreeMap<String, u64>,
    last_lib_progress: BTreeMap<String, u64>,
    tip_stall_intervals: Vec<Value>,
    lib_stall_intervals: Vec<Value>,
    per_node_tip_advance_counts: BTreeMap<String, usize>,
    per_node_lib_advance_counts: BTreeMap<String, usize>,
    unique_tip_blocks: BTreeSet<String>,
}

impl ActiveChainState {
    #[expect(
        clippy::too_many_arguments,
        reason = "The sample updates owned node and safety evidence together"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "The sample preserves per-node progress, transitions, and ancestry evidence"
    )]
    async fn record_observations(
        &mut self,
        world: &CucumberWorld,
        observations: &[FinalityNodeObservation],
        observed_at_unix_ms: u64,
        active_started_at: u64,
        attack_candidates: &BTreeSet<HeaderId>,
        attack_boundary_present: bool,
        safety: &mut FinalizedSafety,
    ) -> (Vec<Value>, usize) {
        let mut unresponsive_count = 0;
        let mut node_snapshot_values = Vec::new();
        for observation in observations {
            let node_name = observation.node_name.as_str();
            if observation.consensus.is_some() {
                let new_tip = tip_from_observation(observation);
                let previous_tip = self.last_tips.get(node_name).cloned();
                if let Some(previous) = previous_tip
                    && previous.id != new_tip.id
                    && let Some(node) = world.nodes_info.get(node_name)
                {
                    let transition = observe_tip_transition(
                        node_name,
                        &node.client,
                        previous,
                        new_tip.clone(),
                        &mut self.header_caches,
                        attack_candidates,
                    )
                    .await;
                    if transition["status"].as_str() == Some("reorganization") {
                        if transition["last_common_ancestor"]["status"].as_str() == Some("verified")
                        {
                            self.tip_reorganizations.push(transition.clone());
                        } else {
                            self.tip_reorganizations_unverified.push(transition.clone());
                        }
                    } else if transition["status"].as_str() == Some("unverified") {
                        self.tip_reorganizations_unverified.push(transition.clone());
                    }
                    self.tip_transitions.push(transition);
                }
                if self
                    .last_tips
                    .get(node_name)
                    .is_none_or(|old| old.id != new_tip.id)
                {
                    let previous_at = self
                        .last_tip_progress
                        .insert(node_name.to_owned(), observed_at_unix_ms)
                        .unwrap_or(active_started_at);
                    if observed_at_unix_ms.saturating_sub(previous_at) > 0 {
                        self.tip_stall_intervals.push(json!({
                            "node": node_name,
                            "started_at_unix_ms": previous_at,
                            "ended_at_unix_ms": observed_at_unix_ms,
                            "duration_ms": observed_at_unix_ms.saturating_sub(previous_at),
                        }));
                    }
                    *self
                        .per_node_tip_advance_counts
                        .entry(node_name.to_owned())
                        .or_default() += 1;
                    self.unique_tip_blocks.insert(new_tip.id_text.clone());
                    self.last_tips.insert(node_name.to_owned(), new_tip);
                }
                if observation.continuity == LibContinuity::Verified {
                    let previous_at = self
                        .last_lib_progress
                        .insert(node_name.to_owned(), observed_at_unix_ms)
                        .unwrap_or(active_started_at);
                    if observed_at_unix_ms.saturating_sub(previous_at) > 0 {
                        self.lib_stall_intervals.push(json!({
                            "node": node_name,
                            "started_at_unix_ms": previous_at,
                            "ended_at_unix_ms": observed_at_unix_ms,
                            "duration_ms": observed_at_unix_ms.saturating_sub(previous_at),
                        }));
                    }
                    *self
                        .per_node_lib_advance_counts
                        .entry(node_name.to_owned())
                        .or_default() += 1;
                }
            } else {
                unresponsive_count += 1;
            }
            if observation.continuity == LibContinuity::RegressionOrUnrelated {
                self.continuity_regressions
                    .push(observation.snapshot.clone());
            } else if observation.continuity == LibContinuity::Unverified {
                self.continuity_unverified
                    .push(observation.snapshot.clone());
            }
            if !active_ancestry_complete(observation, attack_boundary_present) {
                safety.ancestry_unverified = true;
                let mut evidence = observation.snapshot.clone();
                evidence["ancestry_classification"] = json!(if observation
                    .consensus
                    .as_ref()
                    .is_some_and(|state| state.tip_descends_from_lib == Some(false))
                {
                    "contradicted"
                } else {
                    "unverified"
                });
                self.ancestry_gaps.push(evidence);
            }
            node_snapshot_values.push(observation.snapshot.clone());
        }
        (node_snapshot_values, unresponsive_count)
    }
}

fn active_ancestry_complete(
    observation: &FinalityNodeObservation,
    attack_boundary_present: bool,
) -> bool {
    required_active_ancestry_complete(
        observation.tip_path_complete,
        observation.tip_descends_from_lib(),
        observation.finalized_path_complete,
        attack_boundary_present,
    )
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "Each flag represents an independent required ancestry observation"
)]
const fn required_active_ancestry_complete(
    tip_path: bool,
    tip_descends_lib: bool,
    finalized_path: bool,
    attack_boundary_present: bool,
) -> bool {
    tip_path && tip_descends_lib && (!attack_boundary_present || finalized_path)
}

#[derive(Clone, Copy)]
struct WorkloadCriteria {
    budget: usize,
    initial_pending: usize,
    interval: Duration,
    readiness_timeout: Duration,
    max_tip_height_skew: u64,
}

struct ActiveEnvironmentRequest<'a> {
    scenario: &'a str,
    expected_siblings: usize,
    artifact_path: &'a Path,
    workload: WorkloadCriteria,
}

struct PreparedActiveEnvironment {
    provenance: Value,
    configuration: Value,
    readiness: Value,
    log_tail: AuditLogTail,
}

#[derive(Clone, Copy)]
struct ExposureCriteria {
    min_opportunities: usize,
    min_nonempty: usize,
    min_delivered: usize,
    cooldown_blocks: u64,
    min_duration: Duration,
    timeout: Duration,
}

#[derive(Debug, serde::Serialize)]
struct ExposureObservation {
    observed_at_unix_ms: u64,
    complete_genuine_opportunities: usize,
    nonempty_submitted_opportunities: usize,
    delivered_opportunities: usize,
    requirements_met: bool,
}

const fn exposure_requirements_met(
    complete: usize,
    nonempty: usize,
    delivered: usize,
    criteria: ExposureCriteria,
) -> bool {
    complete >= criteria.min_opportunities
        && nonempty >= criteria.min_nonempty
        && delivered >= criteria.min_delivered
}

fn sustained_requirements_met(
    criteria: ExposureCriteria,
    complete: usize,
    nonempty: usize,
    submitted: usize,
    delivered: usize,
    cooldown_blocks: u64,
    duration: Duration,
) -> bool {
    exposure_requirements_met(complete, submitted, delivered, criteria)
        && nonempty >= criteria.min_nonempty
        && cooldown_blocks >= criteria.cooldown_blocks
        && duration >= criteria.min_duration
}

#[cfg(test)]
mod exposure_tests {
    use super::*;

    #[test]
    fn exposure_cooldown_and_duration_are_all_required() {
        let criteria = ExposureCriteria {
            min_opportunities: 6,
            min_nonempty: 3,
            min_delivered: 2,
            cooldown_blocks: 10,
            min_duration: Duration::from_secs(300),
            timeout: Duration::from_secs(3600),
        };
        assert!(sustained_requirements_met(
            criteria,
            6,
            3,
            3,
            2,
            10,
            Duration::from_secs(300)
        ));
        assert!(!sustained_requirements_met(
            criteria,
            5,
            3,
            3,
            2,
            10,
            Duration::from_secs(300)
        ));
        assert!(!sustained_requirements_met(
            criteria,
            6,
            3,
            2,
            2,
            10,
            Duration::from_secs(300)
        ));
        assert!(!sustained_requirements_met(
            criteria,
            6,
            3,
            3,
            1,
            10,
            Duration::from_secs(300)
        ));
        assert!(!sustained_requirements_met(
            criteria,
            6,
            3,
            3,
            2,
            9,
            Duration::from_secs(300)
        ));
        assert!(!sustained_requirements_met(
            criteria,
            6,
            3,
            3,
            2,
            10,
            Duration::from_secs(299)
        ));
    }
}

#[derive(Default)]
struct ActiveLogObservations {
    nodes: BTreeMap<String, Vec<ObservedLogLine>>,
    opportunities: BTreeMap<String, ProducerOpportunity>,
    attack_candidates: BTreeSet<HeaderId>,
    read_failures: Vec<Value>,
}

impl ActiveLogObservations {
    fn read_nodes(
        &mut self,
        tail: &mut AuditLogTail,
        participants: &SecurityAuditParticipants,
        final_read: bool,
    ) -> usize {
        let mut failures = 0;
        for name in node_names(participants) {
            match tail.read_new_lines(name) {
                Ok(lines) => {
                    self.nodes.entry(name.to_owned()).or_default().extend(
                        lines.into_iter().filter(|line| {
                            line.text.contains("security_audit_")
                                || line
                                    .text
                                    .contains("logos_blockchain::chain::leader: proposed block ")
                        }),
                    );
                }
                Err(error) => {
                    failures += 1;
                    let mut record =
                        json!({"node": name, "observed_at_unix_ms": unix_now_ms(), "error": error});
                    if final_read {
                        record["final_read"] = json!(true);
                    }
                    self.read_failures.push(record);
                }
            }
        }
        failures
    }

    fn update_opportunities(&mut self, producer: &str) -> (Option<AttackEvidence>, usize) {
        let lines = self
            .nodes
            .get(producer)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for opportunity in find_producer_opportunities(lines) {
            if let Some(id) = opportunity.ordinary.get("ordinary_block_id") {
                self.opportunities.entry(id.clone()).or_insert(opportunity);
            }
        }
        let mut failures = 0;
        for opportunity in self.opportunities.values() {
            match attack_evidence_from_log(opportunity) {
                Ok(evidence) => {
                    self.attack_candidates.insert(evidence.ordinary_id);
                    self.attack_candidates
                        .extend(evidence.siblings.iter().map(|(_, sibling)| sibling.id));
                }
                Err(_) => failures += 1,
            }
        }
        let latest = self
            .opportunities
            .values()
            .max_by_key(|opportunity| opportunity.ordinary_line)
            .and_then(|opportunity| attack_evidence_from_log(opportunity).ok());
        (latest, failures)
    }
}

fn criteria_table(step: &Step) -> Result<BTreeMap<String, u64>, StepError> {
    let table = step.table.as_ref().ok_or(StepError::MissingTable)?;
    parse_criteria_rows(&table.rows)
}

fn parse_criteria_rows(rows: &[Vec<String>]) -> Result<BTreeMap<String, u64>, StepError> {
    if rows.first().map(Vec::as_slice) != Some(&["criterion".to_owned(), "value".to_owned()]) {
        return Err(StepError::InvalidArgument {
            message: "criteria table requires criterion and value columns".to_owned(),
        });
    }
    let mut values = BTreeMap::new();
    for row in rows.iter().skip(1) {
        let [key, value] = row.as_slice() else {
            return Err(StepError::InvalidArgument {
                message: "each criterion needs a name and value".to_owned(),
            });
        };
        let parsed: u64 = value.parse().map_err(|_| StepError::InvalidArgument {
            message: format!("invalid value for criterion {key}: {value}"),
        })?;
        if values.insert(key.clone(), parsed).is_some() {
            return Err(StepError::InvalidArgument {
                message: format!("criterion {key} must be unique"),
            });
        }
    }
    Ok(values)
}

fn require_criteria(values: &BTreeMap<String, u64>, required: &[&str]) -> StepResult {
    if values.len() != required.len()
        || values.keys().any(|name| !required.contains(&name.as_str()))
    {
        return Err(StepError::InvalidArgument {
            message: format!("criteria must be exactly: {}", required.join(", ")),
        });
    }
    Ok(())
}

fn criterion<T: TryFrom<u64>>(values: &BTreeMap<String, u64>, name: &str) -> Result<T, StepError> {
    values
        .get(name)
        .copied()
        .and_then(|value| T::try_from(value).ok())
        .ok_or_else(|| StepError::InvalidArgument {
            message: format!("missing or invalid criterion {name}"),
        })
}

fn positive_criterion<T: TryFrom<u64>>(
    values: &BTreeMap<String, u64>,
    name: &str,
) -> Result<T, StepError> {
    if values.get(name) == Some(&0) {
        return Err(StepError::InvalidArgument {
            message: format!("criterion {name} must be positive"),
        });
    }
    criterion(values, name)
}

fn workload_criteria(
    world: &CucumberWorld,
    values: &BTreeMap<String, u64>,
) -> Result<WorkloadCriteria, StepError> {
    let workload =
        audit_workload(world).map_err(|message| StepError::InvalidArgument { message })?;
    let SecurityAuditWorkloadPolicy::Sustained {
        budget,
        initial_pending,
        interval_seconds,
    } = workload.policy
    else {
        return Err(StepError::InvalidArgument {
            message: "sustained attack requires a sustained workload".to_owned(),
        });
    };
    Ok(WorkloadCriteria {
        budget,
        initial_pending,
        interval: Duration::from_secs(interval_seconds),
        readiness_timeout: Duration::from_secs(positive_criterion(values, "readiness_seconds")?),
        max_tip_height_skew: criterion(values, "maximum_tip_height_skew")?,
    })
}

async fn prepare_active_environment(
    world: &mut CucumberWorld,
    request: ActiveEnvironmentRequest<'_>,
) -> Result<PreparedActiveEnvironment, StepError> {
    let ActiveEnvironmentRequest {
        scenario,
        expected_siblings,
        artifact_path,
        workload,
    } = request;
    world.reset_wallet_scanner_after_current_iteration().await;
    let provenance = match verify_runtime_configuration_with_siblings(
        world,
        scenario,
        expected_siblings,
    )
    .await
    {
        Ok(provenance) => provenance,
        Err(error) => {
            write_json(
                artifact_path,
                &json!({
                    "status": "workload_or_leadership_setup_failure",
                    "failure_classification": "effective_runtime_configuration_invalid",
                    "failure": error,
                    "scenario": scenario,
                    "expected_producer_siblings": expected_siblings,
                    "observed_at_unix_ms": unix_now_ms(),
                }),
            )?;
            return Err(StepError::StepFail {
                message: format!("effective runtime configuration is invalid: {error}"),
            });
        }
    };
    let configuration = consensus_configuration(world);
    if configuration["status"].as_str() != Some("read") {
        write_json(
            artifact_path,
            &json!({
                "status": "workload_or_leadership_setup_failure",
            "failure_classification": "consensus_configuration_unverified",
                "consensus_configuration": configuration,
            }),
        )?;
        return Err(StepError::StepFail {
            message: "node deployment configuration is unverified".to_owned(),
        });
    }
    let (readiness, log_tail) = match wait_for_blend_readiness(world, workload.readiness_timeout)
        .await
    {
        Ok(readiness) => readiness,
        Err(error) => {
            write_json(
                artifact_path,
                &json!({
                    "status": "workload_or_leadership_setup_failure",
                    "failure_classification": "blend_readiness_failed",
                    "failure": error,
                    "scenario": scenario,
                    "provenance": provenance,
                    "observed_at_unix_ms": unix_now_ms(),
                }),
            )?;
            return Err(StepError::StepFail {
                message: format!("Blend readiness failed before sustained observation: {error}"),
            });
        }
    };
    Ok(PreparedActiveEnvironment {
        provenance,
        configuration,
        readiness,
        log_tail,
    })
}

struct OpportunitySummary {
    records: Vec<Value>,
    indexed: BTreeMap<String, Value>,
    nonempty: usize,
    nonempty_submitted: usize,
    delivered: BTreeSet<String>,
}

struct OpportunitySummaryContext<'a> {
    world: &'a CucumberWorld,
    participants: &'a SecurityAuditParticipants,
    opportunities: &'a BTreeMap<String, ProducerOpportunity>,
    node_logs: &'a BTreeMap<String, Vec<ObservedLogLine>>,
    observed_ordinary_bodies: &'a BTreeMap<String, Value>,
    submitted_transactions: &'a [Value],
    last_tips: &'a BTreeMap<String, NodeTip>,
    last_verified_libs: &'a BTreeMap<String, (HeaderId, u64)>,
    header_caches: &'a mut BTreeMap<String, HeaderCache>,
    attack_branch_candidates: &'a mut BTreeSet<HeaderId>,
}

async fn summarize_opportunities(context: OpportunitySummaryContext<'_>) -> OpportunitySummary {
    let OpportunitySummaryContext {
        world,
        participants,
        opportunities,
        node_logs,
        observed_ordinary_bodies,
        submitted_transactions,
        last_tips,
        last_verified_libs,
        header_caches,
        attack_branch_candidates,
    } = context;
    let mut opportunity_records = Vec::new();
    let mut attack_opportunity_records = BTreeMap::<String, Value>::new();
    let mut nonempty_opportunities = 0usize;
    let mut nonempty_submitted_body_opportunities = 0usize;
    let mut delivered_opportunities = BTreeSet::new();
    let submitted_hashes = submitted_transactions
        .iter()
        .filter_map(|transaction| transaction["transaction_hash"].as_str().map(str::to_owned))
        .collect::<BTreeSet<_>>();
    for (ordinary_id, opportunity) in opportunities {
        let (mut record, mut evidence) =
            summarize_attack_opportunity(world, participants, opportunity, node_logs).await;
        if record["ordinary_body"]["available"].as_bool() != Some(true)
            && let Some(body) = observed_ordinary_bodies.get(ordinary_id)
        {
            record["ordinary_body"] = body.clone();
            record["ordinary_body"]["retrieved_during_active_window"] = json!(true);
            evidence.body_root = body["body_root"].as_str().unwrap_or_default().to_owned();
            evidence.transaction_hashes = body["transaction_hashes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            record["attack_evidence"]["body_root"] = json!(evidence.body_root);
            record["attack_evidence"]["ordered_transaction_hashes"] =
                json!(evidence.transaction_hashes);
            record["body_integrity"] = compare_sibling_bodies(
                &record["ordinary_body"],
                record["sibling_blocks"]
                    .as_array()
                    .map_or(&[], Vec::as_slice),
            );
        }
        record["canonical_branch_by_node_at_active_window_end"] = canonical_membership_by_node(
            world,
            participants,
            &evidence,
            last_tips,
            last_verified_libs,
            header_caches,
        )
        .await;
        let body_hashes = record["ordinary_body"]["transaction_hashes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let contains_submitted_transaction = body_hashes
            .iter()
            .any(|hash| submitted_hashes.contains(hash));
        record["body_integrity"]["contains_submitted_transaction"] =
            json!(contains_submitted_transaction);
        if record["ordinary_body"]["transaction_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
        {
            nonempty_opportunities += 1;
            if contains_submitted_transaction {
                nonempty_submitted_body_opportunities += 1;
            }
        }
        if record["ordered_acceptance_by_distinct_peer"].as_bool() == Some(true) {
            delivered_opportunities.insert(ordinary_id.clone());
        }
        attack_branch_candidates.insert(evidence.ordinary_id);
        for (_, sibling) in &evidence.siblings {
            attack_branch_candidates.insert(sibling.id);
        }
        opportunity_records.push(record.clone());
        attack_opportunity_records.insert(ordinary_id.clone(), record);
    }
    OpportunitySummary {
        records: opportunity_records,
        indexed: attack_opportunity_records,
        nonempty: nonempty_opportunities,
        nonempty_submitted: nonempty_submitted_body_opportunities,
        delivered: delivered_opportunities,
    }
}

fn node_names(participants: &SecurityAuditParticipants) -> Vec<&str> {
    std::iter::once(participants.producer.as_str())
        .chain(participants.honest_nodes.iter().map(String::as_str))
        .collect()
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

async fn observe_active_exposure(
    world: &CucumberWorld,
    participants: &SecurityAuditParticipants,
    opportunities: &BTreeMap<String, ProducerOpportunity>,
    node_logs: &BTreeMap<String, Vec<ObservedLogLine>>,
    submitted: &[Value],
    observed_bodies: &mut BTreeMap<String, Value>,
    criteria: ExposureCriteria,
) -> ExposureObservation {
    let submitted_hashes = submitted
        .iter()
        .filter_map(|transaction| transaction["transaction_hash"].as_str())
        .collect::<BTreeSet<_>>();
    let mut complete = 0;
    let mut nonempty_submitted = 0;
    let mut delivered = 0;
    for (ordinary_id, opportunity) in opportunities {
        if attack_evidence_from_log(opportunity).is_err() {
            continue;
        }
        complete += 1;
        if !observed_bodies.contains_key(ordinary_id) {
            let mut body = fetch_body_evidence(
                world,
                &participants.producer,
                opportunity.ordinary.get("ordinary_block_id"),
            )
            .await;
            if body["available"].as_bool() == Some(true) {
                body["observed_at_unix_ms"] = json!(unix_now_ms());
                observed_bodies.insert(ordinary_id.clone(), body);
            }
        }
        let body = observed_bodies.get(ordinary_id).unwrap_or(&Value::Null);
        if body["transaction_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
            && body["transaction_hashes"].as_array().is_some_and(|hashes| {
                hashes
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|hash| submitted_hashes.contains(hash))
            })
        {
            nonempty_submitted += 1;
        }
        let siblings = opportunity
            .siblings
            .iter()
            .filter_map(|(_, sibling)| sibling.get("sibling_block_id").cloned())
            .collect::<BTreeSet<_>>();
        let honest_nodes = participants
            .honest_nodes
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let stages = receiver_stages(node_logs, &siblings, &opportunity.ordinary, &honest_nodes);
        if stages.iter().any(|(identity, stage)| {
            honest_nodes.contains(&identity.node.as_str())
                && stage.has_ordered_blend_to_acceptance()
        }) {
            delivered += 1;
        }
    }
    ExposureObservation {
        observed_at_unix_ms: unix_now_ms(),
        complete_genuine_opportunities: complete,
        nonempty_submitted_opportunities: nonempty_submitted,
        delivered_opportunities: delivered,
        requirements_met: exposure_requirements_met(
            complete,
            nonempty_submitted,
            delivered,
            criteria,
        ),
    }
}

fn write_json(path: &Path, value: &Value) -> StepResult {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).map_err(|error| StepError::StepFail {
            message: format!("cannot encode {}: {error}", path.display()),
        })?,
    )
    .map_err(|error| StepError::StepFail {
        message: format!("cannot write {}: {error}", path.display()),
    })
}

async fn submit_workload_transaction(
    world: &mut CucumberWorld,
    index: usize,
    submitted: &mut Vec<Value>,
    preparation_failures: &mut Vec<Value>,
    submission_failures: &mut Vec<Value>,
) -> Result<(), ()> {
    let producer = audit_participants(world)
        .map_err(|error| {
            preparation_failures.push(json!({"error": error}));
        })?
        .producer
        .clone();
    let workload = audit_workload(world).cloned().map_err(|error| {
        preparation_failures.push(json!({"error": error}));
    })?;
    let alias = format!("SECURITY_AUDIT_SUSTAINED_{index:03}");
    if let Err(error) = prepare_transfer_transaction(
        world,
        "sustained security audit workload",
        alias.clone(),
        workload.amount,
        workload.sender.clone(),
        workload.receiver.clone(),
    )
    .await
    {
        preparation_failures.push(json!({
            "alias": alias,
            "attempted_at_unix_ms": unix_now_ms(),
            "error": error.to_string(),
        }));
        return Err(());
    }
    let signed = match world.resolve_prepared_transaction(&alias) {
        Ok(signed) => signed,
        Err(error) => {
            preparation_failures.push(json!({
                "alias": alias,
                "attempted_at_unix_ms": unix_now_ms(),
                "error": error.to_string(),
            }));
            return Err(());
        }
    };
    let transaction_hash = tx_hash_to_hex(&signed.hash());
    let target_nodes = vec![producer];
    match submit_prepared_transaction_to_nodes(
        world,
        "sustained security audit workload",
        alias.clone(),
        target_nodes.clone(),
    )
    .await
    {
        Ok(()) => {
            submitted.push(json!({
                "alias": alias,
                "transaction_hash": transaction_hash,
                "submitted_at_unix_ms": unix_now_ms(),
                "amount": workload.amount,
                "target_nodes": target_nodes,
            }));
            Ok(())
        }
        Err(error) => {
            submission_failures.push(json!({
                "alias": alias,
                "transaction_hash": transaction_hash,
                "attempted_at_unix_ms": unix_now_ms(),
                "error": error.to_string(),
                "submission_to_all_nodes_confirmed": false,
            }));
            Err(())
        }
    }
}

fn attack_evidence_from_log(opportunity: &ProducerOpportunity) -> Result<AttackEvidence, String> {
    let ordinary_id_text = opportunity
        .ordinary
        .get("ordinary_block_id")
        .ok_or_else(|| "ordinary block ID is missing".to_owned())?
        .clone();
    let parent_id_text = opportunity
        .ordinary
        .get("parent_block_id")
        .ok_or_else(|| "ordinary parent block ID is missing".to_owned())?
        .clone();
    let slot = opportunity
        .ordinary
        .get("slot")
        .ok_or_else(|| "ordinary slot is missing".to_owned())?
        .parse::<u64>()
        .map_err(|error| format!("ordinary slot is invalid: {error}"))?;
    let siblings = opportunity
        .siblings
        .iter()
        .map(|(index, sibling)| {
            let id_text = sibling
                .get("sibling_block_id")
                .ok_or_else(|| format!("sibling {index} ID is missing"))?;
            Ok((
                *index,
                AttackCandidate {
                    id: parse_header_id(id_text)?,
                },
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(AttackEvidence {
        ordinary_id: parse_header_id(&ordinary_id_text)?,
        ordinary_id_text,
        parent_id: parse_header_id(&parent_id_text)?,
        parent_id_text,
        slot,
        body_root: String::new(),
        transaction_hashes: Vec::new(),
        siblings,
    })
}

async fn sample_all_nodes(
    world: &CucumberWorld,
    participants: &SecurityAuditParticipants,
    attack: Option<&AttackEvidence>,
    last_verified_libs: &mut BTreeMap<String, (HeaderId, u64)>,
    caches: &mut BTreeMap<String, HeaderCache>,
) -> Vec<FinalityNodeObservation> {
    let mut observations = Vec::with_capacity(participants.honest_nodes.len() + 1);
    for node_name in node_names(participants) {
        let Some(node) = world.nodes_info.get(node_name) else {
            observations.push(unresponsive_observation(node_name, "node was not started"));
            continue;
        };
        let Some(cache) = caches.get_mut(node_name) else {
            observations.push(unresponsive_observation(
                node_name,
                "header cache was not allocated",
            ));
            continue;
        };
        let previous_lib = last_verified_libs.get(node_name).copied();
        let mut observation =
            observe_sustained_node(node_name, &node.client, attack, cache, previous_lib).await;
        if let Some(state) = observation.consensus.as_ref() {
            if previous_lib.is_some_and(|(previous_id, _)| previous_id == state.lib_id) {
                observation.continuity = LibContinuity::Unchanged;
                observation.snapshot["lib_continuity"] = json!("unchanged");
            }
            if observation.continuity == LibContinuity::Verified || previous_lib.is_none() {
                last_verified_libs.insert(node_name.to_owned(), (state.lib_id, state.lib_slot));
                if previous_lib.is_none() {
                    observation.continuity = LibContinuity::Baseline;
                    observation.snapshot["lib_continuity"] = json!("baseline");
                }
            }
        }
        observations.push(observation);
    }
    observations
}

fn unresponsive_observation(node_name: &str, error: &str) -> FinalityNodeObservation {
    let observed_at_unix_ms = unix_now_ms();
    FinalityNodeObservation {
        node_name: node_name.to_owned(),
        observed_at_unix_ms,
        snapshot: json!({
            "node": node_name,
            "observed_at_unix_ms": observed_at_unix_ms,
            "responsive": false,
            "error": error,
        }),
        consensus: None,
        tip_path_complete: false,
        finalized_path_complete: false,
        attack_branch: AttackBranch::Unverified,
        finalized_attack_branch: AttackBranch::Unverified,
        continuity: LibContinuity::Unverified,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Builds one consistent ancestry and continuity observation for a node"
)]
async fn observe_sustained_node(
    node_name: &str,
    client: &NodeHttpClient,
    attack: Option<&AttackEvidence>,
    cache: &mut HeaderCache,
    previous_lib: Option<(HeaderId, u64)>,
) -> FinalityNodeObservation {
    let observed_at_unix_ms = unix_now_ms();
    let consensus = match client.consensus_info().await {
        Ok(consensus) => consensus,
        Err(error) => {
            return unresponsive_observation(node_name, &error.to_string());
        }
    };
    let info = &consensus.cryptarchia_info;
    let tip_id = info.tip;
    let lib_id = info.lib;
    let tip_height = info.height;
    let tip_slot = u64::from(info.slot);
    let lib_slot = u64::from(info.lib_slot);
    let tip_to_lib = walk_to_target(client, tip_id, lib_id, lib_slot, cache).await;
    let tip_descends_from_lib = ancestry_presence(&tip_to_lib, lib_id);
    let (tip_path, finalized_path, attack_branch, finalized_branch) = if let Some(attack) = attack {
        let tip_path = walk_to_target(client, tip_id, attack.parent_id, attack.slot, cache).await;
        let finalized_path = match proven_below_target_slot_path(lib_slot, attack.slot) {
            Some(path) => path,
            None => walk_to_target(client, lib_id, attack.parent_id, attack.slot, cache).await,
        };
        let tip_branch =
            classify_attack_branch(&tip_path.links, tip_path.complete_below_slot, attack);
        let finalized_branch = classify_attack_branch(
            &finalized_path.links,
            finalized_path.complete_below_slot,
            attack,
        );
        (tip_path, finalized_path, tip_branch, finalized_branch)
    } else {
        (
            AncestryPath {
                links: tip_to_lib.links.clone(),
                target_reached: tip_to_lib.target_reached,
                complete_below_slot: tip_to_lib.complete_below_slot,
                error: tip_to_lib.error.clone(),
            },
            AncestryPath {
                links: Vec::new(),
                target_reached: true,
                complete_below_slot: true,
                error: None,
            },
            AttackBranch::None,
            AttackBranch::None,
        )
    };
    let continuity = if let Some((previous_id, previous_slot)) = previous_lib {
        let path = if previous_id == lib_id {
            AncestryPath {
                links: Vec::new(),
                target_reached: true,
                complete_below_slot: true,
                error: None,
            }
        } else {
            walk_between_finalized_blocks(
                client,
                lib_id,
                lib_slot,
                previous_id,
                previous_slot,
                cache,
            )
            .await
        };
        if previous_id == lib_id {
            LibContinuity::Unchanged
        } else {
            match ancestry_presence(&path, previous_id) {
                Some(true) => LibContinuity::Verified,
                Some(false) => LibContinuity::RegressionOrUnrelated,
                None => LibContinuity::Unverified,
            }
        }
    } else {
        LibContinuity::Baseline
    };
    let consensus_state = ConsensusState {
        tip_id,
        tip_id_text: tip_id.encode_hex::<String>(),
        tip_height,
        tip_slot,
        lib_id,
        lib_id_text: lib_id.encode_hex::<String>(),
        lib_slot,
        tip_descends_from_lib,
    };
    let mut snapshot = json!({
        "node": node_name,
        "observed_at_unix_ms": observed_at_unix_ms,
        "responsive": true,
        "phase": format!("{:?}", consensus.phase),
        "tip_id": consensus_state.tip_id_text,
        "tip_height": tip_height,
        "tip_slot": tip_slot,
        "lib_id": consensus_state.lib_id_text,
        "lib_slot": lib_slot,
        "tip_descends_from_lib": tip_descends_from_lib,
        "tip_to_lib_ancestry": ancestry_path_json(&tip_to_lib),
        "lib_continuity": continuity,
        "lib_continuity_ancestry": previous_lib.map(|(id, slot)| json!({
            "previous_lib_id": id.encode_hex::<String>(),
            "previous_lib_slot": slot,
            "new_lib_id": consensus_state.lib_id_text,
            "new_lib_slot": lib_slot,
            "status": continuity,
        })),
        "header_lookup_failures": header_cache_failures_json(cache),
    });
    if let Some(attack) = attack {
        snapshot["attack_slot"] = json!(attack.slot);
        snapshot["canonical_attack_branch"] = attack_branch_json(&attack_branch, attack);
        snapshot["finalized_attack_branch"] = attack_branch_json(&finalized_branch, attack);
        snapshot["tip_path_to_attack_parent"] = json!(ancestry_path_json(&tip_path));
        snapshot["finalized_path_to_attack_parent"] = json!(ancestry_path_json(&finalized_path));
    }
    FinalityNodeObservation {
        node_name: node_name.to_owned(),
        observed_at_unix_ms,
        snapshot,
        consensus: Some(consensus_state),
        tip_path_complete: tip_path.complete_below_slot || tip_path.target_reached,
        finalized_path_complete: finalized_path.complete_below_slot
            || finalized_path.target_reached,
        attack_branch,
        finalized_attack_branch: finalized_branch,
        continuity,
    }
}

fn tip_from_observation(observation: &FinalityNodeObservation) -> NodeTip {
    let state = observation
        .consensus
        .as_ref()
        .expect("tip requested from a responsive observation");
    NodeTip {
        id: state.tip_id,
        id_text: state.tip_id_text.clone(),
        height: state.tip_height,
        slot: state.tip_slot,
        observed_at_unix_ms: observation.observed_at_unix_ms,
    }
}

fn honest_observations(
    observations: &[FinalityNodeObservation],
    participants: &SecurityAuditParticipants,
) -> Vec<FinalityNodeObservation> {
    participants
        .honest_nodes
        .iter()
        .filter_map(|name| {
            observations
                .iter()
                .find(|observation| observation.node_name == *name)
        })
        .map(|observation| FinalityNodeObservation {
            node_name: observation.node_name.clone(),
            observed_at_unix_ms: observation.observed_at_unix_ms,
            snapshot: observation.snapshot.clone(),
            consensus: observation.consensus.clone(),
            tip_path_complete: observation.tip_path_complete,
            finalized_path_complete: observation.finalized_path_complete,
            attack_branch: observation.attack_branch.clone(),
            finalized_attack_branch: observation.finalized_attack_branch.clone(),
            continuity: observation.continuity,
        })
        .collect()
}

async fn sample_mempools(
    world: &CucumberWorld,
    participants: &SecurityAuditParticipants,
    submitted: &[Value],
) -> Vec<Value> {
    let submitted_hashes = submitted
        .iter()
        .filter_map(|transaction| transaction["transaction_hash"].as_str().map(str::to_owned))
        .collect::<BTreeSet<_>>();
    let mut samples = Vec::with_capacity(participants.honest_nodes.len() + 1);
    for name in node_names(participants) {
        let Some(node) = world.nodes_info.get(name) else {
            samples
                .push(json!({"node": name, "available": false, "error": "node was not started"}));
            continue;
        };
        match node.client.test_mempool_view().await {
            Ok(hashes) => {
                let pending = hashes
                    .iter()
                    .map(tx_hash_to_hex)
                    .filter(|hash| submitted_hashes.contains(hash))
                    .collect::<Vec<_>>();
                samples.push(json!({
                    "node": name,
                    "available": true,
                    "pending_submitted_transaction_count": pending.len(),
                    "pending_submitted_transaction_hashes": pending,
                }));
            }
            Err(error) => samples.push(json!({
                "node": name,
                "available": false,
                "error": error.to_string(),
            })),
        }
    }
    samples
}

#[expect(
    clippy::too_many_lines,
    reason = "Preserves all construction, body, storage, and per-peer acceptance evidence"
)]
async fn summarize_attack_opportunity(
    world: &CucumberWorld,
    participants: &SecurityAuditParticipants,
    opportunity: &ProducerOpportunity,
    node_logs: &BTreeMap<String, Vec<ObservedLogLine>>,
) -> (Value, AttackEvidence) {
    let evidence = attack_evidence_from_log(opportunity).unwrap_or_else(|_| AttackEvidence {
        ordinary_id: HeaderId::from([0; 32]),
        ordinary_id_text: String::new(),
        parent_id: HeaderId::from([0; 32]),
        parent_id_text: String::new(),
        slot: 0,
        body_root: String::new(),
        transaction_hashes: Vec::new(),
        siblings: Vec::new(),
    });
    let ordinary_body = fetch_body_evidence(
        world,
        &participants.producer,
        opportunity.ordinary.get("ordinary_block_id"),
    )
    .await;
    let mut ordinary_storage_by_node = Vec::new();
    for node_name in node_names(participants) {
        ordinary_storage_by_node.push(json!({
            "node": node_name,
            "block_api": fetch_body_evidence(
                world,
                node_name,
                opportunity.ordinary.get("ordinary_block_id"),
            ).await,
        }));
    }
    let sibling_ids = opportunity
        .siblings
        .iter()
        .filter_map(|(_, sibling)| sibling.get("sibling_block_id").cloned())
        .collect::<BTreeSet<_>>();
    let stages = receiver_stages(
        node_logs,
        &sibling_ids,
        &opportunity.ordinary,
        &participants
            .honest_nodes
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    let accepted_bodies = query_accepted_blocks(world, opportunity, &stages).await;
    let mut sibling_records = Vec::new();
    let mut ordered_acceptance_nodes = BTreeSet::new();
    for (index, sibling) in &opportunity.siblings {
        let sibling_id = sibling.get("sibling_block_id").cloned().unwrap_or_default();
        let body = fetch_body_evidence(world, &participants.producer, Some(&sibling_id)).await;
        let mut block_storage_by_node = Vec::new();
        for node_name in node_names(participants) {
            block_storage_by_node.push(json!({
                "node": node_name,
                "block_api": fetch_body_evidence(world, node_name, Some(&sibling_id)).await,
            }));
        }
        let mut peer_stages = Vec::new();
        for node_name in participants.honest_nodes.iter().map(String::as_str) {
            let item = stages.get(&ReceiverIdentity {
                node: node_name.to_owned(),
                sibling_id: sibling_id.clone(),
            });
            if let Some(item) = item {
                if item.has_ordered_blend_to_acceptance() {
                    ordered_acceptance_nodes.insert(node_name.to_owned());
                }
                peer_stages.push(json!({
                    "node": node_name,
                    "ordered_blend_to_acceptance": item.has_ordered_blend_to_acceptance(),
                    "matching_events": item.matching_events.iter().map(|event| json!({
                        "stage": event.stage,
                        "parent_block_id": event.parent_block_id,
                        "slot": event.slot,
                        "timestamp": event.timestamp,
                        "reason": event.reason,
                        "log_path": event.path,
                        "log_offset": event.offset,
                    })).collect::<Vec<_>>(),
                    "mismatched_events": item.mismatched_events.iter().map(|event| json!({
                        "stage": event.stage,
                        "parent_block_id": event.parent_block_id,
                        "slot": event.slot,
                        "reason": event.reason,
                        "log_path": event.path,
                        "log_offset": event.offset,
                    })).collect::<Vec<_>>(),
                }));
            } else {
                peer_stages.push(json!({
                    "node": node_name,
                    "ordered_blend_to_acceptance": false,
                    "matching_events": [],
                    "mismatched_events": [],
                }));
            }
        }
        let enqueue_event = sibling.clone();
        let construction_failures =
            matching_construction_failures(node_logs, &participants.producer, &sibling_id);
        sibling_records.push(json!({
            "sibling_index": index,
            "sibling_block_id": sibling_id,
            "relay_enqueue_event": enqueue_event,
            "producer_block_api": body,
            "block_storage_by_node": block_storage_by_node,
            "honest_peer_validation": peer_stages,
            "construction_failures": construction_failures,
        }));
    }
    let ordinary_body_root = ordinary_body["body_root"].as_str().unwrap_or_default();
    let mut evidence = evidence;
    ordinary_body_root.clone_into(&mut evidence.body_root);
    evidence.transaction_hashes = ordinary_body["transaction_hashes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let body_integrity = compare_sibling_bodies(&ordinary_body, &sibling_records);
    let record = json!({
        "ordinary_block_id": opportunity.ordinary.get("ordinary_block_id"),
        "parent_block_id": opportunity.ordinary.get("parent_block_id"),
        "slot": opportunity.ordinary.get("slot"),
        "ordinary_submission": opportunity.ordinary,
        "ordinary_submitted_line": opportunity.ordinary_line,
        "ordinary_constructed": true,
        "ordinary_relay_enqueued": identity_bool(&opportunity.ordinary, "ordinary_relay_enqueued"),
        "ordinary_before_sibling_construction": true,
        "ordinary_body": ordinary_body,
        "sibling_construction_and_enqueue": opportunity.siblings.iter().map(|(line, event)| json!({
            "line_index": line,
            "sibling_index": event.get("sibling_index"),
            "sibling_block_id": event.get("sibling_block_id"),
            "relay_enqueued": identity_bool(event, "relay_enqueued"),
            "event": event,
        })).collect::<Vec<_>>(),
        "timing": opportunity.timing,
        "timing_line": opportunity.timing_line,
        "sibling_blocks": sibling_records,
        "ordinary_block_storage_by_node": ordinary_storage_by_node,
        "accepted_block_api_observations": accepted_bodies,
        "body_integrity": body_integrity,
        "ordered_acceptance_peer_count": ordered_acceptance_nodes.len(),
        "ordered_acceptance_peer_names": ordered_acceptance_nodes,
        "ordered_acceptance_by_distinct_peer": !ordered_acceptance_nodes.is_empty(),
        "attack_evidence": {
            "ordinary_block_id": evidence.ordinary_id_text,
            "parent_block_id": evidence.parent_id_text,
            "attack_slot": evidence.slot,
            "body_root": evidence.body_root,
            "ordered_transaction_hashes": evidence.transaction_hashes,
            "siblings": evidence.siblings.iter().map(|(index, sibling)| json!({
                "sibling_index": index,
                "sibling_block_id": sibling.id.encode_hex::<String>(),
            })).collect::<Vec<_>>(),
        },
    });
    (record, evidence)
}

fn compare_sibling_bodies(ordinary: &Value, siblings: &[Value]) -> Value {
    let ordinary_available = ordinary["available"].as_bool() == Some(true);
    let ordinary_root = ordinary_available
        .then(|| ordinary["body_root"].as_str())
        .flatten();
    let ordinary_hashes = ordinary_available
        .then(|| ordinary["transaction_hashes"].as_array())
        .flatten()
        .filter(|hashes| hashes.iter().all(Value::is_string));
    let mut retrieved = 0;
    let mut roots = (0, 0);
    let mut hashes = (0, 0);
    for sibling in siblings {
        let body = &sibling["producer_block_api"];
        if body["available"].as_bool() != Some(true) {
            continue;
        }
        retrieved += 1;
        if let (Some(expected), Some(actual)) = (ordinary_root, body["body_root"].as_str()) {
            roots.0 += 1;
            roots.1 += usize::from(expected == actual);
        }
        if let (Some(expected), Some(actual)) = (
            ordinary_hashes,
            body["transaction_hashes"]
                .as_array()
                .filter(|values| values.iter().all(Value::is_string)),
        ) {
            hashes.0 += 1;
            hashes.1 += usize::from(expected == actual);
        }
    }
    json!({
        "retrieved_sibling_bodies": retrieved,
        "sibling_records": siblings.len(),
        "body_root": comparison_outcome(roots.0, roots.1, siblings.len()),
        "ordered_transaction_hashes": comparison_outcome(hashes.0, hashes.1, siblings.len()),
    })
}

fn comparison_outcome(compared: usize, matched: usize, total: usize) -> Value {
    json!({
        "status": if matched < compared { "mismatched" } else if compared == 0 { "unverified" } else { "matched" },
        "comparisons": compared,
        "successful_comparisons": matched,
        "all_siblings_compared_and_matched": total > 0 && compared == total && matched == total,
    })
}

async fn canonical_membership_by_node(
    world: &CucumberWorld,
    participants: &SecurityAuditParticipants,
    attack: &AttackEvidence,
    tips: &BTreeMap<String, NodeTip>,
    libs: &BTreeMap<String, (HeaderId, u64)>,
    caches: &mut BTreeMap<String, HeaderCache>,
) -> Value {
    let mut memberships = Vec::new();
    for node_name in participants.honest_nodes.iter().map(String::as_str) {
        let Some(node) = world.nodes_info.get(node_name) else {
            memberships.push(
                json!({"node": node_name, "status": "unverified", "error": "node was not started"}),
            );
            continue;
        };
        let (Some(tip), Some((lib_id, lib_slot)), Some(cache)) = (
            tips.get(node_name),
            libs.get(node_name),
            caches.get_mut(node_name),
        ) else {
            memberships.push(json!({"node": node_name, "status": "unverified", "error": "tip, LIB, or cache baseline was missing"}));
            continue;
        };
        let canonical_path =
            walk_to_target(&node.client, tip.id, attack.parent_id, attack.slot, cache).await;
        let canonical = classify_attack_branch(
            &canonical_path.links,
            canonical_path.complete_below_slot,
            attack,
        );
        let finalized_path = if *lib_slot < attack.slot {
            proven_below_target_slot_path(*lib_slot, attack.slot)
                .expect("LIB below attack slot has an empty proven prefix")
        } else {
            walk_to_target(&node.client, *lib_id, attack.parent_id, attack.slot, cache).await
        };
        let finalized = classify_attack_branch(
            &finalized_path.links,
            finalized_path.complete_below_slot,
            attack,
        );
        memberships.push(json!({
            "node": node_name,
            "observed_at_unix_ms": tip.observed_at_unix_ms,
            "tip_id": tip.id_text,
            "tip_height": tip.height,
            "lib_id": lib_id.encode_hex::<String>(),
            "lib_slot": lib_slot,
            "canonical_attack_branch": attack_branch_json(&canonical, attack),
            "canonical_ancestry": ancestry_path_json(&canonical_path),
            "finalized_attack_branch": attack_branch_json(&finalized, attack),
            "finalized_ancestry": ancestry_path_json(&finalized_path),
        }));
    }
    Value::Array(memberships)
}

async fn fetch_body_evidence(
    world: &CucumberWorld,
    node_name: &str,
    block_id: Option<&String>,
) -> Value {
    let Some(block_id) = block_id else {
        return json!({"available": false, "error": "block ID was missing"});
    };
    let id = match parse_header_id(block_id) {
        Ok(id) => id,
        Err(error) => return json!({"available": false, "error": error}),
    };
    let Some(node) = world.nodes_info.get(node_name) else {
        return json!({"available": false, "error": format!("{node_name} was not started")});
    };
    match node.client.block(&id).await {
        Ok(Some(block)) => {
            let body = summarize_api_block(&block);
            json!({
                "available": true,
                "block_id": body.block_id,
                "parent_block_id": body.parent_block_id,
                "slot": body.slot,
                "body_root": body.body_root,
                "transaction_count": body.transaction_hashes.len(),
                "transaction_hashes": body.transaction_hashes,
                "evidence_source": format!("NodeHttpClient::block at {node_name}"),
            })
        }
        Ok(None) => json!({"available": false, "error": "block API returned no block"}),
        Err(error) => json!({"available": false, "error": error.to_string()}),
    }
}

fn matching_construction_failures(
    node_logs: &BTreeMap<String, Vec<ObservedLogLine>>,
    producer: &str,
    ordinary_id: &str,
) -> Vec<Value> {
    node_logs
        .get(producer)
        .into_iter()
        .flatten()
        .filter_map(|line| {
            let fields = parse_log_fields(&line.text);
            (fields.get("event").map(String::as_str)
                == Some("security_audit_sibling_construction_failed")
                && fields.get("ordinary_block_id").map(String::as_str) == Some(ordinary_id))
            .then(|| {
                json!({
                    "log_path": line.path,
                    "log_offset": line.offset,
                    "fields": fields,
                    "raw": line.text,
                })
            })
        })
        .collect()
}

async fn align_observations(
    world: &CucumberWorld,
    observations: &[FinalityNodeObservation],
    caches: &mut BTreeMap<String, HeaderCache>,
    names: &[&str],
    max_tip_height_skew: u64,
) -> Value {
    let states = names
        .iter()
        .filter_map(|name| {
            observations
                .iter()
                .find(|observation| observation.node_name == *name)
                .and_then(|observation| observation.consensus.as_ref())
                .map(|state| ((*name).to_owned(), state.clone()))
        })
        .collect::<Vec<_>>();
    if states.len() != names.len() {
        return json!({
            "aligned": false,
            "reason": "one or more compared nodes were unresponsive",
            "responsive_node_count": states.len(),
            "expected_node_count": names.len(),
        });
    }
    let min_height = states
        .iter()
        .map(|(_, state)| state.tip_height)
        .min()
        .unwrap_or_default();
    let max_height = states
        .iter()
        .map(|(_, state)| state.tip_height)
        .max()
        .unwrap_or_default();
    let height_skew = max_height.saturating_sub(min_height);
    if !tip_height_skew_allowed(height_skew, max_tip_height_skew) {
        return json!({
            "aligned": false,
            "reason": "tip heights exceed the configured tolerance",
            "common_height": min_height,
            "height_skew": height_skew,
            "max_allowed_height_skew": max_tip_height_skew,
        });
    }
    let mut ids = Vec::new();
    let mut errors = Vec::new();
    for (name, state) in &states {
        let (Some(node), Some(cache)) = (world.nodes_info.get(name), caches.get_mut(name)) else {
            errors.push(json!({"node": name, "error": "node or ancestry cache was missing"}));
            continue;
        };
        match block_id_at_height(
            &node.client,
            state.tip_id,
            state.tip_height,
            min_height,
            cache,
        )
        .await
        {
            Ok(id) => ids.push((name, id)),
            Err(error) => errors.push(json!({"node": name, "error": error})),
        }
    }
    if !errors.is_empty() || ids.len() != names.len() {
        return json!({
            "aligned": false,
            "reason": "canonical ancestry at the common height was unavailable",
            "common_height": min_height,
            "height_skew": height_skew,
            "block_ids_at_common_height": ids.iter().map(|(node, id)| json!({
                "node": node,
                "block_id": id.encode_hex::<String>(),
            })).collect::<Vec<_>>(),
            "errors": errors,
        });
    }
    let aligned = canonical_tips_aligned(
        height_skew,
        &ids.iter().map(|(_, id)| *id).collect::<Vec<_>>(),
        max_tip_height_skew,
    );
    json!({
        "aligned": aligned,
        "common_height": min_height,
        "height_skew": height_skew,
        "block_ids_at_common_height": ids.iter().map(|(node, id)| json!({
            "node": node,
            "block_id": id.encode_hex::<String>(),
        })).collect::<Vec<_>>(),
        "errors": errors,
    })
}

const fn tip_height_skew_allowed(actual: u64, maximum: u64) -> bool {
    actual <= maximum
}

#[expect(
    clippy::too_many_lines,
    reason = "Preserves transition and ancestry evidence together"
)]
async fn observe_tip_transition(
    node_name: &str,
    client: &NodeHttpClient,
    previous: NodeTip,
    current: NodeTip,
    caches: &mut BTreeMap<String, HeaderCache>,
    attack_candidates: &BTreeSet<HeaderId>,
) -> Value {
    let Some(cache) = caches.get_mut(node_name) else {
        return json!({
            "node": node_name,
            "status": "unverified",
            "previous_tip_id": previous.id_text,
            "previous_tip_height": previous.height,
            "new_tip_id": current.id_text,
            "new_tip_height": current.height,
            "error": "header cache was not allocated",
        });
    };
    let path = walk_to_target(client, current.id, previous.id, previous.slot, cache).await;
    let relation = ancestry_presence(&path, previous.id);
    let transition = classify_tip_transition(previous.id, current.id, relation);
    let status = match transition {
        TipTransition::Descendant => "descendant_of_previous_tip",
        TipTransition::Reorganization => "reorganization",
        TipTransition::Unverified => "unverified",
        TipTransition::Unchanged => "unchanged",
    };
    if matches!(
        transition,
        TipTransition::Unchanged | TipTransition::Descendant
    ) {
        return json!({
            "node": node_name,
            "status": status,
            "previous_observed_at_unix_ms": previous.observed_at_unix_ms,
            "observed_at_unix_ms": current.observed_at_unix_ms,
            "previous_tip_id": previous.id_text,
            "previous_tip_height": previous.height,
            "new_tip_id": current.id_text,
            "new_tip_height": current.height,
            "new_tip_descends_from_previous_tip": relation,
            "supporting_ancestry": ancestry_path_json(&path),
        });
    }
    let lca = find_last_common_ancestor(
        client,
        previous.clone(),
        current.clone(),
        cache,
        attack_candidates,
    )
    .await;
    let mut verified_transition = transition;
    let lca_evidence = match lca {
        Ok((ancestor, ancestor_height, old_branch, new_branch, old_attacks, new_attacks)) => {
            match reorganization_depths(previous.height, current.height, ancestor_height) {
                Some((rolled_back, new_count)) => {
                    verified_transition = if rolled_back == 0 {
                        TipTransition::Descendant
                    } else {
                        TipTransition::Reorganization
                    };
                    json!({
                    "status": "verified",
                    "block_id": ancestor.encode_hex::<String>(),
                    "height": ancestor_height,
                    "rolled_back_block_count": rolled_back,
                    "new_branch_block_count": new_count,
                    "old_branch_block_ids": old_branch.iter().map(hex::ToHex::encode_hex::<String>).collect::<Vec<_>>(),
                    "new_branch_block_ids": new_branch.iter().map(hex::ToHex::encode_hex::<String>).collect::<Vec<_>>(),
                    "attack_candidate_ids_on_previous_branch": old_attacks,
                    "attack_candidate_ids_on_new_branch": new_attacks,
                    })
                }
                None => json!({
                    "status": "unverified",
                    "error": "common-ancestor height exceeds one or both observed tip heights",
                }),
            }
        }
        Err(error) => json!({
            "status": "unverified",
            "error": error,
        }),
    };
    let status = match verified_transition {
        TipTransition::Descendant => "descendant_of_previous_tip",
        TipTransition::Reorganization => "reorganization",
        TipTransition::Unverified => "unverified",
        TipTransition::Unchanged => "unchanged",
    };
    json!({
        "node": node_name,
        "status": status,
        "previous_observed_at_unix_ms": previous.observed_at_unix_ms,
        "observed_at_unix_ms": current.observed_at_unix_ms,
        "previous_tip_id": previous.id_text,
        "previous_tip_height": previous.height,
        "previous_tip_slot": previous.slot,
        "new_tip_id": current.id_text,
        "new_tip_height": current.height,
        "new_tip_slot": current.slot,
        "new_tip_descends_from_previous_tip": match verified_transition {
            TipTransition::Descendant => Some(true),
            TipTransition::Reorganization => Some(false),
            TipTransition::Unverified | TipTransition::Unchanged => None,
        },
        "supporting_ancestry": ancestry_path_json(&path),
        "last_common_ancestor": lca_evidence,
        "reorganization_depth_is_lower_bound": true,
    })
}

async fn find_last_common_ancestor(
    client: &NodeHttpClient,
    previous: NodeTip,
    current: NodeTip,
    cache: &mut HeaderCache,
    attack_candidates: &BTreeSet<HeaderId>,
) -> Result<
    (
        HeaderId,
        u64,
        Vec<HeaderId>,
        Vec<HeaderId>,
        Vec<String>,
        Vec<String>,
    ),
    String,
> {
    let mut old_id = previous.id;
    let mut new_id = current.id;
    let mut old_height = previous.height;
    let mut new_height = current.height;
    let mut old_branch = Vec::new();
    let mut new_branch = Vec::new();
    for _ in 0..FINALITY_MAX_ANCESTRY_DEPTH {
        if old_id == new_id {
            if old_height != new_height {
                return Err("common block has inconsistent observed heights".to_owned());
            }
            let old_candidates = attack_candidate_members(&old_branch, old_id, attack_candidates);
            let new_candidates = attack_candidate_members(&new_branch, new_id, attack_candidates);
            return Ok((
                old_id,
                old_height,
                old_branch,
                new_branch,
                old_candidates,
                new_candidates,
            ));
        }
        if old_height >= new_height {
            if old_height == 0 {
                return Err(
                    "different genesis identities have no verified common ancestor".to_owned(),
                );
            }
            let link = fetch_header_link(client, old_id, cache).await?;
            if link.parent == old_id {
                return Err(format!(
                    "self-parented header {} reached before a common ancestor",
                    old_id.encode_hex::<String>()
                ));
            }
            old_branch.push(old_id);
            old_id = link.parent;
            old_height = old_height.saturating_sub(1);
        }
        if new_height >= old_height && old_id != new_id {
            if new_height == 0 {
                return Err(
                    "different genesis identities have no verified common ancestor".to_owned(),
                );
            }
            let link = fetch_header_link(client, new_id, cache).await?;
            if link.parent == new_id {
                return Err(format!(
                    "self-parented header {} reached before a common ancestor",
                    new_id.encode_hex::<String>()
                ));
            }
            new_branch.push(new_id);
            new_id = link.parent;
            new_height = new_height.saturating_sub(1);
        }
    }
    Err(format!(
        "common ancestor search exceeded {FINALITY_MAX_ANCESTRY_DEPTH} headers"
    ))
}

const fn reorganization_depths(
    previous_tip_height: u64,
    new_tip_height: u64,
    ancestor_height: u64,
) -> Option<(u64, u64)> {
    if ancestor_height > previous_tip_height || ancestor_height > new_tip_height {
        return None;
    }
    Some((
        previous_tip_height - ancestor_height,
        new_tip_height - ancestor_height,
    ))
}

fn attack_candidate_members(
    branch: &[HeaderId],
    ancestor: HeaderId,
    candidates: &BTreeSet<HeaderId>,
) -> Vec<String> {
    branch
        .iter()
        .copied()
        .chain(std::iter::once(ancestor))
        .filter(|id| candidates.contains(id))
        .map(|id| id.encode_hex::<String>())
        .collect()
}
