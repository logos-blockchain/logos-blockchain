use std::{collections::HashSet, time::Duration};

use lb_common_http_client::ApiBlock;
use lb_core::mantle::{
    Note, NoteId, Op, OpProof, SignedMantleTx,
    ledger::{Inputs, Outputs},
    ops::transfer::TransferOp,
    traits::Hashable as _,
    transactions::{hash::TxHash, mantle_tx::RawMantleTx, states::Unverified},
};
use lb_groth16::Fr;
use lb_key_management_system_service::keys::{ZkKey, ZkPublicKey};
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use crate::{
    common::{chain::scan_chain_until, wallet::WalletUtxos},
    cucumber::{
        error::StepError,
        steps::{
            TARGET, transactions::utils::create_and_submit_transaction_hashes_with_utxo_cache,
        },
        wallet::sync::{WalletSendReadiness, wait_wallet_send_ready},
        world::{CucumberWorld, WalletType},
    },
};

/// Submit a transaction that passes stateless validation but is invalid against
/// ledger state.
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

    let signed_tx = create_stateful_invalid_transaction();
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

/// Submit a transaction that fails stateless validation.
pub async fn submit_stateless_invalid_transfer_transaction(
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

    let signed_tx = create_stateless_invalid_transaction();
    let outcome = node
        .submit_transaction(&signed_tx)
        .await
        .map_err(|e| e.to_string());

    info!(
        target: TARGET,
        "Submitted stateless-invalid transfer transaction `{transaction_alias}` to `{node_name}`: {outcome:?}"
    );

    world.remember_submission_outcome(transaction_alias, outcome);

    Ok(())
}

/// Assert that a previously submitted transaction was rejected during
/// preverification.
pub fn transaction_is_rejected_during_preverification(
    world: &CucumberWorld,
    transaction_alias: &str,
) -> Result<(), StepError> {
    match world.resolve_submission_outcome(transaction_alias)? {
        Ok(()) => Err(StepError::LogicalError {
            message: format!(
                "expected transaction `{transaction_alias}` to be rejected during preverification, but it was accepted"
            ),
        }),
        Err(message) if message.contains("Inputs cannot be empty") => Ok(()),
        Err(other) => Err(StepError::LogicalError {
            message: format!(
                "expected transaction `{transaction_alias}` to be rejected mentioning 'Inputs cannot be empty', got: {other}"
            ),
        }),
    }
}

pub async fn submit_funded_transfer_transaction(
    world: &mut CucumberWorld,
    step: &str,
    transaction_alias: String,
    amount: u64,
    sender_wallet_name: String,
    receiver_wallet_name: String,
) -> Result<(), StepError> {
    let sender_wallet = world.resolve_wallet(&sender_wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;
    let receiver = world.resolve_recipient(&receiver_wallet_name)?;

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

    let mut available_utxos = WalletUtxos::new();
    let best_node_info = wait_wallet_send_ready(
        world,
        step,
        &sender_wallet_name,
        180,
        amount,
        WalletSendReadiness::TotalValueOnly,
        &mut available_utxos,
        &HashSet::new(),
    )
    .await?;

    let tx_hashes = create_and_submit_transaction_hashes_with_utxo_cache(
        world,
        step,
        &sender_wallet_name,
        &[(receiver.public_key, amount)],
        Some(&best_node_info),
        Some(&mut available_utxos),
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
        "Submitted funded transfer transaction `{transaction_alias}` from `{sender_wallet_name}` to `{}`",
        receiver.label
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
        consensus.cryptarchia_info.tip,
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

/// Builds a transaction that fails stateless validation: a `TransferOp` with no
/// inputs.
pub fn create_stateless_invalid_transaction() -> SignedMantleTx<Unverified> {
    let output_note = Note::new(1000, ZkPublicKey::new(1u8.into()));
    let transfer_op = TransferOp::new(Inputs::empty(), Outputs::new([output_note]));

    build_signed_transfer(transfer_op)
}

/// Builds a transaction that passes stateless validation but is invalid against
/// ledger state: its input note id does not correspond to any real UTXO.
fn create_stateful_invalid_transaction() -> SignedMantleTx<Unverified> {
    let nonexistent_input = NoteId(Fr::from(0xDEAD_BEEFu64));
    let output_note = Note::new(1000, ZkPublicKey::new(1u8.into()));
    let transfer_op = TransferOp::new(
        Inputs::new([nonexistent_input]),
        Outputs::new([output_note]),
    );

    build_signed_transfer(transfer_op)
}

fn build_signed_transfer(transfer_op: TransferOp) -> SignedMantleTx<Unverified> {
    let mantle_tx = RawMantleTx([Op::Transfer(transfer_op)].into());

    let transfer_proof = ZkKey::multi_sign(&[], &mantle_tx.hash().to_fr())
        .expect("invalid transfer proof should still be constructible");

    SignedMantleTx::new(mantle_tx, [OpProof::ZkSig(transfer_proof)].into())
}
