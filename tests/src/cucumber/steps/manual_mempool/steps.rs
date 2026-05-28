use cucumber::{gherkin::Step, then, when};

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::manual_mempool::{
        actions::{
            prepare_transfer_transaction, submit_prepared_transaction_to_nodes,
            try_submit_invalid_transaction,
        },
        assertions::{
            assert_transaction_not_pending_on_all_nodes, assert_transaction_pending_on_nodes,
            assert_transaction_remains_not_pending_on_all_nodes,
        },
    },
    world::CucumberWorld,
};

#[when(
    expr = "I prepare transfer transaction {string} of {int} LGO from wallet {string} to wallet {string}"
)]
async fn step_prepare_transfer_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    amount: u64,
    sender_wallet_name: String,
    receiver_wallet_name: String,
) -> StepResult {
    prepare_transfer_transaction(
        world,
        &step.value,
        transaction_alias,
        amount,
        sender_wallet_name,
        receiver_wallet_name,
    )
    .await
}

#[when(expr = "I submit prepared transaction {string} to nodes:")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_submit_prepared_transaction_to_nodes(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
) -> StepResult {
    let node_names = parse_node_names_table(step)?;

    submit_prepared_transaction_to_nodes(world, &step.value, transaction_alias, node_names).await
}

#[when(expr = "I try to submit invalid transaction {string} to node {string}")]
async fn step_try_submit_invalid_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    node_name: String,
) -> StepResult {
    try_submit_invalid_transaction(world, &step.value, transaction_alias, node_name).await
}

#[then(expr = "transaction {string} is pending in mempool of nodes in {int} seconds:")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_transaction_pending_in_node_mempools(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let node_names = parse_node_names_table(step)?;

    assert_transaction_pending_on_nodes(world, transaction_alias, node_names, timeout_seconds).await
}

#[then(expr = "transaction {string} is not pending in mempool of all nodes in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_transaction_not_pending_in_all_mempools(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let _ = step;

    assert_transaction_not_pending_on_all_nodes(world, transaction_alias, timeout_seconds).await
}

#[then(
    expr = "transaction {string} remains not pending in mempool of all nodes for {int} blocks in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_transaction_remains_not_pending_in_all_mempools(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    blocks: u64,
    timeout_seconds: u64,
) -> StepResult {
    let _ = step;

    assert_transaction_remains_not_pending_on_all_nodes(
        world,
        transaction_alias,
        blocks,
        timeout_seconds,
    )
    .await
}

fn parse_node_names_table(step: &Step) -> Result<Vec<String>, StepError> {
    let table = step.table.as_ref().ok_or(StepError::MissingTable)?;

    if table.rows.is_empty() || table.rows[0].len() != 1 || table.rows[0][0].trim() != "node_name" {
        return Err(StepError::InvalidArgument {
            message: "Expected table columns: | node_name |".to_owned(),
        });
    }

    table
        .rows
        .iter()
        .skip(1)
        .map(|row| {
            if row.len() != 1 {
                return Err(StepError::InvalidArgument {
                    message: "Each node row must have exactly one column".to_owned(),
                });
            }

            Ok(row[0].trim().to_owned())
        })
        .collect()
}
