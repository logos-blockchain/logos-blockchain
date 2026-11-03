use super::{
    Step, StepResult, ZoneWorld, prepare_zone_channel_config, save_zone_checkpoint,
    sign_prepared_zone_channel_config, single_column_table, start_named_sequencer_with_startup,
    submit_prepared_zone_channel_config, submit_zone_channel_config, when, zone_config_row,
};

#[when(expr = "I save current checkpoint of sequencer {string} as {string}")]
fn step_save_zone_checkpoint(
    world: &mut ZoneWorld,
    step: &Step,
    sequencer_alias: String,
    checkpoint_alias: String,
) -> StepResult {
    save_zone_checkpoint(world, step, sequencer_alias, checkpoint_alias)
}

#[when(expr = "I restart zone sequencer {string} from checkpoint {string}")]
async fn step_restart_zone_sequencer_from_checkpoint(
    world: &mut ZoneWorld,
    step: &Step,
    sequencer_alias: String,
    checkpoint_alias: String,
) -> StepResult {
    let checkpoint = world.zone.resolve_checkpoint(checkpoint_alias)?;
    let startup = world.zone.sequencer_startup_for(&sequencer_alias);

    start_named_sequencer_with_startup(world, step, &sequencer_alias, Some(checkpoint), startup)
        .await
}

#[when(expr = "I restart zone sequencer {string} fresh")]
async fn step_restart_zone_sequencer_fresh(
    world: &mut ZoneWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    let startup = world.zone.sequencer_startup_for(&sequencer_alias);
    start_named_sequencer_with_startup(world, step, &sequencer_alias, None, startup).await
}

#[when(expr = "sequencer {string} submits zone config transaction {string} authorizing:")]
async fn step_submit_zone_channel_config_transaction(
    world: &mut ZoneWorld,
    step: &Step,
    sequencer_alias: String,
    transaction_alias: String,
) -> StepResult {
    let authorized_aliases =
        single_column_table(step, "alias", "authorized zone sequencer aliases")?;

    submit_zone_channel_config(
        world,
        step,
        &sequencer_alias,
        transaction_alias,
        authorized_aliases,
        0,
        0,
    )
    .await
}

#[when(
    expr = "sequencer {string} submits zone config transaction {string} with posting timeframe {int} and timeout {int} authorizing:"
)]
async fn step_submit_zone_channel_config_transaction_with_posting_window(
    world: &mut ZoneWorld,
    step: &Step,
    sequencer_alias: String,
    transaction_alias: String,
    posting_timeframe: u32,
    posting_timeout: u32,
) -> StepResult {
    let authorized_aliases =
        single_column_table(step, "alias", "authorized zone sequencer aliases")?;

    submit_zone_channel_config(
        world,
        step,
        &sequencer_alias,
        transaction_alias,
        authorized_aliases,
        posting_timeframe,
        posting_timeout,
    )
    .await
}

#[when(
    expr = "sequencer {string} prepares zone config transaction {string} with threshold {int} authorizing:"
)]
async fn step_prepare_zone_channel_config(
    world: &mut ZoneWorld,
    step: &Step,
    sequencer_alias: String,
    transaction_alias: String,
    threshold: u16,
) -> StepResult {
    let authorized_aliases =
        single_column_table(step, "alias", "authorized zone sequencer aliases")?;

    prepare_zone_channel_config(
        world,
        step,
        &sequencer_alias,
        transaction_alias,
        authorized_aliases,
        threshold,
        0,
    )
    .await
}

#[when(
    expr = "sequencer {string} prepares zone config transaction {string} with threshold {int} and posting timeframe {int} authorizing:"
)]
async fn step_prepare_zone_channel_config_with_timeframe(
    world: &mut ZoneWorld,
    step: &Step,
    sequencer_alias: String,
    transaction_alias: String,
    threshold: u16,
    posting_timeframe: u32,
) -> StepResult {
    let authorized_aliases =
        single_column_table(step, "alias", "authorized zone sequencer aliases")?;

    prepare_zone_channel_config(
        world,
        step,
        &sequencer_alias,
        transaction_alias,
        authorized_aliases,
        threshold,
        posting_timeframe,
    )
    .await
}

#[when(expr = "sequencer {string} signs prepared zone config transaction {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Cucumber string captures are provided as owned `String`s"
)]
fn step_sign_prepared_zone_channel_config(
    world: &mut ZoneWorld,
    step: &Step,
    signer_alias: String,
    transaction_alias: String,
) -> StepResult {
    sign_prepared_zone_channel_config(world, step, &signer_alias, transaction_alias)
}

#[when(expr = "sequencer {string} submits prepared zone config transaction {string}")]
async fn step_submit_prepared_zone_channel_config(
    world: &mut ZoneWorld,
    step: &Step,
    sequencer_alias: String,
    transaction_alias: String,
) -> StepResult {
    submit_prepared_zone_channel_config(world, step, &sequencer_alias, transaction_alias).await
}

#[when(expr = "sequencer {string} submits zone config transaction:")]
async fn step_submit_zone_channel_config_transaction_from_table(
    world: &mut ZoneWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    let row = zone_config_row(step)?;

    submit_zone_channel_config(
        world,
        step,
        &sequencer_alias,
        row.config_name,
        row.authorized_sequencers,
        row.posting_timeframe,
        row.posting_timeout,
    )
    .await
}
