use super::*;

#[derive(Clone)]
pub(super) struct ReceiverEvent {
    pub(super) stage: &'static str,
    pub(super) parent_block_id: Option<String>,
    pub(super) slot: Option<String>,
    pub(super) timestamp: Option<[u32; 7]>,
    pub(super) reason: Option<String>,
    pub(super) path: PathBuf,
    pub(super) offset: u64,
}

pub(super) struct SelectedSiblingReception {
    pub(super) node: String,
    pub(super) sibling_id: String,
    pub(super) event: ReceiverEvent,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct ReceiverIdentity {
    pub(super) node: String,
    pub(super) sibling_id: String,
}

#[derive(Clone, Default)]
pub(super) struct ReceiverEvidence {
    pub(super) matching_events: Vec<ReceiverEvent>,
    pub(super) mismatched_events: Vec<ReceiverEvent>,
}

pub(super) struct HonestReceiverObservations {
    pub(super) evidence: HashMap<ReceiverIdentity, ReceiverEvidence>,
    pub(super) producer_had_no_subscriber_errors: bool,
}

impl ReceiverEvidence {
    pub(super) fn has_stage(&self, stage: &str) -> bool {
        self.matching_events
            .iter()
            .any(|event| event.stage == stage)
    }

    pub(super) fn has_ordered_blend_to_acceptance(&self) -> bool {
        self.matching_events
            .iter()
            .filter(|blend| blend.stage == "blend_received")
            .any(|blend| {
                self.matching_events
                    .iter()
                    .filter(|chain| chain.stage == "chain_proposal_received")
                    .filter(|chain| receiver_event_precedes(blend, chain))
                    .any(|chain| {
                        self.matching_events
                            .iter()
                            .filter(|reconstructed| reconstructed.stage == "reconstructed")
                            .filter(|reconstructed| receiver_event_precedes(chain, reconstructed))
                            .any(|reconstructed| {
                                self.matching_events.iter().any(|accepted| {
                                    accepted.stage == "accepted"
                                        && receiver_event_precedes(reconstructed, accepted)
                                })
                            })
                    })
            })
    }

    pub(super) fn has_terminal_stage(&self) -> bool {
        [
            "accepted",
            "rejected",
            "apply_failed",
            "not_processed",
            "reconstruction_failed",
        ]
        .iter()
        .any(|stage| self.has_stage(stage))
    }

    pub(super) fn stage_names(&self) -> Vec<&'static str> {
        self.ordered_events()
            .into_iter()
            .map(|event| event.stage)
            .collect()
    }

    pub(super) fn ordered_events(&self) -> Vec<&ReceiverEvent> {
        let mut events = self.matching_events.iter().collect::<Vec<_>>();
        events.sort_by(|first, second| match (first.timestamp, second.timestamp) {
            (Some(first_time), Some(second_time)) => match first_time.cmp(&second_time) {
                std::cmp::Ordering::Equal if first.path == second.path => {
                    first.offset.cmp(&second.offset)
                }
                ordering => ordering,
            },
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        events
    }
}

fn receiver_event_precedes(first: &ReceiverEvent, second: &ReceiverEvent) -> bool {
    let (Some(first_time), Some(second_time)) = (first.timestamp, second.timestamp) else {
        return false;
    };
    match first_time.cmp(&second_time) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Equal => first.path == second.path && first.offset < second.offset,
        std::cmp::Ordering::Greater => false,
    }
}
pub(super) async fn wait_for_selected_reception(
    state: &mut AuditScenarioState,
    opportunity: &ProducerOpportunity,
    honest_nodes: &[String],
    deadline: Instant,
) -> Result<SelectedSiblingReception, String> {
    loop {
        let mut error = None;
        for node in honest_nodes {
            match state.tails.read_new_lines(node) {
                Ok(lines) => state.logs.entry(node.clone()).or_default().extend(lines),
                Err(message) => {
                    error = Some(message);
                    break;
                }
            }
        }
        if let Some(error) = error {
            break Err(error);
        }
        if Instant::now() <= deadline
            && let Some(trigger) =
                selected_sibling_reception(&state.logs, opportunity, honest_nodes)
        {
            break Ok(trigger);
        }
        if Instant::now() >= deadline {
            break Err(
                "selected sibling Blend reception was not observed before the deadline".to_owned(),
            );
        }
        sleep(Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())))
            .await;
    }
}

pub(super) fn selected_sibling_reception(
    logs: &BTreeMap<String, Vec<ObservedLogLine>>,
    opportunity: &ProducerOpportunity,
    honest_nodes: &[String],
) -> Option<SelectedSiblingReception> {
    let ids = opportunity
        .siblings
        .iter()
        .filter_map(|(_, sibling)| sibling.get("sibling_block_id").cloned())
        .collect::<BTreeSet<_>>();
    let nodes = honest_nodes.iter().map(String::as_str).collect::<Vec<_>>();
    receiver_stages(logs, &ids, &opportunity.ordinary, &nodes)
        .into_iter()
        .flat_map(|(identity, evidence)| {
            evidence
                .matching_events
                .into_iter()
                .filter(|event| event.stage == "blend_received" && event.timestamp.is_some())
                .map(move |event| SelectedSiblingReception {
                    node: identity.node.clone(),
                    sibling_id: identity.sibling_id.clone(),
                    event,
                })
        })
        .min_by(|a, b| {
            a.event
                .timestamp
                .cmp(&b.event.timestamp)
                .then_with(|| a.node.cmp(&b.node))
                .then_with(|| a.event.offset.cmp(&b.event.offset))
        })
}
pub(super) async fn record_nonempty_sibling_propagation(
    world: &mut CucumberWorld,
    minimum_accepted: usize,
    timeout: Duration,
) -> StepResult {
    let artifact_dir = &world.lifecycle.scenario_base_dir;
    fs::create_dir_all(artifact_dir).map_err(|error| StepError::StepFail {
        message: format!("cannot create nonempty audit evidence directory: {error}"),
    })?;

    let mut state = world
        .security_audit_state
        .take()
        .ok_or_else(|| StepError::StepFail {
            message: "audit opportunity state unavailable".to_owned(),
        })?;
    let result =
        observe_nonempty_sibling_propagation(world, &mut state, timeout, minimum_accepted).await;
    world.security_audit_state = Some(state);
    let log_capture = original_node_logs(world);
    let mut report = match result {
        Ok(report) => report,
        Err(error) => {
            let mut details = error.details;
            details["status"] = json!("failed");
            details["failure"] = json!(error.message);
            details
        }
    };
    report["artifact_dir"] = json!(artifact_dir);
    match &log_capture {
        Ok(paths) => {
            report["node_logs"] = json!(paths);
            report["node_logs_kind"] = json!("original");
        }
        Err(error) => report["node_log_capture_error"] = json!(error),
    }
    let report_path = artifact_dir.join("nonempty-opportunity.json");
    fs::write(
        &report_path,
        serde_json::to_vec_pretty(&report).map_err(|error| StepError::StepFail {
            message: format!("cannot encode nonempty audit evidence: {error}"),
        })?,
    )
    .map_err(|error| StepError::StepFail {
        message: format!("cannot write {}: {error}", report_path.display()),
    })?;

    if let Err(error) = log_capture {
        return Err(StepError::StepFail {
            message: format!(
                "nonempty audit evidence was collected, but original node logs could not be located: {error}; evidence: {}",
                report_path.display()
            ),
        });
    }

    if report["nonempty_honest_peer_validation"].as_bool() == Some(true) {
        tracing::info!(
            target: TARGET,
            event = "security_audit_nonempty_scenario_evidence",
            evidence_path = %report_path.display(),
            "An honest node reconstructed and applied a same-slot sibling with the ordinary block transaction body"
        );
        Ok(())
    } else {
        let failure = report["failure"]
            .as_str()
            .unwrap_or("no honest node validated a sibling with the matching nonempty body");
        Err(StepError::StepFail {
            message: format!(
                "nonempty security-audit sibling assertion failed: {failure}; evidence: {}",
                report_path.display()
            ),
        })
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Collects all correlated producer and receiver evidence for one selected opportunity"
)]
pub(super) async fn observe_sibling_propagation(
    world: &CucumberWorld,
    state: &mut AuditScenarioState,
    minimum_accepted: usize,
    receiver_timeout: Duration,
) -> Result<Value, AuditFailure> {
    let participants = audit_participants(world).map_err(|message| AuditFailure {
        message,
        details: json!({}),
    })?;
    let opportunity = state.opportunity.as_ref().ok_or_else(|| AuditFailure {
        message: "no selected opportunity".to_owned(),
        details: json!({}),
    })?;
    if minimum_accepted > opportunity.siblings.len() {
        return Err(AuditFailure {
            message: "required accepted siblings exceeds selected sibling count".to_owned(),
            details: json!({}),
        });
    }
    let provenance = &state.provenance;
    let readiness = &state.readiness;
    let event_logger = BlendDiagnosticEventLogger::from_world(world);
    let receiver_observations = observe_honest_receivers(
        &mut state.tails,
        &mut state.logs,
        opportunity,
        receiver_timeout,
        minimum_accepted,
        (&event_logger, "security_audit_receiver_observed"),
    )
    .await
    .map_err(|message| AuditFailure {
        message,
        details: json!({"provenance": provenance, "blend_readiness": readiness}),
    })?;
    let receiver_stages = &receiver_observations.evidence;
    let no_subscriber_errors = receiver_observations.producer_had_no_subscriber_errors;
    let storage_events = query_accepted_blocks(world, opportunity, receiver_stages).await;

    let mut sibling_outcomes = Vec::new();
    let mut accepted_count = 0usize;
    for (_, identity) in &opportunity.siblings {
        let block_id = identity
            .get("sibling_block_id")
            .cloned()
            .unwrap_or_default();
        let mut receivers = Vec::new();
        let mut delivered = false;
        let mut reconstructed = false;
        let mut accepted = false;
        let mut rejected = false;
        let mut reconstruction_failed = false;
        let mut not_processed = false;
        let mut sibling_accepted_via_blend = false;

        for node in participants.honest_nodes.iter().map(String::as_str) {
            let evidence = receiver_stages
                .get(&ReceiverIdentity {
                    node: node.to_owned(),
                    sibling_id: block_id.clone(),
                })
                .cloned()
                .unwrap_or_default();
            let node_delivered = evidence.has_stage("blend_received");
            let node_reconstructed = evidence.has_stage("reconstructed");
            let node_accepted = evidence.has_stage("accepted");
            delivered |= node_delivered;
            reconstructed |= node_reconstructed;
            accepted |= node_accepted;
            rejected |= evidence.has_stage("rejected") || evidence.has_stage("apply_failed");
            reconstruction_failed |= evidence.has_stage("reconstruction_failed");
            not_processed |= evidence.has_stage("not_processed");
            let ordered_blend_to_acceptance = evidence.has_ordered_blend_to_acceptance();
            sibling_accepted_via_blend |= ordered_blend_to_acceptance;
            receivers.push(json!({
                "node": node,
                "node_identity": world.cluster.node_peer_ids.get(node).map(ToString::to_string),
                "blend_received": node_delivered,
                "chain_proposal_received": evidence.has_stage("chain_proposal_received"),
                "reconstructed": node_reconstructed,
                "reconstruction_failed": evidence.has_stage("reconstruction_failed"),
                "accepted": node_accepted,
                "rejected": evidence.has_stage("rejected"),
                "apply_failed": evidence.has_stage("apply_failed"),
                "not_processed": evidence.has_stage("not_processed"),
                "ordered_blend_to_acceptance": ordered_blend_to_acceptance,
                "storage_observed": storage_events.iter().any(|event| {
                    event["node"].as_str() == Some(node)
                        && event["block_id"].as_str() == Some(block_id.as_str())
                }),
                "canonical_status": "unknown",
                "observed_stages": evidence.stage_names(),
                "evidence_events": evidence.ordered_events().iter().map(|event| json!({
                    "stage": event.stage,
                    "timestamp": format_log_timestamp(event.timestamp),
                    "reason": event.reason,
                    "source_file": event.path,
                    "source_offset": event.offset,
                })).collect::<Vec<_>>(),
                "mismatched_identity_events": evidence.mismatched_events.iter().map(|event| json!({
                    "stage": event.stage,
                    "parent_block_id": event.parent_block_id,
                    "slot": event.slot,
                    "timestamp": format_log_timestamp(event.timestamp),
                    "reason": event.reason,
                    "source_file": event.path,
                    "source_offset": event.offset,
                })).collect::<Vec<_>>(),
            }));
        }
        accepted_count += usize::from(sibling_accepted_via_blend);

        let outcome = if sibling_accepted_via_blend {
            "accepted_after_blend_reception"
        } else if rejected {
            "reconstructed_or_received_but_rejected"
        } else if reconstruction_failed {
            "received_but_reconstruction_failed"
        } else if not_processed {
            "received_but_not_processed"
        } else if delivered && !reconstructed {
            "blend_received_reconstruction_not_observed"
        } else if no_subscriber_errors && !delivered {
            "delivery_not_observed_with_no_subscribers_error_in_producer_log"
        } else if identity_bool(identity, "relay_enqueued") {
            "local_enqueue_succeeded_peer_delivery_not_observed"
        } else {
            "local_relay_enqueue_failed"
        };

        sibling_outcomes.push(json!({
            "sibling_index": identity.get("sibling_index"),
            "ordinary_block_id": identity.get("ordinary_block_id"),
            "sibling_block_id": block_id,
            "parent_block_id": identity.get("parent_block_id"),
            "slot": identity.get("slot"),
            "relay_enqueued": identity_bool(identity, "relay_enqueued"),
            "outcome": outcome,
            "blend_received_by_any_honest_node": delivered,
            "reconstructed_by_any_honest_node": reconstructed,
            "accepted_by_any_honest_node": accepted,
            "receivers": receivers,
        }));
    }

    let accepted_via_blend = accepted_count >= minimum_accepted;
    let timing = &opportunity.timing;
    let summary = json!({
        "configured_siblings": parse_usize(timing, "configured_siblings"),
        "constructed_siblings": parse_usize(timing, "constructed_siblings"),
        "failed_siblings": parse_usize(timing, "failed_siblings"),
        "relay_enqueued_siblings": parse_usize(timing, "relay_enqueued_siblings"),
        "ordinary_self_applied": identity_bool(timing, "ordinary_self_applied"),
        "ordinary_relay_enqueued": identity_bool(timing, "ordinary_relay_enqueued"),
        "slot_to_ordinary_submission_ms": timing.get("slot_to_ordinary_submission_ms"),
        "ordinary_proposal_ms": timing.get("ordinary_proposal_ms"),
        "sibling_construction_ms": timing.get("sibling_construction_ms"),
        "sibling_blend_enqueue_ms": timing.get("sibling_blend_enqueue_ms"),
        "additional_processing_ms": timing.get("additional_processing_ms"),
        "total_leadership_processing_ms": timing.get("total_leadership_processing_ms"),
    });

    Ok(json!({
        "status": if accepted_via_blend { "passed" } else { "failed" },
        "failure": if accepted_via_blend { Value::Null } else {
            json!("no single honest node showed Blend reception, reconstruction, and successful consensus application for the same sibling")
        },
        "honest_peer_validation": accepted_via_blend,
        "cluster_nodes": participants.honest_nodes.len() + 1,
        "blend_provider_count": world.cluster.blend_core_nodes,
        "producer": participants.producer,
        "producer_runtime_name": world.nodes_info.get(&participants.producer).map(|node| node.runtime_name.as_str()),
        "blend_readiness": readiness,
        "provenance": provenance,
        "ordinary_block_id": opportunity.ordinary.get("ordinary_block_id"),
        "parent_block_id": opportunity.ordinary.get("parent_block_id"),
        "slot": opportunity.ordinary.get("slot"),
        "ordinary_submitted_log_line": opportunity.ordinary_line,
        "first_sibling_log_line": opportunity.siblings.first().map(|(line, _)| line),
        "timing_log_line": opportunity.timing_line,
        "ordinary_submission_preceded_sibling_construction": opportunity.ordinary_line
            < opportunity.siblings.first().map_or(usize::MAX, |(line, _)| *line),
        "timing": summary,
        "producer_no_peers_subscribed_to_topic_errors_after_readiness": no_subscriber_errors,
        "siblings": sibling_outcomes,
        "storage_observations": storage_events,
        "receiver_observability": {
            "blend_topic_subscription_api": "not exposed; matching decapsulated sibling event is direct per-payload reception evidence",
            "canonical_selection": "not asserted; ordinary acceptance may be non-canonical"
        },
    }))
}

#[expect(
    clippy::too_many_lines,
    reason = "Records per-sibling body and receiver evidence together"
)]
async fn observe_nonempty_sibling_propagation(
    world: &CucumberWorld,
    state: &mut AuditScenarioState,
    receiver_timeout: Duration,
    minimum_accepted: usize,
) -> Result<Value, AuditFailure> {
    let participants = audit_participants(world).map_err(|message| AuditFailure {
        message,
        details: json!({}),
    })?;
    let selection = state.nonempty.as_ref().ok_or_else(|| AuditFailure {
        message: "nonempty opportunity not selected".to_owned(),
        details: json!({}),
    })?;
    let opportunity = state.opportunity.as_ref().ok_or_else(|| AuditFailure {
        message: "opportunity not selected".to_owned(),
        details: json!({}),
    })?;
    let NonemptySelection {
        ordinary_body,
        pre_attack_consensus,
        submitted_hashes,
        submitted_transactions,
        transaction_observations,
        skipped_opportunities,
        examined_count,
        workload,
        selected_at_unix_ms,
        ..
    } = selection;
    let provenance = &state.provenance;
    let readiness = &state.readiness;
    let tails = &mut state.tails;
    let producer_log_map = &mut state.logs;
    let event_logger = BlendDiagnosticEventLogger::from_world(world);
    let receiver_observations = observe_honest_receivers(
        tails,
        producer_log_map,
        opportunity,
        receiver_timeout,
        minimum_accepted,
        (&event_logger, "security_audit_nonempty_receiver_observed"),
    )
    .await
    .map_err(|message| AuditFailure {
        message,
        details: json!({
            "provenance": provenance,
            "blend_readiness": readiness,
            "ordinary_block": ordinary_body,
            "siblings": opportunity.siblings.iter().map(|(_, event)| event).collect::<Vec<_>>(),
        }),
    })?;
    let receiver_stages = &receiver_observations.evidence;
    let no_subscriber_errors = receiver_observations.producer_had_no_subscriber_errors;
    let storage_events = query_accepted_blocks(world, opportunity, receiver_stages).await;
    let ordinary_hashes = &ordinary_body.transaction_hashes;
    let mut sibling_outcomes = Vec::new();
    let mut accepted_body_path = false;
    let mut accepted_bodies_all_match = true;
    let mut accepted_sibling_count = 0usize;
    let mut voucher_commitments = BTreeMap::<String, String>::new();
    let mut voucher_evidence_valid = true;
    for (_, identity) in &opportunity.siblings {
        let sibling_id = identity
            .get("sibling_block_id")
            .cloned()
            .unwrap_or_default();
        let mut receiver_reports = Vec::new();
        let mut accepted = false;
        let mut matching_ordered_path = false;
        for node in participants.honest_nodes.iter().map(String::as_str) {
            let evidence = receiver_stages
                .get(&ReceiverIdentity {
                    node: node.to_owned(),
                    sibling_id: sibling_id.clone(),
                })
                .cloned()
                .unwrap_or_default();
            let storage = storage_events.iter().find(|event| {
                event["node"].as_str() == Some(node)
                    && event["block_id"].as_str() == Some(sibling_id.as_str())
            });
            let has_stored_block =
                storage.and_then(|event| event["stored"].as_bool()) == Some(true);
            if has_stored_block {
                let voucher = storage.and_then(|event| event["voucher_commitment"].as_str());
                voucher_evidence_valid &= record_voucher_commitment(
                    &mut voucher_commitments,
                    &ordinary_body.voucher_commitment,
                    &sibling_id,
                    voucher,
                );
            }
            let root_matches = storage
                .and_then(|event| event["body_root"].as_str())
                .is_some_and(|root| root == ordinary_body.body_root);
            let stored_hashes = storage
                .and_then(|event| event["transaction_hashes"].as_array())
                .map(|hashes| {
                    hashes
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                });
            let hashes_match = stored_hashes
                .as_ref()
                .is_some_and(|hashes| hashes == ordinary_hashes);
            let body_matches = storage
                .and_then(|event| event["body_root"].as_str())
                .zip(stored_hashes.as_ref())
                .is_some_and(|(root, hashes)| same_transaction_body(ordinary_body, root, hashes));
            let ordered_path = evidence.has_ordered_blend_to_acceptance();
            accepted |= evidence.has_stage("accepted");
            if evidence.has_stage("accepted") && (!has_stored_block || !body_matches) {
                accepted_bodies_all_match = false;
            }
            accepted_body_path |= ordered_path && has_stored_block && body_matches;
            matching_ordered_path |= ordered_path && has_stored_block && body_matches;
            let reasons = evidence
                .ordered_events()
                .into_iter()
                .filter_map(|event| {
                    event.reason.as_ref().map(|reason| json!({
                    "stage": event.stage,
                    "reason": reason,
                    "transaction_references_unavailable": reason.contains("UnresolvedReference"),
                }))
                })
                .collect::<Vec<_>>();
            receiver_reports.push(json!({
                "node": node,
                "node_identity": world.cluster.node_peer_ids.get(node).map(ToString::to_string),
                "blend_received": evidence.has_stage("blend_received"),
                "chain_proposal_received": evidence.has_stage("chain_proposal_received"),
                "reconstructed": evidence.has_stage("reconstructed"),
                "reconstruction_failed": evidence.has_stage("reconstruction_failed"),
                "accepted": evidence.has_stage("accepted"),
                "rejected": evidence.has_stage("rejected"),
                "apply_failed": evidence.has_stage("apply_failed"),
                "not_processed": evidence.has_stage("not_processed"),
                "ordered_blend_to_acceptance": ordered_path,
                "explicit_reasons": reasons,
                "transaction_references_unavailable": reasons.iter().any(|reason| {
                    reason["transaction_references_unavailable"] == true
                }),
                "full_block_available": has_stored_block,
                "body_root_matches_ordinary": root_matches,
                "ordered_transaction_hashes_match_ordinary": hashes_match,
                "body_matches_ordinary": body_matches,
                "transaction_count": storage.and_then(|event| event["transaction_count"].as_u64()),
                "body_root": storage.and_then(|event| event["body_root"].as_str()),
                "voucher_commitment": storage.and_then(|event| event["voucher_commitment"].as_str()),
                "transaction_hashes": storage.and_then(|event| event["transaction_hashes"].as_array()),
                "storage_error": storage.and_then(|event| event["error"].as_str()),
                "observed_stages": evidence.stage_names(),
                "evidence_events": evidence.ordered_events().iter().map(|event| json!({
                    "stage": event.stage,
                    "timestamp": format_log_timestamp(event.timestamp),
                    "reason": event.reason,
                    "source_file": event.path,
                    "source_offset": event.offset,
                })).collect::<Vec<_>>(),
                "mismatched_identity_events": evidence.mismatched_events.iter().map(|event| json!({
                    "stage": event.stage,
                    "parent_block_id": event.parent_block_id,
                    "slot": event.slot,
                    "timestamp": format_log_timestamp(event.timestamp),
                    "reason": event.reason,
                    "source_file": event.path,
                    "source_offset": event.offset,
                })).collect::<Vec<_>>(),
            }));
        }
        accepted_sibling_count += usize::from(matching_ordered_path);
        sibling_outcomes.push(json!({
            "sibling_index": identity.get("sibling_index"),
            "ordinary_block_id": identity.get("ordinary_block_id"),
            "sibling_block_id": sibling_id,
            "parent_block_id": identity.get("parent_block_id"),
            "slot": identity.get("slot"),
            "relay_enqueued": identity_bool(identity, "relay_enqueued"),
            "accepted_by_any_honest_node": accepted,
            "receivers": receiver_reports,
        }));
    }

    let submitted_hashes_in_body = ordinary_body
        .transaction_hashes
        .iter()
        .filter(|hash| submitted_hashes.contains(*hash))
        .cloned()
        .collect::<Vec<_>>();
    let integrity = sibling_body_integrity(ordinary_body, opportunity);
    let passed = accepted_body_path
        && accepted_sibling_count >= minimum_accepted
        && accepted_bodies_all_match
        && voucher_evidence_valid
        && !submitted_hashes_in_body.is_empty()
        && integrity;
    let timing = &opportunity.timing;
    Ok(json!({
        "status": if passed { "passed" } else { "failed" },
        "failure": if passed { Value::Null } else {
            json!("no ordered honest-peer acceptance was paired with an available sibling block whose body root and ordered transaction hashes exactly match the nonempty ordinary block")
        },
        "failure_classification": if passed { Value::Null } else { json!("sibling_validation_or_body_integrity") },
        "nonempty_honest_peer_validation": passed,
        "honest_peer_validation": accepted_body_path,
        "cluster_nodes": participants.honest_nodes.len() + 1,
        "blend_provider_count": world.cluster.blend_core_nodes,
        "pre_attack_consensus_snapshots": pre_attack_consensus,
        "producer": participants.producer,
        "producer_runtime_name": world.nodes_info.get(&participants.producer).map(|node| node.runtime_name.as_str()),
        "blend_readiness": readiness,
        "provenance": provenance,
        "transaction_budget": workload.budget,
        "opportunity_search_window_seconds": workload.window_seconds,
        "transactions_submitted": submitted_transactions.len(),
        "submitted_transactions": submitted_transactions,
        "transaction_observations": transaction_observations,
        "producer_opportunities_examined": examined_count,
        "observation_completed_unix_ms": selected_at_unix_ms,
        "empty_or_nonmatching_opportunities_skipped": skipped_opportunities.len(),
        "skipped_opportunities": skipped_opportunities,
        "selected_ordinary_block": {
            "block_id": ordinary_body.block_id,
            "parent_block_id": ordinary_body.parent_block_id,
            "slot": ordinary_body.slot,
            "body_root": ordinary_body.body_root,
            "voucher_commitment": ordinary_body.voucher_commitment,
            "transaction_count": ordinary_body.transaction_hashes.len(),
            "transaction_hashes": ordinary_body.transaction_hashes,
            "submitted_transaction_hashes_in_body": submitted_hashes_in_body,
        },
        "ordinary_submission_preceded_sibling_construction": opportunity.ordinary_line
            < opportunity.siblings.first().map_or(usize::MAX, |(line, _)| *line),
        "timing": {
            "configured_siblings": parse_usize(timing, "configured_siblings"),
            "constructed_siblings": parse_usize(timing, "constructed_siblings"),
            "failed_siblings": parse_usize(timing, "failed_siblings"),
            "relay_enqueued_siblings": parse_usize(timing, "relay_enqueued_siblings"),
            "ordinary_self_applied": identity_bool(timing, "ordinary_self_applied"),
            "ordinary_relay_enqueued": identity_bool(timing, "ordinary_relay_enqueued"),
            "slot_to_ordinary_submission_ms": timing.get("slot_to_ordinary_submission_ms"),
            "ordinary_proposal_ms": timing.get("ordinary_proposal_ms"),
            "sibling_construction_ms": timing.get("sibling_construction_ms"),
            "sibling_blend_enqueue_ms": timing.get("sibling_blend_enqueue_ms"),
            "additional_processing_ms": timing.get("additional_processing_ms"),
            "total_leadership_processing_ms": timing.get("total_leadership_processing_ms"),
        },
        "producer_no_peers_subscribed_to_topic_errors_after_readiness": no_subscriber_errors,
        "sibling_body_integrity": integrity,
        "retrieved_voucher_commitments_valid": (!voucher_commitments.is_empty()).then_some(voucher_evidence_valid),
        "accepted_bodies_all_match_ordinary": storage_events.iter().any(|event| event["stored"] == true).then_some(accepted_bodies_all_match),
        "siblings": sibling_outcomes,
        "storage_observations": storage_events,
        "receiver_observability": {
            "blend_topic_subscription_api": "not exposed; matching decapsulated sibling event is direct per-payload reception evidence",
            "canonical_selection": "not asserted; accepted sibling may be non-canonical",
            "producer_enqueue_is_not_peer_delivery": true,
        },
    }))
}
async fn observe_honest_receivers(
    tails: &mut AuditLogTail,
    logs: &mut BTreeMap<String, Vec<ObservedLogLine>>,
    opportunity: &ProducerOpportunity,
    timeout: Duration,
    minimum_accepted: usize,
    event_logger: (&BlendDiagnosticEventLogger, &str),
) -> Result<HonestReceiverObservations, String> {
    let participants = tails.participants.clone();
    let ids = opportunity
        .siblings
        .iter()
        .filter_map(|(_, event)| event.get("sibling_block_id").cloned())
        .collect::<BTreeSet<_>>();
    let deadline = Instant::now() + timeout;
    let mut recorded_events = HashSet::new();
    loop {
        for node in &participants.honest_nodes {
            logs.entry(node.clone())
                .or_default()
                .extend(tails.read_new_lines(node)?);
        }
        logs.entry(participants.producer.clone())
            .or_default()
            .extend(tails.read_new_lines(&participants.producer)?);
        let honest_nodes = participants
            .honest_nodes
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let stages = receiver_stages(logs, &ids, &opportunity.ordinary, &honest_nodes);
        {
            let (logger, event_name) = event_logger;
            for (identity, evidence) in &stages {
                for (matches_identity, event) in evidence
                    .matching_events
                    .iter()
                    .map(|event| (true, event))
                    .chain(
                        evidence
                            .mismatched_events
                            .iter()
                            .map(|event| (false, event)),
                    )
                {
                    if recorded_events.insert((
                        identity.node.clone(),
                        event.path.clone(),
                        event.offset,
                    )) {
                        logger.append_named_timeline_record(
                            event_name,
                            &json!({
                                "node": identity.node,
                                "sibling_id": identity.sibling_id,
                                "stage": event.stage,
                                "matches_selected_identity": matches_identity,
                                "parent_block_id": event.parent_block_id,
                                "slot": event.slot,
                                "event_timestamp": format_log_timestamp(event.timestamp),
                                "reason": event.reason,
                                "source_file": event.path,
                                "source_offset": event.offset,
                            }),
                        );
                    }
                }
            }
        }
        let has_valid_acceptance = ids
            .iter()
            .filter(|id| {
                stages.iter().any(|(identity, evidence)| {
                    identity.sibling_id == **id && evidence.has_ordered_blend_to_acceptance()
                })
            })
            .count()
            >= minimum_accepted;
        let all_terminal = ids.iter().all(|id| {
            stages.iter().any(|(identity, observed)| {
                identity.sibling_id == *id && observed.has_terminal_stage()
            })
        });
        if (has_valid_acceptance && all_terminal) || Instant::now() >= deadline {
            let no_subscriber_errors = logs.get(&participants.producer).is_some_and(|producer| {
                producer
                    .iter()
                    .any(|line| line.text.contains("NoPeersSubscribedToTopic"))
            });
            return Ok(HonestReceiverObservations {
                evidence: stages,
                producer_had_no_subscriber_errors: no_subscriber_errors,
            });
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub(super) fn receiver_stages(
    logs: &BTreeMap<String, Vec<ObservedLogLine>>,
    ids: &BTreeSet<String>,
    ordinary: &HashMap<String, String>,
    honest_nodes: &[&str],
) -> HashMap<ReceiverIdentity, ReceiverEvidence> {
    let mut stages: HashMap<ReceiverIdentity, ReceiverEvidence> = HashMap::new();
    for &node in honest_nodes {
        for line in logs.get(node).into_iter().flatten() {
            let fields = parse_log_fields(&line.text);
            let Some(stage) = receiver_stage(fields.get("event").map(String::as_str)) else {
                continue;
            };
            let Some(block_id) = fields
                .get("sibling_block_id")
                .or_else(|| fields.get("block_id"))
            else {
                continue;
            };
            if !ids.contains(block_id) {
                continue;
            }
            let event = ReceiverEvent {
                stage,
                parent_block_id: fields.get("parent_block_id").cloned(),
                slot: fields.get("slot").cloned(),
                timestamp: parse_log_timestamp(&line.text),
                reason: fields.get("reason").cloned(),
                path: line.path.clone(),
                offset: line.offset,
            };
            let evidence = stages
                .entry(ReceiverIdentity {
                    node: node.to_owned(),
                    sibling_id: block_id.clone(),
                })
                .or_default();
            if event.parent_block_id.as_ref() == ordinary.get("parent_block_id")
                && event.slot.as_ref() == ordinary.get("slot")
            {
                evidence.matching_events.push(event);
            } else {
                evidence.mismatched_events.push(event);
            }
        }
    }
    stages
}

fn receiver_stage(event: Option<&str>) -> Option<&'static str> {
    match event? {
        "security_audit_blend_payload_decapsulated" => Some("blend_received"),
        "security_audit_chain_proposal_received" => Some("chain_proposal_received"),
        "security_audit_proposal_reconstructed" => Some("reconstructed"),
        "security_audit_proposal_reconstruction_failed" => Some("reconstruction_failed"),
        "security_audit_proposal_accepted" => Some("accepted"),
        "security_audit_proposal_rejected" => Some("rejected"),
        "security_audit_proposal_apply_failed" => Some("apply_failed"),
        "security_audit_proposal_not_processed" => Some("not_processed"),
        _ => None,
    }
}

pub(super) async fn query_accepted_blocks(
    world: &CucumberWorld,
    opportunity: &ProducerOpportunity,
    stages: &HashMap<ReceiverIdentity, ReceiverEvidence>,
) -> Vec<Value> {
    let mut observations = Vec::new();
    for (identity, receiver_stages) in stages {
        if !receiver_stages.has_stage("accepted") {
            continue;
        }
        let node_name = &identity.node;
        let block_id = &identity.sibling_id;
        let Some(node) = world.nodes_info.get(node_name) else {
            continue;
        };
        let Ok(header_id) = parse_header_id(block_id) else {
            continue;
        };
        let block_result = node.client.block(&header_id).await;
        let block_error = block_result.as_ref().err().map(ToString::to_string);
        let block = block_result.as_ref().ok().and_then(Option::as_ref);
        let identity = opportunity
            .siblings
            .iter()
            .find(|(_, event)| event.get("sibling_block_id") == Some(block_id))
            .map(|(_, event)| event);
        observations.push(json!({
            "timestamp": SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0.0, |duration| duration.as_secs_f64()),
            "event": "security_audit_block_storage_observed",
            "node": node_name,
            "block_id": block_id,
            "parent_block_id": identity.and_then(|event| event.get("parent_block_id")),
            "slot": identity.and_then(|event| event.get("slot")),
            "stored": block.is_some(),
            "body_root": block.map(|block| block.header.body_root.to_string()),
            "voucher_commitment": block.map(|block| format!("{:?}", block.header.proof_of_leadership.voucher_cm())),
            "transaction_count": block.map(|block| block.transactions.len()),
            "transaction_hashes": block.map(|block| {
                block.transactions.iter()
                    .map(|transaction| tx_hash_to_hex(&transaction.hash()))
                    .collect::<Vec<_>>()
            }),
            "error": block_error,
            "evidence_source": "NodeHttpClient::block",
        }));
    }
    observations
}
