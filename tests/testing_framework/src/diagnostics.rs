use std::collections::{BTreeMap, BTreeSet};

#[path = "diagnostics/system_monitor/mod.rs"]
mod system_monitor;

use async_trait::async_trait;
use futures::future::join_all;
use lb_blend_service::message::NetworkInfo as BlendNetworkInfo;
use lb_tx_service::MempoolMetrics;
use testing_framework_core::scenario::{
    DynError, Expectation, RunContext, RunHandle, RunMetrics, Runner, Scenario, ScenarioError,
};
use thiserror::Error;

use crate::{BlockFeed, LbcEnv, NodeHttpClient, node::DeploymentPlan};

#[doc(hidden)]
pub fn register_system_monitor_output_file(path: &std::path::Path) {
    system_monitor::register_output_file(path);
}

#[doc(hidden)]
pub fn unregister_system_monitor_output_file(path: &std::path::Path) {
    system_monitor::unregister_output_file(path);
}

#[doc(hidden)]
pub fn record_system_monitor_event(label: &str, detail: impl Into<String>) {
    system_monitor::record_event(label, detail);
}

fn render_system_monitor() -> String {
    system_monitor::render_recent_summary()
        .unwrap_or_else(|| "system_monitor:\n  unavailable".to_owned())
}

#[derive(Debug, Error)]
pub enum ScenarioRunDiagnosticsError {
    #[error("scenario run failed: {source}")]
    AlreadyCaptured {
        #[source]
        source: ScenarioError,
    },
    #[error("scenario run failed: {source}\n\nfailure diagnostics:\n{report}")]
    Captured {
        #[source]
        source: ScenarioError,
        report: String,
    },
}

impl ScenarioRunDiagnosticsError {
    #[must_use]
    pub fn report(&self) -> Option<&str> {
        match self {
            Self::AlreadyCaptured { .. } => None,
            Self::Captured { report, .. } => Some(report),
        }
    }
}

pub async fn run_with_failure_diagnostics<Caps>(
    runner: Runner<LbcEnv>,
    scenario: &mut Scenario<LbcEnv, Caps>,
) -> Result<RunHandle<LbcEnv>, ScenarioRunDiagnosticsError>
where
    Caps: Send + Sync,
{
    let sources = DiagnosticSources::capture(runner.context());

    match runner.run(scenario).await {
        Ok(handle) => Ok(handle),
        Err(source) if scenario_error_has_diagnostics(&source) => {
            Err(ScenarioRunDiagnosticsError::AlreadyCaptured { source })
        }
        Err(source) => {
            record_system_monitor_event("scenario_failure", source.to_string());

            Err(ScenarioRunDiagnosticsError::Captured {
                report: sources.render().await,
                source,
            })
        }
    }
}

pub struct FailureDiagnosticsExpectation<X> {
    inner: X,
}

impl<X> FailureDiagnosticsExpectation<X> {
    #[must_use]
    pub const fn new(inner: X) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl<X> Expectation<LbcEnv> for FailureDiagnosticsExpectation<X>
where
    X: Expectation<LbcEnv>,
{
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn init(
        &mut self,
        descriptors: &DeploymentPlan,
        run_metrics: &RunMetrics,
    ) -> Result<(), DynError> {
        self.inner.init(descriptors, run_metrics)
    }

    async fn start_capture(&mut self, ctx: &RunContext<LbcEnv>) -> Result<(), DynError> {
        match self.inner.start_capture(ctx).await {
            Ok(()) => Ok(()),
            Err(source) => Err(add_failure_diagnostics(ctx, source).await),
        }
    }

    async fn check_during_capture(&mut self, ctx: &RunContext<LbcEnv>) -> Result<(), DynError> {
        match self.inner.check_during_capture(ctx).await {
            Ok(()) => Ok(()),
            Err(source) => Err(add_failure_diagnostics(ctx, source).await),
        }
    }

    async fn evaluate(&mut self, ctx: &RunContext<LbcEnv>) -> Result<(), DynError> {
        match self.inner.evaluate(ctx).await {
            Ok(()) => Ok(()),
            Err(source) => Err(add_failure_diagnostics(ctx, source).await),
        }
    }
}

#[derive(Debug, Error)]
#[error("{source}\n\nfailure diagnostics:\n{report}")]
struct FailureDiagnosticsError {
    #[source]
    source: DynError,
    report: String,
}

async fn add_failure_diagnostics(ctx: &RunContext<LbcEnv>, source: DynError) -> DynError {
    if dyn_error_has_diagnostics(&source) {
        return source;
    }

    record_system_monitor_event("expectation_failure", source.to_string());

    Box::new(FailureDiagnosticsError {
        report: collect_failure_report(ctx).await,
        source,
    })
}

pub async fn collect_failure_report(context: &RunContext<LbcEnv>) -> String {
    DiagnosticSources::capture(context).render().await
}

struct DiagnosticSources {
    cluster_control_profile: &'static str,
    node_clients: Vec<NodeHttpClient>,
    block_feed: Option<BlockFeed>,
}

impl DiagnosticSources {
    fn capture(context: &RunContext<LbcEnv>) -> Self {
        Self {
            cluster_control_profile: context.cluster_control_profile().as_str(),
            node_clients: context.node_clients().snapshot(),
            block_feed: context.extension::<BlockFeed>(),
        }
    }

    async fn render(self) -> String {
        let diagnostics = join_all(
            self.node_clients
                .iter()
                .cloned()
                .enumerate()
                .map(async |(index, client)| NodeDiagnostic::collect(index, client).await),
        )
        .await;
        let summary = ClusterSummary::build(
            self.cluster_control_profile,
            self.node_clients.len(),
            &diagnostics,
        );

        let sections = [
            summary.render(),
            summary.render_connectivity(),
            self.render_block_feed(),
            render_system_monitor(),
            render_nodes(&diagnostics),
        ];

        sections.join("\n\n")
    }

    fn render_block_feed(&self) -> String {
        let Some(block_feed) = &self.block_feed else {
            return "block_feed:\n  unavailable".to_owned();
        };

        let mut lines = vec!["block_feed:".to_owned()];

        if let Some(observation) = block_feed.latest_observation() {
            let snapshot = observation.snapshot();
            let tip_heights = snapshot
                .node_heads
                .iter()
                .filter_map(|head| head.tip_height)
                .collect::<Vec<_>>();
            let lib_heights = snapshot
                .node_heads
                .iter()
                .filter_map(|head| head.lib_height)
                .collect::<Vec<_>>();
            let distinct_tips = snapshot
                .node_heads
                .iter()
                .map(|head| head.tip)
                .collect::<BTreeSet<_>>()
                .len();
            let distinct_libs = snapshot
                .node_heads
                .iter()
                .map(|head| head.lib)
                .collect::<BTreeSet<_>>()
                .len();

            lines.push(format!(
                "  cycle={} observed_at={:?} nodes={} distinct_tips={} distinct_libs={}",
                observation.cycle(),
                observation.observed_at(),
                snapshot.node_heads.len(),
                distinct_tips,
                distinct_libs
            ));
            lines.push(format!(
                "  tip_heights={} lib_heights={}",
                format_range_u64(&tip_heights),
                format_range_u64(&lib_heights)
            ));
            lines.push(format!(
                "  retained_headers={} parent_edges={} pruned_blocks_total={} missing_tip_heights={} missing_lib_heights={}",
                snapshot.header_heights.len(),
                snapshot.parent_edges.len(),
                snapshot.pruned_blocks_total,
                snapshot
                    .node_heads
                    .iter()
                    .filter(|head| head.tip_height.is_none())
                    .count(),
                snapshot
                    .node_heads
                    .iter()
                    .filter(|head| head.lib_height.is_none())
                    .count()
            ));
        } else {
            lines.push("  no successful observation yet".to_owned());
        }

        if let Some(last_error) = block_feed.last_error() {
            lines.push(format!(
                "  last_error=cycle:{} stage:{:?} sources:{} observed_at:{:?} message:{}",
                last_error.cycle,
                last_error.stage,
                last_error.source_count,
                last_error.observed_at,
                last_error.message
            ));
        }

        lines.join("\n")
    }
}

struct NodeDiagnostic {
    label: String,
    base_url: String,
    testing_url: Option<String>,
    consensus: Result<ConsensusSnapshot, String>,
    network: Result<NetworkSnapshot, String>,
    blend: Result<Option<BlendSnapshot>, String>,
    mempool: Result<MempoolSnapshot, String>,
}

impl NodeDiagnostic {
    async fn collect(index: usize, client: NodeHttpClient) -> Self {
        let (consensus, network, blend, mempool) = tokio::join!(
            client.consensus_info(),
            client.network_info(),
            client.blend_info(),
            client.mantle_metrics(),
        );

        Self {
            label: format!("node-{index}"),
            base_url: client.base_url().to_string(),
            testing_url: client.testing_url().map(ToString::to_string),
            consensus: consensus
                .map(ConsensusSnapshot::from)
                .map_err(|error| error.to_string()),
            network: network
                .map(NetworkSnapshot::from)
                .map_err(|error| error.to_string()),
            blend: blend
                .map(|value| value.map(BlendSnapshot::from))
                .map_err(|error| error.to_string()),
            mempool: mempool
                .map(MempoolSnapshot::from)
                .map_err(|error| error.to_string()),
        }
    }

    fn render(&self, peer_labels: &BTreeMap<String, String>) -> String {
        let testing_url = self
            .testing_url
            .clone()
            .unwrap_or_else(|| "none".to_owned());

        format!(
            "  {label} base_url={base_url} testing_url={testing_url}\n    consensus: {consensus}\n    network: {network}\n    blend: {blend}\n    mempool: {mempool}",
            label = self.label,
            base_url = self.base_url,
            consensus = self.format_consensus(),
            network = self.format_network(peer_labels),
            blend = self.format_blend(peer_labels),
            mempool = self.format_mempool(),
        )
    }

    fn peer_id(&self) -> Option<&str> {
        self.network.as_ref().ok().map(|info| info.peer_id.as_str())
    }

    fn format_consensus(&self) -> String {
        match &self.consensus {
            Ok(info) => format!(
                "ok height={} slot={} mode={}",
                info.height, info.slot, info.mode
            ),
            Err(error) => format!("error={error}"),
        }
    }

    fn format_network(&self, peer_labels: &BTreeMap<String, String>) -> String {
        match &self.network {
            Ok(info) => {
                let peers = info
                    .connected_peers
                    .iter()
                    .map(|peer| resolve_peer_label(peer_labels, peer))
                    .collect::<Vec<_>>();
                format!(
                    "ok peer_id={} peers={} connections={} pending={} connected_to=[{}] listen_addrs={}",
                    info.peer_id,
                    info.n_peers,
                    info.n_connections,
                    info.n_pending_connections,
                    peers.join(", "),
                    info.listen_address_count
                )
            }
            Err(error) => format!("error={error}"),
        }
    }

    fn format_blend(&self, peer_labels: &BTreeMap<String, String>) -> String {
        match &self.blend {
            Ok(Some(info)) => info.core_info.as_ref().map_or_else(|| format!(
                    "ok node_id={} mode=edge",
                    resolve_peer_label(peer_labels, &info.node_id)
                ), |core_info| {
                    let current = core_info
                        .current_session_peers
                        .iter()
                        .map(|(peer, healthy)| {
                            let suffix = if *healthy { "healthy" } else { "unhealthy" };
                            format!("{}:{suffix}", resolve_peer_label(peer_labels, peer))
                        })
                        .collect::<Vec<_>>();
                    let old = core_info.old_session_peers.as_ref().map_or_else(
                        || "none".to_owned(),
                        |peers| {
                            peers
                                .iter()
                                .map(|peer| resolve_peer_label(peer_labels, peer))
                                .collect::<Vec<_>>()
                                .join(", ")
                        },
                    );

                    format!(
                        "ok node_id={} mode=core session_peers={} unhealthy={} old_session=[{}] current_session=[{}]",
                        resolve_peer_label(peer_labels, &info.node_id),
                        core_info.current_session_peers.len(),
                        core_info
                            .current_session_peers
                            .iter()
                            .filter(|(_, healthy)| !healthy)
                            .count(),
                        old,
                        current.join(", ")
                    )
                }),
            Ok(None) => "unavailable".to_owned(),
            Err(error) => format!("error={error}"),
        }
    }

    fn format_mempool(&self) -> String {
        match &self.mempool {
            Ok(info) => format!(
                "ok pending_items={} last_item_timestamp={}",
                info.pending_items, info.last_item_timestamp
            ),
            Err(error) => format!("error={error}"),
        }
    }
}

#[derive(Clone)]
struct ConsensusSnapshot {
    height: u64,
    slot: u64,
    mode: String,
}

impl From<lb_chain_service::ChainServiceInfo> for ConsensusSnapshot {
    fn from(value: lb_chain_service::ChainServiceInfo) -> Self {
        Self {
            height: value.cryptarchia_info.height,
            slot: value.cryptarchia_info.slot.into_inner(),
            mode: format!("{:?}", value.mode),
        }
    }
}

#[derive(Clone)]
struct NetworkSnapshot {
    peer_id: String,
    connected_peers: Vec<String>,
    n_peers: usize,
    n_connections: u32,
    n_pending_connections: u32,
    listen_address_count: usize,
}

impl From<lb_network_service::backends::libp2p::Libp2pInfo> for NetworkSnapshot {
    fn from(value: lb_network_service::backends::libp2p::Libp2pInfo) -> Self {
        let mut connected_peers = value
            .connected_peers
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        connected_peers.sort();

        Self {
            peer_id: value.peer_id.to_string(),
            connected_peers,
            n_peers: value.n_peers,
            n_connections: value.n_connections,
            n_pending_connections: value.n_pending_connections,
            listen_address_count: value.listen_addresses.len(),
        }
    }
}

#[derive(Clone)]
struct BlendSnapshot {
    node_id: String,
    core_info: Option<BlendCoreSnapshot>,
}

#[derive(Clone)]
struct BlendCoreSnapshot {
    current_session_peers: Vec<(String, bool)>,
    old_session_peers: Option<Vec<String>>,
}

impl From<BlendNetworkInfo<lb_network_service::backends::libp2p::PeerId>> for BlendSnapshot {
    fn from(value: BlendNetworkInfo<lb_network_service::backends::libp2p::PeerId>) -> Self {
        let core_info = value.core_info.map(|core_info| {
            let mut current_session_peers = core_info
                .current_session_peers
                .into_iter()
                .map(|(peer, healthy)| (peer.to_string(), healthy))
                .collect::<Vec<_>>();
            current_session_peers.sort_by(|left, right| left.0.cmp(&right.0));

            let old_session_peers = core_info.old_session_peers.map(|peers| {
                let mut peers = peers
                    .into_iter()
                    .map(|peer| peer.to_string())
                    .collect::<Vec<_>>();
                peers.sort();
                peers
            });

            BlendCoreSnapshot {
                current_session_peers,
                old_session_peers,
            }
        });

        Self {
            node_id: value.node_id.to_string(),
            core_info,
        }
    }
}

#[derive(Clone)]
struct MempoolSnapshot {
    pending_items: usize,
    last_item_timestamp: u64,
}

impl From<MempoolMetrics> for MempoolSnapshot {
    fn from(value: MempoolMetrics) -> Self {
        Self {
            pending_items: value.pending_items,
            last_item_timestamp: value.last_item_timestamp,
        }
    }
}

struct ClusterSummary {
    cluster_control_profile: String,
    node_count: usize,
    consensus_ok: usize,
    network_ok: usize,
    blend_ok: usize,
    blend_unavailable: usize,
    mempool_ok: usize,
    height_range: String,
    slot_range: String,
    mode_counts: String,
    peer_range: String,
    connection_range: String,
    pending_total: u64,
    blend_session_peer_range: String,
    blend_unhealthy_total: usize,
    blend_transitioning_nodes: Vec<String>,
    mempool_pending_range: String,
    mempool_pending_total: usize,
    connectivity: ConnectivitySummary,
}

impl ClusterSummary {
    #[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
    fn build(
        cluster_control_profile: &str,
        node_count: usize,
        diagnostics: &[NodeDiagnostic],
    ) -> Self {
        let consensus = diagnostics
            .iter()
            .filter_map(|node| node.consensus.as_ref().ok())
            .collect::<Vec<_>>();
        let network = diagnostics
            .iter()
            .filter_map(|node| node.network.as_ref().ok())
            .collect::<Vec<_>>();
        let mempools = diagnostics
            .iter()
            .filter_map(|node| node.mempool.as_ref().ok())
            .collect::<Vec<_>>();
        let blend_ok = diagnostics
            .iter()
            .filter(|node| matches!(node.blend, Ok(Some(_))))
            .count();
        let blend_unavailable = diagnostics
            .iter()
            .filter(|node| matches!(node.blend, Ok(None)))
            .count();

        let mut mode_counts = BTreeMap::<String, usize>::new();
        for snapshot in &consensus {
            *mode_counts.entry(snapshot.mode.clone()).or_default() += 1;
        }

        let blend_snapshots = diagnostics
            .iter()
            .filter_map(|node| match &node.blend {
                Ok(Some(info)) => Some((&node.label, info)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let blend_core_snapshots = blend_snapshots
            .iter()
            .filter_map(|(label, info)| {
                info.core_info.as_ref().map(|core_info| (*label, core_info))
            })
            .collect::<Vec<_>>();

        Self {
            cluster_control_profile: cluster_control_profile.to_owned(),
            node_count,
            consensus_ok: consensus.len(),
            network_ok: network.len(),
            blend_ok,
            blend_unavailable,
            mempool_ok: mempools.len(),
            height_range: format_range_u64(
                &consensus
                    .iter()
                    .map(|snapshot| snapshot.height)
                    .collect::<Vec<_>>(),
            ),
            slot_range: format_range_u64(
                &consensus
                    .iter()
                    .map(|snapshot| snapshot.slot)
                    .collect::<Vec<_>>(),
            ),
            mode_counts: format_counts(mode_counts),
            peer_range: format_range_usize(
                &network
                    .iter()
                    .map(|snapshot| snapshot.n_peers)
                    .collect::<Vec<_>>(),
            ),
            connection_range: format_range_u32(
                &network
                    .iter()
                    .map(|snapshot| snapshot.n_connections)
                    .collect::<Vec<_>>(),
            ),
            pending_total: network
                .iter()
                .map(|snapshot| u64::from(snapshot.n_pending_connections))
                .sum(),
            blend_session_peer_range: format_range_usize(
                &blend_core_snapshots
                    .iter()
                    .map(|(_, info)| info.current_session_peers.len())
                    .collect::<Vec<_>>(),
            ),
            blend_unhealthy_total: blend_core_snapshots
                .iter()
                .map(|(_, info)| {
                    info.current_session_peers
                        .iter()
                        .filter(|(_, healthy)| !healthy)
                        .count()
                })
                .sum(),
            blend_transitioning_nodes: blend_core_snapshots
                .iter()
                .filter(|(_, info)| info.old_session_peers.is_some())
                .map(|(label, _)| (*label).clone())
                .collect(),
            mempool_pending_range: format_range_usize(
                &mempools
                    .iter()
                    .map(|snapshot| snapshot.pending_items)
                    .collect::<Vec<_>>(),
            ),
            mempool_pending_total: mempools.iter().map(|snapshot| snapshot.pending_items).sum(),
            connectivity: ConnectivitySummary::build(diagnostics),
        }
    }

    fn render(&self) -> String {
        [
            "cluster_summary:".to_owned(),
            format!(
                "  control_profile={} nodes={}",
                self.cluster_control_profile, self.node_count
            ),
            format!(
                "  api_ok consensus={}/{} network={}/{} blend={}/{} blend_unavailable={} mempool={}/{}",
                self.consensus_ok,
                self.node_count,
                self.network_ok,
                self.node_count,
                self.blend_ok,
                self.node_count,
                self.blend_unavailable,
                self.mempool_ok,
                self.node_count
            ),
            format!(
                "  consensus height_range={} slot_range={} modes={}",
                self.height_range, self.slot_range, self.mode_counts
            ),
            format!(
                "  network peer_range={} connection_range={} pending_total={}",
                self.peer_range, self.connection_range, self.pending_total
            ),
            format!(
                "  blend session_peer_range={} unhealthy_total={} transitioning_nodes=[{}]",
                self.blend_session_peer_range,
                self.blend_unhealthy_total,
                self.blend_transitioning_nodes.join(", ")
            ),
            format!(
                "  mempool pending_range={} pending_total={}",
                self.mempool_pending_range, self.mempool_pending_total
            ),
        ]
        .join("\n")
    }

    fn render_connectivity(&self) -> String {
        self.connectivity.render()
    }
}

struct ConnectivitySummary {
    peer_labels: BTreeMap<String, String>,
    symmetric_links: Vec<String>,
    asymmetric_links: Vec<String>,
    unknown_edges: Vec<String>,
    per_node: Vec<String>,
}

impl ConnectivitySummary {
    fn build(diagnostics: &[NodeDiagnostic]) -> Self {
        let peer_labels = diagnostics
            .iter()
            .filter_map(|node| {
                node.peer_id()
                    .map(|peer_id| (peer_id.to_owned(), node.label.clone()))
            })
            .collect::<BTreeMap<_, _>>();

        let mut directed = BTreeSet::<(String, String)>::new();
        let mut unknown_edges = Vec::new();
        let mut per_node = Vec::new();

        for node in diagnostics {
            let Ok(network) = &node.network else {
                per_node.push(format!("  {} -> unavailable", node.label));
                continue;
            };

            let mut known = Vec::new();
            let mut unknown = Vec::new();
            for peer in &network.connected_peers {
                if let Some(label) = peer_labels.get(peer) {
                    directed.insert((node.label.clone(), label.clone()));
                    known.push(label.clone());
                } else {
                    unknown.push(peer.clone());
                    unknown_edges.push(format!("{} -> {}", node.label, peer));
                }
            }

            per_node.push(format!(
                "  {} -> [{}] unknown=[{}]",
                node.label,
                known.join(", "),
                unknown.join(", ")
            ));
        }

        let mut symmetric_pairs = BTreeSet::new();
        let mut asymmetric_links = Vec::new();
        for (left, right) in &directed {
            if directed.contains(&(right.clone(), left.clone())) {
                let ordered = if left <= right {
                    format!("{left} <-> {right}")
                } else {
                    format!("{right} <-> {left}")
                };
                symmetric_pairs.insert(ordered);
            } else {
                asymmetric_links.push(format!("{left} -> {right}"));
            }
        }

        Self {
            peer_labels,
            symmetric_links: symmetric_pairs.into_iter().collect(),
            asymmetric_links,
            unknown_edges,
            per_node,
        }
    }

    fn render(&self) -> String {
        [
            "connectivity:".to_owned(),
            format!("  identified_nodes={}", self.peer_labels.len()),
            format!("  symmetric_links=[{}]", self.symmetric_links.join(", ")),
            format!("  asymmetric_links=[{}]", self.asymmetric_links.join(", ")),
            format!("  unknown_peer_edges=[{}]", self.unknown_edges.join(", ")),
            "  per_node:".to_owned(),
            self.per_node.join("\n"),
        ]
        .join("\n")
    }
}

fn render_nodes(diagnostics: &[NodeDiagnostic]) -> String {
    if diagnostics.is_empty() {
        return "node_api_snapshots:\n  no node clients available".to_owned();
    }

    let peer_labels = diagnostics
        .iter()
        .filter_map(|node| {
            node.peer_id()
                .map(|peer_id| (peer_id.to_owned(), node.label.clone()))
        })
        .collect::<BTreeMap<_, _>>();

    let mut lines = vec!["node_api_snapshots:".to_owned()];
    for diagnostic in diagnostics {
        lines.push(diagnostic.render(&peer_labels));
    }

    lines.join("\n")
}

fn resolve_peer_label(peer_labels: &BTreeMap<String, String>, peer: &str) -> String {
    peer_labels
        .get(peer)
        .cloned()
        .unwrap_or_else(|| format!("unknown:{peer}"))
}

fn scenario_error_has_diagnostics(error: &ScenarioError) -> bool {
    error.to_string().contains("failure diagnostics:")
}

fn dyn_error_has_diagnostics(error: &DynError) -> bool {
    error.to_string().contains("failure diagnostics:")
}

fn format_counts(counts: BTreeMap<String, usize>) -> String {
    if counts.is_empty() {
        return "none".to_owned();
    }

    counts
        .into_iter()
        .map(|(label, count)| format!("{label}={count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_range_u64(values: &[u64]) -> String {
    if values.is_empty() {
        return "n/a".to_owned();
    }

    let min = values.iter().min().copied().unwrap_or_default();
    let max = values.iter().max().copied().unwrap_or_default();
    format!("{min}..{max}")
}

fn format_range_usize(values: &[usize]) -> String {
    if values.is_empty() {
        return "n/a".to_owned();
    }

    let min = values.iter().min().copied().unwrap_or_default();
    let max = values.iter().max().copied().unwrap_or_default();
    format!("{min}..{max}")
}

fn format_range_u32(values: &[u32]) -> String {
    if values.is_empty() {
        return "n/a".to_owned();
    }

    let min = values.iter().min().copied().unwrap_or_default();
    let max = values.iter().max().copied().unwrap_or_default();
    format!("{min}..{max}")
}
