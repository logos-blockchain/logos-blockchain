use lb_core::header::HeaderId;

use super::*;

const MAX_CONVERGENCE_ANCESTOR_WALK: u64 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeConsensusSnapshot {
    pub node_name: String,
    pub height: u64,
    pub tip: HeaderId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalConvergence {
    pub minimum_height: u64,
    pub maximum_height: u64,
    pub height_difference: u64,
    pub allowed_height_difference: u64,
    pub common_ancestor: HeaderId,
    pub node_tips: Vec<NodeConsensusSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeightRange {
    minimum: u64,
    maximum: u64,
}

impl HeightRange {
    const fn difference(self) -> u64 {
        self.maximum - self.minimum
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AncestorFetchError {
    Retryable(String),
    Invalid(String),
}

fn height_range(snapshots: &[NodeConsensusSnapshot]) -> Result<HeightRange, &'static str> {
    let minimum = snapshots
        .iter()
        .map(|snapshot| snapshot.height)
        .min()
        .ok_or("no fresh node consensus observations")?;
    let maximum = snapshots
        .iter()
        .map(|snapshot| snapshot.height)
        .max()
        .ok_or("no fresh node consensus observations")?;
    Ok(HeightRange { minimum, maximum })
}

fn height_requirements_met(
    range: HeightRange,
    required_minimum: Option<u64>,
    allowed_difference: u64,
) -> bool {
    range.difference() <= allowed_difference
        && required_minimum.is_none_or(|minimum| range.minimum >= minimum)
}

fn all_ancestors_match(ancestors: &[(String, HeaderId)]) -> bool {
    ancestors
        .first()
        .is_some_and(|(_, first)| ancestors.iter().all(|(_, id)| id == first))
}

fn complete_snapshot_set(
    results: Vec<Result<NodeConsensusSnapshot, (String, String)>>,
) -> Result<Vec<NodeConsensusSnapshot>, Vec<(String, String)>> {
    let mut snapshots = Vec::with_capacity(results.len());
    let mut failures = Vec::new();
    for result in results {
        match result {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(failure) => failures.push(failure),
        }
    }
    if failures.is_empty() {
        Ok(snapshots)
    } else {
        Err(failures)
    }
}

async fn ancestor_at_height<Id, Fetch, FetchFuture>(
    tip: Id,
    distance: u64,
    mut fetch: Fetch,
) -> Result<Id, AncestorFetchError>
where
    Id: Clone + Eq + ::std::hash::Hash + ::std::fmt::Debug,
    Fetch: FnMut(Id) -> FetchFuture,
    FetchFuture: Future<Output = Result<Option<(Id, u64)>, AncestorFetchError>>,
{
    if distance > MAX_CONVERGENCE_ANCESTOR_WALK {
        return Err(AncestorFetchError::Invalid(format!(
            "Convergence ancestry walk of {distance} blocks exceeds its internal safety limit"
        )));
    }

    if distance == 0 {
        return Ok(tip);
    }

    let mut current = tip;
    let mut child_slot = None;
    let mut visited = HashSet::new();
    for traversed in 0..=distance {
        if !visited.insert(current.clone()) {
            return Err(AncestorFetchError::Invalid(format!(
                "Convergence ancestry contains a cycle at block {current:?}"
            )));
        }
        let (parent, slot) = fetch(current.clone()).await?.ok_or_else(|| {
            AncestorFetchError::Retryable(format!(
                "Canonical ancestor {current:?} is temporarily unavailable"
            ))
        })?;
        if child_slot.is_some_and(|child| slot >= child) {
            return Err(AncestorFetchError::Invalid(format!(
                "Convergence ancestor {current:?} has slot {slot}, not strictly before its child at slot {}",
                child_slot.expect("checked as Some")
            )));
        }
        if traversed == distance {
            return Ok(current);
        }
        current = parent;
        child_slot = Some(slot);
    }
    unreachable!("inclusive ancestry walk returns at the requested height")
}

/// Wait for fresh canonical observations from every running node, then compare
/// each sampled tip's ancestor at the lowest current tip height.
#[expect(
    clippy::too_many_lines,
    reason = "The polling loop keeps complete-snapshot, ancestry, and deadline diagnostics together"
)]
pub async fn wait_for_canonical_convergence(
    world: &mut CucumberWorld,
    step: &str,
    required_minimum: Option<u64>,
    allowed_difference: u64,
    timeout_seconds: u64,
) -> Result<CanonicalConvergence, StepError> {
    let mut clients = world
        .nodes_info
        .iter()
        .map(|(name, node)| (name.clone(), node.client.clone()))
        .collect::<Vec<_>>();
    clients.sort_by(|left, right| left.0.cmp(&right.0));
    if clients.is_empty() {
        return Err(StepError::StepFail {
            message: format!("Step `{step}` error: no running nodes to compare"),
        });
    }

    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_seconds);
    let mut last_observations = HashMap::<String, String>::new();
    let mut poll_count = 0usize;

    loop {
        let poll_results = tokio::time::timeout(
            timeout.saturating_sub(start.elapsed()),
            futures_util::future::join_all(clients.iter().map(async |(name, client)| {
                match client.consensus_info().await {
                    Ok(info) => Ok(NodeConsensusSnapshot {
                        node_name: name.clone(),
                        height: info.cryptarchia_info.height,
                        tip: info.cryptarchia_info.tip,
                    }),
                    Err(error) => Err((name.clone(), error)),
                }
            })),
        )
        .await
        .map_err(|_| StepError::StepFail {
            message: format!("Step `{step}` error: consensus polling exceeded {timeout_seconds}s"),
        })?;

        let mut current_poll = Vec::with_capacity(poll_results.len());
        for result in poll_results {
            match result {
                Ok(snapshot) => {
                    let node = world.nodes_info.get_mut(&snapshot.node_name).ok_or_else(|| StepError::LogicalError {
                        message: format!("Step `{step}` error: node '{}' disappeared during consensus polling", snapshot.node_name),
                    })?;
                    node.upsert_tip(snapshot.height, snapshot.tip.encode_hex::<String>());
                    current_poll.push(Ok(snapshot));
                }
                Err((name, error)) => {
                    return Err(StepError::StepFail {
                        message: format!(
                            "Step `{step}` error: node `{name}` consensus query failed: {error}"
                        ),
                    });
                }
            }
        }
        {
            let snapshots =
                complete_snapshot_set(current_poll).map_err(|failures| StepError::StepFail {
                    message: format!(
                        "Step `{step}` error: incomplete consensus poll: {failures:?}"
                    ),
                })?;
            let range = height_range(&snapshots).map_err(|message| StepError::StepFail {
                message: format!("Step `{step}` error: {message}"),
            })?;
            if height_requirements_met(range, required_minimum, allowed_difference) {
                let ancestry_results = tokio::time::timeout(
                    timeout.saturating_sub(start.elapsed()),
                    futures_util::future::join_all(snapshots.iter().map(|snapshot| {
                        let client = clients
                            .iter()
                            .find(|(name, _)| name == &snapshot.node_name)
                            .expect("snapshot node has a client")
                            .1
                            .clone();
                        let distance = snapshot.height - range.minimum;
                        let node_name = snapshot.node_name.clone();
                        let tip = snapshot.tip;
                        async move {
                            let ancestor = ancestor_at_height(
                                tip,
                                distance,
                                async |id| match client.block(&id).await {
                                    Ok(Some(block)) => Ok(Some((
                                        block.header.parent_block,
                                        block.header.slot.into_inner(),
                                    ))),
                                    Ok(None) => Err(AncestorFetchError::Retryable(format!(
                                        "Canonical ancestor {id:?} is not available yet (HTTP 404)"
                                    ))),
                                    Err(error) => Err(AncestorFetchError::Invalid(format!(
                                        "Failed to retrieve canonical ancestor {id:?}: {error}"
                                    ))),
                                },
                            )
                            .await;
                            (node_name, ancestor)
                        }
                    })),
                )
                .await
                .map_err(|_| StepError::StepFail {
                    message: format!(
                        "Step `{step}` error: ancestry lookup exceeded {timeout_seconds}s"
                    ),
                })?;

                let mut ancestors = Vec::with_capacity(ancestry_results.len());
                let mut retry_ancestry = false;
                for (name, result) in ancestry_results {
                    match result {
                        Ok(id) => ancestors.push((name, id)),
                        Err(AncestorFetchError::Retryable(message)) => {
                            last_observations.insert(name, message);
                            retry_ancestry = true;
                        }
                        Err(AncestorFetchError::Invalid(message)) => {
                            return Err(StepError::StepFail {
                                message: format!(
                                    "Step `{step}` error: canonical ancestry lookup on node `{name}` failed: {message}"
                                ),
                            });
                        }
                    }
                }

                if !retry_ancestry {
                    if all_ancestors_match(&ancestors) {
                        return Ok(CanonicalConvergence {
                            minimum_height: range.minimum,
                            maximum_height: range.maximum,
                            height_difference: range.difference(),
                            allowed_height_difference: allowed_difference,
                            common_ancestor: ancestors[0].1,
                            node_tips: snapshots,
                        });
                    }
                    for (name, ancestor) in ancestors {
                        last_observations.insert(
                            name,
                            format!(
                                "canonical ancestor at height {} is {ancestor:?}",
                                range.minimum
                            ),
                        );
                    }
                }
            } else {
                for snapshot in &snapshots {
                    last_observations.insert(
                        snapshot.node_name.clone(),
                        format!("current height {}, tip {:?}", snapshot.height, snapshot.tip),
                    );
                }
            }
        }

        if poll_count.is_multiple_of(50) {
            let observations = format_node_observations(&last_observations);
            info!(
                target: TARGET,
                "Waiting for canonical node convergence - elapsed: {:.2?}; {observations}",
                start.elapsed()
            );
        }
        if start.elapsed() >= timeout {
            let requirement = required_minimum.map_or_else(
                || format!("within {allowed_difference} blocks"),
                |minimum| {
                    format!("at minimum height {minimum} and within {allowed_difference} blocks")
                },
            );
            return Err(StepError::StepFail {
                message: format!(
                    "Step `{step}` error: nodes did not reach canonical convergence {requirement} within {timeout_seconds}s; {}",
                    format_node_observations(&last_observations)
                ),
            });
        }
        sleep(Duration::from_millis(100)).await;
        poll_count += 1;
    }
}

fn format_node_observations(observations: &HashMap<String, String>) -> String {
    let mut entries = observations.iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    if entries.is_empty() {
        "no complete observations yet".to_owned()
    } else {
        entries
            .into_iter()
            .map(|(name, observation)| format!("{name}: {observation}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

pub async fn nodes_converged(
    world: &mut CucumberWorld,
    step: &str,
    min_height: Option<u64>,
    max_diff_height: u64,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_canonical_convergence(world, step, min_height, max_diff_height, time_out_seconds)
        .await
        .map(|_| ())
}

pub async fn ensure_all_nodes_agree_on_lib(
    world: &CucumberWorld,
    step: &str,
    time_out_seconds: u64,
) -> StepResult {
    super::super::lib_assertions::wait_for_shared_lib(world, step, None, time_out_seconds).await
}

#[cfg(test)]
mod convergence_tests {
    use super::*;

    fn id(value: u8) -> HeaderId {
        HeaderId::from([value; 32])
    }

    fn snapshot(name: &str, height: u64, tip: u8) -> NodeConsensusSnapshot {
        NodeConsensusSnapshot {
            node_name: name.to_owned(),
            height,
            tip: id(tip),
        }
    }

    async fn ancestor_from(
        tip: HeaderId,
        distance: u64,
        parents: &HashMap<HeaderId, (HeaderId, u64)>,
    ) -> Result<HeaderId, AncestorFetchError> {
        ancestor_at_height(tip, distance, async |block| {
            Ok(parents.get(&block).copied())
        })
        .await
    }

    #[test]
    fn equal_current_tips_converge_and_equal_height_forks_do_not() {
        let tips = vec![snapshot("NODE_1", 4, 4), snapshot("NODE_2", 4, 4)];
        let range = height_range(&tips).expect("nonempty tips");
        assert!(height_requirements_met(range, None, 0));
        assert!(all_ancestors_match(&[
            ("NODE_1".into(), id(4)),
            ("NODE_2".into(), id(4))
        ]));

        let forks = vec![snapshot("NODE_1", 4, 4), snapshot("NODE_2", 4, 5)];
        assert!(!all_ancestors_match(&[
            ("NODE_1".into(), id(4)),
            ("NODE_2".into(), id(5))
        ]));
        assert_eq!(height_range(&forks).expect("nonempty tips").difference(), 0);
    }

    #[tokio::test]
    async fn unequal_compatible_tips_match_the_ancestor_at_the_minimum_height() {
        let lower = id(1);
        let upper = id(2);
        let parents = HashMap::from([(upper, (lower, 2)), (lower, (id(0), 1))]);
        let tips = vec![snapshot("NODE_1", 4, 2), snapshot("NODE_2", 3, 1)];
        let range = height_range(&tips).expect("nonempty tips");
        assert!(height_requirements_met(range, Some(3), 1));
        let first = ancestor_from(upper, 1, &parents).await.expect("ancestor");
        let second = ancestor_from(lower, 0, &parents)
            .await
            .expect("tip at common height");
        assert_eq!(first, lower);
        assert!(all_ancestors_match(&[
            ("NODE_1".into(), first),
            ("NODE_2".into(), second)
        ]));

        let fork = id(3);
        let forked_parents = HashMap::from([(upper, (fork, 2)), (fork, (id(0), 1))]);
        let first = ancestor_from(upper, 1, &forked_parents)
            .await
            .expect("ancestor");
        assert!(!all_ancestors_match(&[
            ("NODE_1".into(), first),
            ("NODE_2".into(), lower)
        ]));
    }

    #[test]
    fn height_gap_and_minimum_apply_to_current_tips_only() {
        let compatible_tips = vec![snapshot("NODE_1", 5, 5), snapshot("NODE_2", 3, 3)];
        let range = height_range(&compatible_tips).expect("nonempty tips");
        assert!(!height_requirements_met(range, Some(3), 1));
        assert!(!height_requirements_met(range, Some(4), 3));

        // A diagnostic high-water mark of 120 is deliberately absent from the
        // fresh snapshot; the current canonical height is what governs success.
        let current = vec![snapshot("NODE_1", 118, 8)];
        let range = height_range(&current).expect("nonempty tips");
        assert!(!height_requirements_met(range, Some(120), 0));
        assert!(height_requirements_met(range, Some(118), 0));
    }

    #[test]
    fn current_fork_tips_override_historically_matching_observations() {
        let historical_matching = [("NODE_1", 10, 1), ("NODE_2", 10, 1)];
        assert_eq!(historical_matching[0].2, historical_matching[1].2);

        let current_forked = vec![snapshot("NODE_1", 12, 2), snapshot("NODE_2", 12, 3)];
        let range = height_range(&current_forked).expect("nonempty tips");
        assert!(height_requirements_met(range, None, 0));
        assert!(!all_ancestors_match(&[
            ("NODE_1".into(), id(2)),
            ("NODE_2".into(), id(3))
        ]));
    }

    #[test]
    fn incomplete_node_poll_cannot_use_the_successful_nodes_as_convergence() {
        let results = vec![
            Ok(snapshot("NODE_1", 5, 1)),
            Err(("NODE_2".to_owned(), "consensus query failed".to_owned())),
        ];
        let failures = complete_snapshot_set(results).expect_err("partial poll is incomplete");
        assert_eq!(failures[0].0, "NODE_2");
    }

    #[tokio::test]
    async fn missing_ancestor_is_retryable_and_cycles_or_nonincreasing_slots_are_invalid() {
        let missing = ancestor_at_height(id(1), 1, async |_| {
            Err(AncestorFetchError::Retryable(
                "temporary missing block".to_owned(),
            ))
        })
        .await
        .expect_err("missing ancestor must not match");
        assert!(matches!(missing, AncestorFetchError::Retryable(_)));

        let tip = id(1);
        let parent = id(2);
        let cycle = ancestor_at_height(tip, 2, async |current| {
            Ok(Some((if current == tip { parent } else { tip }, 1)))
        })
        .await
        .expect_err("cycle must be rejected");
        assert!(matches!(cycle, AncestorFetchError::Invalid(_)));

        for (parent_slot, description) in [(100, "equal"), (101, "greater")] {
            let invalid_order = ancestor_at_height(tip, 1, async |current| {
                Ok(Some((
                    parent,
                    if current == tip { 100 } else { parent_slot },
                )))
            })
            .await
            .expect_err("equal or later parent slot must be rejected");
            assert!(
                matches!(invalid_order, AncestorFetchError::Invalid(_)),
                "{description} parent slot should be invalid"
            );
        }

        for parent_slot in [99, 90] {
            let valid = ancestor_at_height(tip, 1, async |current| {
                Ok(Some((
                    parent,
                    if current == tip { 100 } else { parent_slot },
                )))
            })
            .await
            .expect("strictly earlier parent slot, including skipped slots, is valid");
            assert_eq!(valid, parent);
        }
    }

    #[test]
    fn empty_node_set_has_no_height_range_or_convergence() {
        assert!(height_range(&[]).is_err());
        assert!(!all_ancestors_match(&[]));
    }
}
