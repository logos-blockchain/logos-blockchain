//! Cucumber wallet transaction submission workflow.
//!
//! This adapter resolves scenario wallets, syncs spendable state, applies
//! scenario fee policy, submits signed transactions, and records reservations.

use std::time::Duration;

use hex::ToHex as _;
use lb_core::{
    codec::SerializeOp as _,
    mantle::{OpProof, TxHash, Utxo},
};
use lb_http_api_common::bodies::wallet::transfer_funds::WalletTransferFundsRequestBody;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_testing_framework::{NodeHttpClient, configs::wallet::WalletAccount};
use tracing::warn;

use crate::{
    common::{
        chain,
        wallet::{
            PreparedWalletTransaction, SignedWalletTransaction, WalletFundingResources,
            WalletFundingSource, WalletTransactionError, WalletTransactionIntent, WalletUtxos,
            prepare_wallet_transaction,
        },
    },
    cucumber::{
        error::StepError,
        fee_reserve::{DEFAULT_STORAGE_GAS_PRICE, ScenarioFeeFundingError},
        wallet::{
            TARGET,
            best_node::{BestNodeInfo, sanitize_best_node_info},
            sync::sync_available_utxos_for_user_wallets,
        },
        world::{CucumberWorld, WalletInfo, WalletType},
    },
};

pub struct PreparedUserWalletSubmission {
    wallet: WalletInfo,
    submission: PreparedWalletTransaction,
}

impl PreparedUserWalletSubmission {
    pub(crate) const fn tx_hash(&self) -> TxHash {
        self.submission.tx_hash()
    }
}

pub async fn create_and_submit_transaction(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    receivers: &[(ZkPublicKey, u64)],
    best_node_info: Option<&BestNodeInfo>,
) -> Result<String, StepError> {
    let tx_hashes = create_and_submit_transaction_hashes(
        world,
        step,
        sender_wallet_name,
        receivers,
        best_node_info,
    )
    .await?;

    let tx_hashes_hex = tx_hashes
        .iter()
        .map(|h| {
            h.to_bytes()
                .unwrap()
                .to_ascii_lowercase()
                .encode_hex::<String>()
        })
        .collect::<Vec<_>>()
        .join(", ");

    Ok(tx_hashes_hex)
}

pub async fn create_and_submit_transaction_hashes(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    receivers: &[(ZkPublicKey, u64)],
    best_node_info: Option<&BestNodeInfo>,
) -> Result<Vec<TxHash>, StepError> {
    let wallet = world.resolve_wallet(sender_wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    let tx_hashes = match wallet.wallet_type {
        WalletType::User { .. } => {
            vec![
                submit_user_wallet_transaction(world, step, &wallet, receivers, best_node_info)
                    .await?,
            ]
        }
        WalletType::Funding { .. } => {
            let mut tx_hashes = Vec::with_capacity(receivers.len());
            for (receiver_pk, value) in receivers {
                let body = WalletTransferFundsRequestBody {
                    tip: None,
                    change_public_key: wallet.public_key()?,
                    funding_public_keys: vec![wallet.public_key()?],
                    recipient_public_key: *receiver_pk,
                    amount: *value,
                };
                let tx_hash = world
                    .submit_funding_wallet_transaction(&wallet, body)
                    .await
                    .inspect_err(|e| {
                        warn!(target: TARGET, "Step `{}` error: {e}", step);
                    })?;
                tx_hashes.push(tx_hash);
            }
            tx_hashes
        }
    };

    Ok(tx_hashes)
}

pub async fn wait_for_transactions_inclusion(
    client: &NodeHttpClient,
    tx_hashes: &[TxHash],
    timeout: Duration,
) -> Result<(), StepError> {
    if chain::wait_for_transactions_inclusion(client, tx_hashes, timeout).await {
        return Ok(());
    }

    Err(StepError::Timeout {
        message: format!(
            "Timed out waiting for {} submitted transaction(s): {:?}",
            tx_hashes.len(),
            tx_hashes
        ),
    })
}

pub async fn wait_for_wallet_submitted_transactions_inclusion(
    world: &CucumberWorld,
    wallet_name: &str,
    timeout: Duration,
) -> Result<(), StepError> {
    let tx_hashes = world.wallets.submitted_tx_hashes_for(wallet_name).to_vec();
    let wallet_node_name = world.resolve_wallet(wallet_name)?.node_name;
    let client = &world
        .nodes_info
        .get(&wallet_node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node for wallet '{wallet_name}' not found"),
        })?
        .started_node
        .client;

    wait_for_transactions_inclusion(client, &tx_hashes, timeout).await
}

pub async fn submit_prepared_user_wallet_transaction(
    world: &mut CucumberWorld,
    step: &str,
    prepared: PreparedUserWalletSubmission,
    extra_op_proofs: Vec<OpProof>,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<TxHash, StepError> {
    let PreparedUserWalletSubmission { wallet, submission } = prepared;
    let signed_submission = submission
        .sign_with_leading_proofs(extra_op_proofs)
        .map_err(wallet_transaction_error)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;
    let tx_hash = signed_submission.tx_hash();

    let (_, best_node_client, _) =
        sanitize_best_node_info(world, &wallet.wallet_name, best_node_info).await?;
    world
        .submit_transaction(&wallet, signed_submission.signed_tx(), best_node_client)
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

    record_wallet_submission(world, &wallet, &signed_submission);
    Ok(tx_hash)
}

pub(crate) async fn prepare_user_wallet_transaction_submission(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    transaction_intent: WalletTransactionIntent,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<PreparedUserWalletSubmission, StepError> {
    let wallet = world.resolve_wallet(sender_wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    let wallet_account = match &wallet.wallet_type {
        WalletType::User { wallet_account } => wallet_account,
        WalletType::Funding { .. } => {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "Wallet `{sender_wallet_name}` must be a user wallet for this step"
                ),
            });
        }
    };

    let available_utxos =
        sync_available_utxos_for_user_wallets(world, step, best_node_info).await?;
    let sender_available_utxos =
        available_utxos
            .get(sender_wallet_name)
            .cloned()
            .ok_or(StepError::LogicalError {
                message: format!("Wallet '{sender_wallet_name}' not found in updated balances"),
            })?;
    let scenario_fee_funds =
        scenario_fee_account_state(world, sender_wallet_name, &available_utxos)?;

    let funding_resources = user_wallet_funding_resources(
        wallet_account.clone(),
        sender_available_utxos,
        scenario_fee_funds,
    );
    let submission = prepare_wallet_transaction(transaction_intent, funding_resources)
        .map_err(wallet_transaction_error)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

    Ok(PreparedUserWalletSubmission { wallet, submission })
}

async fn submit_user_wallet_transaction(
    world: &mut CucumberWorld,
    step: &str,
    wallet: &WalletInfo,
    receivers: &[(ZkPublicKey, u64)],
    best_node_info: Option<&BestNodeInfo>,
) -> Result<TxHash, StepError> {
    let prepared = prepare_user_wallet_transaction_submission(
        world,
        step,
        &wallet.wallet_name,
        WalletTransactionIntent::transfer(receivers, DEFAULT_STORAGE_GAS_PRICE)
            .map_err(wallet_transaction_error)?,
        best_node_info,
    )
    .await?;

    submit_prepared_user_wallet_transaction(world, step, prepared, Vec::new(), best_node_info).await
}

fn user_wallet_funding_resources(
    wallet_account: WalletAccount,
    sender_available_utxos: Vec<Utxo>,
    scenario_fee_funds: Option<WalletFundingSource>,
) -> WalletFundingResources {
    let sender = WalletFundingSource::new(wallet_account, sender_available_utxos);

    match scenario_fee_funds {
        Some(fee_sponsor) => WalletFundingResources::fee_sponsored(sender, fee_sponsor),
        None => WalletFundingResources::new(sender),
    }
}

fn wallet_transaction_error(error: WalletTransactionError) -> StepError {
    match error {
        WalletTransactionError::Funding(error) => StepError::WalletError(error),
        WalletTransactionError::Signing(error) => StepError::ZkSignError(error),
        WalletTransactionError::Verification(error) => StepError::VerificationError(error),
        WalletTransactionError::Gas(error) => StepError::LogicalError {
            message: error.to_string(),
        },
        WalletTransactionError::OutputTotalOverflow => StepError::LogicalError {
            message: error.to_string(),
        },
        error @ (WalletTransactionError::MissingFundingInput { .. }
        | WalletTransactionError::MissingSigningKey { .. }) => StepError::LogicalError {
            message: error.to_string(),
        },
    }
}

fn record_wallet_submission(
    world: &mut CucumberWorld,
    wallet: &WalletInfo,
    signed_submission: &SignedWalletTransaction,
) {
    let wallet_name = wallet.wallet_name.as_str();
    let group_key = world
        .node_to_group
        .get(&wallet.node_name)
        .cloned()
        .unwrap_or_default();
    let reserved_inputs = signed_submission.reserved_inputs();
    let recorded = world.wallets.record_wallet_reservation(
        wallet_name.to_owned(),
        signed_submission.tx_hash(),
        reserved_inputs,
        signed_submission.spent_fee(),
    );

    world.fee_state.reserve_for_wallet(
        wallet_name.to_owned(),
        group_key,
        recorded.into_fee_sponsor_reserved_inputs(),
    );
}

fn scenario_fee_account_state(
    world: &CucumberWorld,
    wallet_name: &str,
    available_utxos: &WalletUtxos,
) -> Result<Option<WalletFundingSource>, StepError> {
    let group_key = group_key_for_wallet(world, wallet_name)?;

    world
        .fee_state
        .funding_source_for_group(&group_key, available_utxos)
        .map_err(|error| scenario_fee_funding_error(wallet_name, &error))
}

fn scenario_fee_funding_error(wallet_name: &str, error: &ScenarioFeeFundingError) -> StepError {
    StepError::LogicalError {
        message: format!(
            "Scenario fee account state for wallet '{wallet_name}' is invalid: {error}"
        ),
    }
}

fn group_key_for_wallet(world: &CucumberWorld, wallet_name: &str) -> Result<String, StepError> {
    let wallet = world.resolve_wallet(wallet_name)?;
    Ok(world
        .node_to_group
        .get(&wallet.node_name)
        .cloned()
        .unwrap_or_default())
}
