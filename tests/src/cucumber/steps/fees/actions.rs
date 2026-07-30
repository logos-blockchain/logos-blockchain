//! Actions behind the fee-market steps: building and submitting fee-paying
//! transactions and recording per-block gas prices.

use std::{collections::HashSet, num::NonZero, time::Duration};

use cucumber::gherkin::Step;
use futures::{StreamExt as _, future::join_all};
use lb_common_http_client::ApiBlock;
use lb_core::{
    header::HeaderId,
    mantle::{traits::Hashable as _, transactions::GasPrices},
};
use lb_http_api_common::{
    bodies::wallet::transfer_funds::WalletTransferFundsRequestBody,
    queries::{BlockFilter, BlockSortOrder, BlocksStreamQuery},
};
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

/// Records gas prices from genesis until `slots` have passed from the first
/// streamed block. The historical prefix comes from a range request after the
/// live feed is subscribed, avoiding a gap between history and future events.
/// Missed ancestors backfill any later gaps in the feed.
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
    let mut initial_history_loaded = false;

    while let Some(event) = block_stream.next().await {
        let slot = u64::from(event.block.header.slot);
        let stop_at = *stop_at_slot.get_or_insert(slot + slots);

        let blocks = if initial_history_loaded {
            block_and_missed_ancestors(client, event.block, last_recorded, genesis_id).await?
        } else {
            initial_history_loaded = true;
            initial_history_and_first_block(client, event.block, genesis_id).await?
        };

        for block in blocks {
            last_recorded = block.header.id;
            records.push(price_record_for(client, &block).await?);
        }

        if slot >= stop_at {
            return Ok(records);
        }
    }

    Err("blocks stream ended before the requested slots were observed".to_owned())
}

/// Loads the canonical chain from genesis through the first live block's
/// parent, then appends that live block after verifying both sources connect.
async fn initial_history_and_first_block(
    client: &NodeHttpClient,
    first_live_block: ApiBlock,
    genesis_id: HeaderId,
) -> Result<Vec<ApiBlock>, String> {
    let first_live_slot = u64::from(first_live_block.header.slot);
    let mut history = if first_live_block.header.parent_block == genesis_id {
        Vec::new()
    } else {
        let mut stream = client
            .blocks_range_stream(BlocksStreamQuery {
                slot_from: Some(0),
                slot_to: Some(first_live_slot.saturating_sub(1)),
                order: Some(BlockSortOrder::Ascending),
                blocks_limit: None,
                server_batch_size: NonZero::new(1_000),
                block_filter: Some(BlockFilter::MutableAndImmutable),
            })
            .await
            .map_err(|source| format!("blocks range request failed: {source}"))?;

        let mut history = Vec::new();
        while let Some(event) = stream.next().await {
            if event.block.header.id != genesis_id {
                history.push(event.block);
            }
        }
        history
    };

    let history_tip = history.last().map_or(genesis_id, |block| block.header.id);
    if history_tip != first_live_block.header.parent_block {
        return Err(format!(
            "blocks range ended at {history_tip}, but the first live block points to {}; the \
             chain reorganized while connecting history to the live feed",
            first_live_block.header.parent_block,
        ));
    }

    history.push(first_live_block);
    Ok(history)
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

pub struct ConcurrentTransferRequest {
    pub amount: u64,
    pub funding_wallets: Vec<String>,
    pub recipient_wallet: String,
    pub node: String,
    pub alias: String,
}

/// Fires wallet transfer requests at the same time and records each transaction
/// under its alias.
pub async fn concurrently_fund_transfers(
    world: &mut CucumberWorld,
    step: &Step,
    requests: Vec<ConcurrentTransferRequest>,
) -> StepResult {
    if requests.len() < 2 {
        return Err(StepError::InvalidArgument {
            message: "concurrent transfers require at least two requests".to_owned(),
        });
    }

    let mut aliases = HashSet::with_capacity(requests.len());
    let mut prepared = Vec::with_capacity(requests.len());

    for request in requests {
        if !aliases.insert(request.alias.clone()) {
            return Err(StepError::InvalidArgument {
                message: format!("duplicate concurrent transfer alias `{}`", request.alias),
            });
        }

        let funding_accounts = request
            .funding_wallets
            .iter()
            .map(|wallet| user_wallet_account(world, step, wallet))
            .collect::<Result<Vec<_>, _>>()?;
        let recipient = user_wallet_account(world, step, &request.recipient_wallet)?;
        let client = world.resolve_node_http_client(&request.node)?;

        let funding_public_keys = funding_accounts
            .iter()
            .map(WalletAccount::public_key)
            .collect::<Vec<_>>();
        let transfer = WalletTransferFundsRequestBody {
            tip: None,
            change_public_key: funding_public_keys[0],
            funding_public_keys,
            recipient_public_key: recipient.public_key(),
            amount: request.amount,
        };

        prepared.push((request.alias, client, transfer));
    }

    let responses = join_all(
        prepared
            .into_iter()
            .map(async |(alias, client, request)| (alias, client.transfer_funds(request).await)),
    )
    .await;

    let mut hashes = HashSet::with_capacity(responses.len());
    let mut submitted = Vec::with_capacity(responses.len());

    for (alias, response) in responses {
        let hash = response
            .map_err(|source| StepError::StepFail {
                message: format!(
                    "Step `{}` error: concurrent transfer `{alias}` failed: {source}",
                    step.value
                ),
            })?
            .hash;

        if !hashes.insert(hash) {
            return Err(StepError::StepFail {
                message: format!(
                    "Step `{}` error: the wallet built the same transaction for multiple \
                     concurrent requests",
                    step.value
                ),
            });
        }

        submitted.push((alias, hash));
    }

    info!(
        target: TARGET,
        "Concurrently funded {} transfers", submitted.len(),
    );

    for (alias, hash) in submitted {
        world.remember_submitted_transaction(alias, hash);
    }

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
