use super::{
    CucumberWorld, NodeSnapshot, StepError, StepResult,
    create_snapshot_all_nodes_with_wallet_state, create_snapshot_node_with_wallet_state, given,
    prepare_wallet_snapshot_restore_if_present, then, validate_snapshot_path_component, when,
};

#[given(expr = "I will create a snapshot {string} of all nodes when stopping")]
#[when(expr = "I will create a snapshot {string} of all nodes when stopping")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Required by cucumber expression"
)]
fn step_set_snapshot_all_nodes_on_stop(
    world: &mut CucumberWorld,
    snapshot_name: String,
) -> StepResult {
    if snapshot_name.trim().is_empty() {
        return Err(StepError::InvalidArgument {
            message: "Snapshot name cannot be empty".to_owned(),
        });
    }
    validate_snapshot_path_component(&snapshot_name, "Snapshot name")?;
    let snapshot_name = snapshot_name.trim().to_owned();
    world.snapshots.save.node_state = Some(snapshot_name.clone());
    world.snapshots.save.extensions = Some(snapshot_name);
    Ok(())
}

#[given(expr = "I will initialize started nodes from snapshot {string} source node {string}")]
#[when(expr = "I will initialize started nodes from snapshot {string} source node {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Required by cucumber expression"
)]
fn step_set_node_snapshot_on_startup(
    world: &mut CucumberWorld,
    snapshot_name: String,
    node_name: String,
) -> StepResult {
    validate_snapshot_path_component(&snapshot_name, "Snapshot name")?;
    validate_snapshot_path_component(&node_name, "Node name")?;

    let snapshot_name = snapshot_name.trim().to_owned();
    world.snapshots.node_snapshot_on_startup = Some(NodeSnapshot {
        name: snapshot_name.clone(),
        node: node_name.trim().to_owned(),
    });
    world.snapshots.restore.extensions = Some(snapshot_name);
    if let Some(snapshot_name) = world.snapshots.restore.extensions.clone() {
        prepare_wallet_snapshot_restore_if_present(&snapshot_name, world)?;
    }
    Ok(())
}

#[given(expr = "I create a snapshot {string} of all nodes")]
#[when(expr = "I create a snapshot {string} of all nodes")]
async fn step_create_snapshot_all_nodes_now(
    world: &mut CucumberWorld,
    snapshot_name: String,
) -> StepResult {
    create_snapshot_all_nodes_with_wallet_state(world, &snapshot_name).await
}

#[given(expr = "I create a snapshot {string} of node {string}")]
#[when(expr = "I create a snapshot {string} of node {string}")]
#[then(expr = "I create a snapshot {string} of node {string}")]
async fn step_create_snapshot_node_now(
    world: &mut CucumberWorld,
    snapshot_name: String,
    node_name: String,
) -> StepResult {
    create_snapshot_node_with_wallet_state(world, &snapshot_name, &node_name).await
}
