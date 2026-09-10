use super::{
    CucumberWorld, Duration, Step, StepError, StepResult, TARGET, WalletType,
    assert_tracked_wallet_fees_equal_sponsored_fee_account_spend, drain_all_node_wallets,
    drain_node_wallet, drain_user_wallet, then, wait_for_wallet_submitted_transactions_inclusion,
    warn, when,
};

#[when(expr = "I drain wallet {string} into {string}")]
#[then(expr = "I drain wallet {string} into {string}")]
async fn step_drain_wallet(
    world: &mut CucumberWorld,
    step: &Step,
    sender_wallet_name: String,
    receiver_wallet_name: String,
) -> StepResult {
    let sender = world.resolve_wallet(&sender_wallet_name)?;
    let receiver = world.resolve_recipient(&receiver_wallet_name)?;
    let sender_pk = sender.public_key()?;
    let receiver_pk = receiver.public_key;

    if sender_pk == receiver_pk {
        return Err(StepError::InvalidArgument {
            message: format!("Cannot drain wallet `{sender_wallet_name}` into itself"),
        });
    }

    match sender.wallet_type {
        WalletType::User { .. } => {
            drain_user_wallet(world, &step.value, &sender, receiver_pk).await
        }
        WalletType::Funding { .. } => drain_node_wallet(world, &sender, receiver_pk).await,
    }
}

#[when(expr = "I drain all node {string} wallets into {string}")]
#[then(expr = "I drain all node {string} wallets into {string}")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require a mutable World reference"
)]
async fn step_drain_all_node_wallets(
    world: &mut CucumberWorld,
    node_name: String,
    receiver_wallet_name: String,
) -> StepResult {
    drain_all_node_wallets(world, &node_name, &receiver_wallet_name).await
}

#[when(expr = "wallet {string} has all submitted transactions settled in {int} seconds")]
#[then(expr = "wallet {string} has all submitted transactions settled in {int} seconds")]
#[when(expr = "wallet {string} has all submitted transactions included in {int} seconds")]
#[then(expr = "wallet {string} has all submitted transactions included in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_wallet_has_all_submitted_transactions_settled(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_wallet_submitted_transactions_inclusion(
        world,
        &wallet_name,
        Duration::from_secs(time_out_seconds),
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })
}

#[when(expr = "tracked wallet fees equal sponsored fee account spent fees")]
#[then(expr = "tracked wallet fees equal sponsored fee account spent fees")]
async fn step_tracked_wallet_fees_equal_sponsored_fee_account_spend(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    assert_tracked_wallet_fees_equal_sponsored_fee_account_spend(world, &step.value).await
}
