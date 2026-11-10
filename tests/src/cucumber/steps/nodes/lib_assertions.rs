use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use cucumber::gherkin::Step;
use futures_util::future::join_all;
use lb_chain_service::CryptarchiaInfo;
use lb_core::header::HeaderId;
use lb_testing_framework::NodeHttpClient;
use tokio::time::{Instant, sleep, timeout_at};
use tracing::{info, warn};

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::TARGET,
    world::CucumberWorld,
};

#[cucumber::when(expr = "all nodes share the same LIB at or above height {int} in {int} seconds")]
#[cucumber::then(expr = "all nodes share the same LIB at or above height {int} in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_all_nodes_share_lib_at_or_above_height(
    world: &mut CucumberWorld,
    step: &Step,
    min_height: u64,
    timeout_secs: u64,
) -> StepResult {
    wait_for_shared_lib(world, &step.value, Some(min_height), timeout_secs).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LibSnapshot {
    node_name: String,
    tip: String,
    tip_height: u64,
    lib_hash: String,
    lib_height: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LibFailureKind {
    Retryable,
    Invalid,
    Fatal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LibObservationFailure {
    node_name: String,
    kind: LibFailureKind,
    context: String,
    message: String,
}

impl LibObservationFailure {
    fn new(node_name: &str, kind: LibFailureKind, message: impl Into<String>) -> Self {
        Self {
            node_name: node_name.to_owned(),
            kind,
            context: "consensus snapshot unavailable".to_owned(),
            message: message.into(),
        }
    }

    fn with_consensus(mut self, consensus: &CryptarchiaInfo) -> Self {
        self.context = format!(
            "current tip {:?}, height {}, LIB {:?}",
            consensus.tip, consensus.height, consensus.lib
        );
        self
    }

    fn with_lib_height(mut self, height: u64) -> Self {
        self.context = format!("{}; resolved LIB height {height}", self.context);
        self
    }

    fn describe(&self) -> String {
        format!("{}; last error: {}", self.context, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LibResolveError {
    Retryable(String),
    Invalid(String),
    Fatal(String),
}

async fn resolve_lib_height_with<Fetch, FetchFuture>(
    node_name: &str,
    tip: HeaderId,
    tip_height: u64,
    target_lib: HeaderId,
    mut fetch: Fetch,
) -> Result<u64, LibResolveError>
where
    Fetch: FnMut(HeaderId) -> FetchFuture,
    FetchFuture: Future<Output = Result<Option<(HeaderId, u64)>, LibResolveError>>,
{
    if tip == target_lib {
        return Ok(tip_height);
    }

    let mut current = tip;
    let mut current_height = tip_height;
    let mut child_slot = None;
    let mut visited = HashSet::new();

    loop {
        if !visited.insert(current) {
            return Err(LibResolveError::Invalid(format!(
                "node `{node_name}` LIB ancestry contains a cycle at block {current:?}"
            )));
        }

        // Genesis is identified by the sampled chain height and is not served
        // by the ordinary block endpoint.
        if current_height == 0 {
            return if current == target_lib {
                Ok(0)
            } else {
                Err(LibResolveError::Invalid(format!(
                    "node `{node_name}` reached genesis {current:?} before sampled LIB {target_lib:?}"
                )))
            };
        }

        let Some((parent, slot)) = fetch(current).await? else {
            return Err(LibResolveError::Retryable(format!(
                "node `{node_name}` block {current:?} is temporarily unavailable while locating LIB {target_lib:?}"
            )));
        };

        if child_slot.is_some_and(|child| slot >= child) {
            return Err(LibResolveError::Invalid(format!(
                "node `{node_name}` parent {current:?} at slot {slot} is not strictly before its child at slot {}",
                child_slot.expect("checked as Some")
            )));
        }

        if (slot == 0) != (current_height == 0) {
            return Err(LibResolveError::Invalid(format!(
                "node `{node_name}` block {current:?} at slot {slot} is inconsistent with ancestry height {current_height}"
            )));
        }

        if current == target_lib {
            return Ok(current_height);
        }

        if slot == 0 {
            return Err(LibResolveError::Invalid(format!(
                "node `{node_name}` reached genesis {current:?} before sampled LIB {target_lib:?}"
            )));
        }

        current = parent;
        current_height -= 1;
        child_slot = Some(slot);
    }
}

async fn resolve_lib_height(
    node_name: &str,
    client: &NodeHttpClient,
    consensus: &CryptarchiaInfo,
    deadline: Instant,
) -> Result<u64, LibObservationFailure> {
    let target_lib = consensus.lib;
    let result = resolve_lib_height_with(
        node_name,
        consensus.tip,
        consensus.height,
        target_lib,
        async |current| match timeout_at(deadline, client.block(&current)).await {
            Err(_) => Err(LibResolveError::Retryable(format!(
                "block query for {current:?} reached the shared-LIB deadline"
            ))),
            Ok(Ok(Some(block))) => Ok(Some((
                block.header.parent_block,
                block.header.slot.into_inner(),
            ))),
            Ok(Ok(None)) => Ok(None),
            Ok(Err(error)) => {
                let message = format!("block query for {current:?} failed: {error}");
                Err(LibResolveError::Fatal(message))
            }
        },
    )
    .await;

    result.map_err(|error| {
        let (kind, message) = match error {
            LibResolveError::Retryable(message) => (LibFailureKind::Retryable, message),
            LibResolveError::Invalid(message) => (LibFailureKind::Invalid, message),
            LibResolveError::Fatal(message) => (LibFailureKind::Fatal, message),
        };
        LibObservationFailure::new(node_name, kind, message).with_consensus(consensus)
    })
}

fn lib_snapshot_matches(sampled_lib: HeaderId, current_lib: HeaderId) -> bool {
    sampled_lib == current_lib
}

async fn fetch_node_lib_snapshot(
    node_name: &str,
    client: &NodeHttpClient,
    deadline: Instant,
) -> Result<LibSnapshot, LibObservationFailure> {
    let consensus = match timeout_at(deadline, client.consensus_info()).await {
        Err(_) => {
            return Err(LibObservationFailure::new(
                node_name,
                LibFailureKind::Retryable,
                "consensus query reached the shared-LIB deadline",
            ));
        }
        Ok(Ok(consensus)) => consensus.cryptarchia_info,
        Ok(Err(error)) => {
            return Err(LibObservationFailure::new(
                node_name,
                LibFailureKind::Fatal,
                format!("consensus query failed: {error}"),
            ));
        }
    };

    let lib_height = resolve_lib_height(node_name, client, &consensus, deadline)
        .await
        .map_err(|error| error.with_consensus(&consensus))?;

    let current_consensus = match timeout_at(deadline, client.consensus_info()).await {
        Err(_) => {
            return Err(LibObservationFailure::new(
                node_name,
                LibFailureKind::Retryable,
                "verification consensus query reached the shared-LIB deadline",
            )
            .with_consensus(&consensus)
            .with_lib_height(lib_height));
        }
        Ok(Ok(current)) => current.cryptarchia_info,
        Ok(Err(error)) => {
            return Err(LibObservationFailure::new(
                node_name,
                LibFailureKind::Fatal,
                format!("verification consensus query failed: {error}"),
            )
            .with_consensus(&consensus)
            .with_lib_height(lib_height));
        }
    };
    if !lib_snapshot_matches(consensus.lib, current_consensus.lib) {
        return Err(LibObservationFailure::new(
            node_name,
            LibFailureKind::Retryable,
            format!(
                "canonical state changed during LIB ancestry resolution (new tip {:?} at height {}, LIB {:?})",
                current_consensus.tip, current_consensus.height, current_consensus.lib
            ),
        )
        .with_consensus(&consensus)
        .with_lib_height(lib_height));
    }

    Ok(LibSnapshot {
        node_name: node_name.to_owned(),
        tip: format!("{:?}", consensus.tip),
        tip_height: consensus.height,
        lib_hash: format!("{:?}", consensus.lib),
        lib_height,
    })
}

fn format_snapshot(snapshot: &LibSnapshot) -> String {
    format!(
        "tip {} at height {}, LIB {} at height {}",
        snapshot.tip, snapshot.tip_height, snapshot.lib_hash, snapshot.lib_height
    )
}

fn validate_shared_lib(snapshots: &[LibSnapshot], min_height: Option<u64>) -> Result<(), String> {
    let Some((baseline, peers)) = snapshots.split_first() else {
        return Err("LIB assertion requires at least one running node".to_owned());
    };

    if let Some(minimum) = min_height
        && let Some(snapshot) = snapshots
            .iter()
            .find(|snapshot| snapshot.lib_height < minimum)
    {
        return Err(format!(
            "node `{}` has LIB height {}, below required height {minimum}",
            snapshot.node_name, snapshot.lib_height
        ));
    }

    if let Some(snapshot) = peers
        .iter()
        .find(|snapshot| snapshot.lib_hash != baseline.lib_hash)
    {
        return Err(format!(
            "nodes do not share the same LIB: `{}` has {}, `{}` has {}",
            baseline.node_name, baseline.lib_hash, snapshot.node_name, snapshot.lib_hash
        ));
    }

    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "The polling loop keeps complete snapshots, error classification, and deadline diagnostics together"
)]
#[expect(
    clippy::cognitive_complexity,
    reason = "The poll explicitly distinguishes partial, transient, invalid, and matching node observations"
)]
async fn wait_for_shared_lib_with<Fetch, FetchFuture>(
    step: &str,
    node_names: &[String],
    min_height: Option<u64>,
    timeout: Duration,
    mut fetch: Fetch,
) -> StepResult
where
    Fetch: FnMut(Instant) -> FetchFuture,
    FetchFuture: Future<Output = Vec<Result<LibSnapshot, LibObservationFailure>>>,
{
    if node_names.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "LIB assertion requires at least one running node".to_owned(),
        });
    }

    let start = Instant::now();
    let deadline = start + timeout;
    let expected = node_names.iter().cloned().collect::<HashSet<_>>();
    let mut last_observations = node_names
        .iter()
        .map(|name| (name.clone(), "no fresh observation yet".to_owned()))
        .collect::<HashMap<_, _>>();
    let mut poll_count = 0usize;

    loop {
        if poll_count > 0 && Instant::now() >= deadline {
            return Err(shared_lib_timeout_error(
                step,
                min_height,
                timeout,
                &last_observations,
            ));
        }

        let Ok(results) = timeout_at(deadline, fetch(deadline)).await else {
            return Err(shared_lib_timeout_error(
                step,
                min_height,
                timeout,
                &last_observations,
            ));
        };

        let mut complete = Vec::with_capacity(node_names.len());
        let mut observed_names = HashSet::new();
        let mut retryable_failure = false;

        for result in results {
            match result {
                Ok(snapshot) => {
                    if !expected.contains(&snapshot.node_name)
                        || !observed_names.insert(snapshot.node_name.clone())
                    {
                        return Err(StepError::StepFail {
                            message: format!(
                                "Step `{step}` error: invalid duplicate or unexpected LIB observation for `{}`",
                                snapshot.node_name
                            ),
                        });
                    }
                    last_observations
                        .insert(snapshot.node_name.clone(), format_snapshot(&snapshot));
                    complete.push(snapshot);
                }
                Err(failure) => {
                    if !expected.contains(&failure.node_name)
                        || !observed_names.insert(failure.node_name.clone())
                    {
                        return Err(StepError::StepFail {
                            message: format!(
                                "Step `{step}` error: invalid duplicate or unexpected LIB failure for `{}`",
                                failure.node_name
                            ),
                        });
                    }
                    last_observations.insert(failure.node_name.clone(), failure.describe());
                    match failure.kind {
                        LibFailureKind::Retryable => retryable_failure = true,
                        LibFailureKind::Invalid | LibFailureKind::Fatal => {
                            return Err(StepError::StepFail {
                                message: format!(
                                    "Step `{step}` error: node `{}` has a non-retryable LIB observation failure: {}",
                                    failure.node_name,
                                    failure.describe()
                                ),
                            });
                        }
                    }
                }
            }
        }

        for name in node_names {
            if !observed_names.contains(name) {
                retryable_failure = true;
                last_observations.insert(
                    name.clone(),
                    "no fresh observation returned in this polling attempt".to_owned(),
                );
            }
        }

        if !retryable_failure && complete.len() == node_names.len() {
            if let Err(message) = validate_shared_lib(&complete, min_height) {
                for snapshot in &complete {
                    last_observations.insert(snapshot.node_name.clone(), format_snapshot(snapshot));
                }
                if poll_count.is_multiple_of(50) {
                    warn!(target: TARGET, "Waiting for shared LIB: {message}");
                }
            } else {
                info!(
                    target: TARGET,
                    "All nodes agree on LIB in {:.2?}",
                    start.elapsed()
                );
                return Ok(());
            }
        }

        if Instant::now() >= deadline {
            return Err(shared_lib_timeout_error(
                step,
                min_height,
                timeout,
                &last_observations,
            ));
        }

        if poll_count.is_multiple_of(50) {
            info!(
                target: TARGET,
                "Waiting for shared LIB after {:.2?}; {}",
                start.elapsed(),
                format_observations(&last_observations)
            );
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        sleep(Duration::from_millis(100).min(remaining)).await;
        poll_count += 1;
    }
}

fn format_observations(observations: &HashMap<String, String>) -> String {
    let mut observations = observations.iter().collect::<Vec<_>>();
    observations.sort_by(|left, right| left.0.cmp(right.0));
    observations
        .into_iter()
        .map(|(name, observation)| format!("{name}: {observation}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn shared_lib_timeout_error(
    step: &str,
    min_height: Option<u64>,
    timeout: Duration,
    observations: &HashMap<String, String>,
) -> StepError {
    StepError::StepFail {
        message: format!(
            "Step `{step}` error: nodes did not share the same LIB{} within {:.2}s; {}",
            min_height.map_or_else(String::new, |height| format!(
                " at or above height {height}"
            )),
            timeout.as_secs_f64(),
            format_observations(observations)
        ),
    }
}

pub async fn wait_for_shared_lib(
    world: &CucumberWorld,
    step: &str,
    min_height: Option<u64>,
    timeout_secs: u64,
) -> StepResult {
    let mut clients = world
        .nodes_info
        .iter()
        .map(|(name, node)| (name.clone(), node.client.clone()))
        .collect::<Vec<_>>();
    clients.sort_by(|left, right| left.0.cmp(&right.0));
    let node_names = clients
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();

    wait_for_shared_lib_with(
        step,
        &node_names,
        min_height,
        Duration::from_secs(timeout_secs),
        |deadline| {
            join_all(
                clients
                    .iter()
                    .map(|(name, client)| fetch_node_lib_snapshot(name, client, deadline)),
            )
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(node: &str, tip: &str, tip_height: u64, lib: &str, lib_height: u64) -> LibSnapshot {
        LibSnapshot {
            node_name: node.to_owned(),
            tip: tip.to_owned(),
            tip_height,
            lib_hash: lib.to_owned(),
            lib_height,
        }
    }

    fn transient(node: &str) -> Result<LibSnapshot, LibObservationFailure> {
        Err(LibObservationFailure::new(
            node,
            LibFailureKind::Retryable,
            "ancestor block temporarily unavailable",
        ))
    }

    #[test]
    fn shared_lib_requires_exact_identity_and_requested_minimum() {
        let common = vec![
            snapshot("NODE_1", "tip1", 10, "lib", 5),
            snapshot("NODE_2", "tip2", 9, "lib", 5),
        ];
        assert!(validate_shared_lib(&common, Some(5)).is_ok());
        assert!(validate_shared_lib(&common, Some(6)).is_err());

        let different = vec![
            snapshot("NODE_1", "tip1", 10, "lib1", 5),
            snapshot("NODE_2", "tip2", 9, "lib2", 5),
        ];
        assert!(validate_shared_lib(&different, None).is_err());
        assert!(validate_shared_lib(&[], None).is_err());
    }

    #[test]
    fn sampled_lib_must_remain_current_after_ancestry_resolution() {
        assert!(lib_snapshot_matches(id(2), id(2)));
        assert!(!lib_snapshot_matches(id(2), id(4)));
    }

    #[tokio::test]
    async fn transient_partial_observation_recovers_without_stale_success() {
        let names = vec!["NODE_1".to_owned(), "NODE_2".to_owned()];
        let mut polls = 0;
        let result = wait_for_shared_lib_with(
            "shared LIB",
            &names,
            Some(5),
            Duration::from_secs(1),
            |_| {
                polls += 1;
                let current = polls;
                async move {
                    if current == 1 {
                        vec![
                            Err(LibObservationFailure::new(
                                "NODE_1",
                                LibFailureKind::Retryable,
                                "sampled LIB changed during ancestry resolution",
                            )),
                            Err(LibObservationFailure::new(
                                "NODE_2",
                                LibFailureKind::Retryable,
                                "ancestor block temporarily unavailable (HTTP 404)",
                            )),
                        ]
                    } else {
                        vec![
                            Ok(snapshot("NODE_1", "tip", 7, "lib", 5)),
                            Ok(snapshot("NODE_2", "tip", 7, "lib", 5)),
                        ]
                    }
                }
            },
        )
        .await;
        assert!(result.is_ok());
        assert!(polls >= 2);
    }

    #[tokio::test]
    async fn partial_poll_without_a_failed_node_record_cannot_establish_agreement() {
        let names = vec!["NODE_1".to_owned(), "NODE_2".to_owned()];
        let result = wait_for_shared_lib_with(
            "shared LIB",
            &names,
            None,
            Duration::from_millis(20),
            async |_| vec![Ok(snapshot("NODE_1", "tip", 7, "lib", 5))],
        )
        .await;
        let error = result.expect_err("one node cannot establish shared LIB agreement");
        assert!(error.to_string().contains("NODE_2"));
        assert!(error.to_string().contains("no fresh observation"));
    }

    #[tokio::test]
    async fn empty_node_set_fails_explicitly() {
        let result = wait_for_shared_lib_with(
            "shared LIB",
            &[],
            None,
            Duration::from_secs(1),
            async |_| Vec::new(),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn repeated_transient_observations_timeout_with_node_diagnostics() {
        let names = vec!["NODE_1".to_owned()];
        let result = wait_for_shared_lib_with(
            "shared LIB",
            &names,
            None,
            Duration::from_millis(20),
            async |_| vec![transient("NODE_1")],
        )
        .await;
        let error = result.expect_err("repeated transient observations should time out");
        assert!(error.to_string().contains("NODE_1"));
        assert!(
            error
                .to_string()
                .contains("ancestor block temporarily unavailable")
        );
    }

    #[tokio::test]
    async fn http_observation_failure_is_not_retried() {
        let names = vec!["NODE_1".to_owned()];
        let mut polls = 0;
        let result =
            wait_for_shared_lib_with("shared LIB", &names, None, Duration::from_secs(1), |_| {
                polls += 1;
                async {
                    vec![Err(LibObservationFailure::new(
                        "NODE_1",
                        LibFailureKind::Fatal,
                        "consensus HTTP request failed",
                    ))]
                }
            })
            .await;
        assert!(result.is_err());
        assert_eq!(polls, 1);
    }

    #[tokio::test]
    async fn structural_ancestry_failure_is_not_retried() {
        let names = vec!["NODE_1".to_owned()];
        let mut polls = 0;
        let result =
            wait_for_shared_lib_with("shared LIB", &names, None, Duration::from_secs(1), |_| {
                polls += 1;
                async {
                    vec![Err(LibObservationFailure::new(
                        "NODE_1",
                        LibFailureKind::Invalid,
                        "equal-slot parent-child ancestry",
                    ))]
                }
            })
            .await;
        assert!(result.is_err());
        assert_eq!(polls, 1);
    }

    fn id(value: u8) -> HeaderId {
        HeaderId::from([value; 32])
    }

    async fn resolve_from(
        tip: HeaderId,
        tip_height: u64,
        target: HeaderId,
        blocks: HashMap<HeaderId, (HeaderId, u64)>,
    ) -> Result<u64, LibResolveError> {
        let blocks = &blocks;
        resolve_lib_height_with("NODE_1", tip, tip_height, target, async move |current| {
            Ok(blocks.get(&current).copied())
        })
        .await
    }

    #[tokio::test]
    async fn lib_ancestry_requires_strict_slots_and_accepts_skipped_slots() {
        let tip = id(1);
        let lib = id(2);
        let genesis = id(3);
        for (lib_slot, valid) in [(99, true), (100, false), (101, false), (90, true)] {
            let blocks = HashMap::from([(tip, (lib, 100)), (lib, (genesis, lib_slot))]);
            let result = resolve_from(tip, 2, lib, blocks).await;
            if valid {
                assert_eq!(result.expect("valid slot order"), 1);
            } else {
                assert!(matches!(result, Err(LibResolveError::Invalid(_))));
            }
        }
    }

    #[tokio::test]
    async fn lib_ancestry_rejects_cycles_missing_blocks_and_unreachable_libs() {
        let tip = id(1);
        let lib = id(2);
        let genesis = id(3);
        let cycle = HashMap::from([(tip, (lib, 10)), (lib, (tip, 9))]);
        assert!(matches!(
            resolve_from(tip, 3, id(4), cycle).await,
            Err(LibResolveError::Invalid(_))
        ));

        assert!(matches!(
            resolve_from(tip, 2, lib, HashMap::new()).await,
            Err(LibResolveError::Retryable(_))
        ));
        assert!(matches!(
            resolve_from(tip, 2, lib, HashMap::from([(tip, (lib, 10))])).await,
            Err(LibResolveError::Retryable(_))
        ));

        let missing_lib = HashMap::from([(tip, (genesis, 10)), (genesis, (genesis, 0))]);
        assert!(matches!(
            resolve_from(tip, 1, lib, missing_lib).await,
            Err(LibResolveError::Invalid(_))
        ));

        let genesis_lib = HashMap::from([(tip, (genesis, 10))]);
        assert_eq!(
            resolve_from(tip, 1, genesis, genesis_lib)
                .await
                .expect("genesis is a valid LIB"),
            0
        );
    }
}
