//! Actions behind the fee-market steps: building and submitting fee-paying
//! transactions and recording per-block gas prices.

use std::time::Duration;

use cucumber::gherkin::Step;
use futures::StreamExt as _;
use lb_common_http_client::ApiBlock;
use lb_core::{
    header::HeaderId,
    mantle::{traits::Hashable as _, transactions::GasPrices},
};
use lb_http_api_common::bodies::wallet::transfer_funds::WalletTransferFundsRequestBody;
use lb_testing_framework::{NodeHttpClient, configs::wallet::WalletAccount};
use tokio::time::timeout;
use tracing::info;

use crate::{
    common::fee_spec::{self, GasPriceRecord},
    cucumber::{
        error::{StepError, StepResult},
        steps::TARGET,
        world::{CucumberWorld, WalletType},
    },
};

/// Builds a self-transfer whose balance is the mandatory fee plus `tip` at
/// the live gas prices, checks the size predictor against the actual
/// encoding, submits it, and records it under the alias.
pub async fn submit_self_transfer_with_tip(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: &str,
    node_name: &str,
    transaction_alias: String,
    tip: i128,
) -> StepResult {
    let account = user_wallet_account(world, step, wallet_name)?;
    let client = world.resolve_node_http_client(node_name)?;
    let prices = live_gas_prices(&client, step).await?;

    let signed_tx =
        fee_spec::self_transfer_paying_fee_at(&world.genesis_block_utxos, &account, &prices, tip);

    fee_spec::check_size_prediction(&signed_tx, &prices).map_err(|message| {
        StepError::StepFail {
            message: format!("Step `{}` error: {message}", step.value),
        }
    })?;

    let tx_hash = signed_tx.hash();

    client
        .submit_transaction(&signed_tx)
        .await
        .map_err(|source| StepError::StepFail {
            message: format!(
                "Step `{}` error: transaction submission failed: {source}",
                step.value
            ),
        })?;

    info!(
        target: TARGET,
        "Submitted self-transfer `{transaction_alias}` ({tx_hash:?}) from wallet \
         '{wallet_name}' with tip {tip} at prices execution={:?} storage={:?}",
        prices.execution_base_gas_price, prices.storage_gas_price,
    );

    world.remember_submitted_transaction(transaction_alias.clone(), tx_hash);
    world.remember_prepared_transaction(transaction_alias, signed_tx);

    Ok(())
}

/// Records the gas prices after every block into the world, for the
/// recorded-prices assertions.
pub async fn record_per_block_gas_prices(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: &str,
    slots: u64,
    timeout_secs: u64,
) -> StepResult {
    let client = world.resolve_node_http_client(node_name)?;
    let genesis_id = world
        .genesis_block_id
        .ok_or_else(|| StepError::LogicalError {
            message: format!(
                "Step `{}` error: genesis block id is not available for this cluster",
                step.value
            ),
        })?;

    let records = timeout(
        Duration::from_secs(timeout_secs),
        record_gas_prices(&client, genesis_id, slots),
    )
    .await
    .map_err(|_| StepError::StepFail {
        message: format!(
            "Step `{}` error: timed out after {timeout_secs}s before observing {slots} slots",
            step.value
        ),
    })?
    .map_err(|message| StepError::StepFail {
        message: format!("Step `{}` error: {message}", step.value),
    })?;

    info!(
        target: TARGET,
        "Recorded gas prices for {} blocks on node '{node_name}'", records.len()
    );

    world.recorded_gas_prices = records;

    Ok(())
}

/// Records the gas prices after every block, from genesis until `slots`
/// slots have passed from the first streamed block.
async fn record_gas_prices(
    client: &NodeHttpClient,
    genesis_id: HeaderId,
    slots: u64,
) -> Result<Vec<GasPriceRecord>, String> {
    let mut block_stream = client
        .blocks_stream()
        .await
        .map_err(|source| format!("blocks stream request failed: {source}"))?;

    let mut records = Vec::new();
    let mut last_recorded = genesis_id;
    let mut stop_at_slot: Option<u64> = None;

    while let Some(event) = block_stream.next().await {
        let slot = u64::from(event.block.header.slot);
        let stop_at = *stop_at_slot.get_or_insert(slot + slots);

        for block in
            block_and_missed_ancestors(client, event.block, last_recorded, genesis_id).await?
        {
            last_recorded = block.header.id;
            records.push(price_record_for(client, &block).await?);
        }

        if slot >= stop_at {
            return Ok(records);
        }
    }

    Err("blocks stream ended before the requested slots were observed".to_owned())
}

/// The streamed block plus any ancestors the stream subscription missed,
/// oldest first: walks parent links back until it reconnects with the last
/// recorded block.
async fn block_and_missed_ancestors(
    client: &NodeHttpClient,
    newest: ApiBlock,
    last_recorded: HeaderId,
    genesis_id: HeaderId,
) -> Result<Vec<ApiBlock>, String> {
    let mut parent = newest.header.parent_block;
    let mut chain = vec![newest];

    while parent != last_recorded {
        if parent == genesis_id {
            return Err(
                "the parent chain reached genesis without meeting the last recorded block; \
                 the chain reorganized and the linear price recording does not apply"
                    .to_owned(),
            );
        }

        let block = client
            .block(&parent)
            .await
            .map_err(|source| format!("parent block fetch failed: {source}"))?
            .ok_or_else(|| format!("parent block {parent} not found"))?;

        parent = block.header.parent_block;
        chain.push(block);
    }

    chain.reverse();

    Ok(chain)
}

/// The gas prices the node reports right after this block, together with the
/// block's execution gas per the spec table.
async fn price_record_for(
    client: &NodeHttpClient,
    block: &ApiBlock,
) -> Result<GasPriceRecord, String> {
    let prices = client
        .gas_prices(Some(block.header.id))
        .await
        .map_err(|source| format!("gas prices request failed: {source}"))?;

    Ok(GasPriceRecord {
        slot: u64::from(block.header.slot),
        execution_price: prices.execution_base_gas_price.into_inner(),
        storage_price: prices.storage_gas_price.into_inner(),
        block_execution_gas: fee_spec::spec_block_execution_gas(block)?,
    })
}

/// Fires two wallet transfer requests at the same time, funded from the same
/// key set, and records both transactions under their aliases.
pub async fn concurrently_fund_transfers(
    world: &mut CucumberWorld,
    step: &Step,
    amount: u64,
    funder_names: [&str; 2],
    recipient_names: [&str; 2],
    node_name: &str,
    aliases: [String; 2],
) -> StepResult {
    let funder_a = user_wallet_account(world, step, funder_names[0])?;
    let funder_b = user_wallet_account(world, step, funder_names[1])?;
    let recipient_a = user_wallet_account(world, step, recipient_names[0])?;
    let recipient_b = user_wallet_account(world, step, recipient_names[1])?;
    let client = world.resolve_node_http_client(node_name)?;

    let funding_public_keys = vec![funder_a.public_key(), funder_b.public_key()];
    let request_for = |recipient: &WalletAccount| WalletTransferFundsRequestBody {
        tip: None,
        change_public_key: funder_a.public_key(),
        funding_public_keys: funding_public_keys.clone(),
        recipient_public_key: recipient.public_key(),
        amount,
    };

    let (response_a, response_b) = tokio::join!(
        client.transfer_funds(request_for(&recipient_a)),
        client.transfer_funds(request_for(&recipient_b)),
    );

    let hash_a = response_a
        .map_err(|source| StepError::StepFail {
            message: format!(
                "Step `{}` error: first concurrent transfer request failed: {source}",
                step.value
            ),
        })?
        .hash;

    let hash_b = response_b
        .map_err(|source| StepError::StepFail {
            message: format!(
                "Step `{}` error: second concurrent transfer request failed: {source}",
                step.value
            ),
        })?
        .hash;

    if hash_a == hash_b {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{}` error: the wallet built the same transaction for both requests",
                step.value
            ),
        });
    }

    let [alias_a, alias_b] = aliases;

    info!(
        target: TARGET,
        "Concurrently funded transfers `{alias_a}` ({hash_a:?}) and `{alias_b}` ({hash_b:?})",
    );

    world.remember_submitted_transaction(alias_a, hash_a);
    world.remember_submitted_transaction(alias_b, hash_b);

    Ok(())
}

/// Resolves a wallet name to its user wallet account, or fails for funding
/// wallets, which have no locally held secret key.
pub fn user_wallet_account(
    world: &CucumberWorld,
    step: &Step,
    wallet_name: &str,
) -> Result<WalletAccount, StepError> {
    let wallet_info =
        world
            .wallet_info
            .get(wallet_name)
            .ok_or_else(|| StepError::LogicalError {
                message: format!(
                    "Step `{}` error: unknown wallet `{wallet_name}`",
                    step.value
                ),
            })?;

    match &wallet_info.wallet_type {
        WalletType::User { wallet_account } => Ok(wallet_account.clone()),
        WalletType::Funding { .. } => Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: wallet `{wallet_name}` is a funding wallet; fee steps \
                 need a user wallet",
                step.value
            ),
        }),
    }
}

/// The gas prices at the node's current tip, used to fund transactions at
/// live prices.
pub async fn live_gas_prices(client: &NodeHttpClient, step: &Step) -> Result<GasPrices, StepError> {
    let prices = client
        .gas_prices(None)
        .await
        .map_err(|source| StepError::StepFail {
            message: format!(
                "Step `{}` error: gas prices request failed: {source}",
                step.value
            ),
        })?;

    Ok(GasPrices {
        execution_base_gas_price: prices.execution_base_gas_price,
        storage_gas_price: prices.storage_gas_price,
    })
}
