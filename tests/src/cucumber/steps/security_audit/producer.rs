use super::*;
use crate::cucumber::wallet::submissions::is_user_wallet_funds_deficit;

fn requires_submitted_transaction(
    workload: Option<&SecurityAuditWorkload>,
) -> Result<bool, String> {
    match workload.map(|value| value.policy) {
        None => Ok(false),
        Some(SecurityAuditWorkloadPolicy::Nonempty { .. }) => Ok(true),
        Some(SecurityAuditWorkloadPolicy::Sustained { .. }) => {
            Err("sustained workload cannot select a single nonempty opportunity".to_owned())
        }
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    #[test]
    fn selection_uses_the_declared_workload_policy() {
        let mut workload = SecurityAuditWorkload {
            sender: "sender".to_owned(),
            receiver: "receiver".to_owned(),
            amount: 10,
            policy: SecurityAuditWorkloadPolicy::Nonempty {
                budget: 80,
                pending_seconds: 30,
            },
        };
        assert!(!requires_submitted_transaction(None).unwrap());
        assert!(requires_submitted_transaction(Some(&workload)).unwrap());
        workload.policy = SecurityAuditWorkloadPolicy::Sustained {
            budget: 80,
            initial_pending: 8,
            interval_seconds: 25,
        };
        assert!(requires_submitted_transaction(Some(&workload)).is_err());
    }
}

#[when(
    expr = "the audit observes a genuine sibling opportunity with at least {int} valid siblings within {int} seconds"
)]
pub(super) async fn step_audit_opportunity(
    world: &mut CucumberWorld,
    minimum: usize,
    seconds: u64,
) -> StepResult {
    let count =
        audit_sibling_count(world).map_err(|message| StepError::InvalidArgument { message })?;
    if !valid_sibling_minimum(count, minimum) || seconds == 0 {
        return Err(StepError::InvalidArgument {
            message: "invalid sibling minimum or leadership duration".to_owned(),
        });
    }
    if requires_submitted_transaction(world.security_audit_workload.as_ref())
        .map_err(|message| StepError::InvalidArgument { message })?
    {
        return select_nonempty_opportunity(world, minimum, seconds).await;
    }
    let event_logger = BlendDiagnosticEventLogger::from_world(world);
    let state = world
        .security_audit_state
        .as_mut()
        .ok_or_else(|| StepError::StepFail {
            message: "Blend readiness has not been established".to_owned(),
        })?;
    if state.opportunity.is_some() {
        return Err(StepError::InvalidArgument {
            message: "an audit opportunity is already selected".to_owned(),
        });
    }
    let producer = state.tails.participants.producer.clone();
    let opportunity = wait_for_producer_opportunity(
        &mut state.tails,
        state.logs.entry(producer.clone()).or_default(),
        minimum,
        Duration::from_secs(seconds),
    )
    .await
    .map_err(|message| StepError::StepFail { message })?;
    if parse_usize(&opportunity.ordinary, "configured_siblings") != Some(count) {
        return Err(StepError::StepFail {
            message: "producer opportunity contradicts declared sibling configuration".to_owned(),
        });
    }
    record_observed_audit_opportunity(&mut state.provenance, 1);
    let producer_logs = &state.logs[&producer];
    let source = |index: usize| {
        producer_logs.get(index).map(|line| {
            json!({
                "source_file": line.path,
                "source_offset": line.offset,
                "event_timestamp": format_log_timestamp(parse_log_timestamp(&line.text)),
            })
        })
    };
    event_logger.append_named_timeline_record(
        "security_audit_opportunity_selected",
        &json!({
            "producer": producer,
            "ordinary": opportunity.ordinary,
            "ordinary_source": source(opportunity.ordinary_line),
            "siblings": opportunity.siblings.iter().map(|(index, identity)| json!({
                "identity": identity,
                "source": source(*index),
            })).collect::<Vec<_>>(),
        }),
    );
    state.opportunity = Some(opportunity);
    Ok(())
}

#[then(
    expr = "at least {int} sibling proposal is accepted by a declared honest node through an ordered Blend-to-consensus path within {int} seconds"
)]
#[expect(
    clippy::too_many_lines,
    reason = "Preserves the full receiver evidence report"
)]
pub(super) async fn step_verify_sibling_propagation(
    world: &mut CucumberWorld,
    minimum_accepted: usize,
    seconds: u64,
) -> StepResult {
    if minimum_accepted == 0 || seconds == 0 {
        return Err(StepError::InvalidArgument {
            message: "sibling count must be positive".to_owned(),
        });
    }
    if world
        .security_audit_state
        .as_ref()
        .is_some_and(|state| state.nonempty.is_some())
    {
        return record_nonempty_sibling_propagation(
            world,
            minimum_accepted,
            Duration::from_secs(seconds),
        )
        .await;
    }
    let artifact_dir = &world.lifecycle.scenario_base_dir;
    fs::create_dir_all(artifact_dir).map_err(|error| StepError::StepFail {
        message: format!("cannot create audit evidence directory: {error}"),
    })?;

    let mut state = world
        .security_audit_state
        .take()
        .ok_or_else(|| StepError::StepFail {
            message: "audit opportunity state is unavailable".to_owned(),
        })?;
    let result = observe_sibling_propagation(
        world,
        &mut state,
        minimum_accepted,
        Duration::from_secs(seconds),
    )
    .await;
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
    let storage_events = report["storage_observations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|event| serde_json::to_string(event).ok())
        .collect::<Vec<_>>();
    let mut storage_evidence = storage_events.join("\n");
    if !storage_events.is_empty() {
        storage_evidence.push('\n');
    }
    fs::write(
        artifact_dir.join("receiver-observations.jsonl"),
        storage_evidence,
    )
    .map_err(|error| StepError::StepFail {
        message: format!("cannot write receiver storage evidence: {error}"),
    })?;
    let report_path = artifact_dir.join("opportunity.json");
    fs::write(
        &report_path,
        serde_json::to_vec_pretty(&report).map_err(|error| StepError::StepFail {
            message: format!("cannot encode audit evidence: {error}"),
        })?,
    )
    .map_err(|error| StepError::StepFail {
        message: format!("cannot write {}: {error}", report_path.display()),
    })?;

    if let Err(error) = log_capture {
        return Err(StepError::StepFail {
            message: format!(
                "audit evidence was collected, but original node logs could not be located: {error}; evidence: {}",
                report_path.display()
            ),
        });
    }

    if report["honest_peer_validation"].as_bool() == Some(true) {
        tracing::info!(
            target: TARGET,
            event = "security_audit_scenario_evidence",
            evidence_path = %report_path.display(),
            "An honest node received, reconstructed, and applied a same-slot sibling"
        );
        Ok(())
    } else {
        let failure = report["failure"]
            .as_str()
            .unwrap_or("no honest node validation evidence was observed");
        Err(StepError::StepFail {
            message: format!(
                "security-audit sibling assertion failed: {failure}; evidence: {}",
                report_path.display()
            ),
        })
    }
}

async fn select_nonempty_opportunity(
    world: &mut CucumberWorld,
    minimum: usize,
    seconds: u64,
) -> StepResult {
    let count =
        audit_sibling_count(world).map_err(|message| StepError::InvalidArgument { message })?;
    if !valid_sibling_minimum(count, minimum) {
        return Err(StepError::InvalidArgument {
            message: "invalid minimum sibling count".to_owned(),
        });
    }
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut state = world
        .security_audit_state
        .take()
        .ok_or_else(|| StepError::StepFail {
            message: "Blend readiness has not been established".to_owned(),
        })?;
    if state.opportunity.is_some() {
        world.security_audit_state = Some(state);
        return Err(StepError::InvalidArgument {
            message: "an audit opportunity is already selected".to_owned(),
        });
    }
    let result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
        world.reset_wallet_scanner_after_current_iteration().await;
        select_nonempty_sibling_opportunity(world, &mut state, deadline, seconds).await
    })
    .await
    .unwrap_or_else(|_| {
        Err(AuditFailure {
            message: "nonempty opportunity observation exceeded its deadline".to_owned(),
            details: json!({"failure_classification": "opportunity_observation_timeout_unverified"}),
        })
    });
    world.security_audit_state = Some(state);
    if let Err(failure) = result {
        let artifact_dir = &world.lifecycle.scenario_base_dir;
        fs::create_dir_all(artifact_dir).map_err(|error| StepError::StepFail {
            message: format!("cannot create evidence directory: {error}"),
        })?;
        let path = artifact_dir.join("nonempty-opportunity.json");
        let logs = original_node_logs(world);
        let report = json!({ "status": "failed", "failure": failure.message, "details": failure.details, "node_logs": logs.as_ref().ok(), "node_logs_kind": "original", "node_log_capture_error": logs.err() });
        fs::write(
            &path,
            serde_json::to_vec_pretty(&report).map_err(|error| StepError::StepFail {
                message: error.to_string(),
            })?,
        )
        .map_err(|error| StepError::StepFail {
            message: error.to_string(),
        })?;
        return Err(StepError::StepFail {
            message: format!("{}; evidence: {}", report["failure"], path.display()),
        });
    }
    Ok(())
}

#[when(
    expr = "the audit producer is stopped upon the first selected sibling's Blend reception by a declared honest node within {int} seconds"
)]
pub(super) async fn step_stop_on_selected_sibling_reception(
    world: &mut CucumberWorld,
    seconds: u64,
) -> StepResult {
    if seconds == 0 {
        return Err(StepError::InvalidArgument {
            message: "reception deadline must be positive".to_owned(),
        });
    }
    let mut state = world
        .security_audit_state
        .take()
        .ok_or_else(|| StepError::StepFail {
            message: "no selected opportunity".to_owned(),
        })?;
    let selection = state.nonempty.as_ref().ok_or_else(|| StepError::StepFail {
        message: "no selected nonempty opportunity".to_owned(),
    })?;
    let selected_at = selection.selected_at;
    let selected_at_unix_ms = selection.selected_at_unix_ms;
    let opportunity = state
        .opportunity
        .as_ref()
        .ok_or_else(|| StepError::StepFail {
            message: "no selected opportunity".to_owned(),
        })?
        .clone();
    let participants = state.tails.participants.clone();
    let deadline = selected_at + Duration::from_secs(seconds);
    let result = wait_for_selected_reception(
        &mut state,
        &opportunity,
        &participants.honest_nodes,
        deadline,
    )
    .await;
    let observed_at = Instant::now();
    let observed_at_unix_ms = unix_timestamp_ms();
    // Always stop the producer, including on a missing or unreadable trigger.
    let stop_result = crate::cucumber::steps::nodes::stop_node(
        world,
        "stop on selected sibling reception",
        &participants.producer,
    )
    .await;
    let stopped_at = world
        .lifecycle
        .node_stopped_at
        .get(&participants.producer)
        .copied();
    let prompt = stopped_at.is_some_and(|stop| stop <= deadline);
    let report = json!({
        "status": if result.is_ok() && stop_result.is_ok() && prompt { "observed" } else { "unverified" },
        "selected_at_unix_ms": selected_at_unix_ms,
        "trigger_observed_at_unix_ms": result.as_ref().ok().map(|_| observed_at_unix_ms),
        "reception_timing_reference": "The reception timing fields measure when the harness observed the log event, not necessarily when the network received the proposal; the trigger retains the event timestamp and source location.",
        "selection_to_reception_ms": result.as_ref().ok().map(|_| observed_at.duration_since(selected_at).as_millis()),
        "reception_to_stop_ms": result.as_ref().ok().and_then(|_| stopped_at.map(|stop| stop.duration_since(observed_at).as_millis())),
        "selection_to_stop_ms": stopped_at.map(|stop| stop.duration_since(selected_at).as_millis()),
        "stop_completed_unix_ms": stopped_at.map(|stop| unix_timestamp_ms().saturating_sub(u64::try_from(stop.elapsed().as_millis()).unwrap_or(u64::MAX))),
        "deadline_seconds": seconds,
        "trigger": result.as_ref().ok().map(|trigger| json!({ "node": trigger.node, "sibling_id": trigger.sibling_id, "event_timestamp": format_log_timestamp(trigger.event.timestamp), "source_log": trigger.event.path, "source_offset": trigger.event.offset, "parent_block_id": trigger.event.parent_block_id, "slot": trigger.event.slot })),
        "error": result.as_ref().err(),
        "stop_error": stop_result.as_ref().err().map(ToString::to_string),
    });
    let path = world
        .lifecycle
        .scenario_base_dir
        .join("shutdown-trigger.json");
    fs::create_dir_all(path.parent().expect("artifact path has a parent")).map_err(|error| {
        StepError::StepFail {
            message: error.to_string(),
        }
    })?;
    fs::write(
        &path,
        serde_json::to_vec_pretty(&report).map_err(|error| StepError::StepFail {
            message: error.to_string(),
        })?,
    )
    .map_err(|error| StepError::StepFail {
        message: error.to_string(),
    })?;
    world.security_audit_state = Some(state);
    stop_result?;
    if let Err(error) = result {
        return Err(StepError::StepFail {
            message: format!(
                "shutdown trigger unverified: {error}; evidence: {}",
                path.display()
            ),
        });
    }
    if !prompt {
        return Err(StepError::StepFail {
            message: format!(
                "producer shutdown exceeded the selection deadline; evidence: {}",
                path.display()
            ),
        });
    }
    Ok(())
}

#[then(
    "the selected ordinary block contains a submitted transaction and its siblings preserve its identity"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step entrypoints require a mutable world"
)]
pub(super) fn step_verify_selected_nonempty_body(world: &mut CucumberWorld) -> StepResult {
    let state = world
        .security_audit_state
        .as_ref()
        .ok_or_else(|| StepError::StepFail {
            message: "audit opportunity state unavailable".to_owned(),
        })?;
    let selection = state.nonempty.as_ref().ok_or_else(|| StepError::StepFail {
        message: "nonempty opportunity not selected".to_owned(),
    })?;
    let opportunity = state
        .opportunity
        .as_ref()
        .ok_or_else(|| StepError::StepFail {
            message: "opportunity not selected".to_owned(),
        })?;
    if !has_nonempty_submitted_body(
        &selection.ordinary_body.transaction_hashes,
        &selection.submitted_hashes,
    ) || !sibling_body_integrity(&selection.ordinary_body, opportunity)
    {
        return Err(StepError::StepFail {
            message: "selected opportunity failed transaction or sibling identity checks"
                .to_owned(),
        });
    }
    Ok(())
}

#[then("accepted siblings have the ordinary block's body root and ordered transactions")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step entrypoints require a mutable world"
)]
pub(super) fn step_verify_accepted_sibling_bodies(world: &mut CucumberWorld) -> StepResult {
    let path = world
        .lifecycle
        .scenario_base_dir
        .join("nonempty-opportunity.json");
    let bytes = fs::read(&path).map_err(|error| StepError::StepFail {
        message: format!("cannot read {}: {error}", path.display()),
    })?;
    let report: Value = serde_json::from_slice(&bytes).map_err(|error| StepError::StepFail {
        message: format!("cannot parse {}: {error}", path.display()),
    })?;
    if report["honest_peer_validation"] != true
        || report["accepted_bodies_all_match_ordinary"] != true
        || report["sibling_body_integrity"] != true
    {
        return Err(StepError::StepFail {
            message: format!(
                "accepted sibling body integrity is not verified; evidence: {}",
                path.display()
            ),
        });
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "Runs the bounded nonempty transaction workload and correlates the selected block bodies"
)]
pub(super) async fn select_nonempty_sibling_opportunity(
    world: &mut CucumberWorld,
    state: &mut AuditScenarioState,
    global_deadline: Instant,
    window_seconds: u64,
) -> Result<(), AuditFailure> {
    let participants = audit_participants(world)
        .cloned()
        .map_err(|message| AuditFailure {
            message,
            details: json!({}),
        })?;
    let configured_siblings = audit_sibling_count(world).map_err(|message| AuditFailure {
        message,
        details: json!({}),
    })?;
    let transfer = audit_workload(world)
        .cloned()
        .map_err(|message| AuditFailure {
            message,
            details: json!({}),
        })?;
    let SecurityAuditWorkloadPolicy::Nonempty {
        budget,
        pending_seconds,
    } = transfer.policy
    else {
        return Err(AuditFailure {
            message: "nonempty selection requires a pending-sampling workload".to_owned(),
            details: json!({}),
        });
    };
    let workload = SecurityAuditNonemptyWorkload {
        budget,
        window_seconds,
        pending_seconds,
    };
    let provenance = &mut state.provenance;
    let readiness = &state.readiness;
    let tails = &mut state.tails;
    let pre_attack_consensus = capture_pre_attack_consensus(world).await;

    world
        .ensure_wallet_scanner_started()
        .await
        .map_err(|error| AuditFailure {
            message: format!("could not start the wallet scanner for the workload: {error}"),
            details: json!({
                "provenance": provenance,
                "blend_readiness": readiness,
            }),
        })?;

    // `wait_for_blend_readiness` snapshots all current log offsets before this
    // function prepares or submits any workload transactions.
    let mut opportunity_state = NonemptyOpportunityState::default();
    let mut submitted_hashes = BTreeSet::new();
    let mut submitted_transactions = Vec::new();
    let mut transaction_observations = Vec::new();
    let mut selected: Option<(ProducerOpportunity, ApiBlock)> = None;
    let mut last_mempool_status = "not_observed".to_owned();
    let mut mempool_observation_timed_out = false;
    let mut last_funding_error = None;

    'transactions: while submitted_transactions.len() < workload.budget {
        if Instant::now() >= global_deadline {
            break;
        }
        let transaction_index = submitted_transactions.len() + 1;
        let alias = format!("SECURITY_AUDIT_NONEMPTY_{transaction_index}");
        let preparation = tokio::time::timeout_at(tokio::time::Instant::from_std(global_deadline), prepare_transfer_transaction(
            world,
            "security audit nonempty sibling workload",
            alias.clone(),
            transfer.amount,
            transfer.sender.clone(),
            transfer.receiver.clone(),
        ))
        .await
        .map_err(|_| AuditFailure {
            message: format!("workload transaction preparation exceeded the observation deadline: {alias}"),
            details: json!({"provenance": provenance, "blend_readiness": readiness, "submitted_transactions": submitted_transactions, "failure_classification": "workload_preparation_timeout_unverified"}),
        })?;
        if let Err(error) = preparation {
            if is_user_wallet_funds_deficit(&error) {
                last_funding_error = Some(error.to_string());
                transaction_observations.push(json!({
                    "alias": alias,
                    "status": "funds_temporarily_unavailable",
                    "error": error.to_string(),
                }));
                let retry_at = (Instant::now() + Duration::from_secs(2)).min(global_deadline);
                while Instant::now() < retry_at {
                    opportunity_state.logs.extend(
                        tails.read_new_lines(&participants.producer).map_err(|message| AuditFailure {
                            message,
                            details: json!({"provenance": provenance, "submitted_transactions": submitted_transactions}),
                        })?,
                    );
                    let selection = tokio::time::timeout_at(
                        tokio::time::Instant::from_std(global_deadline),
                        opportunity_state.select(
                            world,
                            &participants.producer,
                            configured_siblings,
                            &submitted_hashes,
                        ),
                    )
                    .await;
                    match selection {
                        Ok(Ok(Some(candidate))) => {
                            selected = Some(candidate);
                            break 'transactions;
                        }
                        Ok(Ok(None)) => {}
                        Ok(Err(mut error)) => {
                            error.details["provenance"] = provenance.clone();
                            error.details["blend_readiness"] = readiness.clone();
                            error.details["submitted_transactions"] = json!(submitted_transactions);
                            return Err(error);
                        }
                        Err(_) => {
                            opportunity_state.last_body_error =
                                Some("opportunity lookup exceeded observation deadline".to_owned());
                            break 'transactions;
                        }
                    }
                    sleep(
                        Duration::from_millis(250)
                            .min(retry_at.saturating_duration_since(Instant::now())),
                    )
                    .await;
                }
                continue;
            }
            return Err(AuditFailure {
                message: format!(
                    "could not prepare independent workload transaction {alias}: {error}"
                ),
                details: json!({
                    "provenance": provenance,
                    "blend_readiness": readiness,
                    "transactions_prepared": submitted_transactions,
                    "failure_classification": "workload_preparation_failed",
                }),
            });
        }
        last_funding_error = None;
        let signed = world
            .resolve_prepared_transaction(&alias)
            .map_err(|error| AuditFailure {
                message: format!("prepared workload transaction {alias} disappeared: {error}"),
                details: json!({ "provenance": provenance, "failure_classification": "workload_preparation_failed" }),
            })?;
        let transaction_hash = tx_hash_to_hex(&signed.hash());
        tokio::time::timeout_at(tokio::time::Instant::from_std(global_deadline), submit_prepared_transaction_to_nodes(
            world,
            "security audit nonempty sibling workload",
            alias.clone(),
            std::iter::once(&participants.producer)
                .chain(&participants.honest_nodes)
                .cloned()
                .collect(),
        ))
        .await
        .map_err(|_| AuditFailure {
            message: format!("workload transaction submission exceeded the observation deadline: {alias}"),
            details: json!({"provenance": provenance, "blend_readiness": readiness, "submitted_transactions": submitted_transactions, "in_flight_transaction_hash": transaction_hash, "failure_classification": "workload_submission_timeout_unverified"}),
        })?
        .map_err(|error| AuditFailure {
            message: format!(
                "could not submit workload transaction {alias} to the declared nodes: {error}"
            ),
            details: json!({
                "provenance": provenance,
                "blend_readiness": readiness,
                "pre_attack_consensus_snapshots": pre_attack_consensus,
                "submitted_transactions": submitted_transactions,
                "failure_classification": "workload_submission_failed",
            }),
        })?;
        submitted_hashes.insert(transaction_hash.clone());
        submitted_transactions.push(json!({
            "alias": alias,
            "transaction_hash": transaction_hash,
        }));

        let submitted_hash = signed.hash();
        let transaction_deadline =
            (Instant::now() + Duration::from_secs(workload.pending_seconds)).min(global_deadline);
        let mut saw_transaction_pending = false;
        let mut mempool_samples = Vec::new();

        loop {
            opportunity_state
                .logs
                .extend(
                    tails
                        .read_new_lines(&participants.producer)
                        .map_err(|message| AuditFailure {
                            message,
                            details: json!({
                                "provenance": provenance,
                                "blend_readiness": readiness,
                                "submitted_transactions": submitted_transactions,
                                "skipped_opportunities": opportunity_state.skipped,
                            }),
                        })?,
                );

            let selection = tokio::time::timeout_at(
                tokio::time::Instant::from_std(global_deadline),
                opportunity_state.select(
                    world,
                    &participants.producer,
                    configured_siblings,
                    &submitted_hashes,
                ),
            )
            .await;
            let selection = match selection {
                Ok(Ok(selection)) => selection,
                Ok(Err(mut error)) => {
                    error.details["provenance"] = provenance.clone();
                    error.details["blend_readiness"] = readiness.clone();
                    error.details["submitted_transactions"] = json!(submitted_transactions);
                    return Err(error);
                }
                Err(_) => {
                    opportunity_state.last_body_error =
                        Some("opportunity lookup exceeded observation deadline".to_owned());
                    transaction_observations.push(json!({"alias": alias, "transaction_hash": transaction_hash, "observed_pending_before_deadline": saw_transaction_pending, "mempool_observations": mempool_samples, "last_mempool_status": last_mempool_status}));
                    break 'transactions;
                }
            };
            if let Some(candidate) = selection {
                transaction_observations.push(json!({
                    "alias": alias,
                    "transaction_hash": transaction_hash,
                    "observed_pending_before_candidate": saw_transaction_pending,
                    "mempool_observations": mempool_samples,
                    "last_mempool_status": last_mempool_status,
                }));
                selected = Some(candidate);
                break 'transactions;
            }

            if nonempty_workload_action(
                Instant::now(),
                global_deadline,
                Some(transaction_deadline),
                submitted_transactions.len(),
                workload.budget,
            ) != NonemptyWorkloadAction::Observe
            {
                break;
            }

            // The final transaction's bounded mempool observation is complete;
            // continue polling producer events without growing its sample list.
            if submitted_transactions.len() >= workload.budget
                && Instant::now() >= transaction_deadline
            {
                sleep(Duration::from_millis(250)).await;
                continue;
            }

            let views = tokio::time::timeout_at(
                tokio::time::Instant::from_std(global_deadline),
                join_all(
                    std::iter::once(&participants.producer)
                        .chain(&participants.honest_nodes)
                        .map(async |name| {
                            (
                                name.as_str(),
                                world.nodes_info[name].client.test_mempool_view().await,
                            )
                        }),
                ),
            )
            .await;
            let Ok(views) = views else {
                mempool_observation_timed_out = true;
                "mempool observation exceeded the observation deadline"
                    .clone_into(&mut last_mempool_status);
                break;
            };
            let all_views_succeeded = views.iter().all(|(_, view)| view.is_ok());
            let pending_nodes = views
                .iter()
                .filter_map(|(name, view)| {
                    view.as_ref()
                        .ok()
                        .filter(|hashes| hashes.contains(&submitted_hash))
                        .map(|_| *name)
                })
                .collect::<Vec<_>>();
            saw_transaction_pending |= !pending_nodes.is_empty();
            last_mempool_status = views
                .iter()
                .map(|(name, view)| match view {
                    Ok(hashes) => format!("{name}:pending={}", hashes.contains(&submitted_hash)),
                    Err(error) => format!("{name}:error={error}"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            mempool_samples.push(json!({
                "observed_at_utc": SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0.0, |duration| duration.as_secs_f64()),
                "pending_nodes": pending_nodes,
                "all_node_views_succeeded": all_views_succeeded,
                "observations": last_mempool_status,
            }));
            if nonempty_workload_action(
                Instant::now(),
                global_deadline,
                Some(transaction_deadline),
                submitted_transactions.len(),
                workload.budget,
            ) != NonemptyWorkloadAction::Observe
            {
                break;
            }
            sleep(Duration::from_millis(250)).await;
        }
        transaction_observations.push(json!({
            "alias": alias,
            "transaction_hash": transaction_hash,
            "observed_pending_before_disappearing": saw_transaction_pending,
            "mempool_observations": mempool_samples,
            "last_mempool_status": last_mempool_status,
        }));
    }

    let Some((opportunity, ordinary_block)) = selected else {
        let ordinary_event_count = opportunity_state
            .logs
            .iter()
            .filter(|line| {
                parse_log_fields(&line.text)
                    .get("event")
                    .map(String::as_str)
                    == Some("security_audit_ordinary_submitted")
            })
            .count();
        let complete_opportunity_count = find_producer_opportunities(&opportunity_state.logs).len();
        let leadership_observed = opportunity_state.logs.iter().any(|line| {
            matches!(
                parse_log_fields(&line.text)
                    .get("event")
                    .map(String::as_str),
                Some("security_audit_ordinary_submitted" | "security_audit_leadership_timing")
            )
        });
        let failure_classification = if mempool_observation_timed_out
            || !opportunity_state.incomplete_body_ids.is_empty()
            || ordinary_event_count > complete_opportunity_count
            || (leadership_observed && opportunity_state.examined_ids.is_empty())
        {
            "opportunity_evidence_unverified"
        } else if last_funding_error.is_some() {
            "workload_funds_unavailable"
        } else if leadership_observed {
            "leadership_without_qualifying_nonempty_body"
        } else {
            "no_producer_leadership_observed"
        };
        return Err(AuditFailure {
            message: format!(
                "{} did not produce a verified qualifying sibling opportunity containing one of the submitted transactions after {} bounded submissions ({failure_classification})",
                participants.producer,
                submitted_transactions.len()
            ),
            details: json!({
                "provenance": provenance,
                "blend_readiness": readiness,
                "submitted_transactions": submitted_transactions,
                "transaction_observations": transaction_observations,
                "producer_opportunities_examined": opportunity_state.examined_ids.len(),
                "producer_ordinary_event_count": ordinary_event_count,
                "producer_complete_opportunity_count": complete_opportunity_count,
                "empty_or_nonmatching_opportunities_skipped": opportunity_state.skipped.len(),
                "skipped_opportunities": opportunity_state.skipped,
                "unavailable_ordinary_body_ids": opportunity_state.incomplete_body_ids,
                "producer_logs": opportunity_state.logs.iter().map(|line| json!({"line": line.text, "path": line.path, "offset": line.offset})).collect::<Vec<_>>(),
                "last_mempool_status": last_mempool_status,
                "last_body_error": opportunity_state.last_body_error,
                "last_funding_error": last_funding_error,
                "failure_classification": failure_classification,
            }),
        });
    };
    record_observed_audit_opportunity(provenance, 1);

    let ordinary_body = summarize_api_block(&ordinary_block);
    state
        .logs
        .insert(participants.producer.clone(), opportunity_state.logs);
    state.opportunity = Some(opportunity);
    state.nonempty = Some(NonemptySelection {
        ordinary_body,
        pre_attack_consensus,
        submitted_hashes,
        submitted_transactions,
        transaction_observations,
        skipped_opportunities: opportunity_state.skipped,
        examined_count: opportunity_state.examined_ids.len(),
        workload,
        selected_at_unix_ms: unix_timestamp_ms(),
        selected_at: Instant::now(),
    });
    if let (Some(opportunity), Some(selection)) = (&state.opportunity, &state.nonempty) {
        let producer_logs = &state.logs[&participants.producer];
        let event_source = |index: usize| {
            producer_logs.get(index).map(|line| {
                json!({
                    "source_file": line.path,
                    "source_offset": line.offset,
                    "event_timestamp": format_log_timestamp(parse_log_timestamp(&line.text)),
                })
            })
        };
        BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
            "security_audit_nonempty_opportunity_selected",
            &json!({
                "producer": participants.producer,
                "selected_at_unix_ms": selection.selected_at_unix_ms,
                "ordinary_block": selection.ordinary_body,
                "ordinary_source": event_source(opportunity.ordinary_line),
                "siblings": opportunity.siblings.iter().map(|(index, identity)| json!({
                    "identity": identity,
                    "source": event_source(*index),
                })).collect::<Vec<_>>(),
                "submitted_transaction_hashes": selection.submitted_hashes,
            }),
        );
    }
    Ok(())
}

pub(super) async fn wait_for_blend_readiness(
    world: &CucumberWorld,
    timeout: Duration,
) -> Result<(Value, AuditLogTail), String> {
    let participants = audit_participants(world)?;
    let deadline = Instant::now() + timeout;
    let mut last_status;
    loop {
        let mut all_ready = true;
        let mut samples = Vec::new();
        for node_name in std::iter::once(&participants.producer).chain(&participants.honest_nodes) {
            let node = world
                .nodes_info
                .get(node_name)
                .ok_or_else(|| format!("node {node_name} was not started"))?;
            let (network, blend) =
                tokio::join!(node.client.network_info(), node.client.blend_info());
            let network = network.map_err(|error| error.to_string());
            let blend = blend.map_err(|error| error.to_string());
            let network_peers = network.as_ref().ok().map_or(0, |value| value.n_peers);
            let blend_peers = blend
                .as_ref()
                .ok()
                .and_then(Option::as_ref)
                .and_then(|value| value.core_info.as_ref())
                .map_or(0, |core| {
                    core.current_epoch_peers
                        .iter()
                        .filter(|(_, healthy)| *healthy)
                        .count()
                });
            let ready = network_peers > 0 && blend_peers > 0;
            all_ready &= ready;
            samples.push(json!({
                "node": node_name,
                "node_identity": world.cluster.node_peer_ids.get(node_name).map(ToString::to_string),
                "network_peer_count": network_peers,
                "healthy_blend_core_peer_count": blend_peers,
                "network_info_error": network.err(),
                "blend_info_error": blend.err(),
                "ready": ready,
            }));
        }
        last_status = serde_json::to_string(&samples).unwrap_or_else(|_| "unavailable".to_owned());
        if all_ready {
            let tails = AuditLogTail::after_readiness(world)?;
            let mut baseline = Vec::new();
            for name in std::iter::once(&participants.producer).chain(&participants.honest_nodes) {
                baseline.push(json!({
                    "node": name,
                    "new_log_events_observed_after_readiness_only": true,
                    "log_file_count": tails.paths.get(name).map_or(0, Vec::len),
                }));
            }
            return Ok((
                json!({
                    "established": true,
                    "observed_at_utc": SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|error| error.to_string())?
                        .as_secs_f64(),
                    "nodes": samples,
                    "log_offsets_captured_after_readiness": baseline,
                    "topic_subscription_api": "not exposed; receiver decapsulation events will establish per-sibling delivery",
                }),
                tails,
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Blend network readiness did not establish within {}s; last peer samples: {last_status}",
                timeout.as_secs()
            ));
        }
        sleep(Duration::from_secs(1)).await;
    }
}

pub(super) async fn wait_for_producer_opportunity(
    tails: &mut AuditLogTail,
    logs: &mut Vec<ObservedLogLine>,
    minimum_siblings: usize,
    timeout: Duration,
) -> Result<ProducerOpportunity, String> {
    let producer = tails.participants.producer.clone();
    let deadline = Instant::now() + timeout;
    loop {
        logs.extend(tails.read_new_lines(&producer)?);
        if let Some(opportunity) = find_producer_opportunities(logs)
            .into_iter()
            .find(|opportunity| opportunity.siblings.len() >= minimum_siblings)
        {
            return Ok(opportunity);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{producer} did not complete a genuine leadership opportunity within {}s after Blend readiness",
                timeout.as_secs(),
            ));
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub(super) fn find_producer_opportunities(lines: &[ObservedLogLine]) -> Vec<ProducerOpportunity> {
    let parsed = lines
        .iter()
        .enumerate()
        .filter(|(_, raw)| raw.text.contains("security_audit_"))
        .map(|(line, raw)| (line, raw, parse_log_fields(&raw.text)))
        .collect::<Vec<_>>();
    let mut opportunities = Vec::new();
    for (ordinary_line, ordinary_record, ordinary) in &parsed {
        if ordinary
            .get("event")
            .is_none_or(|event| event != "security_audit_ordinary_submitted")
        {
            continue;
        }
        let Some(configured_siblings) =
            parse_usize(ordinary, "configured_siblings").filter(|count| *count > 0)
        else {
            continue;
        };
        let Some(ordinary_id) = ordinary.get("ordinary_block_id") else {
            continue;
        };
        let Some(parent) = ordinary.get("parent_block_id") else {
            continue;
        };
        let Some(slot) = ordinary.get("slot") else {
            continue;
        };
        let mut siblings = parsed
            .iter()
            .filter(|(_, record, fields)| {
                log_line_precedes(ordinary_record, record)
                    && fields
                        .get("event")
                        .is_some_and(|event| event == "security_audit_sibling_submitted")
                    && fields.get("ordinary_block_id") == Some(ordinary_id)
                    && fields.get("parent_block_id") == Some(parent)
                    && fields.get("slot") == Some(slot)
            })
            .map(|(line, record, fields)| (*line, record, fields.clone()))
            .collect::<Vec<_>>();
        siblings.sort_by_key(|(_, _, fields)| {
            parse_usize(fields, "sibling_index").unwrap_or(usize::MAX)
        });
        if siblings.len() != configured_siblings {
            continue;
        }
        let indexes = siblings
            .iter()
            .filter_map(|(_, _, fields)| parse_usize(fields, "sibling_index"))
            .collect::<BTreeSet<_>>();
        let ids = siblings
            .iter()
            .filter_map(|(_, _, fields)| fields.get("sibling_block_id"))
            .collect::<BTreeSet<_>>();
        if indexes.len() != configured_siblings
            || !indexes.iter().copied().eq(1..=configured_siblings)
            || ids.len() != configured_siblings
            || ids.contains(ordinary_id)
        {
            continue;
        }
        let Some((timing_line, _, timing)) = parsed.iter().find(|(_, record, fields)| {
            siblings
                .iter()
                .all(|(_, sibling_record, _)| log_line_precedes(sibling_record, record))
                && fields
                    .get("event")
                    .is_some_and(|event| event == "security_audit_leadership_timing")
                && fields.get("parent_block_id") == Some(parent)
                && fields.get("slot") == Some(slot)
        }) else {
            continue;
        };
        if parse_usize(timing, "configured_siblings") != Some(configured_siblings)
            || parse_usize(timing, "constructed_siblings") != Some(configured_siblings)
            || !identity_bool(timing, "ordinary_self_applied")
            || !identity_bool(timing, "ordinary_relay_enqueued")
            || !identity_bool(ordinary, "ordinary_self_applied")
            || !identity_bool(ordinary, "ordinary_relay_enqueued")
        {
            continue;
        }
        opportunities.push(ProducerOpportunity {
            ordinary: ordinary.clone(),
            ordinary_line: *ordinary_line,
            siblings: siblings
                .into_iter()
                .map(|(line, _, fields)| (line, fields))
                .collect(),
            timing: timing.clone(),
            timing_line: *timing_line,
        });
    }
    opportunities
}

pub(super) fn log_line_precedes(first: &ObservedLogLine, second: &ObservedLogLine) -> bool {
    let (Some(first_time), Some(second_time)) = (
        parse_log_timestamp(&first.text),
        parse_log_timestamp(&second.text),
    ) else {
        return false;
    };
    match first_time.cmp(&second_time) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Equal => first.path == second.path && first.offset < second.offset,
        std::cmp::Ordering::Greater => false,
    }
}

pub(super) fn summarize_api_block(block: &ApiBlock) -> BlockBodyEvidence {
    BlockBodyEvidence {
        block_id: block.header.id.to_string(),
        parent_block_id: block.header.parent_block.to_string(),
        slot: u64::from(block.header.slot),
        body_root: block.header.body_root.to_string(),
        voucher_commitment: format!("{:?}", block.header.proof_of_leadership.voucher_cm()),
        transaction_hashes: block
            .transactions
            .iter()
            .map(|transaction| tx_hash_to_hex(&transaction.hash()))
            .collect(),
    }
}

pub(super) fn parse_header_id(value: &str) -> Result<HeaderId, String> {
    let bytes = value
        .strip_prefix("0x")
        .map_or_else(|| hex::decode(value), hex::decode)
        .map_err(|error| format!("invalid block ID {value}: {error}"))?;
    HeaderId::try_from(bytes.as_slice())
        .map_err(|error| format!("invalid block ID {value}: {error}"))
}

pub(super) fn has_nonempty_submitted_body(
    transaction_hashes: &[String],
    submitted_hashes: &BTreeSet<String>,
) -> bool {
    !transaction_hashes.is_empty()
        && transaction_hashes
            .iter()
            .any(|transaction_hash| submitted_hashes.contains(transaction_hash))
}

pub(super) fn same_transaction_body(
    ordinary: &BlockBodyEvidence,
    body_root: &str,
    transaction_hashes: &[String],
) -> bool {
    ordinary.body_root == body_root && ordinary.transaction_hashes == transaction_hashes
}

pub(super) fn record_voucher_commitment(
    seen: &mut BTreeMap<String, String>,
    ordinary: &str,
    sibling_id: &str,
    voucher: Option<&str>,
) -> bool {
    let Some(voucher) = voucher else {
        return false;
    };
    let valid = voucher != ordinary
        && seen
            .get(sibling_id)
            .is_none_or(|previous| previous == voucher)
        && seen
            .iter()
            .all(|(id, previous)| id == sibling_id || previous != voucher);
    seen.insert(sibling_id.to_owned(), voucher.to_owned());
    valid
}

pub(super) fn sibling_body_integrity(
    ordinary: &BlockBodyEvidence,
    opportunity: &ProducerOpportunity,
) -> bool {
    let sibling_ids = opportunity
        .siblings
        .iter()
        .filter_map(|(_, sibling)| sibling.get("sibling_block_id"))
        .collect::<BTreeSet<_>>();
    let configured = parse_usize(&opportunity.ordinary, "configured_siblings");
    configured.is_some_and(|count| {
        count > 0 && sibling_ids.len() == count && opportunity.siblings.len() == count
    }) && !sibling_ids.contains(&ordinary.block_id)
        && {
            let expected_slot = ordinary.slot.to_string();
            opportunity.siblings.iter().all(|(_, sibling)| {
                sibling.get("parent_block_id") == Some(&ordinary.parent_block_id)
                    && sibling.get("slot") == Some(&expected_slot)
            })
        }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(super) struct AuditConsensusParameters {
    pub(super) security_parameter_k: u32,
    pub(super) slot_duration_secs: u64,
    pub(super) uncle_reference_window_in_block: u32,
    pub(super) slot_activation_coeff_numerator: u32,
    pub(super) slot_activation_coeff_denominator: u32,
    pub(super) epoch_stake_distribution_stabilization: u8,
    pub(super) epoch_period_nonce_buffer: u8,
    pub(super) epoch_period_nonce_stabilization: u8,
    pub(super) slots_per_epoch: u64,
}
