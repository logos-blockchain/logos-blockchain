//! Steps exercising the node's `/wallet/fund` HTTP endpoint: fund a
//! transaction from the node's wallet, assemble the returned proofs and
//! submit the result to the mempool.

use std::{collections::HashSet, time::Duration};

use cucumber::{gherkin::Step, then, when};
use lb_common_http_client::ApiBlock;
use lb_core::mantle::{
    MantleTransaction, Note, Op, OpProof, TxHash,
    gas::GasCost,
    ops::channel::{
        ChannelId, MsgId,
        inscribe::{Inscription, InscriptionOp},
    },
    traits::Hashable as _,
    transactions::{builder::MantleTxBuilder, mantle_tx::MantleTx as _},
};
use lb_http_api_common::bodies::wallet::fund::{WalletFundRequestBody, WalletFundResponseBody};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
use lb_testing_framework::NodeHttpClient;
use tracing::info;

use crate::{
    common::chain::{scan_chain_until, wait_for_transactions_inclusion},
    cucumber::{
        error::{StepError, StepResult},
        steps::TARGET,
        world::CucumberWorld,
    },
};

/// Fund a payment transaction from the node's wallet: the fund endpoint must
/// pull wallet inputs to cover the output, append the fee transfer and return
/// a transfer proof that the ledger accepts.
#[when(
    expr = "I fund a transaction paying {int} LGO from node {string} wallet to wallet {string} as {string}"
)]
async fn step_fund_payment_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    amount: u64,
    node_name: String,
    receiver_wallet_name: String,
    transaction_alias: String,
) -> StepResult {
    fund_and_submit_payment(
        world,
        step,
        amount,
        &node_name,
        &receiver_wallet_name,
        transaction_alias,
    )
    .await?;
    Ok(())
}

async fn fund_and_submit_payment(
    world: &mut CucumberWorld,
    step: &Step,
    amount: u64,
    node_name: &str,
    receiver_wallet_name: &str,
    transaction_alias: String,
) -> Result<TxHash, StepError> {
    let receiver_pk = world.resolve_wallet(receiver_wallet_name)?.public_key()?;
    let funding_wallet = world.funding_wallet(node_name)?;
    let funding_pk = funding_wallet.public_key()?;
    let client = world.resolve_node_http_client(node_name)?;

    let tx_builder = MantleTxBuilder::new()
        .add_ledger_output(Note::new(amount, receiver_pk))
        .map_err(|source| StepError::LogicalError {
            message: format!(
                "Step `{}` error: failed to add output: {source}",
                step.value
            ),
        })?;

    let response = fund_via_node(&client, step, tx_builder, funding_pk).await?;

    let Some(transfer_proof) = response.transfer_proof else {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: funding a payment must return a transfer proof",
                step.value
            ),
        });
    };
    let has_transfer = response
        .funded_tx
        .ops()
        .iter()
        .any(|op| matches!(op, Op::Transfer(_)));
    if !has_transfer {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: funded payment must contain a transfer op",
                step.value
            ),
        });
    }

    let signed_tx = MantleTransaction::new(response.funded_tx, [transfer_proof].into());
    let tx_hash = signed_tx.hash();

    world
        .submit_transaction(&funding_wallet, &signed_tx, &client)
        .await?;
    world.remember_submitted_transaction(transaction_alias.clone(), tx_hash);

    info!(
        target: TARGET,
        "Submitted funded payment `{transaction_alias}` of {amount} LGO from node `{node_name}` wallet"
    );

    Ok(tx_hash)
}

/// Serially fund and submit `count` payments from the node's wallet, waiting
/// for each transaction to be included before funding the next — the cadence
/// of a sequencer funding continuously inside the non-finalized window.
/// Aliases are `{prefix}_1..{prefix}_{count}`.
#[expect(
    clippy::too_many_arguments,
    reason = "argument count mirrors the step expression"
)]
#[when(
    expr = "I fund {int} transactions paying {int} LGO from node {string} wallet to wallet {string} waiting {int} seconds each as prefix {string}"
)]
async fn step_fund_payment_transactions_serially(
    world: &mut CucumberWorld,
    step: &Step,
    count: u32,
    amount: u64,
    node_name: String,
    receiver_wallet_name: String,
    timeout_seconds: u64,
    prefix: String,
) -> StepResult {
    for i in 1..=count {
        let alias = format!("{prefix}_{i}");
        let tx_hash = fund_and_submit_payment(
            world,
            step,
            amount,
            &node_name,
            &receiver_wallet_name,
            alias.clone(),
        )
        .await?;

        let client = world.resolve_node_http_client(&node_name)?;
        let included = wait_for_transactions_inclusion(
            &client,
            &[tx_hash],
            Duration::from_secs(timeout_seconds),
        )
        .await;
        if !included {
            return Err(StepError::LogicalError {
                message: format!(
                    "Transaction `{alias}` was not included on node `{node_name}` within {timeout_seconds} seconds"
                ),
            });
        }
    }

    Ok(())
}

/// Continuously fund and submit `count` payments without waiting for
/// inclusion. The funding wallet is expected to be under-provisioned: a fund
/// call that fails (typically insufficient funds while all notes are
/// reserved or in flight) is retried until it succeeds or the per-transaction
/// deadline passes. Pacing therefore comes from note scarcity — the moment a
/// reservation is released, the next retry grabs that exact note, which makes
/// every wrongful (fork-time) release convert into a note reuse.
#[expect(
    clippy::too_many_arguments,
    reason = "argument count mirrors the step expression"
)]
#[when(
    expr = "I fund {int} transactions paying {int} LGO from node {string} wallet to wallet {string} retrying every {int} seconds for {int} seconds each as prefix {string}"
)]
async fn step_fund_payment_transactions_with_retry(
    world: &mut CucumberWorld,
    step: &Step,
    count: u32,
    amount: u64,
    node_name: String,
    receiver_wallet_name: String,
    retry_seconds: u64,
    timeout_seconds: u64,
    prefix: String,
) -> StepResult {
    for i in 1..=count {
        let alias = format!("{prefix}_{i}");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);

        loop {
            match fund_and_submit_payment(
                world,
                step,
                amount,
                &node_name,
                &receiver_wallet_name,
                alias.clone(),
            )
            .await
            {
                Ok(_) => break,
                Err(error) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(StepError::LogicalError {
                            message: format!(
                                "Funding `{alias}` kept failing for {timeout_seconds} seconds; last error: {error}"
                            ),
                        });
                    }
                    info!(
                        target: TARGET,
                        "Funding `{alias}` failed ({error}); retrying in {retry_seconds}s"
                    );
                    tokio::time::sleep(Duration::from_secs(retry_seconds)).await;
                }
            }
        }
    }

    Ok(())
}

/// Wait for a single node to reach a height, polling only that node — usable
/// while other cluster nodes are deliberately stopped (the regular height
/// step health-checks every node and fails on intentionally-down peers).
#[expect(clippy::needless_pass_by_ref_mut, reason = "Required by Cucumber")]
#[when(expr = "node {string} alone reaches height {int} in {int} seconds")]
#[then(expr = "node {string} alone reaches height {int} in {int} seconds")]
async fn step_node_alone_reaches_height(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    height: u64,
    timeout_seconds: u64,
) -> StepResult {
    let client = world.resolve_node_http_client(&node_name)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);

    loop {
        let current = match client.consensus_info().await {
            Ok(info) => info.cryptarchia_info.height,
            Err(_) => 0,
        };
        if current >= height {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(StepError::LogicalError {
                message: format!(
                    "Step `{}` error: node `{node_name}` did not reach height {height} within {timeout_seconds} seconds (currently {current})",
                    step.value
                ),
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Every remembered transaction whose alias starts with `prefix` is
/// FINALIZED on the given node within the shared deadline: all of them found
/// in one walk of the immutable chain at or below LIB. Finalization is
/// branch-independent, so two transactions spending the same note can never
/// both satisfy this — unlike tip-inclusion checks, which can be fooled by
/// observing different momentary views.
#[expect(clippy::needless_pass_by_ref_mut, reason = "Required by Cucumber")]
#[when(
    expr = "all transactions with prefix {string} are finalized on node {string} in {int} seconds"
)]
#[then(
    expr = "all transactions with prefix {string} are finalized on node {string} in {int} seconds"
)]
async fn step_transactions_with_prefix_finalized(
    world: &mut CucumberWorld,
    step: &Step,
    prefix: String,
    node_name: String,
    timeout_seconds: u64,
) -> StepResult {
    let transactions = world.submitted_transactions_with_prefix(&prefix);
    if transactions.is_empty() {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: no submitted transactions with prefix `{prefix}`",
                step.value
            ),
        });
    }
    let expected: HashSet<TxHash> = transactions.iter().map(|(_, tx_hash)| *tx_hash).collect();

    // Distinct fund calls must yield distinct transactions: identical intents
    // are separated by their funded inputs. Two aliases sharing a hash means
    // the wallet reused a note and thereby merged two payments into one tx.
    if expected.len() != transactions.len() {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: {} submissions with prefix `{prefix}` produced only {} distinct transactions — the wallet reused a note and merged payments",
                step.value,
                transactions.len(),
                expected.len()
            ),
        });
    }

    let client = world.resolve_node_http_client(&node_name)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);

    loop {
        if let Ok(consensus) = client.consensus_info().await {
            let mut scanned_blocks = HashSet::new();
            let mut found = HashSet::new();
            let all_found = scan_chain_until(
                consensus.cryptarchia_info.lib,
                &mut scanned_blocks,
                async |header_id| client.block(&header_id).await.ok().flatten(),
                |block: &ApiBlock| {
                    for tx in &block.transactions {
                        let hash = tx.hash();
                        if expected.contains(&hash) {
                            found.insert(hash);
                        }
                    }
                    (found == expected).then_some(())
                },
            )
            .await
            .is_some();

            if all_found {
                return Ok(());
            }
        }

        if tokio::time::Instant::now() >= deadline {
            let aliases: Vec<&str> = transactions
                .iter()
                .map(|(alias, _)| alias.as_str())
                .collect();
            return Err(StepError::LogicalError {
                message: format!(
                    "Not all transactions with prefix `{prefix}` were finalized on node `{node_name}` within {timeout_seconds} seconds: {aliases:?}"
                ),
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Every remembered transaction whose alias starts with `prefix` is included
/// on the given node within the shared deadline.
#[expect(clippy::needless_pass_by_ref_mut, reason = "Required by Cucumber")]
#[when(
    expr = "all transactions with prefix {string} are included on node {string} in {int} seconds"
)]
#[then(
    expr = "all transactions with prefix {string} are included on node {string} in {int} seconds"
)]
async fn step_transactions_with_prefix_included(
    world: &mut CucumberWorld,
    step: &Step,
    prefix: String,
    node_name: String,
    timeout_seconds: u64,
) -> StepResult {
    let transactions = world.submitted_transactions_with_prefix(&prefix);
    if transactions.is_empty() {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: no submitted transactions with prefix `{prefix}`",
                step.value
            ),
        });
    }

    let client = world.resolve_node_http_client(&node_name)?;
    let tx_hashes: Vec<TxHash> = transactions.iter().map(|(_, tx_hash)| *tx_hash).collect();
    let included =
        wait_for_transactions_inclusion(&client, &tx_hashes, Duration::from_secs(timeout_seconds))
            .await;
    if !included {
        let aliases: Vec<&str> = transactions
            .iter()
            .map(|(alias, _)| alias.as_str())
            .collect();
        return Err(StepError::LogicalError {
            message: format!(
                "Not all transactions with prefix `{prefix}` were included on node `{node_name}` within {timeout_seconds} seconds: {aliases:?}"
            ),
        });
    }

    Ok(())
}

/// Fund an inscription-only transaction: at non-zero gas prices the node
/// appends a fee transfer paid from the selected wallet and returns the
/// transfer proof. The caller signs the channel op over the FUNDED hash and
/// assembles the proofs in op order — the split-signing flow a zone
/// sequencer uses.
#[when(
    expr = "I fund an inscription transaction from wallet {string} via node {string} as {string}"
)]
async fn step_fund_inscription_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    funding_wallet_name: String,
    node_name: String,
    transaction_alias: String,
) -> StepResult {
    let funding_wallet = world.funding_wallet(&node_name)?;
    let funding_pk = funding_wallet.public_key()?;
    let client = world.resolve_node_http_client(&node_name)?;

    // Deterministic sequencer key claiming a fresh channel; every scenario
    // runs on a fresh chain, so the channel cannot pre-exist.
    let signing_key = Ed25519Key::from_bytes(&[7u8; 32]);
    let channel_id = ChannelId::from(signing_key.public_key().to_bytes());
    let inscription =
        Inscription::try_from(b"wallet fund endpoint".to_vec()).map_err(|source| {
            StepError::LogicalError {
                message: format!(
                    "Step `{}` error: failed to build inscription: {source}",
                    step.value
                ),
            }
        })?;
    let inscription_op = InscriptionOp {
        channel_id,
        inscription,
        parent: MsgId::root(),
        signer: signing_key.public_key(),
    };

    let tx_builder = MantleTxBuilder::new()
        .push_op(Op::ChannelInscribe(inscription_op))
        .map_err(|source| StepError::LogicalError {
            message: format!(
                "Step `{}` error: failed to push inscription op: {source}",
                step.value
            ),
        })?;

    let response = fund_via_node(&client, step, tx_builder, funding_pk).await?;

    let Some(transfer_proof) = response.transfer_proof else {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: funding an inscription at non-zero gas prices must return a \
                transfer proof",
                step.value
            ),
        });
    };
    let ops = response.funded_tx.ops();
    if ops.len() != 2
        || !matches!(ops[0], Op::ChannelInscribe(_))
        || !matches!(ops[1], Op::Transfer(_))
    {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: funded inscription must be [inscribe, fee transfer], got {} ops",
                step.value,
                ops.len()
            ),
        });
    }

    let tx_hash = response.funded_tx.hash();
    let signature = signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref());
    let signed_tx = MantleTransaction::new(
        response.funded_tx,
        [OpProof::Ed25519Sig(signature), transfer_proof].into(),
    );

    world
        .submit_transaction(&funding_wallet, &signed_tx, &client)
        .await?;
    world.remember_submitted_transaction(transaction_alias.clone(), tx_hash);

    info!(
        target: TARGET,
        "Submitted inscription `{transaction_alias}` funded by wallet `{funding_wallet_name}` via \
         node `{node_name}` fund endpoint"
    );

    Ok(())
}

async fn fund_via_node(
    client: &NodeHttpClient,
    step: &Step,
    tx_builder: MantleTxBuilder,
    funding_pk: ZkPublicKey,
) -> Result<WalletFundResponseBody, StepError> {
    client
        .fund_tx(WalletFundRequestBody {
            tip: None,
            tx_builder,
            change_public_key: funding_pk,
            funding_public_keys: vec![funding_pk],
            max_tx_fee: GasCost::new(u64::MAX),
            priority_fee_percent: 0,
        })
        .await
        .map_err(|source| StepError::StepFail {
            message: format!("Step `{}` error: fund request failed: {source}", step.value),
        })
}
