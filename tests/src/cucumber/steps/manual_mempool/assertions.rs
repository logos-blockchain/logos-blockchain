use std::time::Duration;

use lb_core::mantle::TxHash;
use tokio::time::{sleep, timeout};

use crate::{
    cucumber::{error::StepError, world::CucumberWorld},
    non_zero,
};

pub async fn assert_transaction_not_pending_on_all_nodes(
    world: &CucumberWorld,
    transaction_alias: String,
    timeout_seconds: u64,
) -> Result<(), StepError> {
    let tx_hash = world.resolve_submitted_transaction(&transaction_alias)?;
    let node_names = all_node_names(world);

    let wait_result = timeout(Duration::from_secs(timeout_seconds), async {
        loop {
            let views = collect_mempool_views(world, &node_names).await?;

            if views.all_absent(tx_hash) {
                return Ok(());
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await;

    if let Ok(result) = wait_result {
        return result;
    }

    let observed = describe_mempool_views(world, &node_names).await;

    Err(StepError::Timeout {
        message: format!(
            "Timed out waiting for transaction `{transaction_alias}` ({tx_hash:?}) to be absent from every node mempool. Observed views: {observed}"
        ),
    })
}

pub async fn assert_transaction_pending_on_nodes(
    world: &CucumberWorld,
    transaction_alias: String,
    node_names: Vec<String>,
    timeout_seconds: u64,
) -> Result<(), StepError> {
    let tx_hash = world.resolve_submitted_transaction(&transaction_alias)?;

    let wait_result = timeout(Duration::from_secs(timeout_seconds), async {
        loop {
            let views = collect_mempool_views(world, &node_names).await?;

            if views.all_contain(tx_hash) {
                return Ok(());
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await;

    if let Ok(result) = wait_result {
        return result;
    }

    let observed = describe_mempool_views(world, &node_names).await;

    Err(StepError::Timeout {
        message: format!(
            "Timed out waiting for transaction `{transaction_alias}` ({tx_hash:?}) to be pending in mempools of nodes: {}. Observed views: {observed}",
            node_names.join(", ")
        ),
    })
}

pub async fn assert_transaction_remains_not_pending_on_all_nodes(
    world: &CucumberWorld,
    transaction_alias: String,
    blocks: u64,
    timeout_seconds: u64,
) -> Result<(), StepError> {
    let tx_hash = world.resolve_submitted_transaction(&transaction_alias)?;
    let required_blocks = non_zero!("blocks", blocks)?.get();
    let node_names = all_node_names(world);

    let wait_result = timeout(Duration::from_secs(timeout_seconds), async {
        let start_height = loop {
            let views = collect_mempool_views(world, &node_names).await?;

            if views.all_absent(tx_hash) {
                break minimum_node_height(world).await?;
            }

            sleep(Duration::from_millis(500)).await;
        };
        let target_height = start_height.saturating_add(required_blocks);

        loop {
            let views = collect_mempool_views(world, &node_names).await?;

            if !views.all_absent(tx_hash) {
                return Err(StepError::StepFail {
                    message: format!(
                        "Transaction `{transaction_alias}` became pending again in at least one node mempool. Observed views: {}",
                        views.describe()
                    ),
                });
            }

            if minimum_node_height(world).await? >= target_height {
                return Ok(());
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await;

    if let Ok(result) = wait_result {
        return result;
    }

    let observed = describe_mempool_views(world, &node_names).await;

    Err(StepError::Timeout {
        message: format!(
            "Timed out waiting for transaction `{transaction_alias}` to remain absent from every node mempool for {required_blocks} blocks. Observed views: {observed}"
        ),
    })
}

async fn collect_mempool_views(
    world: &CucumberWorld,
    node_names: &[String],
) -> Result<MempoolViews, StepError> {
    let mut nodes = Vec::with_capacity(node_names.len());

    for node_name in node_names {
        let client = world.resolve_node_http_client(node_name)?;
        let pending = client.test_mempool_view().await?;

        nodes.push(NodeMempoolView {
            node_name: node_name.clone(),
            pending,
        });
    }

    Ok(MempoolViews { nodes })
}

async fn describe_mempool_views(world: &CucumberWorld, node_names: &[String]) -> String {
    let mut views = Vec::new();

    for node_name in node_names {
        let view = match world.resolve_node_http_client(node_name) {
            Ok(client) => match client.test_mempool_view().await {
                Ok(items) => format!("{items:?}"),
                Err(error) => format!("error: {error}"),
            },
            Err(error) => format!("error: {error}"),
        };

        views.push(format!("{node_name}={view}"));
    }

    views.join("; ")
}

fn all_node_names(world: &CucumberWorld) -> Vec<String> {
    let mut node_names = world.all_node_names();
    node_names.sort();
    node_names
}

async fn minimum_node_height(world: &CucumberWorld) -> Result<u64, StepError> {
    let mut min_height = None;

    for node_name in world.all_node_names() {
        let client = world.resolve_node_http_client(&node_name)?;
        let height = client.consensus_info().await?.cryptarchia_info.height;
        min_height = Some(min_height.map_or(height, |current: u64| current.min(height)));
    }

    min_height.ok_or_else(|| StepError::StepFail {
        message: "No nodes are available to inspect mempool status".to_owned(),
    })
}

struct MempoolViews {
    nodes: Vec<NodeMempoolView>,
}

impl MempoolViews {
    fn all_absent(&self, tx_hash: TxHash) -> bool {
        self.nodes.iter().all(|node| !node.contains(tx_hash))
    }

    fn all_contain(&self, tx_hash: TxHash) -> bool {
        self.nodes.iter().all(|node| node.contains(tx_hash))
    }

    fn describe(&self) -> String {
        self.nodes
            .iter()
            .map(NodeMempoolView::describe)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

struct NodeMempoolView {
    node_name: String,
    pending: Vec<TxHash>,
}

impl NodeMempoolView {
    fn contains(&self, tx_hash: TxHash) -> bool {
        self.pending.contains(&tx_hash)
    }

    fn describe(&self) -> String {
        format!("{}={:?}", self.node_name, self.pending)
    }
}
