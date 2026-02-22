use std::{marker::PhantomData, time::Duration};

use async_trait::async_trait;
use futures::future::join_all;
use testing_framework_core::scenario::{DynError, Expectation, RunContext};
use thiserror::Error;
use tokio::time::sleep;

use crate::{TopologyConfig, framework::LbcEnv, node::NodeHttpClient, workloads::LbcScenarioEnv};

#[derive(Clone, Copy, Debug)]
/// Checks that every node reaches near the highest observed height within an
/// allowance.
pub struct ConsensusLiveness<E = LbcEnv> {
    lag_allowance: u64,
    _env: PhantomData<fn() -> E>,
}

impl<E> Default for ConsensusLiveness<E> {
    fn default() -> Self {
        Self {
            lag_allowance: LAG_ALLOWANCE,
            _env: PhantomData,
        }
    }
}

const LAG_ALLOWANCE: u64 = 2;
const REQUEST_RETRIES: usize = 15;
const REQUEST_RETRY_DELAY: Duration = Duration::from_secs(2);
const PROGRESS_PROBES: usize = 4;
const PROGRESS_PROBE_DELAY: Duration = Duration::from_secs(5);
const MAX_LAG_ALLOWANCE: u64 = 5;

#[async_trait]
impl<E> Expectation<E> for ConsensusLiveness<E>
where
    E: LbcScenarioEnv,
{
    fn name(&self) -> &'static str {
        "consensus_liveness"
    }

    async fn evaluate(&mut self, ctx: &RunContext<E>) -> Result<(), DynError> {
        Self::ensure_participants(ctx)?;

        let target_hint = Self::target_blocks(ctx);

        tracing::info!(target_hint, "consensus liveness: collecting samples");

        let check = Self::collect_results_with_progress(ctx).await;
        self.report(target_hint, check)
    }

    async fn check_during_capture(&mut self, ctx: &RunContext<E>) -> Result<(), DynError> {
        Self::ensure_participants(ctx)
    }
}

fn consensus_target_blocks<E: LbcScenarioEnv>(ctx: &RunContext<E>) -> u64 {
    let config: &TopologyConfig = ctx.descriptors().config();
    let Some(slot_duration) = config.slot_duration else {
        return 0;
    };

    let active_slot_coeff = config.active_slot_coeff;
    let run_secs = ctx.run_duration().as_secs_f64();
    let slot_secs = slot_duration.as_secs_f64();
    let expected = run_secs / slot_secs * active_slot_coeff;
    expected.floor().max(0.0) as u64
}

#[derive(Debug, Error)]
enum ConsensusLivenessIssue {
    #[error("{node} height {height} below target {target}")]
    HeightBelowTarget {
        node: String,
        height: u64,
        target: u64,
    },
    #[error("{node} consensus_info failed: {source}")]
    RequestFailed {
        node: String,
        #[source]
        source: DynError,
    },
}

#[derive(Debug, Error)]
enum ConsensusLivenessError {
    #[error("consensus liveness requires at least one validator")]
    MissingParticipants,
    #[error("consensus liveness violated (target={target}):\n{details}")]
    Violations {
        target: u64,
        #[source]
        details: ViolationIssues,
    },
}

#[derive(Debug, Error)]
#[error("{message}")]
struct ViolationIssues {
    issues: Vec<ConsensusLivenessIssue>,
    message: String,
}

impl<E> ConsensusLiveness<E>
where
    E: LbcScenarioEnv,
{
    fn target_blocks(ctx: &RunContext<E>) -> u64 {
        consensus_target_blocks(ctx)
    }

    fn ensure_participants(ctx: &RunContext<E>) -> Result<(), DynError> {
        if ctx.node_clients().is_empty() {
            Err(Box::new(ConsensusLivenessError::MissingParticipants))
        } else {
            Ok(())
        }
    }

    async fn collect_results_with_progress(ctx: &RunContext<E>) -> LivenessCheck {
        for probe in 0..PROGRESS_PROBES {
            let check = Self::collect_results(ctx).await;
            if check.max_height() > 0 || probe + 1 == PROGRESS_PROBES {
                return check;
            }

            tracing::warn!(
                probe = probe + 1,
                retries = PROGRESS_PROBES,
                delay_secs = PROGRESS_PROBE_DELAY.as_secs(),
                "consensus liveness: no chain progress observed yet; retrying"
            );
            sleep(PROGRESS_PROBE_DELAY).await;
        }

        LivenessCheck::default()
    }

    async fn collect_results(ctx: &RunContext<E>) -> LivenessCheck {
        let clients = ctx.node_clients().snapshot();
        let results = join_all(
            clients
                .iter()
                .enumerate()
                .map(|(idx, client)| Self::collect_node_sample(idx, client)),
        )
        .await;

        let (samples, issues) = split_samples_and_issues(results, clients.len());

        LivenessCheck { samples, issues }
    }

    async fn collect_node_sample(
        idx: usize,
        client: &NodeHttpClient,
    ) -> Result<NodeSample, ConsensusLivenessIssue> {
        let node = format!("node-{idx}");

        for attempt in 0..REQUEST_RETRIES {
            match Self::fetch_cluster_info(client).await {
                Ok(sample) => {
                    tracing::debug!(
                        node = %node,
                        height = sample.height,
                        tip = ?sample.tip,
                        attempt,
                        "consensus_info collected"
                    );
                    return Ok(NodeSample {
                        label: node,
                        height: sample.height,
                        tip: sample.tip,
                    });
                }

                Err(err) if attempt + 1 == REQUEST_RETRIES => {
                    tracing::warn!(node = %node, %err, "consensus_info failed after retries");
                    return Err(ConsensusLivenessIssue::RequestFailed { node, source: err });
                }

                Err(_) => sleep(REQUEST_RETRY_DELAY).await,
            }
        }

        Err(ConsensusLivenessIssue::RequestFailed {
            node,
            source: "consensus_info retries exhausted".into(),
        })
    }

    async fn fetch_cluster_info(client: &NodeHttpClient) -> Result<ConsensusInfoSample, DynError> {
        client
            .consensus_info()
            .await
            .map(|info| ConsensusInfoSample {
                height: info.height,
                tip: format!("{:?}", info.tip),
            })
            .map_err(Into::into)
    }

    #[must_use]
    pub const fn with_lag_allowance(mut self, lag_allowance: u64) -> Self {
        self.lag_allowance = lag_allowance;
        self
    }

    fn effective_lag_allowance(&self, target: u64) -> u64 {
        (target / 10).clamp(self.lag_allowance, MAX_LAG_ALLOWANCE)
    }

    fn report(&self, target_hint: u64, check: LivenessCheck) -> Result<(), DynError> {
        if check.samples.is_empty() {
            return Err(Box::new(ConsensusLivenessError::MissingParticipants));
        }

        let max_height = check.max_height();
        let max_height_nodes = format_max_height_nodes(&check.samples, max_height);
        let target = choose_target_height(target_hint, max_height);
        let lag_allowance = self.effective_lag_allowance(target);

        let mut issues = check.issues;
        issues.extend(height_lag_issues(&check.samples, lag_allowance, target));

        if issues.is_empty() {
            let observed_heights: Vec<_> = check.samples.iter().map(|s| s.height).collect();
            let observed_tips: Vec<_> = check.samples.iter().map(|s| s.tip.clone()).collect();
            log_liveness_success(
                target,
                check.samples.len(),
                &observed_heights,
                &observed_tips,
            );
            Ok(())
        } else {
            log_liveness_issues(max_height, max_height_nodes.as_deref(), &issues);

            Err(Box::new(ConsensusLivenessError::Violations {
                target,
                details: ViolationIssues::new(issues, max_height_nodes.as_deref()),
            }))
        }
    }
}

fn log_liveness_success(target: u64, samples: usize, heights: &[u64], tips: &[String]) {
    tracing::info!(
        target,
        samples,
        heights = ?heights,
        tips = ?tips,
        "consensus liveness expectation satisfied"
    );
}

fn log_liveness_issues(
    max_height: u64,
    max_height_nodes: Option<&str>,
    issues: &[ConsensusLivenessIssue],
) {
    if let Some(nodes) = max_height_nodes {
        tracing::warn!(
            max_height,
            nodes,
            "consensus liveness: highest observed node(s)"
        );
    }

    for issue in issues {
        tracing::warn!(?issue, "consensus liveness issue");
    }
}

const fn choose_target_height(target_hint: u64, max_height: u64) -> u64 {
    if target_hint == 0 || target_hint > max_height {
        max_height
    } else {
        target_hint
    }
}

fn split_samples_and_issues(
    results: Vec<Result<NodeSample, ConsensusLivenessIssue>>,
    expected_samples: usize,
) -> (Vec<NodeSample>, Vec<ConsensusLivenessIssue>) {
    let mut samples = Vec::with_capacity(expected_samples);
    let mut issues = Vec::new();

    for result in results {
        match result {
            Ok(sample) => samples.push(sample),
            Err(issue) => issues.push(issue),
        }
    }

    (samples, issues)
}

fn height_lag_issues(
    samples: &[NodeSample],
    lag_allowance: u64,
    target: u64,
) -> Vec<ConsensusLivenessIssue> {
    samples
        .iter()
        .filter(|sample| sample.height + lag_allowance < target)
        .map(|sample| ConsensusLivenessIssue::HeightBelowTarget {
            node: sample.label.clone(),
            height: sample.height,
            target,
        })
        .collect()
}

struct ConsensusInfoSample {
    height: u64,
    tip: String,
}

struct NodeSample {
    label: String,
    height: u64,
    tip: String,
}

#[derive(Default)]
struct LivenessCheck {
    samples: Vec<NodeSample>,
    issues: Vec<ConsensusLivenessIssue>,
}

impl LivenessCheck {
    fn max_height(&self) -> u64 {
        self.samples
            .iter()
            .map(|sample| sample.height)
            .max()
            .unwrap_or(0)
    }
}

impl ViolationIssues {
    fn new(issues: Vec<ConsensusLivenessIssue>, max_height_nodes: Option<&str>) -> Self {
        let message = violation_message(&issues, max_height_nodes);
        Self { issues, message }
    }
}

fn violation_message(issues: &[ConsensusLivenessIssue], max_height_nodes: Option<&str>) -> String {
    let mut lines = Vec::new();

    if let Some(nodes) = max_height_nodes {
        lines.push(format!("max_height node(s): {nodes}"));
    }

    lines.extend(issues.iter().map(|issue| format!("- {issue}")));

    lines.join("\n")
}

fn format_max_height_nodes(samples: &[NodeSample], max_height: u64) -> Option<String> {
    let leaders = samples
        .iter()
        .filter(|sample| sample.height == max_height)
        .map(format_sample_summary)
        .collect::<Vec<_>>();

    if leaders.is_empty() {
        None
    } else {
        Some(leaders.join(", "))
    }
}

fn format_sample_summary(sample: &NodeSample) -> String {
    format!(
        "{} (height={}, tip={:?})",
        sample.label, sample.height, sample.tip
    )
}
