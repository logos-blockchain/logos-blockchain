use super::*;

#[when("the sustained sibling attack meets these exposure and cooldown criteria:")]
#[expect(
    clippy::allow_attributes,
    reason = "The Cucumber step signature mandates a mutable world"
)]
#[allow(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_observe_sustained_attack(world: &mut CucumberWorld, step: &Step) -> StepResult {
    let values = criteria_table(step)?;
    require_criteria(
        &values,
        &[
            "complete_opportunities",
            "nonempty_submitted_opportunities",
            "delivered_opportunities",
            "cooldown_canonical_blocks",
            "minimum_active_seconds",
            "active_timeout_seconds",
            "readiness_seconds",
            "maximum_tip_height_skew",
        ],
    )?;
    let workload = workload_criteria(world, &values)?;
    let exposure = ExposureCriteria {
        min_opportunities: positive_criterion(&values, "complete_opportunities")?,
        min_nonempty: positive_criterion(&values, "nonempty_submitted_opportunities")?,
        min_delivered: positive_criterion(&values, "delivered_opportunities")?,
        cooldown_blocks: positive_criterion(&values, "cooldown_canonical_blocks")?,
        min_duration: Duration::from_secs(positive_criterion(&values, "minimum_active_seconds")?),
        timeout: Duration::from_secs(positive_criterion(&values, "active_timeout_seconds")?),
    };
    if exposure.min_duration > exposure.timeout {
        return Err(StepError::InvalidArgument {
            message: "minimum active duration exceeds timeout".to_owned(),
        });
    }
    run_active_window(world, exposure, workload).await
}

#[expect(
    clippy::too_many_lines,
    reason = "Collects one bounded active run and preserves all observations in one report"
)]
#[expect(
    clippy::cognitive_complexity,
    reason = "The active sample loop keeps related safety and liveness evidence synchronized"
)]
pub(super) async fn run_active_window(
    world: &mut CucumberWorld,
    exposure: ExposureCriteria,
    workload: WorkloadCriteria,
) -> StepResult {
    let declared_workload = audit_workload(world)
        .cloned()
        .map_err(|message| StepError::InvalidArgument { message })?;
    let participants = audit_participants(world)
        .map_err(|message| StepError::InvalidArgument { message })?
        .clone();
    let producer = participants.producer.as_str();
    let honest_nodes = participants
        .honest_nodes
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let canonical_reference = participants.canonical_reference.as_str();
    let scenario = "Honest nodes converge after a sustained nonempty sibling attack";
    let artifact_dir = world.lifecycle.scenario_base_dir.clone();
    fs::create_dir_all(&artifact_dir).map_err(|error| StepError::StepFail {
        message: format!("cannot create {}: {error}", artifact_dir.display()),
    })?;
    let artifact_path = artifact_dir.join("active-observations.json");
    let window_limit = exposure.timeout;
    let expected_siblings =
        audit_sibling_count(world).map_err(|message| StepError::InvalidArgument { message })?;
    if expected_siblings == 0 {
        return Err(StepError::InvalidArgument {
            message: "producer sibling configuration contradicts experiment mode".to_owned(),
        });
    }
    let PreparedActiveEnvironment {
        mut provenance,
        configuration,
        readiness,
        mut log_tail,
    } = prepare_active_environment(
        world,
        ActiveEnvironmentRequest {
            scenario,
            expected_siblings,
            artifact_path: &artifact_path,
            workload,
        },
    )
    .await?;
    let mut chain = ActiveChainState {
        header_caches: node_names(&participants)
            .into_iter()
            .map(|name| (name.to_owned(), HeaderCache::default()))
            .collect(),
        ..ActiveChainState::default()
    };
    let mut log_observations = ActiveLogObservations::default();
    let pre_window_observations = sample_all_nodes(
        world,
        &participants,
        None,
        &mut chain.last_verified_libs,
        &mut chain.header_caches,
    )
    .await;
    let pre_window_snapshots = pre_window_observations
        .iter()
        .map(|observation| observation.snapshot.clone())
        .collect::<Vec<_>>();
    let baseline_history_checks = check_finalized_history_agreement(
        world,
        &honest_observations(&pre_window_observations, &participants),
        &mut chain.header_caches,
        0,
    )
    .await;
    let mut safety = FinalizedSafety::default();
    safety.record_pair_reports(&baseline_history_checks);
    for observation in &pre_window_observations {
        if observation.consensus.is_some()
            && (!observation.tip_path_complete || !observation.tip_descends_from_lib())
        {
            safety.ancestry_unverified = true;
        }
    }
    for observation in &pre_window_observations {
        if let Some(state) = observation.consensus.as_ref() {
            let name = observation.node_name.as_str();
            chain
                .last_verified_libs
                .insert(name.to_owned(), (state.lib_id, state.lib_slot));
            chain
                .last_tips
                .insert(name.to_owned(), tip_from_observation(observation));
        }
    }
    if let Err(error) = world.ensure_wallet_scanner_started().await {
        let report = json!({
            "status": "workload_setup_failed",
            "failure_classification": "workload_or_wallet_setup",
            "failure": error.to_string(),
            "scenario": scenario,
            "provenance": provenance,
            "blend_readiness": readiness,
            "pre_window_consensus_snapshots": pre_window_snapshots,
        });
        write_json(&artifact_path, &report)?;
        return Err(StepError::StepFail {
            message: format!(
                "wallet scanner failed; evidence: {}",
                artifact_path.display()
            ),
        });
    }

    let mut submitted_transactions = Vec::new();
    let mut failed_preparations = Vec::new();
    let mut failed_submissions = Vec::new();
    let mut stop_replenishing = false;
    for index in 1..=workload.initial_pending {
        if submit_workload_transaction(
            world,
            index,
            &mut submitted_transactions,
            &mut failed_preparations,
            &mut failed_submissions,
        )
        .await
        .is_err()
        {
            stop_replenishing = true;
            break;
        }
    }

    let active_started_at = unix_timestamp_ms();
    let active_started = StdInstant::now();
    let active_deadline = active_started + window_limit;
    let active_deadline_tokio = TokioInstant::now() + window_limit;
    let mut next_submission = active_started + workload.interval;
    let mut snapshots = Vec::new();
    let mut mempool_observations = Vec::new();
    let mut finalized_history_checks = vec![json!({
        "phase": "pre_attack_baseline",
        "observed_at_unix_ms": unix_now_ms(),
        "pairs": baseline_history_checks,
    })];
    chain.ancestry_gaps = pre_window_observations
        .iter()
        .filter(|observation| {
            observation.consensus.is_some()
                && (!observation.tip_path_complete || !observation.tip_descends_from_lib())
        })
        .map(|observation| observation.snapshot.clone())
        .collect::<Vec<_>>();
    let mut alignment_transitions = Vec::new();
    let mut previous_honest_alignment = None;
    let mut last_sample_at = StdInstant::now();
    chain.last_tip_progress = node_names(&participants)
        .into_iter()
        .map(|name| (name.to_owned(), active_started_at))
        .collect::<BTreeMap<_, _>>();
    chain.last_lib_progress = chain.last_tip_progress.clone();
    let mut sample_error_count = 0usize;
    let mut cooldown_start: Option<NodeTip> = None;
    let mut cooldown_verified_blocks = 0u64;
    let mut cooldown_events = Vec::new();
    let mut exposure_observations = Vec::new();
    let mut observed_ordinary_bodies = BTreeMap::<String, Value>::new();
    let mut exposure_requirements_met = false;
    let mut next_exposure_check = active_started;
    let mut canonical_start = None;
    let mut canonical_end: Option<NodeTip> = None;
    let mut canonical_blocks = 0u64;
    let mut canonical_status = "unverified";
    let mut canonical_error: Option<String> = None;
    let mut canonical_ancestor_at_start: Option<String> = None;
    let mut canonical_descends_from_start: Option<bool> = None;

    while StdInstant::now() < active_deadline {
        if !stop_replenishing
            && submitted_transactions.len() + failed_preparations.len() + failed_submissions.len()
                < workload.budget
            && StdInstant::now() >= next_submission
        {
            let index = submitted_transactions.len()
                + failed_preparations.len()
                + failed_submissions.len()
                + 1;
            if submit_workload_transaction(
                world,
                index,
                &mut submitted_transactions,
                &mut failed_preparations,
                &mut failed_submissions,
            )
            .await
            .is_err()
            {
                stop_replenishing = true;
            }
            next_submission += workload.interval;
        }

        sample_error_count += log_observations.read_nodes(&mut log_tail, &participants, false);
        let (latest_attack, evidence_errors) = log_observations.update_opportunities(producer);
        sample_error_count += evidence_errors;

        let observations = timeout_at(
            active_deadline_tokio,
            sample_all_nodes(
                world,
                &participants,
                latest_attack.as_ref(),
                &mut chain.last_verified_libs,
                &mut chain.header_caches,
            ),
        )
        .await
        .unwrap_or_default();
        if observations.is_empty() {
            break;
        }
        let observed_at_unix_ms = unix_timestamp_ms();
        let elapsed_at_ms = active_started.elapsed().as_millis();
        let (node_snapshot_values, unresponsive_count) = chain
            .record_observations(
                world,
                &observations,
                observed_at_unix_ms,
                active_started_at,
                &log_observations.attack_candidates,
                latest_attack.is_some(),
                &mut safety,
            )
            .await;
        sample_error_count += unresponsive_count;
        let honest_observations = honest_observations(&observations, &participants);
        let history_checks = check_finalized_history_agreement(
            world,
            &honest_observations,
            &mut chain.header_caches,
            latest_attack.as_ref().map_or(0, |attack| attack.slot),
        )
        .await;
        safety.record_pair_reports(&history_checks);
        finalized_history_checks.push(json!({
            "observed_at_unix_ms": observed_at_unix_ms,
            "pairs": history_checks,
        }));
        let alignment = align_observations(
            world,
            &observations,
            &mut chain.header_caches,
            &honest_nodes,
            workload.max_tip_height_skew,
        )
        .await;
        let aligned = alignment["aligned"].as_bool() == Some(true);
        if previous_honest_alignment.is_some_and(|previous| previous != aligned) {
            alignment_transitions.push(json!({
                "observed_at_unix_ms": observed_at_unix_ms,
                "aligned": aligned,
                "details": alignment,
            }));
        }
        previous_honest_alignment = Some(aligned);
        let states = observations
            .iter()
            .filter_map(|observation| {
                Some((
                    observation.node_name.clone(),
                    observation.consensus.clone()?,
                ))
            })
            .collect::<Vec<_>>();
        let common_lib = common_finalized_lib(&states, node_names(&participants).len());
        let round = json!({
            "observed_at_unix_ms": observed_at_unix_ms,
            "elapsed_ms": elapsed_at_ms,
            "nodes": node_snapshot_values,
            "honest_tip_alignment": alignment,
            "all_node_common_lib": common_lib.map(|(id, slot)| json!({
                "lib_id": id.encode_hex::<String>(),
                "lib_slot": slot,
            })),
            "finalized_history_pairs": finalized_history_checks.last(),
        });
        snapshots.push(round);
        let mempool_round = sample_mempools(world, &participants, &submitted_transactions).await;
        mempool_observations.push(json!({
            "observed_at_unix_ms": observed_at_unix_ms,
            "elapsed_ms": elapsed_at_ms,
            "nodes": mempool_round,
        }));
        last_sample_at = StdInstant::now();

        if let Some(observation) = observations
            .iter()
            .find(|item| item.node_name == canonical_reference && item.consensus.is_some())
        {
            let tip = tip_from_observation(observation);
            canonical_end = Some(tip.clone());
            if canonical_start.is_none() {
                canonical_ancestor_at_start = Some(tip.id_text.clone());
                canonical_descends_from_start = Some(true);
                canonical_status = "verified";
                canonical_start = Some(tip.clone());
            }
            if let Some(start) = canonical_start.as_ref() {
                if tip.height < start.height {
                    canonical_status = "unverified";
                    canonical_descends_from_start = Some(false);
                    canonical_error = Some("canonical tip is below the initial height".to_owned());
                } else if let (Some(node), Some(cache)) = (
                    world.nodes_info.get(canonical_reference),
                    chain.header_caches.get_mut(canonical_reference),
                ) {
                    match timeout_at(
                        active_deadline_tokio,
                        block_id_at_height(&node.client, tip.id, tip.height, start.height, cache),
                    )
                    .await
                    {
                        Ok(Ok(ancestor)) => {
                            canonical_ancestor_at_start = Some(ancestor.encode_hex::<String>());
                            let descends = ancestor == start.id;
                            canonical_descends_from_start = Some(descends);
                            if descends {
                                canonical_status = "verified";
                                canonical_blocks = tip.height - start.height;
                                canonical_error = None;
                            } else {
                                canonical_status = "unverified";
                                canonical_error = Some(
                                    "initial tip is not an ancestor of the observed canonical tip"
                                        .to_owned(),
                                );
                            }
                        }
                        Ok(Err(error)) => {
                            canonical_status = "unverified";
                            canonical_descends_from_start = None;
                            canonical_error = Some(error);
                        }
                        Err(_) => break,
                    }
                } else {
                    canonical_status = "unverified";
                    canonical_descends_from_start = None;
                    canonical_error =
                        Some("canonical node or ancestry cache was unavailable".to_owned());
                }
            }
        } else {
            canonical_status = "unverified";
            canonical_error = Some(format!("{canonical_reference} did not respond"));
            canonical_descends_from_start = None;
        }

        {
            if cooldown_start.is_none() && StdInstant::now() >= next_exposure_check {
                let exposure = timeout_at(
                    active_deadline_tokio,
                    observe_active_exposure(
                        world,
                        &participants,
                        &log_observations.opportunities,
                        &log_observations.nodes,
                        &submitted_transactions,
                        &mut observed_ordinary_bodies,
                        exposure,
                    ),
                )
                .await;
                match exposure {
                    Ok(exposure) => {
                        exposure_requirements_met = exposure.requirements_met;
                        exposure_observations.push(exposure);
                    }
                    Err(_) => break,
                }
                next_exposure_check = StdInstant::now() + Duration::from_secs(15);
            }
            if cooldown_start.is_none()
                && exposure_requirements_met
                && active_started.elapsed() >= exposure.min_duration
                && let Some(tip) = chain.last_tips.get(canonical_reference).cloned()
            {
                cooldown_events.push(json!({"event": "cooldown_started", "tip_id": tip.id_text, "height": tip.height, "observed_at_unix_ms": observed_at_unix_ms}));
                cooldown_start = Some(tip);
            }
            if let (Some(start), Some(tip), Some(node), Some(cache)) = (
                cooldown_start.as_ref(),
                chain.last_tips.get(canonical_reference),
                world.nodes_info.get(canonical_reference),
                chain.header_caches.get_mut(canonical_reference),
            ) {
                if tip.height < start.height {
                    cooldown_events.push(json!({"event": "cooldown_reset", "reason": "tip_height_regressed", "previous_start": start.id_text, "new_start": tip.id_text, "observed_at_unix_ms": observed_at_unix_ms}));
                    cooldown_start = Some(tip.clone());
                    cooldown_verified_blocks = 0;
                } else {
                    match timeout_at(
                        active_deadline_tokio,
                        block_id_at_height(&node.client, tip.id, tip.height, start.height, cache),
                    )
                    .await
                    {
                        Ok(Ok(ancestor)) if ancestor == start.id => {
                            cooldown_verified_blocks = tip.height - start.height;
                        }
                        Ok(Ok(_)) => {
                            cooldown_events.push(json!({"event": "cooldown_reset", "reason": "start_not_on_canonical_chain", "previous_start": start.id_text, "new_start": tip.id_text, "observed_at_unix_ms": observed_at_unix_ms}));
                            cooldown_start = Some(tip.clone());
                            cooldown_verified_blocks = 0;
                        }
                        Ok(Err(error)) => {
                            cooldown_events.push(json!({"event": "cooldown_ancestry_unverified", "error": error, "observed_at_unix_ms": observed_at_unix_ms}));
                            cooldown_verified_blocks = 0;
                        }
                        Err(_) => break,
                    }
                }
                if cooldown_verified_blocks >= exposure.cooldown_blocks {
                    cooldown_events.push(json!({"event": "cooldown_completed", "tip_id": tip.id_text, "height": tip.height, "verified_blocks": cooldown_verified_blocks, "observed_at_unix_ms": observed_at_unix_ms}));
                    break;
                }
            }
        }

        let next_tick = (active_started
            + Duration::from_millis(
                u64::try_from(snapshots.len())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(2_000),
            ))
        .min(active_deadline);
        if next_tick > StdInstant::now() {
            sleep(next_tick.saturating_duration_since(StdInstant::now())).await;
        }
    }

    let active_ended_at = unix_timestamp_ms();
    let active_duration_ms = active_started.elapsed().as_millis();
    sample_error_count += log_observations.read_nodes(&mut log_tail, &participants, true);
    let ActiveLogObservations {
        nodes: node_logs,
        opportunities,
        attack_candidates: mut attack_branch_candidates,
        read_failures: log_read_failures,
    } = log_observations;
    let OpportunitySummary {
        records: opportunity_records,
        indexed: attack_opportunity_records,
        nonempty: nonempty_opportunities,
        nonempty_submitted: nonempty_submitted_body_opportunities,
        delivered: delivered_opportunities,
    } = summarize_opportunities(OpportunitySummaryContext {
        world,
        participants: &participants,
        opportunities: &opportunities,
        node_logs: &node_logs,
        observed_ordinary_bodies: &observed_ordinary_bodies,
        submitted_transactions: &submitted_transactions,
        last_tips: &chain.last_tips,
        last_verified_libs: &chain.last_verified_libs,
        header_caches: &mut chain.header_caches,
        attack_branch_candidates: &mut attack_branch_candidates,
    })
    .await;
    if !opportunity_records.is_empty() {
        record_observed_audit_opportunity(&mut provenance, opportunity_records.len());
    }
    for node in node_names(&participants) {
        if let Some(started_at) = chain.last_tip_progress.get(node) {
            let duration_ms = active_ended_at.saturating_sub(*started_at);
            if duration_ms > 0 {
                chain.tip_stall_intervals.push(json!({
                    "node": node,
                    "started_at_unix_ms": started_at,
                    "ended_at_unix_ms": active_ended_at,
                    "duration_ms": duration_ms,
                    "right_censored_at_active_window_end": true,
                }));
            }
        }
        if let Some(started_at) = chain.last_lib_progress.get(node) {
            let duration_ms = active_ended_at.saturating_sub(*started_at);
            if duration_ms > 0 {
                chain.lib_stall_intervals.push(json!({
                    "node": node,
                    "started_at_unix_ms": started_at,
                    "ended_at_unix_ms": active_ended_at,
                    "duration_ms": duration_ms,
                    "right_censored_at_active_window_end": true,
                }));
            }
        }
    }
    let attacked_opportunity_count = opportunity_records.len();
    let workload_setup_failure =
        submitted_transactions.is_empty() && !failed_preparations.is_empty();
    let insufficient_exposure = attacked_opportunity_count < exposure.min_opportunities
        || nonempty_opportunities < exposure.min_nonempty
        || nonempty_submitted_body_opportunities < exposure.min_nonempty
        || delivered_opportunities.len() < exposure.min_delivered;
    let cooldown_incomplete = cooldown_verified_blocks < exposure.cooldown_blocks;
    let duration_incomplete = active_duration_ms < exposure.min_duration.as_millis();
    let requirements_met = sustained_requirements_met(
        exposure,
        attacked_opportunity_count,
        nonempty_opportunities,
        nonempty_submitted_body_opportunities,
        delivered_opportunities.len(),
        cooldown_verified_blocks,
        Duration::from_millis(u64::try_from(active_duration_ms).unwrap_or(u64::MAX)),
    );
    let active_window = json!({
        "started_at_unix_ms": active_started_at,
        "ended_at_unix_ms": active_ended_at,
        "duration_ms": active_duration_ms,
        "minimum_duration_ms": exposure.min_duration.as_millis(),
        "maximum_duration_ms": window_limit.as_millis(),
        "canonical_blocks": {
            "node": canonical_reference, "status": canonical_status,
            "start_tip_id": canonical_start.as_ref().map(|tip| &tip.id_text),
            "start_height": canonical_start.as_ref().map(|tip| tip.height),
            "end_tip_id": canonical_end.as_ref().map(|tip| &tip.id_text),
            "end_height": canonical_end.as_ref().map(|tip| tip.height),
            "verified_blocks": (canonical_status == "verified").then_some(canonical_blocks),
            "ancestry_to_initial_tip": {
                "status": match canonical_descends_from_start {
                    Some(true) => "verified",
                    Some(false) => "mismatched",
                    None => "unverified",
                },
                "initial_tip_id": canonical_start.as_ref().map(|tip| &tip.id_text),
                "observed_ancestor_id": canonical_ancestor_at_start,
                "initial_tip_is_ancestor": canonical_descends_from_start,
                "evidence_source": "verified block_id_at_height lookup on canonical reference tip",
            },
            "error": canonical_error,
        },
        "exposure_observations": exposure_observations,
        "cooldown": {"required_blocks": exposure.cooldown_blocks, "verified_blocks": cooldown_verified_blocks, "verified": cooldown_verified_blocks >= exposure.cooldown_blocks, "events": cooldown_events},
        "sample_interval_ms": SAMPLE_INTERVAL.as_millis(),
        "sample_count": snapshots.len(),
    });
    let workload = json!({
        "budget": workload.budget,
        "transfer_amount": declared_workload.amount,
        "sender_wallet": declared_workload.sender,
        "receiver_wallet": declared_workload.receiver,
        "initial_pending_transactions": workload.initial_pending,
        "submission_interval_ms": workload.interval.as_millis(),
        "submitted_count": submitted_transactions.len(),
        "submitted_transactions": submitted_transactions,
        "failed_preparations": failed_preparations,
        "failed_submissions": failed_submissions,
        "funding_exhausted_or_replenishment_stopped": stop_replenishing,
    });
    let attack_exposure = json!({
        "complete_genuine_opportunities": attacked_opportunity_count,
        "required_complete_opportunities": exposure.min_opportunities,
        "nonempty_ordinary_bodies": nonempty_opportunities,
        "required_nonempty_opportunities": exposure.min_nonempty,
        "nonempty_bodies_containing_submitted_transactions": nonempty_submitted_body_opportunities,
        "required_submitted_transaction_body_opportunities": exposure.min_nonempty,
        "distinct_opportunities_with_ordered_blend_to_acceptance": delivered_opportunities.len(),
        "required_delivered_opportunities": exposure.min_delivered,
        "insufficient_exposure": insufficient_exposure,
    });
    let liveness_metrics = json!({
        "per_node_tip_advance_counts": chain.per_node_tip_advance_counts,
        "per_node_lib_advance_counts": chain.per_node_lib_advance_counts,
        "unique_observed_tip_blocks": chain.unique_tip_blocks.len(),
        "tip_stall_intervals": chain.tip_stall_intervals,
        "lib_stall_intervals": chain.lib_stall_intervals,
        "tip_alignment_transitions": alignment_transitions,
    });
    let safety_metrics = json!({
        "safety_conflict_observed": safety.conflict_observed,
        "ancestry_unverified": safety.ancestry_unverified,
        "lib_continuity_regressions": chain.continuity_regressions,
        "lib_continuity_unverified_observations": chain.continuity_unverified,
        "ancestry_gaps": chain.ancestry_gaps,
        "pre_attack_finalized_history_checks": finalized_history_checks.first(),
        "finalized_history_checks": finalized_history_checks,
    });
    let reorganization_metrics = json!({
        "tip_transition_observations": chain.tip_transitions,
        "tip_reorganizations": chain.tip_reorganizations,
        "tip_reorganizations_unverified": chain.tip_reorganizations_unverified,
        "tip_reorganization_observation_is_lower_bound": true,
    });
    let baseline_and_final = json!({
        "pre_window_consensus_snapshots": pre_window_snapshots,
        "last_active_tips": chain.last_tips.iter().map(|(node, tip)| json!({
            "node": node,
            "tip_id": tip.id_text,
            "tip_height": tip.height,
            "tip_slot": tip.slot,
            "observed_at_unix_ms": tip.observed_at_unix_ms,
        })).collect::<Vec<_>>(),
        "last_verified_libs": chain.last_verified_libs.iter().map(|(node, (id, slot))| json!({
            "node": node,
            "lib_id": id.encode_hex::<String>(),
            "lib_slot": slot,
        })).collect::<Vec<_>>(),
    });
    let cache_summary = chain
        .header_caches
        .iter()
        .map(|(node, cache)| {
            json!({
                "node": node,
                "cached_headers": cache.links.len(),
                "lookup_failures": cache.failures,
            })
        })
        .collect::<Vec<_>>();
    let report = json!({
        "status": if workload_setup_failure { "workload_setup_failed" } else if insufficient_exposure { "insufficient_exposure" } else if cooldown_incomplete { "cooldown_incomplete" } else if duration_incomplete { "minimum_duration_incomplete" } else { "active_window_observed" },
        "workload_setup_failure": workload_setup_failure,
        "workload_setup_failure_classification": workload_setup_failure.then_some("sender_wallet_unavailable_or_unfunded"),
        "failure_classification": if workload_setup_failure { Some("workload_setup_failed") } else if insufficient_exposure { Some("insufficient_exposure") } else if cooldown_incomplete { Some("canonical_block_cooldown_not_verified") } else if duration_incomplete { Some("minimum_duration_incomplete") } else { None },
        "scenario": scenario,
        "experiment_mode": "sustained_equivocation",
        "source_commit": provenance["commit_sha"],
        "provenance": provenance,
        "consensus_configuration": configuration,
        "blend_readiness": readiness,
        "active_window": active_window,
        "baseline_and_final": baseline_and_final,
        "pre_attack_consensus_snapshots": pre_window_snapshots,
        "snapshots": snapshots,
        "mempool_observations": mempool_observations,
        "transaction_workload": workload,
        "attack_exposure": attack_exposure,
        "opportunities": opportunity_records,
        "liveness_metrics": liveness_metrics,
        "reorganization_metrics": reorganization_metrics,
        "safety_metrics": safety_metrics,
        "sample_error_count": sample_error_count,
        "log_read_failures": log_read_failures,
        "last_sample_at_unix_ms": unix_timestamp_ms().saturating_sub(
            u64::try_from(last_sample_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        ),
        "producer_opportunity_count_at_window_end": attacked_opportunity_count,
        "header_cache_summary": cache_summary,
        "artifact_path": artifact_path,
    });
    write_json(&artifact_path, &report)?;
    let opportunities_path = artifact_dir.join("producer-opportunities.json");
    write_json(
        &opportunities_path,
        &json!({
            "attack_opportunities": attack_opportunity_records.into_values().collect::<Vec<_>>(),
            "producer_log_source": world.nodes_info.get(producer).map(|node| node.runtime_dir.display().to_string()),
        }),
    )?;
    if workload_setup_failure || !requirements_met {
        return Err(StepError::StepFail {
            message: format!(
                "sustained attack exposure or cooldown was not verified; evidence: {}",
                artifact_path.display()
            ),
        });
    }
    Ok(())
}
