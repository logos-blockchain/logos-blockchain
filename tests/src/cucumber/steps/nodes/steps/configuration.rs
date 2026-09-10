use super::{
    CucumberWorld, SdpFundingConfig, Step, StepError, StepResult,
    ensure_fee_sponsorship_and_fork_groups_are_not_mixed, given,
    rebuild_pending_local_manual_cluster, set_blend_diagnostic_parameter_set,
    set_deployment_config_override, set_user_config_override, when,
};

#[given(expr = "the cluster uses Blend diagnostic parameter set {string}")]
#[when(expr = "the cluster uses Blend diagnostic parameter set {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Cucumber step arguments must use owned types"
)]
fn step_set_blend_diagnostic_parameter_set(
    world: &mut CucumberWorld,
    step: &Step,
    parameter_set_name: String,
) -> StepResult {
    set_blend_diagnostic_parameter_set(world, &step.value, &parameter_set_name)
}

#[given(expr = "we use IBD peers")]
#[when(expr = "we use IBD peers")]
const fn step_we_use_ibd_peers(world: &mut CucumberWorld) {
    world.startup.populate_ibd_peers_from_initial_peers = Some(true);
}

#[given(expr = "we join an external network")]
#[when(expr = "we join an external network")]
const fn step_we_join_external_network(world: &mut CucumberWorld) {
    world.startup.join_external_network = Some(true);
}

#[given(expr = "we will have distinct node groups to query wallet balances:")]
#[when(expr = "we will have distinct node groups to query wallet balances:")]
fn step_define_node_groups(world: &mut CucumberWorld, step: &Step) -> Result<(), StepError> {
    ensure_fee_sponsorship_and_fork_groups_are_not_mixed(world, &step.value)?;

    let table = step.table.as_ref().ok_or(StepError::LogicalError {
        message: "Expected a data table".to_owned(),
    })?;

    if table.rows.is_empty() || table.rows[0].len() != 2 {
        return Err(StepError::LogicalError {
            message: "Expected table columns: | group_name | node_name |".to_owned(),
        });
    }

    if table.rows[0][0].trim() != "group_name" || table.rows[0][1].trim() != "node_name" {
        return Err(StepError::LogicalError {
            message: "Expected table columns: | group_name | node_name |".to_owned(),
        });
    }

    let assignments = table
        .rows
        .iter()
        .skip(1)
        .map(|row| {
            if row.len() != 2 {
                return Err(StepError::LogicalError {
                    message: "Each node-group row must have exactly two columns".to_owned(),
                });
            }

            Ok((row[0].trim().to_owned(), row[1].trim().to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    world.fork_groups.replace_all(assignments)
}

#[given(expr = "I have user config override {string} as {string}")]
#[when(expr = "I have user config override {string} as {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Required by cucumber expression"
)]
fn step_set_user_config_setting(
    world: &mut CucumberWorld,
    step: &Step,
    setting_path: String,
    setting_value: String,
) -> StepResult {
    set_user_config_override(world, &step.value, &setting_path, &setting_value)
}

#[given(expr = "I have deployment config override {string} as {string}")]
#[when(expr = "I have deployment config override {string} as {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Required by cucumber expression"
)]
fn step_set_deployment_config_setting(
    world: &mut CucumberWorld,
    step: &Step,
    setting_path: String,
    setting_value: String,
) -> StepResult {
    set_deployment_config_override(world, &step.value, &setting_path, &setting_value)
}

#[given(expr = "the first {int} nodes are declared as blend providers")]
#[when(expr = "the first {int} nodes are declared as blend providers")]
fn step_blend_provider_count(world: &mut CucumberWorld, provider_count: usize) -> StepResult {
    world.cluster.blend_core_nodes = Some(provider_count);
    rebuild_pending_local_manual_cluster(world)
}

#[given(expr = "the cluster uses SDP funding of {int} per provider split across {int} notes")]
#[when(expr = "the cluster uses SDP funding of {int} per provider split across {int} notes")]
fn step_set_sdp_funding(
    world: &mut CucumberWorld,
    total_value_per_node: u64,
    target_notes_per_node: usize,
) -> StepResult {
    if target_notes_per_node == 0 {
        return Err(StepError::InvalidArgument {
            message: "SDP funding note count must be greater than zero".to_owned(),
        });
    }

    world.set_sdp_funding_config(SdpFundingConfig::new(
        total_value_per_node,
        target_notes_per_node,
    ));
    rebuild_pending_local_manual_cluster(world)
}

#[given(expr = "no nodes are declared as blend providers")]
#[when(expr = "no nodes are declared as blend providers")]
fn step_no_blend_providers(world: &mut CucumberWorld) -> StepResult {
    world.cluster.blend_core_nodes = Some(0);
    rebuild_pending_local_manual_cluster(world)
}
