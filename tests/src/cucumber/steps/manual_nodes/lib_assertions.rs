use std::time::Duration;

use cucumber::gherkin::Step;
use hex::ToHex as _;
use lb_chain_service::CryptarchiaInfo;
use tokio::time::{Instant, sleep};
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
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut next_progress_log = Instant::now();

    loop {
        let snapshots = fetch_lib_snapshots(world).await?;

        if let Err(err) = validate_shared_lib_at_height(&snapshots, min_height) {
            if Instant::now() >= deadline {
                warn!(
                    target: TARGET,
                    "Step `{}` error: {err}",
                    step.value
                );
                return Err(err);
            }

            if Instant::now() >= next_progress_log {
                info!(
                    target: TARGET,
                    "Waiting for shared LIB at or above height {min_height}: {}",
                    format_lib_snapshots(&snapshots)
                );
                next_progress_log = Instant::now() + Duration::from_secs(5);
            }

            sleep(Duration::from_secs(1)).await;
            continue;
        }

        return Ok(());
    }
}

fn format_lib_snapshots(snapshots: &[LibSnapshot]) -> String {
    snapshots
        .iter()
        .map(|snapshot| {
            format!(
                "{}@{}:{}",
                snapshot.node_name, snapshot.lib_height, snapshot.lib_hash
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone)]
struct LibSnapshot {
    node_name: String,
    lib_hash: String,
    lib_height: u64,
}

async fn resolve_lib_height(
    world: &CucumberWorld,
    node_name: &str,
    consensus: &CryptarchiaInfo,
) -> Result<u64, StepError> {
    let client = world.resolve_node_http_client(node_name)?;
    let target_lib = consensus.lib;
    let mut current = consensus.tip;
    let mut current_height = consensus.height;

    loop {
        if current == target_lib {
            return Ok(current_height);
        }

        let Some(block) = client.block(&current).await? else {
            return Err(StepError::LogicalError {
                message: format!(
                    "node `{node_name}` could not resolve block `{}` while tracing LIB `{}` \
                     from tip `{}`",
                    current.encode_hex::<String>(),
                    target_lib.encode_hex::<String>(),
                    consensus.tip.encode_hex::<String>(),
                ),
            });
        };

        if current_height == 0 {
            return Err(StepError::LogicalError {
                message: format!(
                    "node `{node_name}` reached height 0 before locating LIB `{}`",
                    target_lib.encode_hex::<String>(),
                ),
            });
        }

        current = block.header.parent_block;
        current_height -= 1;
    }
}

async fn fetch_lib_snapshots(world: &CucumberWorld) -> Result<Vec<LibSnapshot>, StepError> {
    let mut node_names = world.nodes_info.keys().cloned().collect::<Vec<_>>();
    node_names.sort();

    let mut snapshots = Vec::with_capacity(node_names.len());
    for node_name in node_names {
        let client = world.resolve_node_http_client(&node_name)?;
        let consensus = client.consensus_info().await?;
        let lib_height = resolve_lib_height(world, &node_name, &consensus).await?;

        snapshots.push(LibSnapshot {
            node_name,
            lib_hash: consensus.lib.encode_hex::<String>(),
            lib_height,
        });
    }

    Ok(snapshots)
}

fn validate_shared_lib_at_height(
    snapshots: &[LibSnapshot],
    min_height: u64,
) -> Result<(), StepError> {
    let Some((baseline, peers)) = snapshots.split_first() else {
        return Err(StepError::InvalidArgument {
            message: "LIB assertion requires at least one running node".to_owned(),
        });
    };

    if let Some(snapshot) = snapshots
        .iter()
        .find(|snapshot| snapshot.lib_height < min_height)
    {
        return Err(StepError::StepFail {
            message: format!(
                "node `{}` has LIB height {}, below required height {min_height}",
                snapshot.node_name, snapshot.lib_height
            ),
        });
    }

    if let Some(snapshot) = peers
        .iter()
        .find(|snapshot| snapshot.lib_hash != baseline.lib_hash)
    {
        return Err(StepError::StepFail {
            message: format!(
                "nodes do not share the same LIB: expected {} from `{}`, got {} on `{}`",
                baseline.lib_hash, baseline.node_name, snapshot.lib_hash, snapshot.node_name
            ),
        });
    }

    Ok(())
}
