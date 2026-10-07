use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::Duration,
};

use lb_node::HeaderId;

use super::SnapshotAnalysis;
use crate::{BlockFeedSnapshot, NodeHeadSnapshot};

/// Periodic runtime snapshot emitted while the monitor is running.
#[expect(
    dead_code,
    reason = "fields are consumed through structured Debug logging"
)]
#[derive(Debug)]
pub(super) struct ProgressSnapshot {
    /// Number of nodes included in the snapshot.
    nodes: usize,
    /// Number of distinct tips currently present.
    tip_set_size: usize,
    /// Number of distinct LIBs currently present.
    lib_set_size: usize,
    /// Highest tip height currently observed.
    max_tip_height: Option<u64>,
    /// Highest LIB height currently observed.
    max_lib_height: Option<u64>,
    /// Current spread between highest and lowest tip heights.
    tip_height_gap: u64,
    /// Current spread between highest and lowest LIB heights.
    lib_height_gap: u64,
    /// Active threshold for cluster tip stalls.
    tip_stall_threshold_secs: u64,
    /// Active threshold for cluster LIB stalls.
    lib_stall_threshold_secs: u64,
    /// Per-node tip/LIB snapshot rendered for human inspection.
    snapshot: String,
}

/// End-of-run monitor summary emitted during expectation evaluation.
#[expect(
    dead_code,
    reason = "fields are consumed through structured Debug logging"
)]
#[derive(Debug)]
pub(super) struct MonitorSummary {
    /// Largest distinct LIB set seen during the run.
    max_lib_set: usize,
    /// Largest distinct tip set seen during the run.
    max_tip_set: usize,
    /// Highest tip height reached during the run.
    max_tip_height: Option<u64>,
    /// Highest LIB height reached during the run.
    max_lib_height: Option<u64>,
    /// Longest interval without cluster tip progress.
    longest_tip_stall_secs: u64,
    /// Longest interval one node lagged behind the moving cluster tip.
    longest_node_tip_stall_secs: u64,
    /// Longest interval without cluster LIB progress.
    longest_lib_stall_secs: u64,
    /// Configured cluster tip-stall threshold.
    tip_stall_threshold_secs: u64,
    /// Configured node tip-stall threshold.
    node_tip_stall_threshold_secs: u64,
    /// Configured cluster LIB-stall threshold.
    lib_stall_threshold_secs: u64,
    /// Largest tip-height spread seen in any snapshot.
    worst_tip_gap: u64,
    /// Largest LIB-height spread seen in any snapshot.
    worst_lib_gap: u64,
    /// Number of ancestry edges retained in the final report.
    observed_blocks: usize,
    /// Final grouping of nodes by tip header.
    final_tip_groups: String,
    /// Final grouping of nodes by LIB header.
    final_lib_groups: String,
    /// Final ancestry relation summary for distinct LIBs.
    final_lib_relations: String,
    /// Final ancestry relation summary for distinct tips.
    final_tip_relations: String,
    /// Final per-node snapshot rendered for human inspection.
    final_snapshot: String,
}

#[derive(Clone, Copy)]
pub(super) struct SummaryState {
    pub(super) max_lib_set: usize,
    pub(super) max_tip_set: usize,
    pub(super) max_tip_height: Option<u64>,
    pub(super) max_lib_height: Option<u64>,
    pub(super) longest_tip_stall: Duration,
    pub(super) longest_node_tip_stall: Duration,
    pub(super) longest_lib_stall: Duration,
    pub(super) tip_stall_threshold: Duration,
    pub(super) node_tip_stall_threshold: Duration,
    pub(super) lib_stall_threshold: Duration,
    pub(super) worst_tip_gap: u64,
    pub(super) worst_lib_gap: u64,
}

/// Distinguishes whether reporting is grouping/relating tips or LIBs.
#[derive(Clone, Copy)]
enum HeaderKind {
    Tip,
    Lib,
}

/// Result of a reachability walk through the ancestry graph.
#[derive(Clone, Copy)]
struct Reachability {
    /// Whether the target was reached while following parents.
    reaches: bool,
    /// Whether the walk terminated cleanly at a known root or target.
    complete: bool,
}

/// Nodes currently sharing the same header in a report.
struct HeaderGroup {
    /// Optional height attached to this header in the snapshot.
    height: Option<u64>,
    /// Nodes currently reporting this header.
    nodes: Vec<String>,
}

/// Rendering helper for summaries derived from one node-head snapshot plus the
/// accumulated ancestry graph.
pub(super) struct ReportFormatter<'a> {
    /// Per-node heads being reported.
    node_heads: &'a [NodeHeadSnapshot],
    /// Accumulated ancestry graph used for relation checks.
    parent_edges: &'a HashMap<HeaderId, HeaderId>,
    /// Cached per-node snapshot string reused across multiple reports.
    snapshot_description: String,
}

impl ReportFormatter<'_> {
    /// Renders a snapshot summary line without constructing a full formatter in
    /// the caller.
    pub(super) fn snapshot_description_for(node_heads: &[NodeHeadSnapshot]) -> String {
        format_snapshot_description(node_heads)
    }

    /// Builds the periodic progress log payload from one analyzed snapshot.
    pub(super) fn progress_snapshot(
        node_heads: &[NodeHeadSnapshot],
        analysis: &SnapshotAnalysis<'_>,
        tip_stall_threshold: Duration,
        lib_stall_threshold: Duration,
    ) -> ProgressSnapshot {
        ProgressSnapshot {
            nodes: node_heads.len(),
            tip_set_size: analysis.tip_set_size,
            lib_set_size: analysis.lib_set_size,
            max_tip_height: analysis.max_tip_height,
            max_lib_height: analysis.max_lib_height,
            tip_height_gap: analysis.tip_height_gap,
            lib_height_gap: analysis.lib_height_gap,
            tip_stall_threshold_secs: tip_stall_threshold.as_secs(),
            lib_stall_threshold_secs: lib_stall_threshold.as_secs(),
            snapshot: Self::snapshot_description_for(node_heads),
        }
    }

    /// Builds the final summary log payload from the monitor state and final
    /// snapshot.
    pub(super) fn summary(
        snapshot: &BlockFeedSnapshot,
        ancestry_edges: &HashMap<HeaderId, HeaderId>,
        state: SummaryState,
    ) -> MonitorSummary {
        let formatter = ReportFormatter::new(&snapshot.node_heads, ancestry_edges);

        MonitorSummary {
            max_lib_set: state.max_lib_set,
            max_tip_set: state.max_tip_set,
            max_tip_height: state.max_tip_height,
            max_lib_height: state.max_lib_height,
            longest_tip_stall_secs: state.longest_tip_stall.as_secs(),
            longest_node_tip_stall_secs: state.longest_node_tip_stall.as_secs(),
            longest_lib_stall_secs: state.longest_lib_stall.as_secs(),
            tip_stall_threshold_secs: state.tip_stall_threshold.as_secs(),
            node_tip_stall_threshold_secs: state.node_tip_stall_threshold.as_secs(),
            lib_stall_threshold_secs: state.lib_stall_threshold.as_secs(),
            worst_tip_gap: state.worst_tip_gap,
            worst_lib_gap: state.worst_lib_gap,
            observed_blocks: ancestry_edges.len().max(snapshot.parent_edges.len()),
            final_tip_groups: formatter.header_groups(HeaderKind::Tip),
            final_lib_groups: formatter.header_groups(HeaderKind::Lib),
            final_lib_relations: formatter.header_relations(HeaderKind::Lib),
            final_tip_relations: formatter.header_relations(HeaderKind::Tip),
            final_snapshot: formatter.snapshot_description().to_owned(),
        }
    }
}

impl<'a> ReportFormatter<'a> {
    /// Creates a formatter bound to one snapshot and ancestry graph.
    pub(super) fn new(
        node_heads: &'a [NodeHeadSnapshot],
        parent_edges: &'a HashMap<HeaderId, HeaderId>,
    ) -> Self {
        Self {
            node_heads,
            parent_edges,
            snapshot_description: format_snapshot_description(node_heads),
        }
    }

    /// Returns the cached per-node snapshot description.
    fn snapshot_description(&self) -> &str {
        &self.snapshot_description
    }

    /// Builds a detailed LIB divergence report with grouping, pairwise
    /// reachability and short ancestry paths.
    pub(super) fn lib_divergence_details(&self, libs: &[HeaderId]) -> String {
        let groups = self.header_groups(HeaderKind::Lib);

        let mut relations = Vec::new();
        for (idx, left) in libs.iter().enumerate() {
            for right in libs.iter().skip(idx + 1) {
                let left_on_right = self.reachability(*right, *left);
                let right_on_left = self.reachability(*left, *right);
                relations.push(format!(
                    "{left:?}<={right:?}:reaches={} complete={} ; {right:?}<={left:?}:reaches={} complete={}",
                    left_on_right.reaches,
                    left_on_right.complete,
                    right_on_left.reaches,
                    right_on_left.complete
                ));
            }
        }

        let paths = libs
            .iter()
            .map(|lib| format!("{lib:?} path={}", self.ancestry_path(*lib, 8)))
            .collect::<Vec<_>>()
            .join(" | ");

        format!(
            "lib_groups={groups}; relations={}; paths={paths}",
            relations.join(" | ")
        )
    }

    /// Groups nodes by either tip or LIB header.
    fn header_groups(&self, kind: HeaderKind) -> String {
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for head in self.node_heads {
            let (header, height) = match kind {
                HeaderKind::Tip => (head.tip, head.tip_height),
                HeaderKind::Lib => (head.lib, head.lib_height),
            };

            let key = format!("{header:?}@{}", format_height(height));
            groups.entry(key).or_default().push(head.node.clone());
        }

        groups
            .into_iter()
            .map(|(header, mut nodes)| {
                nodes.sort();
                format!("{header}<={}", nodes.join(","))
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// Reports ancestry relations between the distinct headers in one snapshot.
    fn header_relations(&self, kind: HeaderKind) -> String {
        let groups = self.collect_header_groups(kind);
        if groups.is_empty() {
            return "none".to_owned();
        }

        let headers = groups.keys().copied().collect::<Vec<_>>();

        headers
            .iter()
            .map(|header| self.format_header_relation(*header, &headers, &groups))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// Builds grouped node ownership for each distinct header.
    fn collect_header_groups(&self, kind: HeaderKind) -> BTreeMap<HeaderId, HeaderGroup> {
        let mut groups = BTreeMap::new();

        for head in self.node_heads {
            let (header, height) = match kind {
                HeaderKind::Tip => (head.tip, head.tip_height),
                HeaderKind::Lib => (head.lib, head.lib_height),
            };

            let entry = groups.entry(header).or_insert_with(|| HeaderGroup {
                height,
                nodes: Vec::new(),
            });

            if entry.height.is_none() {
                entry.height = height;
            }

            entry.nodes.push(head.node.clone());
        }

        groups
    }

    /// Formats one distinct header together with its relations to the others.
    fn format_header_relation(
        &self,
        header: HeaderId,
        headers: &[HeaderId],
        groups: &BTreeMap<HeaderId, HeaderGroup>,
    ) -> String {
        let group = groups
            .get(&header)
            .expect("header group must exist for relation formatting");
        let mut nodes = group.nodes.clone();
        nodes.sort();

        let mut relations = headers
            .iter()
            .filter(|other| **other != header)
            .map(|other| self.relation_summary(header, *other, groups))
            .collect::<Vec<_>>();
        relations.sort();

        format!(
            "{header:?}@{} nodes={} relations=[{}]",
            format_height(group.height),
            nodes.join(","),
            relations.join(", ")
        )
    }

    /// Computes the relation between one header and another for reporting.
    fn relation_summary(
        &self,
        header: HeaderId,
        other: HeaderId,
        groups: &BTreeMap<HeaderId, HeaderGroup>,
    ) -> String {
        let this_on_other = self.reachability(header, other);
        let other_on_this = self.reachability(other, header);
        let relation = if this_on_other.reaches {
            "descends-from"
        } else if other_on_this.reaches {
            "ancestor-of"
        } else if this_on_other.complete && other_on_this.complete {
            "fork"
        } else {
            "unknown"
        };

        format!(
            "{relation}:{other:?}@{}",
            format_height(groups[&other].height)
        )
    }

    /// Walks parent links to determine whether `start` reaches `target`.
    fn reachability(&self, start: HeaderId, target: HeaderId) -> Reachability {
        let mut cursor = start;
        let mut seen = HashSet::new();

        loop {
            if cursor == target {
                return Reachability {
                    reaches: true,
                    complete: true,
                };
            }

            if !seen.insert(cursor) {
                return Reachability {
                    reaches: false,
                    complete: false,
                };
            }

            let Some(parent) = self.parent_edges.get(&cursor).copied() else {
                return Reachability {
                    reaches: false,
                    complete: false,
                };
            };

            if parent == cursor {
                return Reachability {
                    reaches: false,
                    complete: true,
                };
            }

            cursor = parent;
        }
    }

    /// Renders a short ancestry chain starting from `start`.
    fn ancestry_path(&self, start: HeaderId, max_len: usize) -> String {
        let mut path = vec![format!("{start:?}")];
        let mut cursor = start;

        for _ in 0..max_len {
            let Some(parent) = self.parent_edges.get(&cursor).copied() else {
                path.push("unknown-parent".to_owned());
                break;
            };

            if parent == cursor {
                path.push("self-parent".to_owned());
                break;
            }

            path.push(format!("{parent:?}"));
            cursor = parent;
        }

        path.join(" <- ")
    }
}

/// Builds the cached per-node snapshot description used by the formatter.
fn format_snapshot_description(node_heads: &[NodeHeadSnapshot]) -> String {
    node_heads
        .iter()
        .map(|head| {
            format!(
                "node={} tip={:?}@{} lib={:?}@{}",
                head.node,
                head.tip,
                format_height(head.tip_height),
                head.lib,
                format_height(head.lib_height)
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Formats an optional height for compact report output.
fn format_height(height: Option<u64>) -> String {
    height.map_or_else(|| "?".to_owned(), |value| value.to_string())
}
