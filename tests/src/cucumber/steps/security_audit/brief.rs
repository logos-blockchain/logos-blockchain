use super::*;

pub(super) async fn capture_pre_attack_consensus(world: &CucumberWorld) -> Value {
    let Ok(participants) = audit_participants(world) else {
        return json!([]);
    };
    let observations = join_all(participants.honest_nodes.iter().map(async |node_name| {
        let Some(node) = world.nodes_info.get(node_name) else {
            return json!({
                "node": node_name,
                "observed_at_unix_ms": unix_timestamp_ms(),
                "responsive": false,
                "error": "node was not started",
            });
        };

        let observed_at_unix_ms = unix_timestamp_ms();
        let consensus = match node.client.consensus_info().await {
            Ok(consensus) => consensus,
            Err(error) => {
                return json!({
                    "node": node_name,
                    "observed_at_unix_ms": observed_at_unix_ms,
                    "responsive": false,
                    "error": error.to_string(),
                });
            }
        };
        let info = &consensus.cryptarchia_info;
        let mut cache = HeaderCache::default();
        let path = walk_to_target(
            &node.client,
            info.tip,
            info.lib,
            u64::from(info.lib_slot),
            &mut cache,
        )
        .await;
        let descends_from_lib = ancestry_presence(&path, info.lib);

        json!({
            "node": node_name,
            "observed_at_unix_ms": observed_at_unix_ms,
            "responsive": true,
            "phase": format!("{:?}", consensus.phase),
            "tip_id": info.tip.encode_hex::<String>(),
            "tip_height": info.height,
            "tip_slot": u64::from(info.slot),
            "lib_id": info.lib.encode_hex::<String>(),
            "lib_slot": u64::from(info.lib_slot),
            "tip_descends_from_lib": descends_from_lib,
            "tip_to_lib_ancestry": ancestry_path_json(&path),
            "header_lookup_failures": header_cache_failures_json(&cache),
        })
    }))
    .await;
    Value::Array(observations)
}

pub(super) fn classify_attack_branch(
    path: &[HeaderLink],
    complete_below_attack: bool,
    attack: &AttackEvidence,
) -> AttackBranch {
    if let Some(link) = path
        .iter()
        .find(|link| (link.slot, link.parent) == (attack.slot, attack.parent_id))
    {
        if link.id == attack.ordinary_id {
            return AttackBranch::Ordinary;
        }
        if let Some((index, _)) = attack
            .siblings
            .iter()
            .find(|(_, candidate)| candidate.id == link.id)
        {
            return AttackBranch::Sibling(*index);
        }
        return AttackBranch::OtherCompeting(link.id);
    }
    if complete_below_attack {
        AttackBranch::None
    } else {
        AttackBranch::Unverified
    }
}

pub(super) fn attack_branch_json(branch: &AttackBranch, attack: &AttackEvidence) -> Value {
    match branch {
        AttackBranch::Ordinary => json!({
            "kind": "ordinary",
            "block_id": attack.ordinary_id_text,
        }),
        AttackBranch::Sibling(index) => json!({"kind": "sibling", "sibling_index": index}),
        AttackBranch::OtherCompeting(id) => json!({
            "kind": "other_competing_block",
            "block_id": id.encode_hex::<String>(),
        }),
        AttackBranch::None => json!({"kind": "none_of_selected_equivocation_blocks"}),
        AttackBranch::Unverified => json!({"kind": "unverified"}),
    }
}

pub(super) fn canonical_tips_aligned(
    height_skew: u64,
    block_ids_at_common_height: &[HeaderId],
    max_tip_height_skew: u64,
) -> bool {
    height_skew <= max_tip_height_skew
        && block_ids_at_common_height
            .first()
            .is_some_and(|first| block_ids_at_common_height.iter().all(|id| id == first))
}

pub(super) fn common_finalized_lib(
    states: &[(String, ConsensusState)],
    expected_nodes: usize,
) -> Option<(HeaderId, u64)> {
    let (_, first) = states.first()?;
    (states.len() == expected_nodes
        && states
            .iter()
            .all(|(_, state)| state.lib_id == first.lib_id && state.lib_slot == first.lib_slot))
    .then_some((first.lib_id, first.lib_slot))
}

pub(super) fn classify_finalized_history_relation(
    first_id: HeaderId,
    first_slot: u64,
    second_id: HeaderId,
    second_slot: u64,
    first_descends_from_second: Option<bool>,
    second_descends_from_first: Option<bool>,
) -> FinalizedHistoryRelation {
    if first_id == second_id {
        return if first_slot == second_slot {
            FinalizedHistoryRelation::Shared
        } else {
            FinalizedHistoryRelation::Unverified
        };
    }
    if first_descends_from_second == Some(true) || second_descends_from_first == Some(true) {
        FinalizedHistoryRelation::Ancestral
    } else if first_descends_from_second == Some(false) && second_descends_from_first == Some(false)
    {
        FinalizedHistoryRelation::Conflicting
    } else {
        FinalizedHistoryRelation::Unverified
    }
}

pub(super) fn classify_tip_transition(
    previous_tip_id: HeaderId,
    new_tip_id: HeaderId,
    new_tip_descends_from_previous: Option<bool>,
) -> TipTransition {
    if previous_tip_id == new_tip_id {
        TipTransition::Unchanged
    } else {
        match new_tip_descends_from_previous {
            Some(true) => TipTransition::Descendant,
            Some(false) => TipTransition::Reorganization,
            None => TipTransition::Unverified,
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Each LIB pair is classified with symmetric ancestry lookups and evidence"
)]
pub(super) async fn check_finalized_history_agreement(
    world: &CucumberWorld,
    observations: &[FinalityNodeObservation],
    caches: &mut BTreeMap<String, HeaderCache>,
    attack_slot: u64,
) -> Vec<FinalizedHistoryCheck> {
    let mut checks = Vec::new();
    for first_index in 0..observations.len() {
        for second_index in first_index + 1..observations.len() {
            let (Some(first), Some(second)) = (
                observations[first_index].consensus.as_ref(),
                observations[second_index].consensus.as_ref(),
            ) else {
                continue;
            };
            let first_name = observations[first_index].node_name.as_str();
            let second_name = observations[second_index].node_name.as_str();
            let (first_to_second, second_to_first) = if first.lib_id == second.lib_id {
                (None, None)
            } else {
                let (Some(first_node), Some(second_node)) = (
                    world.nodes_info.get(first_name),
                    world.nodes_info.get(second_name),
                ) else {
                    checks.push(FinalizedHistoryCheck {
                        relation: FinalizedHistoryRelation::Unverified,
                        evidence: json!({
                            "first_node": first_name,
                            "first_lib_id": first.lib_id_text,
                            "first_lib_slot": first.lib_slot,
                            "second_node": second_name,
                            "second_lib_id": second.lib_id_text,
                            "second_lib_slot": second.lib_slot,
                            "status": "ancestry_unverified",
                            "attack_slot": attack_slot,
                            "error": "node client was unavailable for finalized-history lookup",
                        }),
                    });
                    continue;
                };
                let first_to_second = match caches.get_mut(first_name) {
                    Some(cache) => {
                        walk_between_finalized_blocks(
                            &first_node.client,
                            first.lib_id,
                            first.lib_slot,
                            second.lib_id,
                            second.lib_slot,
                            cache,
                        )
                        .await
                    }
                    None => AncestryPath {
                        links: Vec::new(),
                        target_reached: false,
                        complete_below_slot: false,
                        error: Some(format!("no ancestry cache allocated for {first_name}")),
                    },
                };
                let second_to_first = match caches.get_mut(second_name) {
                    Some(cache) => {
                        walk_between_finalized_blocks(
                            &second_node.client,
                            second.lib_id,
                            second.lib_slot,
                            first.lib_id,
                            first.lib_slot,
                            cache,
                        )
                        .await
                    }
                    None => AncestryPath {
                        links: Vec::new(),
                        target_reached: false,
                        complete_below_slot: false,
                        error: Some(format!("no ancestry cache allocated for {second_name}")),
                    },
                };
                (Some(first_to_second), Some(second_to_first))
            };
            let first_descends_from_second = first_to_second.as_ref().map_or_else(
                || (first.lib_id == second.lib_id).then_some(true),
                |path| ancestry_presence(path, second.lib_id),
            );
            let second_descends_from_first = second_to_first.as_ref().map_or_else(
                || (first.lib_id == second.lib_id).then_some(true),
                |path| ancestry_presence(path, first.lib_id),
            );
            let relation = classify_finalized_history_relation(
                first.lib_id,
                first.lib_slot,
                second.lib_id,
                second.lib_slot,
                first_descends_from_second,
                second_descends_from_first,
            );
            let status = match relation {
                FinalizedHistoryRelation::Shared => "shared_finalized_history",
                FinalizedHistoryRelation::Ancestral => "ancestral_finalized_histories",
                FinalizedHistoryRelation::Conflicting => "conflicting_finalized_histories",
                FinalizedHistoryRelation::Unverified => "ancestry_unverified",
            };
            checks.push(FinalizedHistoryCheck {
                relation,
                evidence: json!({
                    "first_node": first_name,
                    "first_lib_id": first.lib_id_text,
                    "first_lib_slot": first.lib_slot,
                    "second_node": second_name,
                    "second_lib_id": second.lib_id_text,
                    "second_lib_slot": second.lib_slot,
                    "first_lib_descends_from_second": first_descends_from_second,
                    "second_lib_descends_from_first": second_descends_from_first,
                    "status": status,
                    "attack_slot": attack_slot,
                    "first_to_second_ancestry": first_to_second.as_ref().map(ancestry_path_json),
                    "second_to_first_ancestry": second_to_first.as_ref().map(ancestry_path_json),
                }),
            });
        }
    }
    checks
}
