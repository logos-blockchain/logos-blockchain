use super::{
    CucumberWorld, DriveMode, Duration, Step, StepError, StepResult, ZoneSequencerStartup, given,
    initialize_zone_indexer, log_step_error, parse_optional_submit_depth,
    register_zone_sequencers_with_shared_key, single_column_table,
    start_deposit_reaction_sequencer, start_deposit_withdraw_sequencer, start_named_sequencer,
    start_named_sequencer_with_startup, start_nodes_with_zone_resources, stop_zone_sequencer,
    wait_for_lib_advance, when, zone_account_balances, zone_node_resource_rows,
    zone_sequencer_start_rows, zone_step_error,
};

#[given("I start nodes with wallet and sequencer resources:")]
#[when("I start nodes with wallet and sequencer resources:")]
async fn step_start_nodes_with_wallet_and_sequencer_resources(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    let rows = zone_node_resource_rows(step)?;

    start_nodes_with_zone_resources(world, step, rows).await
}

#[given(expr = "the following zone sequencers share the signing key of {string}:")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Cucumber string captures are provided as owned `String`s"
)]
fn step_zone_sequencers_share_signing_key(
    world: &mut CucumberWorld,
    step: &Step,
    source_alias: String,
) -> StepResult {
    let aliases = single_column_table(step, "alias", "zone sequencer aliases")?;
    register_zone_sequencers_with_shared_key(world, &source_alias, aliases)
}

#[given("the following zone account balances exist:")]
fn step_zone_account_balances(world: &mut CucumberWorld, step: &Step) -> StepResult {
    let balances = zone_account_balances(step)?
        .into_iter()
        .map(|row| (row.account, row.balance))
        .collect();

    world.zone.set_zone_account_balances(balances);

    Ok(())
}

#[when(expr = "I start zone sequencer {string}")]
async fn step_start_zone_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    start_named_sequencer(world, step, sequencer_alias, None, DriveMode::passive()).await
}

#[when(expr = "I start zone sequencer {string} with indexer")]
async fn step_start_zone_sequencer_with_indexer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    start_sequencer_with_indexer(world, step, &sequencer_alias).await
}

async fn start_sequencer_with_indexer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
) -> StepResult {
    start_named_sequencer(world, step, sequencer_alias, None, DriveMode::passive()).await?;
    initialize_zone_indexer(world, step, sequencer_alias)
}

#[when(
    expr = "I start zone sequencer {string} integrating then withdrawing observed deposits with outputs {string}"
)]
async fn step_start_zone_sequencer_deposit_reaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    outputs: String,
) -> StepResult {
    let withdraw_outputs = outputs
        .split(',')
        .map(|amount| {
            amount
                .trim()
                .parse::<u64>()
                .map_err(|error| StepError::InvalidArgument {
                    message: format!("invalid withdraw output amount '{amount}': {error}"),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    start_deposit_reaction_sequencer(world, step, &sequencer_alias, withdraw_outputs).await
}

#[when(
    expr = "I start zone sequencer {string} withdrawing observed deposit of {int} with outputs {string}"
)]
async fn step_start_zone_sequencer_deposit_withdraw(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    target_amount: u64,
    outputs: String,
) -> StepResult {
    let withdraw_outputs = outputs
        .split(',')
        .map(|amount| {
            amount
                .trim()
                .parse::<u64>()
                .map_err(|error| StepError::InvalidArgument {
                    message: format!("invalid withdraw output amount '{amount}': {error}"),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    start_deposit_withdraw_sequencer(
        world,
        step,
        &sequencer_alias,
        target_amount,
        withdraw_outputs,
    )
    .await
}

#[when("I start zone sequencers:")]
async fn step_start_zone_sequencers(world: &mut CucumberWorld, step: &Step) -> StepResult {
    for row in zone_sequencer_start_rows(step)? {
        let alias = row.alias;
        let startup = ZoneSequencerStartup {
            pending_submit_depth: parse_optional_submit_depth(step, &row.pending_submit_depth)?,
            passive_republish_orphans: row.passive_republish_orphans,
        };
        world.zone.set_sequencer_startup(&alias, startup);

        start_named_sequencer_with_startup(world, step, &alias, None, startup).await?;

        if row.indexer {
            initialize_zone_indexer(world, step, &alias)?;
        }
    }

    Ok(())
}

#[when(expr = "I stop zone sequencer {string}")]
fn step_stop_zone_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    let _ = step;
    stop_zone_sequencer(world, sequencer_alias)
}

#[cucumber::when(expr = "the zone LIB advances in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_lib_advances(
    world: &mut CucumberWorld,
    step: &Step,
    timeout_seconds: u64,
) -> StepResult {
    let client = log_step_error(step, world.zone_node_http_client())?;
    let initial_lib_slot = client
        .consensus_info()
        .await
        .map_err(|error| StepError::LogicalError {
            message: format!("Failed to fetch zone consensus info: {error}"),
        })?
        .cryptarchia_info
        .lib_slot;

    wait_for_lib_advance(
        &client,
        initial_lib_slot,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))
}
