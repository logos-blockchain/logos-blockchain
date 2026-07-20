//! Cucumber wallet transaction submission workflow.
//!
//! This adapter resolves scenario wallets, reads spendable state, applies
//! scenario fee policy, submits signed transactions, and records reservations.

use std::{collections::HashSet, time::Duration};

use hex::ToHex as _;
use lb_core::{
    codec::SerializeOp as _,
    mantle::{OpProof, SignedMantleTx, TxHash, Utxo},
};
use lb_http_api_common::bodies::wallet::transfer_funds::WalletTransferFundsRequestBody;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_testing_framework::{NodeHttpClient, configs::wallet::WalletAccount, is_truthy_env};
use tokio::{task::JoinSet, time::timeout};
use tracing::{debug, info, warn};

use crate::{
    common::{
        chain,
        wallet::{
            PreparedWalletTransaction, PreparedWalletTransactionWorkItem, SignedWalletTransaction,
            WalletFundingResources, WalletFundingSource, WalletReservedInputs,
            WalletTransactionError, WalletTransactionIntent, WalletUtxos,
            finalize_prepared_wallet_transaction, prepare_wallet_transaction_work_item,
        },
    },
    cucumber::{
        defaults::CUCUMBER_VERBOSE_CONSOLE,
        error::StepError,
        fee_reserve::{DEFAULT_STORAGE_GAS_PRICE, ScenarioFeeFundingError},
        wallet::{
            TARGET,
            best_node::{BestNodeInfo, get_best_node_info, sanitize_best_node_info},
            sync::current_available_utxos_for_user_wallets,
        },
        world::{CucumberWorld, WalletInfo, WalletType},
    },
};

/// Prepared transaction tied to the scenario wallet that owns it.
///
/// This is used when a test step needs to construct a transaction now and
/// submit it later, optionally after adding extra operation proofs.
pub struct PreparedUserWalletSubmission {
    wallet: WalletInfo,
    submission: PreparedWalletTransaction,
}

/// Signed transaction plus the wallet metadata needed for submission and
/// bookkeeping.
pub(crate) struct SignedUserWalletSubmission {
    wallet: WalletInfo,
    submission: SignedWalletTransaction,
}

/// Transaction whose inputs are selected but whose proofs are not finalized.
///
/// Manual command flows use this to reserve inputs in an in-memory cache before
/// doing expensive signing work concurrently.
pub(crate) struct ReservedUserWalletSubmission {
    wallet: WalletInfo,
    submission: PreparedWalletTransactionWorkItem,
}

impl PreparedUserWalletSubmission {
    pub(crate) const fn tx_hash(&self) -> TxHash {
        self.submission.tx_hash()
    }
}

impl SignedUserWalletSubmission {
    pub(crate) const fn tx_hash(&self) -> TxHash {
        self.submission.tx_hash()
    }

    pub(crate) const fn signed_tx(&self) -> &SignedMantleTx {
        self.submission.signed_tx()
    }

    #[must_use]
    pub fn reserved_inputs(&self) -> WalletReservedInputs {
        self.submission.reserved_inputs()
    }
}

impl ReservedUserWalletSubmission {
    #[must_use]
    pub fn reserved_inputs(&self) -> WalletReservedInputs {
        self.submission.reserved_inputs()
    }
}

/// Reserve inputs for a user-wallet transfer and immediately update the
/// caller's UTXO cache.
///
/// Updating the cache prevents a batch of pending transactions from selecting
/// the same input notes before the scanner state observes the submissions.
pub(crate) async fn reserve_user_wallet_transaction_submission_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    receivers: &[(ZkPublicKey, u64)],
    available_utxos: &mut WalletUtxos,
) -> Result<ReservedUserWalletSubmission, StepError> {
    let reserved = reserve_user_wallet_transaction_submission(
        world,
        step,
        sender_wallet_name,
        WalletTransactionIntent::transfer(receivers, DEFAULT_STORAGE_GAS_PRICE)
            .map_err(wallet_transaction_error)?,
        Some(available_utxos),
    )
    .await?;
    apply_reserved_inputs_to_utxo_cache(available_utxos, reserved.reserved_inputs());
    Ok(reserved)
}

/// Finalize reserved transactions in blocking worker tasks.
///
/// Wallet proof/signing work can be CPU-heavy, so this avoids doing it inline
/// on the async runtime while preserving the first error for the caller.
pub(crate) async fn finalize_reserved_user_wallet_submissions_concurrently(
    step: &str,
    reserved_submissions: Vec<ReservedUserWalletSubmission>,
) -> Result<Vec<SignedUserWalletSubmission>, StepError> {
    if reserved_submissions.is_empty() {
        return Ok(Vec::new());
    }

    let mut join_set = JoinSet::new();
    for reserved_submission in reserved_submissions {
        let step = step.to_owned();
        join_set.spawn_blocking(move || {
            finalize_reserved_user_wallet_submission(&step, reserved_submission)
        });
    }

    let mut signed_submissions = Vec::new();
    let mut first_error = None;

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(signed_submission)) => signed_submissions.push(signed_submission),
            Ok(Err(error)) => {
                first_error.get_or_insert(error);
            }
            Err(error) => {
                first_error.get_or_insert_with(|| StepError::LogicalError {
                    message: format!("Concurrent transaction preparation task failed: {error}"),
                });
            }
        }
    }

    if let Some(error) = first_error {
        return Err(error);
    }

    Ok(signed_submissions)
}

/// Get the best n nodes for a random wallet's fork group. All wallets in the
/// list should be in the same fork group.
async fn get_best_n_nodes_for_submissions(
    world: &CucumberWorld,
    signed_submissions: &[SignedUserWalletSubmission],
    n: usize,
) -> Result<Vec<(String, NodeHttpClient)>, StepError> {
    let mut wallet_names = signed_submissions
        .iter()
        .map(|v| v.wallet.wallet_name.clone())
        .collect::<Vec<_>>();
    wallet_names.sort();
    wallet_names.dedup();
    let best_node_info =
        get_best_node_info(world, wallet_names.first().expect("wallet exists"), None).await?;
    let same_tip_node_names = best_node_info
        .best_nodes
        .values()
        .next()
        .ok_or(StepError::LogicalError {
            message: "No best node info available for submission".to_owned(),
        })?
        .same_tip_nodes
        .iter()
        .take(n.max(1))
        .cloned()
        .collect::<Vec<_>>();
    if same_tip_node_names.is_empty() {
        return Err(StepError::LogicalError {
            message: "No same tip nodes available for submission".to_owned(),
        });
    }

    let mut started_nodes = Vec::with_capacity(n.max(1));
    for node_name in same_tip_node_names {
        if let Some(node_info) = world.nodes_info.get(&node_name) {
            started_nodes.push((node_name, node_info.started_node.client.clone()));
        } else {
            return Err(StepError::LogicalError {
                message: format!("No node info available for {node_name} in world"),
            });
        }
    }

    Ok(started_nodes)
}

/// Submit signed transactions to several nodes sharing the selected majority
/// tip.
///
/// Fanout makes manual/stress scenarios less sensitive to one slow node while
/// still avoiding nodes from a different fork group.
pub(crate) async fn submit_signed_user_wallet_submissions_concurrently(
    world: &mut CucumberWorld,
    signed_submissions: Vec<SignedUserWalletSubmission>,
) -> Result<Vec<(String, TxHash)>, StepError> {
    if signed_submissions.is_empty() {
        return Ok(Vec::new());
    }
    let same_tip_nodes = get_best_n_nodes_for_submissions(world, &signed_submissions, 3).await?;

    let mut join_set = JoinSet::new();

    for signed_submission in signed_submissions {
        let wallet = signed_submission.wallet.clone();
        let same_tip_nodes = same_tip_nodes.clone();

        join_set.spawn(async move {
            let tx_hash = signed_submission.tx_hash();
            let mut submission_errors = Vec::new();

            // Try to submit to each node in same_tip_nodes.
            let mut submitted_nodes = Vec::new();
            for (node_name, node_client) in &same_tip_nodes {
                match timeout(
                    Duration::from_secs(15),
                    node_client.submit_transaction(signed_submission.signed_tx()),
                )
                .await
                {
                    Ok(Ok(())) => {
                        submitted_nodes.push(node_name.clone());
                    }
                    Ok(Err(err)) => {
                        submission_errors.push(format!("{node_name}: {err}"));
                    }
                    Err(_) => {
                        submission_errors.push(format!("{node_name}: timeout"));
                    }
                }
            }
            if !submitted_nodes.is_empty() {
                if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
                    info!(
                        target: TARGET,
                        "Transaction {} submitted successfully to {submitted_nodes:?}",
                        hex::encode(tx_hash.0)
                    );
                }
                return Ok::<_, StepError>(signed_submission);
            }

            // All nodes failed; log and return error.
            let message = format!(
                "Transaction {tx_hash:?} for '{}' failed on all {} nodes: {}",
                wallet.wallet_name,
                same_tip_nodes.len(),
                submission_errors.join("; ")
            );
            warn!(target: TARGET, "{message}");

            Err(StepError::LogicalError { message })
        });
    }

    let mut accepted = Vec::new();
    let mut first_error = None;

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(signed_submission)) => accepted.push(signed_submission),
            Ok(Err(error)) => {
                first_error.get_or_insert(error);
            }
            Err(error) => {
                first_error.get_or_insert_with(|| StepError::LogicalError {
                    message: format!("Concurrent transaction submission task failed: {error}"),
                });
            }
        }
    }

    let mut tx_hashes = Vec::with_capacity(accepted.len());
    for signed_submission in &accepted {
        tx_hashes.push((
            signed_submission.wallet.wallet_name.clone(),
            signed_submission.tx_hash(),
        ));
        record_signed_user_wallet_submission(world, signed_submission)?;
    }

    if let Some(error) = first_error {
        return Err(error);
    }

    Ok(tx_hashes)
}

/// Build, sign, submit, and record one wallet transaction.
///
/// User wallets go through the local signing path. Funding wallets use the node
/// wallet API because their funds are managed by the node-side wallet service.
pub async fn create_and_submit_transaction(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    receivers: &[(ZkPublicKey, u64)],
    best_node_info: Option<&BestNodeInfo>,
    in_memory_available_utxos: Option<&mut WalletUtxos>,
) -> Result<String, StepError> {
    let tx_hashes = create_and_submit_transaction_hashes_with_utxo_cache(
        world,
        step,
        sender_wallet_name,
        receivers,
        best_node_info,
        in_memory_available_utxos,
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

/// Build and submit one or more wallet transactions, optionally using a shared
/// UTXO cache.
///
/// The shared cache is used by batch commands so each transaction sees inputs
/// already reserved by earlier transactions in the same batch.
pub async fn create_and_submit_transaction_hashes_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    receivers: &[(ZkPublicKey, u64)],
    best_node_info: Option<&BestNodeInfo>,
    in_memory_available_utxos: Option<&mut WalletUtxos>,
) -> Result<Vec<TxHash>, StepError> {
    let wallet = world.resolve_wallet(sender_wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    let tx_hashes = match wallet.wallet_type {
        WalletType::User { .. } => {
            let tx_hash = submit_user_wallet_transaction(
                world,
                step,
                &wallet,
                receivers,
                best_node_info,
                in_memory_available_utxos,
            )
            .await?;
            if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
                info!(
                    target: TARGET,
                    "Wallet `{sender_wallet_name}` submitted transaction {} total value {} LGO successfully",
                    tx_hash.to_bytes()?.to_ascii_lowercase().encode_hex::<String>(),
                    receivers.iter().map(|(_, value)| value).sum::<u64>()
                );
            }
            vec![tx_hash]
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
                if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
                    info!(
                        target: TARGET,
                        "Wallet `{sender_wallet_name}` submitted transaction {} total value {} LGO successfully",
                        tx_hash.to_bytes()?.to_ascii_lowercase().encode_hex::<String>(),
                        receivers.iter().map(|(_, value)| value).sum::<u64>(),
                    );
                }
                tx_hashes.push(tx_hash);
            }
            tx_hashes
        }
    };

    Ok(tx_hashes)
}

/// Wait until all supplied transaction hashes are included in chain blocks.
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

/// Wait until all transactions recorded for `wallet_name` are included.
pub async fn wait_for_wallet_submitted_transactions_inclusion(
    world: &CucumberWorld,
    wallet_name: &str,
    timeout: Duration,
) -> Result<(), StepError> {
    let tx_hashes =
        world.with_wallets(|wallets| wallets.submitted_tx_hashes_for(wallet_name).to_vec())?;
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

/// Sign, submit, and record a previously prepared user-wallet transaction.
///
/// This path is used when a step needs to add extra operation proofs before the
/// wallet transaction is finalized.
pub async fn submit_prepared_user_wallet_transaction(
    world: &mut CucumberWorld,
    step: &str,
    prepared: PreparedUserWalletSubmission,
    extra_op_proofs: Vec<OpProof>,
    best_node_info: Option<&BestNodeInfo>,
    in_memory_available_utxos: Option<&mut WalletUtxos>,
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

    record_wallet_submission(
        world,
        &wallet,
        &signed_submission,
        in_memory_available_utxos,
    )?;
    Ok(tx_hash)
}

/// Finalize proofs and signatures for a prepared user-wallet transaction.
pub(crate) fn sign_prepared_user_wallet_transaction(
    step: &str,
    prepared: PreparedUserWalletSubmission,
    extra_op_proofs: Vec<OpProof>,
) -> Result<SignedUserWalletSubmission, StepError> {
    let PreparedUserWalletSubmission { wallet, submission } = prepared;
    let signed_submission = submission
        .sign_with_leading_proofs(extra_op_proofs)
        .map_err(wallet_transaction_error)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

    Ok(SignedUserWalletSubmission {
        wallet,
        submission: signed_submission,
    })
}

fn finalize_reserved_user_wallet_submission(
    step: &str,
    reserved: ReservedUserWalletSubmission,
) -> Result<SignedUserWalletSubmission, StepError> {
    let ReservedUserWalletSubmission { wallet, submission } = reserved;
    let submission = finalize_prepared_wallet_transaction(submission)
        .map_err(wallet_transaction_error)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

    sign_prepared_user_wallet_transaction(
        step,
        PreparedUserWalletSubmission { wallet, submission },
        Vec::new(),
    )
}

/// Record a signed transaction in TF wallet bookkeeping.
pub(crate) fn record_signed_user_wallet_submission(
    world: &mut CucumberWorld,
    signed_submission: &SignedUserWalletSubmission,
) -> Result<(), StepError> {
    record_wallet_submission(
        world,
        &signed_submission.wallet,
        &signed_submission.submission,
        None,
    )
}

/// Prepare a user-wallet transaction without submitting it.
///
/// This resolves wallet state, chooses inputs, applies fee policy, and returns
/// a prepared transaction that can be signed/submitted later.
pub(crate) async fn prepare_user_wallet_transaction_submission(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    transaction_intent: WalletTransactionIntent,
    in_memory_available_utxos: Option<&WalletUtxos>,
) -> Result<PreparedUserWalletSubmission, StepError> {
    let reserved = reserve_user_wallet_transaction_submission(
        world,
        step,
        sender_wallet_name,
        transaction_intent,
        in_memory_available_utxos,
    )
    .await?;
    let ReservedUserWalletSubmission { wallet, submission } = reserved;
    let submission = finalize_prepared_wallet_transaction(submission)
        .map_err(wallet_transaction_error)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

    Ok(PreparedUserWalletSubmission { wallet, submission })
}

async fn reserve_user_wallet_transaction_submission(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    transaction_intent: WalletTransactionIntent,
    in_memory_available_utxos: Option<&WalletUtxos>,
) -> Result<ReservedUserWalletSubmission, StepError> {
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

    let synced_available_utxos;
    let available_utxos = if let Some(cache) = in_memory_available_utxos {
        cache
    } else {
        synced_available_utxos = current_available_utxos_for_user_wallets(world, step).await?;
        &synced_available_utxos
    };

    let sender_available_utxos =
        available_utxos
            .get(sender_wallet_name)
            .cloned()
            .ok_or(StepError::LogicalError {
                message: format!("Wallet '{sender_wallet_name}' not found in updated balances"),
            })?;

    let scenario_fee_funds =
        scenario_fee_account_state(world, sender_wallet_name, available_utxos)?;

    let funding_resources = user_wallet_funding_resources(
        wallet_account.clone(),
        sender_available_utxos,
        scenario_fee_funds,
    );

    let submission = prepare_wallet_transaction_work_item(transaction_intent, funding_resources)
        .map_err(wallet_transaction_error)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

    Ok(ReservedUserWalletSubmission { wallet, submission })
}

async fn submit_user_wallet_transaction(
    world: &mut CucumberWorld,
    step: &str,
    wallet: &WalletInfo,
    receivers: &[(ZkPublicKey, u64)],
    best_node_info: Option<&BestNodeInfo>,
    in_memory_available_utxos: Option<&mut WalletUtxos>,
) -> Result<TxHash, StepError> {
    let prepared = prepare_user_wallet_transaction_submission(
        world,
        step,
        &wallet.wallet_name,
        WalletTransactionIntent::transfer(receivers, DEFAULT_STORAGE_GAS_PRICE)
            .map_err(wallet_transaction_error)?,
        in_memory_available_utxos.as_deref(),
    )
    .await?;

    submit_prepared_user_wallet_transaction(
        world,
        step,
        prepared,
        Vec::new(),
        best_node_info,
        in_memory_available_utxos,
    )
    .await
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
        WalletTransactionError::Builder(error) => StepError::LogicalError {
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
    in_memory_available_utxos: Option<&mut WalletUtxos>,
) -> Result<(), StepError> {
    if let Some(cache) = in_memory_available_utxos {
        apply_submitted_inputs_to_utxo_cache(cache, signed_submission);
    }

    let wallet_name = wallet.wallet_name.as_str();
    let group_key = world
        .node_to_group
        .get(&wallet.node_name)
        .cloned()
        .unwrap_or_default();
    let reserved_inputs = signed_submission.reserved_inputs();
    let recorded = world.with_wallets_mut(|wallets| {
        wallets.record_wallet_reservation(
            wallet_name.to_owned(),
            signed_submission.tx_hash(),
            reserved_inputs,
            signed_submission.spent_fee(),
        )
    })?;

    debug!(
        target: TARGET,
        "Recorded wallet submission: {wallet}, {tx_hash:?}, {sender_inputs}, {fee_sponsor_inputs}, {spent_fee}",
        wallet = wallet_name,
        tx_hash = signed_submission.tx_hash(),
        sender_inputs = recorded.sender_reserved_inputs().len(),
        fee_sponsor_inputs = recorded.fee_sponsor_reserved_inputs().len(),
        spent_fee = signed_submission.spent_fee(),
    );

    world.fee_state.reserve_for_wallet(
        wallet_name.to_owned(),
        group_key,
        recorded.into_fee_sponsor_reserved_inputs(),
    );

    Ok(())
}

fn apply_submitted_inputs_to_utxo_cache(
    cache: &mut WalletUtxos,
    signed_submission: &SignedWalletTransaction,
) {
    apply_reserved_inputs_to_utxo_cache(cache, signed_submission.reserved_inputs());
}

pub fn apply_reserved_inputs_to_utxo_cache(
    cache: &mut WalletUtxos,
    reserved_inputs: WalletReservedInputs,
) {
    let (sender_inputs, fee_sponsor_inputs) = reserved_inputs.into_sender_and_fee_sponsor_inputs();

    let spent_note_ids = sender_inputs
        .into_iter()
        .chain(fee_sponsor_inputs)
        .map(|utxo| utxo.id())
        .collect::<HashSet<_>>();

    for utxos in cache.values_mut() {
        utxos.retain(|utxo| !spent_note_ids.contains(&utxo.id()));
    }
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
