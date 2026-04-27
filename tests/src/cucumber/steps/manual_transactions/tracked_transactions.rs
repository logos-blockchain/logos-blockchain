use std::{collections::HashSet, time::Duration};

use lb_common_http_client::ApiBlock;
use lb_core::mantle::{
    MantleTx, Note, Op, OpProof, SignedMantleTx, Transaction as _, TxHash,
    genesis_tx::GENESIS_STORAGE_GAS_PRICE,
    ledger::{Inputs, Outputs},
    ops::transfer::TransferOp,
};
use lb_key_management_system_service::keys::{ZkKey, ZkPublicKey};
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use crate::{
    common::chain::scan_chain_until,
    cucumber::{
        error::StepError,
        steps::{TARGET, manual_transactions::utils::create_and_submit_transaction_hashes},
        world::{CucumberWorld, WalletType},
    },
};

pub async fn submit_invalid_transfer_transaction(
    world: &mut CucumberWorld,
    step: &str,
    transaction_alias: String,
    node_name: String,
) -> Result<(), StepError> {
    let node = world
        .resolve_node_http_client(&node_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;

    let signed_tx = create_invalid_transaction();
    let tx_hash = signed_tx.hash();

    node.submit_transaction(&signed_tx).await.inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    world.remember_submitted_transaction(transaction_alias.clone(), tx_hash);

    info!(
        target: TARGET,
        "Submitted invalid transfer transaction `{transaction_alias}` to `{node_name}`"
    );

    Ok(())
}

pub async fn submit_funded_transfer_transaction(
    world: &mut CucumberWorld,
    step: &str,
    transaction_alias: String,
    amount: u64,
    sender_wallet_name: String,
    receiver_wallet_name: String,
) -> Result<(), StepError> {
    let wallets = world
        .resolve_wallets(&[sender_wallet_name.clone(), receiver_wallet_name.clone()])
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;
    let sender_wallet = wallets[0].clone();
    let receiver_wallet = wallets[1].clone();

    match &sender_wallet.wallet_type {
        WalletType::User { .. } => {}
        WalletType::Funding { .. } => {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "Wallet `{sender_wallet_name}` must be a user wallet to submit funded transfers"
                ),
            });
        }
    }

    let tx_hashes = create_and_submit_transaction_hashes(
        world,
        step,
        &sender_wallet_name,
        &[(receiver_wallet.public_key()?, amount)],
        None,
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    let [tx_hash] = tx_hashes.as_slice() else {
        return Err(StepError::LogicalError {
            message: format!(
                "Expected exactly one transaction hash for funded transfer `{transaction_alias}`"
            ),
        });
    };

    world.remember_submitted_transaction(transaction_alias.clone(), *tx_hash);

    info!(
        target: TARGET,
        "Submitted funded transfer transaction `{transaction_alias}` from `{sender_wallet_name}` to `{receiver_wallet_name}`"
    );

    Ok(())
}

pub async fn transaction_is_not_included_in_seconds(
    world: &CucumberWorld,
    step: &str,
    transaction_alias: String,
    timeout_seconds: u64,
) -> Result<(), StepError> {
    let tx_hash = world.resolve_submitted_transaction(&transaction_alias)?;
    let node_name = world
        .nodes_info
        .keys()
        .next()
        .cloned()
        .ok_or(StepError::LogicalError {
            message: "No started node available to scan chain state".to_owned(),
        })
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;

    let node = world
        .resolve_node_http_client(&node_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;

    let included = timeout(Duration::from_secs(timeout_seconds), async {
        loop {
            if transaction_is_in_chain(&node, tx_hash).await {
                break true;
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .unwrap_or(false);

    if included {
        return Err(StepError::LogicalError {
            message: format!(
                "Transaction `{transaction_alias}` was unexpectedly included within {timeout_seconds} seconds"
            ),
        });
    }

    Ok(())
}

async fn transaction_is_in_chain(
    client: &lb_testing_framework::NodeHttpClient,
    tx_hash: TxHash,
) -> bool {
    let Ok(consensus) = client.consensus_info().await else {
        return false;
    };

    let mut scanned_blocks = HashSet::new();

    scan_chain_until(
        consensus.tip,
        &mut scanned_blocks,
        async |header_id| client.block(&header_id).await.ok().flatten(),
        |block: &ApiBlock| {
            block
                .transactions
                .iter()
                .any(|tx| tx.hash() == tx_hash)
                .then_some(())
        },
    )
    .await
    .is_some()
}

fn create_invalid_transaction() -> SignedMantleTx {
    let output_note = Note::new(1000, ZkPublicKey::new(1u8.into()));
    let transfer_op = TransferOp::new(Inputs::new(vec![]), Outputs::new(vec![output_note]));

    let mantle_tx = MantleTx {
        ops: vec![Op::Transfer(transfer_op)],
        storage_gas_price: GENESIS_STORAGE_GAS_PRICE,
        execution_gas_price: 0.into(),
    };

    let transfer_proof = ZkKey::multi_sign(&[], mantle_tx.hash().as_ref())
        .expect("invalid transfer proof should still be constructible");

    SignedMantleTx {
        ops_proofs: vec![OpProof::ZkSig(transfer_proof)],
        mantle_tx,
    }
}
