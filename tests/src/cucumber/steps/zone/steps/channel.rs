use super::{
    CucumberWorld, Step, StepResult, publish_atomic_zone_withdraw_transaction,
    save_zone_checkpoint, single_column_table, start_named_sequencer_with_startup,
    submit_atomic_zone_deposit_transaction, submit_zone_channel_config,
    submit_zone_channel_split_transaction, submit_zone_deposit_transaction,
    submit_zone_multi_deposit_transaction, submit_zone_multisig_channel_config,
    submit_zone_withdraw_transaction, when, zone_atomic_withdraw_rows, zone_config_row,
};

#[when(expr = "I save current checkpoint of sequencer {string} as {string}")]
fn step_save_zone_checkpoint(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    checkpoint_alias: String,
) -> StepResult {
    save_zone_checkpoint(world, step, sequencer_alias, checkpoint_alias)
}

#[when(expr = "I restart zone sequencer {string} from checkpoint {string}")]
async fn step_restart_zone_sequencer_from_checkpoint(
    world: &mut CucumberWorld,
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
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    let startup = world.zone.sequencer_startup_for(&sequencer_alias);
    start_named_sequencer_with_startup(world, step, &sequencer_alias, None, startup).await
}

#[when(expr = "sequencer {string} submits zone config transaction {string} authorizing:")]
async fn step_submit_zone_channel_config_transaction(
    world: &mut CucumberWorld,
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
    world: &mut CucumberWorld,
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
    expr = "sequencer {string} submits zone multisig config transaction {string} with threshold {int} authorizing:"
)]
async fn step_submit_zone_multisig_channel_config_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    transaction_alias: String,
    configuration_threshold: u16,
) -> StepResult {
    let authorized_aliases =
        single_column_table(step, "alias", "authorized zone sequencer aliases")?;

    submit_zone_multisig_channel_config(
        world,
        step,
        &sequencer_alias,
        transaction_alias,
        authorized_aliases,
        configuration_threshold,
    )
    .await
}

#[when(expr = "sequencer {string} submits zone config transaction:")]
async fn step_submit_zone_channel_config_transaction_from_table(
    world: &mut CucumberWorld,
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

#[when(
    expr = "I submit zone deposit transaction {string} into channel of {string} of {int} with metadata {string}"
)]
async fn step_submit_zone_deposit_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    channel_alias: String,
    amount: u64,
    metadata: String,
) -> StepResult {
    submit_zone_deposit_transaction(
        world,
        step,
        transaction_alias,
        channel_alias,
        amount,
        metadata
            .into_bytes()
            .try_into()
            .expect("Metadata too large for deposit op."),
    )
    .await
}

#[when(
    expr = "I submit zone deposit transaction {string} into channel of {string} consuming notes valued {string} with metadata {string}"
)]
async fn step_submit_zone_multi_deposit_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    channel_alias: String,
    values: String,
    metadata: String,
) -> StepResult {
    let input_values = values
        .split(',')
        .map(|part| part.trim().parse::<u64>())
        .collect::<Result<Vec<_>, _>>()
        .expect("deposit note values must be a comma-separated list of integers");
    submit_zone_multi_deposit_transaction(
        world,
        step,
        transaction_alias,
        channel_alias,
        input_values,
        metadata
            .into_bytes()
            .try_into()
            .expect("Metadata too large for deposit op."),
    )
    .await
}

#[when(
    expr = "I submit zone deposit transaction {string} into channel of {string} consuming {int} notes of value {int} with metadata {string}"
)]
async fn step_submit_zone_bulk_deposit_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    channel_alias: String,
    count: usize,
    value: u64,
    metadata: String,
) -> StepResult {
    let input_values = vec![value; count];
    submit_zone_multi_deposit_transaction(
        world,
        step,
        transaction_alias,
        channel_alias,
        input_values,
        metadata
            .into_bytes()
            .try_into()
            .expect("Metadata too large for deposit op."),
    )
    .await
}

#[when(expr = "sequencer {string} splits deposit {string} into {int} dust notes as {string}")]
async fn step_submit_zone_channel_split_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    deposit_alias: String,
    dust_count: usize,
    transaction_alias: String,
) -> StepResult {
    submit_zone_channel_split_transaction(
        world,
        step,
        &sequencer_alias,
        &deposit_alias,
        dust_count,
        transaction_alias,
    )
    .await
}

#[when(
    expr = "sequencer {string} submits atomic zone deposit transaction {string} with inscription {string} of {int} with metadata {string}"
)]
async fn step_submit_atomic_zone_deposit_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    transaction_alias: String,
    message_alias: String,
    amount: u64,
    metadata: String,
) -> StepResult {
    submit_atomic_zone_deposit_transaction(
        world,
        step,
        &sequencer_alias,
        transaction_alias,
        message_alias,
        amount,
        metadata
            .into_bytes()
            .try_into()
            .expect("Metadata too large for deposit op."),
    )
    .await
}

#[when(
    expr = "sequencer {string} submits zone withdraw transaction {string} with inscription {string} of {int}"
)]
async fn step_submit_zone_withdraw_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    transaction_alias: String,
    message_alias: String,
    amount: u64,
) -> StepResult {
    submit_zone_withdraw_transaction(
        world,
        step,
        &sequencer_alias,
        transaction_alias,
        message_alias,
        amount,
    )
    .await
}

#[when(expr = "sequencer {string} publishes atomic withdraw {string} with inscription {string}:")]
async fn step_publish_atomic_zone_withdraw_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    bundle_alias: String,
    message_alias: String,
) -> StepResult {
    let withdraw_rows = zone_atomic_withdraw_rows(step)?;
    publish_atomic_zone_withdraw_transaction(
        world,
        step,
        &sequencer_alias,
        bundle_alias,
        message_alias,
        withdraw_rows,
    )
    .await
}
