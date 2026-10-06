//! Cucumber wallet transaction submission workflow.
//!
//! This adapter resolves scenario wallets, reads spendable state, applies
//! fee reserves, submits signed transactions, and records reservations.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{StreamExt as _, stream::FuturesUnordered};
use lb_core::mantle::{
    NoteId, SignedOps, TxHash, Utxo,
    ledger::{MAX_TRANSACTION_INPUTS, verification_mode::StandardMode},
    transactions::{GasPrices, OpProofs, states::Preverified},
};
use lb_http_api_common::bodies::wallet::transfer_funds::WalletTransferFundsRequestBody;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_testing_framework::{NodeHttpClient, configs::wallet::WalletAccount, is_truthy_env};
use lb_wallet::WalletError;
use tokio::{task::JoinSet, time::timeout};
use tracing::{debug, info, warn};

use crate::{
    common::{
        chain,
        wallet::{
            PreparedWalletTransaction, PreparedWalletTransactionWorkItem, SignedWalletTransaction,
            TransactionFeePolicy, WalletFundingResources, WalletFundingSource, WalletId,
            WalletInputSelectionStrategy, WalletReservedInputs, WalletTransactionError,
            WalletTransactionIntent, WalletUtxos, estimate_workload_fee_requirements,
            finalize_prepared_wallet_transaction, prepare_wallet_transaction_work_item,
        },
    },
    cucumber::{
        defaults::CUCUMBER_VERBOSE_CONSOLE,
        error::StepError,
        fee_reserve::ScenarioFeeFundingError,
        utils::tx_hash_to_hex,
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

/// Result of the network-only phase for a batch of signed submissions.
///
/// A transaction is accepted when at least one selected fan-out node accepts
/// it. `first_error` is populated when one or more transactions failed on all
/// selected nodes; accepted transactions are retained so callers can preserve
/// the existing post-attempt bookkeeping behavior before returning that error.
pub(crate) struct SignedUserWalletSubmissionNetworkResult {
    pub(crate) accepted: Vec<SignedUserWalletSubmission>,
    pub(crate) first_error: Option<StepError>,
    pub(crate) failed_transaction_count: usize,
    pub(crate) fanout_node_names: Vec<String>,
    pub(crate) node_selection_duration: Duration,
    pub(crate) preflight_duration: Duration,
    pub(crate) network_submission_duration: Duration,
}

/// Transaction whose inputs are selected but whose proofs are not finalized.
///
/// Manual command flows use this to reserve inputs in an in-memory cache before
/// doing expensive signing work concurrently.
pub(crate) struct ReservedUserWalletSubmission {
    wallet: WalletInfo,
    submission: PreparedWalletTransactionWorkItem,
}

const CONTINUOUS_WORKLOAD_DUST_RATIO: u64 = 15;
const MAX_CONTINUOUS_WORKLOAD_DUST_INPUTS: usize = 10;

const fn continuous_workload_dust_threshold(output_value: u64, base_tx_fee: u64) -> u64 {
    let transfer_value_threshold = output_value / CONTINUOUS_WORKLOAD_DUST_RATIO;
    if transfer_value_threshold > base_tx_fee {
        transfer_value_threshold
    } else {
        base_tx_fee
    }
}

const fn continuous_workload_sender_fee_requirement(base_tx_fee: u64, fee_sponsored: bool) -> u64 {
    if fee_sponsored { 0 } else { base_tx_fee }
}

/// Per-wallet UTXOs prepared for continuous workload transaction batches.
///
/// The ordered map provides the bounded largest-first primary prefix and
/// smallest dust candidates without rebuilding or sorting the wallet's full
/// UTXO list for each transaction. The second map supports removal by
/// reserved note ID.
#[derive(Debug, Default)]
struct WorkloadUtxoPool {
    by_value: BTreeMap<u64, BTreeMap<NoteId, Utxo>>,
    value_by_note_id: HashMap<NoteId, u64>,
    candidate_count: usize,
}

impl WorkloadUtxoPool {
    fn new(utxos: &[Utxo]) -> Self {
        let mut pool = Self::default();
        for utxo in utxos {
            let note_id = utxo.id();
            let value = utxo.note.value;
            pool.by_value
                .entry(value)
                .or_default()
                .insert(note_id, *utxo);
            pool.value_by_note_id.insert(note_id, value);
            pool.candidate_count += 1;
        }
        pool
    }

    const fn len(&self) -> usize {
        self.candidate_count
    }

    fn remove(&mut self, note_id: NoteId) {
        if let Some(value) = self.value_by_note_id.remove(&note_id) {
            let remove_value_bucket = self.by_value.get_mut(&value).is_some_and(|bucket| {
                bucket.remove(&note_id);
                bucket.is_empty()
            });
            self.candidate_count -= 1;
            if remove_value_bucket {
                self.by_value.remove(&value);
            }
        }
    }

    fn primary_candidates(&self) -> Vec<Utxo> {
        self.by_value
            .iter()
            .rev()
            .flat_map(|(_, bucket)| bucket.values().rev())
            .take(MAX_TRANSACTION_INPUTS)
            .copied()
            .collect()
    }

    #[cfg(test)]
    fn primary(&self) -> Option<Utxo> {
        self.primary_candidates().first().copied()
    }

    #[cfg(test)]
    fn candidates(&self, output_value: u64, base_tx_fee: u64) -> Option<WorkloadCandidateSet> {
        let primary = self.primary()?;
        Some(self.candidates_for_primary(&[primary], output_value, base_tx_fee))
    }

    fn candidates_for_primary(
        &self,
        primary_inputs: &[Utxo],
        output_value: u64,
        sender_fee_requirement: u64,
    ) -> WorkloadCandidateSet {
        let dust_threshold =
            continuous_workload_dust_threshold(output_value, sender_fee_requirement);
        let primary_note_ids = primary_inputs.iter().map(Utxo::id).collect::<HashSet<_>>();
        let dust_input_limit = MAX_CONTINUOUS_WORKLOAD_DUST_INPUTS
            .min(MAX_TRANSACTION_INPUTS.saturating_sub(primary_inputs.len()));
        let mut dust = Vec::with_capacity(dust_input_limit);

        dust.extend(
            self.by_value
                .range(..=dust_threshold)
                .flat_map(|(_, bucket)| bucket.values())
                .filter(|utxo| !primary_note_ids.contains(&utxo.id()))
                .take(dust_input_limit)
                .copied(),
        );

        WorkloadCandidateSet {
            primary_inputs: primary_inputs.to_vec(),
            dust,
            dust_threshold,
        }
    }
}

#[derive(Debug)]
struct WorkloadCandidateSet {
    primary_inputs: Vec<Utxo>,
    dust: Vec<Utxo>,
    dust_threshold: u64,
}

impl WorkloadCandidateSet {
    fn dust_value(&self, count: usize) -> u64 {
        self.dust
            .iter()
            .take(count)
            .map(|utxo| utxo.note.value)
            .sum()
    }

    fn inputs(&self, dust_count: usize) -> Vec<Utxo> {
        let dust_count = dust_count.min(self.dust.len());
        let mut inputs = Vec::with_capacity(dust_count + self.primary_inputs.len());
        inputs.extend_from_slice(&self.primary_inputs);
        inputs.extend_from_slice(&self.dust[..dust_count]);
        inputs
    }
}

/// Workload-only candidate pools plus an index for removing actual reserved
/// inputs from the shared wallet cache in constant time.
#[derive(Debug, Default)]
pub struct WorkloadUtxoPools {
    by_wallet: HashMap<WalletId, WorkloadUtxoPool>,
    cache_positions: HashMap<NoteId, (WalletId, usize)>,
}

impl WorkloadUtxoPools {
    #[must_use]
    pub fn from_cache(cache: &WalletUtxos) -> Self {
        let mut pools = Self::default();
        for (wallet_name, utxos) in cache {
            pools
                .by_wallet
                .insert(wallet_name.clone(), WorkloadUtxoPool::new(utxos));
            for (index, utxo) in utxos.iter().enumerate() {
                pools
                    .cache_positions
                    .insert(utxo.id(), (wallet_name.clone(), index));
            }
        }
        pools
    }

    pub fn candidate_count(&self, wallet_name: &str) -> usize {
        self.by_wallet
            .get(wallet_name)
            .map_or(0, WorkloadUtxoPool::len)
    }

    fn primary_candidates(&self, wallet_name: &str) -> Vec<Utxo> {
        self.by_wallet
            .get(wallet_name)
            .map_or_else(Vec::new, WorkloadUtxoPool::primary_candidates)
    }

    fn candidates(
        &self,
        wallet_name: &str,
        primary_inputs: &[Utxo],
        output_value: u64,
        sender_fee_requirement: u64,
    ) -> Option<WorkloadCandidateSet> {
        Some(self.by_wallet.get(wallet_name)?.candidates_for_primary(
            primary_inputs,
            output_value,
            sender_fee_requirement,
        ))
    }

    fn remove_reserved_inputs(
        &mut self,
        cache: &mut WalletUtxos,
        reserved_inputs: WalletReservedInputs,
    ) -> Result<(), StepError> {
        let (sender_inputs, fee_sponsor_inputs) =
            reserved_inputs.into_sender_and_fee_sponsor_inputs();
        for input in sender_inputs.into_iter().chain(fee_sponsor_inputs) {
            let note_id = input.id();
            let Some((wallet_name, index)) = self.cache_positions.remove(&note_id) else {
                continue;
            };
            let Some(utxos) = cache.get_mut(&wallet_name) else {
                return Err(StepError::LogicalError {
                    message: format!(
                        "Workload reservation index refers to missing wallet '{wallet_name}'"
                    ),
                });
            };
            if utxos.get(index).is_none_or(|cached| cached.id() != note_id) {
                return Err(StepError::LogicalError {
                    message: format!(
                        "Workload reservation index is stale for wallet '{wallet_name}' and input {note_id:?}"
                    ),
                });
            }

            let removed = utxos.swap_remove(index);
            if let Some(moved) = utxos.get(index) {
                self.cache_positions
                    .insert(moved.id(), (wallet_name.clone(), index));
            }
            if let Some(pool) = self.by_wallet.get_mut(&wallet_name) {
                pool.remove(removed.id());
            }
        }
        Ok(())
    }
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

    pub(crate) const fn signed_tx(&self) -> &SignedOps<Preverified, StandardMode> {
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
    gas_prices: Option<GasPrices>,
    priority_fee_percent: u64,
) -> Result<ReservedUserWalletSubmission, StepError> {
    let transaction_intent =
        WalletTransactionIntent::transfer(receivers).map_err(wallet_transaction_error)?;
    reserve_user_wallet_transaction_intent_with_utxo_cache(
        world,
        step,
        sender_wallet_name,
        transaction_intent,
        available_utxos,
        gas_prices,
        priority_fee_percent,
    )
    .await
}

/// Reserve inputs for a caller-provided user-wallet transaction intent and
/// immediately update the caller's UTXO cache.
pub(crate) async fn reserve_user_wallet_transaction_intent_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    transaction_intent: WalletTransactionIntent,
    available_utxos: &mut WalletUtxos,
    gas_prices: Option<GasPrices>,
    priority_fee_percent: u64,
) -> Result<ReservedUserWalletSubmission, StepError> {
    let reserved = reserve_user_wallet_transaction_submission(
        world,
        step,
        sender_wallet_name,
        transaction_intent,
        Some(available_utxos),
        None,
        None,
        WalletInputSelectionStrategy::LargestFirst,
        gas_prices,
        priority_fee_percent,
    )
    .await?;
    apply_reserved_inputs_to_utxo_cache(available_utxos, reserved.reserved_inputs());
    Ok(reserved)
}

/// Reserve a continuous workload transaction from its smallest sufficient
/// largest-first primary prefix and a bounded set of the smallest qualifying
/// dust inputs.
///
/// These stress workloads deliberately create a large source UTXO per
/// transaction. They may consume small historical outputs alongside it, but
/// they never fall back to the wallet's full UTXO set.
#[expect(
    clippy::too_many_arguments,
    reason = "Workload transaction reservation inputs"
)]
#[expect(
    clippy::too_many_lines,
    reason = "Keep bounded workload retries and funding diagnostics together"
)]
pub(crate) async fn reserve_workload_transaction_intent_with_primary_and_dust(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    transaction_intent: WalletTransactionIntent,
    output_value: u64,
    available_utxos: &mut WalletUtxos,
    workload_pools: &mut WorkloadUtxoPools,
    gas_prices: Option<GasPrices>,
    priority_fee_percent: u64,
) -> Result<ReservedUserWalletSubmission, StepError> {
    let primary_candidates = workload_pools.primary_candidates(sender_wallet_name);
    let available_candidate_count = workload_pools.candidate_count(sender_wallet_name);
    if primary_candidates.is_empty() {
        return Err(StepError::LogicalError {
            message: format!(
                "Workload funding failed for wallet '{sender_wallet_name}': no primary UTXO is \
                available; output value={output_value}, primary input limit={MAX_TRANSACTION_INPUTS}, \
                available candidate count={available_candidate_count}"
            ),
        });
    }
    let fee_intent = gas_prices.as_ref().map_or_else(
        || transaction_intent.clone(),
        |gas_prices| {
            transaction_intent
                .clone()
                .with_gas_prices(gas_prices.clone())
        },
    );

    let mut primary_reserved = None;
    let mut selected_primary_count = 0;
    let mut last_primary_funding_error = None;
    for primary_count in 1..=primary_candidates.len() {
        match reserve_user_wallet_transaction_submission(
            world,
            step,
            sender_wallet_name,
            transaction_intent.clone(),
            Some(available_utxos),
            Some(&primary_candidates[..primary_count]),
            None,
            WalletInputSelectionStrategy::AllProvided,
            gas_prices.clone(),
            priority_fee_percent,
        )
        .await
        {
            Ok(reserved) => {
                primary_reserved = Some(reserved);
                selected_primary_count = primary_count;
                break;
            }
            Err(error) if is_user_wallet_funds_deficit(&error) => {
                last_primary_funding_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }

    let Some(primary_reserved) = primary_reserved else {
        let attempted_primary_value = primary_candidates
            .iter()
            .map(|utxo| utxo.note.value)
            .sum::<u64>();
        let largest_primary_value = primary_candidates[0].note.value;
        let primary_count = primary_candidates.len();
        let (required_fee_without_change, required_fee_with_change, fee_estimate_error) =
            estimate_workload_fee_requirements(
                &fee_intent,
                &primary_candidates,
                priority_fee_percent,
            )
            .map_or_else(
                |error| (None, None, Some(format!("unavailable ({error})"))),
                |(without_change, with_change)| (Some(without_change), Some(with_change), None),
            );
        let required_fee_without_change = required_fee_without_change
            .map_or_else(|| "unavailable".to_owned(), |fee| fee.to_string());
        let required_fee_with_change = required_fee_with_change
            .map_or_else(|| "unavailable".to_owned(), |fee| fee.to_string());
        let fee_estimate_error = fee_estimate_error.unwrap_or_default();
        let last_error = last_primary_funding_error.map_or_else(
            || "no primary funding attempt was made".to_owned(),
            |e| e.to_string(),
        );
        return Err(StepError::LogicalError {
            message: format!(
                "Continuous workload funding failed for wallet '{sender_wallet_name}': output \
                value={output_value}, largest primary value={largest_primary_value}, bounded \
                largest-first primary candidates attempted={primary_count}, primary input \
                limit={MAX_TRANSACTION_INPUTS}, total candidate value in that prefix=\
                {attempted_primary_value}, primary fee headroom={} (prefix minus output), required \
                fee without change={required_fee_without_change}, required fee with change=\
                {required_fee_with_change}{fee_estimate_error}, \
                available candidate count={available_candidate_count}; no bounded primary prefix \
                funded the transaction: {last_error}",
                attempted_primary_value.saturating_sub(output_value)
            ),
        });
    };

    let primary_inputs = &primary_candidates[..selected_primary_count];
    let (_, base_tx_fee) = estimate_workload_fee_requirements(&fee_intent, primary_inputs, 0)
        .map_err(|error| StepError::LogicalError {
            message: format!(
                "Continuous workload funding reserved a {selected_primary_count}-input primary \
                prefix for wallet '{sender_wallet_name}', but its base transaction fee could not \
                be estimated: {error}"
            ),
        })?;
    let fee_sponsored =
        scenario_fee_account_state(world, sender_wallet_name, available_utxos)?.is_some();
    let sender_fee_requirement =
        continuous_workload_sender_fee_requirement(base_tx_fee, fee_sponsored);
    let candidates = workload_pools
        .candidates(
            sender_wallet_name,
            primary_inputs,
            output_value,
            sender_fee_requirement,
        )
        .expect("workload primary prefix was read from the same candidate pool");

    let eligible_dust_count = candidates.dust.len();
    let max_dust_count = candidates.dust.len();
    let mut last_dust_funding_error = None;

    // Requiring each bounded candidate set lets the real wallet funding
    // calculation account for the additional input fees. Dropping dust from
    // largest selected dust to smallest preserves the smallest cleanup inputs.
    for dust_count in (1..=max_dust_count).rev() {
        let candidate_inputs = candidates.inputs(dust_count);
        match reserve_user_wallet_transaction_submission(
            world,
            step,
            sender_wallet_name,
            transaction_intent.clone(),
            Some(available_utxos),
            Some(&candidate_inputs),
            None,
            WalletInputSelectionStrategy::AllProvided,
            gas_prices.clone(),
            priority_fee_percent,
        )
        .await
        {
            Ok(reserved) => {
                workload_pools
                    .remove_reserved_inputs(available_utxos, reserved.reserved_inputs())?;
                return Ok(reserved);
            }
            Err(error) if is_user_wallet_funds_deficit(&error) => {
                last_dust_funding_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }

    // The primary-only funding attempt already succeeded, so rejected dust
    // remains available and the validated primary reservation is still valid.
    workload_pools.remove_reserved_inputs(available_utxos, primary_reserved.reserved_inputs())?;
    if max_dust_count > 0 && last_dust_funding_error.is_some() {
        debug!(
            target: TARGET,
            wallet = sender_wallet_name,
            selected_primary_count,
            eligible_dust_count,
            dust_threshold = candidates.dust_threshold,
            attempted_dust_value = candidates.dust_value(max_dust_count),
            "Bounded workload dust candidates exceeded funding headroom; using primary prefix"
        );
    }

    Ok(primary_reserved)
}

const fn is_user_wallet_funds_deficit(error: &StepError) -> bool {
    matches!(
        error,
        StepError::WalletError(WalletError::InsufficientFunds { .. })
            | StepError::FundsDeficit { .. }
    )
}

fn restore_preparation_order<T>(
    expected_count: usize,
    completed_submissions: impl IntoIterator<Item = (usize, T)>,
) -> Option<Vec<T>> {
    let mut ordered = std::iter::repeat_with(|| None)
        .take(expected_count)
        .collect::<Vec<_>>();

    for (index, submission) in completed_submissions {
        let slot = ordered.get_mut(index)?;
        if slot.replace(submission).is_some() {
            return None;
        }
    }

    ordered.into_iter().collect()
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

    let submission_count = reserved_submissions.len();
    let mut join_set = JoinSet::new();
    for (index, reserved_submission) in reserved_submissions.into_iter().enumerate() {
        let step = step.to_owned();
        join_set.spawn_blocking(move || {
            (
                index,
                finalize_reserved_user_wallet_submission(&step, reserved_submission),
            )
        });
    }

    let mut completed_signed_submissions = Vec::with_capacity(submission_count);
    let mut first_error = None;

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok((index, Ok(signed_submission))) => {
                completed_signed_submissions.push((index, signed_submission));
            }
            Ok((_, Err(error))) => {
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

    restore_preparation_order(submission_count, completed_signed_submissions).ok_or_else(|| {
        StepError::LogicalError {
            message: "Concurrent transaction finalization did not return each prepared \
                submission exactly once"
                .to_owned(),
        }
    })
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

async fn wait_for_first_fanout_success(
    attempts: Vec<tokio::task::JoinHandle<(String, Result<(), String>)>>,
) -> Result<String, String> {
    let mut pending = FuturesUnordered::new();
    pending.extend(attempts);
    let mut errors = Vec::new();

    while let Some(attempt) = pending.next().await {
        match attempt {
            Ok((node_name, Ok(()))) => {
                // Keep every request that was started in flight. The detached
                // JoinHandles detach on drop, so remaining node submissions
                // continue and complete naturally.
                return Ok(node_name);
            }
            Ok((_, Err(error))) => errors.push(error),
            Err(error) => errors.push(format!("fan-out task failed: {error}")),
        }
    }

    if errors.is_empty() {
        Err("no fan-out node attempts were started".to_owned())
    } else {
        Err(errors.join("; "))
    }
}

fn validate_submission_epoch(
    current_epoch: u64,
    fee_policy: Option<&TransactionFeePolicy>,
) -> Result<(), StepError> {
    let Some(policy) = fee_policy else {
        return Ok(());
    };
    let valid_through_epoch = u64::from(policy.horizon.valid_through_epoch.into_inner());
    if current_epoch > valid_through_epoch {
        return Err(StepError::FeeHorizonExceeded {
            current_epoch,
            prepared_at_epoch: policy.horizon.prepared_at_epoch.into_inner(),
            valid_through_epoch: policy.horizon.valid_through_epoch.into_inner(),
        });
    }
    Ok(())
}

/// Submit signed transactions to several nodes sharing the selected majority
/// tip.
///
/// Fanout makes manual/stress scenarios less sensitive to one slow node while
/// still avoiding nodes from a different fork group.
#[expect(
    clippy::cognitive_complexity,
    reason = "Bounded fan-out submission workflow"
)]
#[expect(
    clippy::too_many_lines,
    reason = "Keep the per-burst network lifecycle and timing boundary explicit"
)]
pub(crate) async fn submit_signed_user_wallet_submissions_to_nodes(
    world: &CucumberWorld,
    signed_submissions: Vec<SignedUserWalletSubmission>,
    fee_policy: Option<&TransactionFeePolicy>,
) -> Result<SignedUserWalletSubmissionNetworkResult, StepError> {
    if signed_submissions.is_empty() {
        return Ok(SignedUserWalletSubmissionNetworkResult {
            accepted: Vec::new(),
            first_error: None,
            failed_transaction_count: 0,
            fanout_node_names: Vec::new(),
            node_selection_duration: Duration::ZERO,
            preflight_duration: Duration::ZERO,
            network_submission_duration: Duration::ZERO,
        });
    }

    let node_selection_started = Instant::now();
    let same_tip_nodes = get_best_n_nodes_for_submissions(world, &signed_submissions, 3).await?;
    let node_selection_duration = node_selection_started.elapsed();
    let fanout_node_names = same_tip_nodes
        .iter()
        .map(|(node_name, _)| node_name.clone())
        .collect::<Vec<_>>();
    info!(target: TARGET, fanout_nodes = ?fanout_node_names, "Selected burst submission fan-out nodes");

    let preflight_started = Instant::now();
    validate_submission_fee_horizon(world, &same_tip_nodes[0].1, fee_policy).await?;
    let preflight_duration = preflight_started.elapsed();
    info!(target: TARGET, preflight_ms = preflight_duration.as_millis(), "Burst fee-horizon preflight completed");

    let network_submission_started = Instant::now();
    info!(target: TARGET, "Burst network submission started");
    let mut join_set = JoinSet::new();

    for signed_submission in signed_submissions {
        let wallet = signed_submission.wallet.clone();
        let same_tip_nodes = same_tip_nodes.clone();
        let same_tip_node_count = same_tip_nodes.len();

        join_set.spawn(async move {
            let tx_hash = signed_submission.tx_hash();
            let signed_tx = Arc::new(signed_submission.signed_tx().clone());
            let attempts = same_tip_nodes
                .into_iter()
                .map(|(node_name, node_client)| {
                    let node_client = node_client;
                    let signed_tx = Arc::clone(&signed_tx);
                    tokio::spawn(async move {
                        let result = timeout(
                            Duration::from_secs(15),
                            node_client.submit_transaction(signed_tx.as_ref()),
                        )
                        .await;
                        let result = match result {
                            Ok(Ok(())) => Ok(()),
                            Ok(Err(err)) => Err(format!("{node_name}: {err}")),
                            Err(_) => Err(format!("{node_name}: timeout")),
                        };
                        (node_name, result)
                    })
                })
                .collect::<Vec<_>>();

            match wait_for_first_fanout_success(attempts).await {
                Ok(accepted_node) => {
                    if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
                        info!(
                            target: TARGET,
                            "Transaction {} accepted by {accepted_node}",
                            hex::encode(tx_hash.0)
                        );
                    }
                    Ok::<_, StepError>(signed_submission)
                }
                Err(errors) => {
                    let message = format!(
                        "Transaction {tx_hash:?} for '{}' failed on all {} nodes: {errors}",
                        wallet.wallet_name, same_tip_node_count,
                    );
                    warn!(target: TARGET, "{message}");

                    Err(StepError::LogicalError { message })
                }
            }
        });
    }

    let mut accepted = Vec::new();
    let mut first_error = None;
    let mut failed_transaction_count = 0;

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(signed_submission)) => accepted.push(signed_submission),
            Ok(Err(error)) => {
                failed_transaction_count += 1;
                first_error.get_or_insert(error);
            }
            Err(error) => {
                failed_transaction_count += 1;
                first_error.get_or_insert_with(|| StepError::LogicalError {
                    message: format!("Concurrent transaction submission task failed: {error}"),
                });
            }
        }
    }

    Ok(SignedUserWalletSubmissionNetworkResult {
        accepted,
        first_error,
        failed_transaction_count,
        fanout_node_names,
        node_selection_duration,
        preflight_duration,
        network_submission_duration: network_submission_started.elapsed(),
    })
}

/// Submit a batch and preserve the legacy convenience behavior of recording
/// accepted submissions before returning any all-nodes failure.
pub(crate) async fn submit_signed_user_wallet_submissions_concurrently(
    world: &mut CucumberWorld,
    signed_submissions: Vec<SignedUserWalletSubmission>,
    fee_policy: Option<&TransactionFeePolicy>,
) -> Result<Vec<(String, TxHash)>, StepError> {
    let result =
        submit_signed_user_wallet_submissions_to_nodes(&*world, signed_submissions, fee_policy)
            .await?;
    let SignedUserWalletSubmissionNetworkResult {
        accepted,
        first_error,
        ..
    } = result;
    let submitted_hashes = record_accepted_signed_user_wallet_submissions(world, &accepted)?;
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(submitted_hashes)
}

/// Record accepted network submissions after any caller-specific diagnostic
/// boundary has been crossed.
pub(crate) fn record_accepted_signed_user_wallet_submissions(
    world: &mut CucumberWorld,
    accepted: &[SignedUserWalletSubmission],
) -> Result<Vec<(String, TxHash)>, StepError> {
    let mut tx_hashes = Vec::with_capacity(accepted.len());
    let mut submissions_by_wallet = BTreeMap::<String, Vec<&SignedUserWalletSubmission>>::new();
    for signed_submission in accepted {
        let wallet_name = signed_submission.wallet.wallet_name.clone();
        tx_hashes.push((wallet_name.clone(), signed_submission.tx_hash()));
        submissions_by_wallet
            .entry(wallet_name)
            .or_default()
            .push(signed_submission);
    }

    for (wallet_name, wallet_submissions) in submissions_by_wallet {
        let Some(representative) = wallet_submissions.first() else {
            continue;
        };
        let group_key = world
            .fork_groups
            .mapping()
            .get(&representative.wallet.node_name)
            .cloned()
            .unwrap_or_default();
        let fee_sponsor_inputs = world
            .with_wallets_mut(|wallets| {
                wallets.record_wallet_reservations(
                    wallet_name.clone(),
                    wallet_submissions.iter().map(|submission| {
                        (
                            submission.tx_hash(),
                            submission.reserved_inputs(),
                            submission.submission.spent_fee(),
                        )
                    }),
                )
            })
            .map_err(|error| StepError::StepFail {
                message: format!(
                    "Post-submission wallet bookkeeping failed after accepted transactions for \
                    wallet '{wallet_name}': {error}"
                ),
            })?;

        world.wallet_registry.fee_state.reserve_for_wallet(
            wallet_name,
            group_key,
            fee_sponsor_inputs,
        );
    }

    Ok(tx_hashes)
}

async fn validate_submission_fee_horizon(
    world: &CucumberWorld,
    client: &NodeHttpClient,
    fee_policy: Option<&TransactionFeePolicy>,
) -> Result<(), StepError> {
    let Some(policy) = fee_policy else {
        return Ok(());
    };
    let consensus = client
        .consensus_info()
        .await
        .map_err(|source| StepError::StepFail {
            message: format!("submission fee-horizon consensus query failed: {source}"),
        })?;
    let current_epoch =
        consensus.cryptarchia_info.slot.into_inner() / world.chain.slots_per_epoch.get();
    validate_submission_epoch(current_epoch, Some(policy))
}

/// Check whether a prepared wallet batch can still be submitted under the
/// cycle's fee horizon. This deliberately queries only the current majority
/// consensus tip.
pub(crate) async fn validate_fee_horizon_after_wallet_batch(
    world: &CucumberWorld,
    policy: &TransactionFeePolicy,
    wallet_name: &str,
    prepared_count: usize,
) -> Result<(), StepError> {
    let (_, _, consensus) = sanitize_best_node_info(world, wallet_name, None).await?;
    let current_epoch = consensus.slot.into_inner() / world.chain.slots_per_epoch.get();
    let valid_through_epoch = u64::from(policy.horizon.valid_through_epoch.into_inner());
    if current_epoch > valid_through_epoch {
        return Err(StepError::FeeHorizonExceededAfterWalletBatch {
            wallet_name: wallet_name.to_owned(),
            prepared_count,
            current_epoch,
            prepared_at_epoch: policy.horizon.prepared_at_epoch.into_inner(),
            valid_through_epoch: policy.horizon.valid_through_epoch.into_inner(),
        });
    }

    Ok(())
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
        .map(tx_hash_to_hex)
        .collect::<Vec<_>>()
        .join(", ");

    Ok(tx_hashes_hex)
}

/// Submit one transaction through a node-owned wallet, directing change to the
/// receiver.
///
/// Drain flows call this sequentially and wait for inclusion between calls so
/// the node wallet can observe each spent input.
pub async fn submit_node_wallet_transfer(
    world: &CucumberWorld,
    sender_wallet_name: &str,
    receiver_public_key: ZkPublicKey,
    amount: u64,
    change_public_key: ZkPublicKey,
) -> Result<TxHash, StepError> {
    let wallet = world.resolve_wallet(sender_wallet_name)?;
    if !wallet.is_node_wallet() {
        return Err(StepError::InvalidArgument {
            message: format!("Wallet `{sender_wallet_name}` must be a node wallet"),
        });
    }

    let body = WalletTransferFundsRequestBody {
        tip: None,
        change_public_key,
        funding_public_keys: vec![wallet.public_key()?],
        recipient_public_key: receiver_public_key,
        amount,
    };
    world.submit_funding_wallet_transaction(&wallet, body).await
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
                     tx_hash_to_hex(&tx_hash),
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
                         tx_hash_to_hex(&tx_hash),
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
    extra_op_proofs: OpProofs,
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
    extra_op_proofs: OpProofs,
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
        OpProofs::empty(),
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
    prepare_user_wallet_transaction_submission_with_change(
        world,
        step,
        sender_wallet_name,
        transaction_intent,
        in_memory_available_utxos,
        None,
    )
    .await
}

/// Prepare a user-wallet transaction with an explicit change recipient.
/// Prepare a user-wallet transaction with an explicit change recipient.
pub(crate) async fn prepare_user_wallet_transaction_submission_with_change(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    transaction_intent: WalletTransactionIntent,
    in_memory_available_utxos: Option<&WalletUtxos>,
    change_public_key: Option<ZkPublicKey>,
) -> Result<PreparedUserWalletSubmission, StepError> {
    prepare_user_wallet_transaction_submission_with_change_and_strategy(
        world,
        step,
        sender_wallet_name,
        transaction_intent,
        in_memory_available_utxos,
        change_public_key,
        WalletInputSelectionStrategy::LargestFirst,
    )
    .await
}

/// Prepare a user-wallet transaction with an explicit change recipient and
/// input-selection strategy.
pub(crate) async fn prepare_user_wallet_transaction_submission_with_change_and_strategy(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    transaction_intent: WalletTransactionIntent,
    in_memory_available_utxos: Option<&WalletUtxos>,
    change_public_key: Option<ZkPublicKey>,
    input_selection_strategy: WalletInputSelectionStrategy,
) -> Result<PreparedUserWalletSubmission, StepError> {
    let reserved = reserve_user_wallet_transaction_submission(
        world,
        step,
        sender_wallet_name,
        transaction_intent,
        in_memory_available_utxos,
        None,
        change_public_key,
        input_selection_strategy,
        None,
        0,
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

#[expect(clippy::too_many_arguments, reason = "Transaction reservation inputs")]
async fn reserve_user_wallet_transaction_submission(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    transaction_intent: WalletTransactionIntent,
    in_memory_available_utxos: Option<&WalletUtxos>,
    sender_candidate_utxos: Option<&[Utxo]>,
    change_public_key: Option<ZkPublicKey>,
    input_selection_strategy: WalletInputSelectionStrategy,
    gas_prices: Option<GasPrices>,
    priority_fee_percent: u64,
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

    let sender_available_utxos = if let Some(candidates) = sender_candidate_utxos {
        candidates.to_vec()
    } else {
        available_utxos
            .get(sender_wallet_name)
            .cloned()
            .ok_or(StepError::LogicalError {
                message: format!("Wallet '{sender_wallet_name}' not found in updated balances"),
            })?
    };

    let scenario_fee_funds =
        scenario_fee_account_state(world, sender_wallet_name, available_utxos)?;

    let funding_resources = user_wallet_funding_resources(
        wallet_account,
        &sender_available_utxos,
        scenario_fee_funds,
        change_public_key,
        input_selection_strategy,
    );

    let transaction_intent = if let Some(gas_prices) = gas_prices {
        transaction_intent.with_gas_prices(gas_prices)
    } else {
        transaction_intent
    };

    let submission = prepare_wallet_transaction_work_item(
        transaction_intent,
        funding_resources,
        priority_fee_percent,
    )
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
        WalletTransactionIntent::transfer(receivers).map_err(wallet_transaction_error)?,
        in_memory_available_utxos.as_deref(),
    )
    .await?;

    submit_prepared_user_wallet_transaction(
        world,
        step,
        prepared,
        OpProofs::empty(),
        best_node_info,
        in_memory_available_utxos,
    )
    .await
}

fn user_wallet_funding_resources(
    wallet_account: &WalletAccount,
    sender_available_utxos: &[Utxo],
    scenario_fee_funds: Option<WalletFundingSource>,
    change_public_key: Option<ZkPublicKey>,
    input_selection_strategy: WalletInputSelectionStrategy,
) -> WalletFundingResources {
    let sender = change_public_key.map_or_else(
        || {
            WalletFundingSource::with_change_pk_and_strategy(
                wallet_account.clone(),
                sender_available_utxos.to_vec(),
                wallet_account.public_key(),
                input_selection_strategy,
            )
        },
        |change_public_key| {
            WalletFundingSource::with_change_pk_and_strategy(
                wallet_account.clone(),
                sender_available_utxos.to_vec(),
                change_public_key,
                input_selection_strategy,
            )
        },
    );

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
        WalletTransactionError::OutputTotalOverflow | WalletTransactionError::FeeAccounting => {
            StepError::LogicalError {
                message: error.to_string(),
            }
        }
        error @ WalletTransactionError::InvalidLeadingInscriptionSigner => {
            StepError::LogicalError {
                message: error.to_string(),
            }
        }
        error @ (WalletTransactionError::MissingFundingInput { .. }
        | WalletTransactionError::MissingSigningKey { .. }) => StepError::LogicalError {
            message: error.to_string(),
        },
        WalletTransactionError::BoundedError(error) => StepError::BoundedError(error),
        WalletTransactionError::SignedOpsError(error) => StepError::LogicalError {
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
        .fork_groups
        .mapping()
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

    world.wallet_registry.fee_state.reserve_for_wallet(
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

#[cfg(test)]
fn apply_reserved_inputs_to_wallet_utxo_cache(
    cache: &mut WalletUtxos,
    sender_wallet_name: &str,
    selected_input_index: Option<usize>,
    reserved_inputs: WalletReservedInputs,
) {
    let (sender_inputs, fee_sponsor_inputs) = reserved_inputs.into_sender_and_fee_sponsor_inputs();

    if fee_sponsor_inputs.is_empty() {
        if let (Some(index), [reserved_input]) = (selected_input_index, sender_inputs.as_slice())
            && let Some(sender_utxos) = cache.get_mut(sender_wallet_name)
            && sender_utxos
                .get(index)
                .is_some_and(|cached_input| cached_input.id() == reserved_input.id())
        {
            sender_utxos.remove(index);
            return;
        }

        let spent_note_ids = sender_inputs.iter().map(Utxo::id).collect::<HashSet<_>>();
        if let Some(sender_utxos) = cache.get_mut(sender_wallet_name) {
            sender_utxos.retain(|utxo| !spent_note_ids.contains(&utxo.id()));
        }
        return;
    }

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
        .wallet_registry
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
        .fork_groups
        .mapping()
        .get(&wallet.node_name)
        .cloned()
        .unwrap_or_default())
}

#[cfg(test)]
mod cache_removal_tests {
    use lb_chain_service::Epoch;
    use lb_core::{header::HeaderId, mantle::Note};
    use rand::{SeedableRng as _, seq::SliceRandom as _};
    use rand_chacha::ChaCha8Rng;

    use super::*;
    use crate::common::wallet::TransactionFeeHorizon;

    fn utxo(value: u64, output_index: usize) -> Utxo {
        Utxo::new(
            [output_index as u8; 32],
            output_index,
            Note::new(value, ZkPublicKey::new(1u8.into())),
        )
    }

    #[test]
    fn workload_retry_recognizes_both_funds_deficit_errors() {
        assert!(is_user_wallet_funds_deficit(&StepError::WalletError(
            WalletError::InsufficientFunds { available: 1 }
        )));
        assert!(is_user_wallet_funds_deficit(&StepError::FundsDeficit {
            available: 1,
            num_utxos_required: 1,
            value_per_utxos_required: 1,
        }));
        assert!(!is_user_wallet_funds_deficit(&StepError::LogicalError {
            message: "not a funding failure".to_owned(),
        }));
    }

    #[tokio::test]
    async fn fanout_accepts_when_all_nodes_succeed() {
        let attempts = ["NODE_A", "NODE_B", "NODE_C"]
            .into_iter()
            .map(|node| {
                let node = node.to_owned();
                tokio::spawn(async move { (node, Ok(())) })
            })
            .collect();

        let accepted = wait_for_first_fanout_success(attempts)
            .await
            .expect("all successful fan-out attempts should accept the transaction");
        assert!(["NODE_A", "NODE_B", "NODE_C"].contains(&accepted.as_str()));
    }

    #[tokio::test]
    async fn fanout_accepts_with_one_success_and_two_failures() {
        let attempts = vec![
            tokio::spawn(async { ("NODE_A".to_owned(), Err("connection refused".to_owned())) }),
            tokio::spawn(async { ("NODE_B".to_owned(), Ok(())) }),
            tokio::spawn(async { ("NODE_C".to_owned(), Err("timeout".to_owned())) }),
        ];

        assert!(wait_for_first_fanout_success(attempts).await.is_ok());
    }

    #[tokio::test]
    async fn fanout_accepts_with_two_successes_and_one_failure() {
        let attempts = vec![
            tokio::spawn(async { ("NODE_A".to_owned(), Ok(())) }),
            tokio::spawn(async { ("NODE_B".to_owned(), Ok(())) }),
            tokio::spawn(async { ("NODE_C".to_owned(), Err("connection refused".to_owned())) }),
        ];

        assert!(wait_for_first_fanout_success(attempts).await.is_ok());
    }

    #[tokio::test]
    async fn fanout_all_failures_are_reported() {
        let attempts = vec![
            tokio::spawn(async { ("NODE_A".to_owned(), Err("connection refused".to_owned())) }),
            tokio::spawn(async { ("NODE_B".to_owned(), Err("timeout".to_owned())) }),
            tokio::spawn(async { ("NODE_C".to_owned(), Err("rejected".to_owned())) }),
        ];

        let error = wait_for_first_fanout_success(attempts)
            .await
            .expect_err("all failed fan-out attempts should fail");
        assert!(error.contains("connection refused"));
        assert!(error.contains("timeout"));
        assert!(error.contains("rejected"));
    }

    #[tokio::test]
    async fn fanout_returns_on_first_success_and_detaches_remaining_attempts() {
        let (slow_success_release, slow_success_wait) = tokio::sync::oneshot::channel();
        let (slow_success_done, slow_success_done_wait) = tokio::sync::oneshot::channel();
        let (slow_failure_release, slow_failure_wait) = tokio::sync::oneshot::channel();
        let (slow_failure_done, slow_failure_done_wait) = tokio::sync::oneshot::channel();

        let attempts = vec![
            tokio::spawn(async { ("NODE_FAST".to_owned(), Ok(())) }),
            tokio::spawn(async move {
                let _ = slow_success_wait.await;
                let _ = slow_success_done.send(());
                ("NODE_SLOW_SUCCESS".to_owned(), Ok(()))
            }),
            tokio::spawn(async move {
                let _ = slow_failure_wait.await;
                let _ = slow_failure_done.send(());
                ("NODE_SLOW_FAILURE".to_owned(), Err("late error".to_owned()))
            }),
        ];

        assert_eq!(
            wait_for_first_fanout_success(attempts)
                .await
                .expect("fast node should accept"),
            "NODE_FAST"
        );

        slow_success_release
            .send(())
            .expect("slow request detached");
        slow_failure_release
            .send(())
            .expect("slow request detached");
        timeout(Duration::from_secs(1), slow_success_done_wait)
            .await
            .expect("slow successful request should complete")
            .expect("slow successful request should send completion");
        timeout(Duration::from_secs(1), slow_failure_done_wait)
            .await
            .expect("slow failing request should complete")
            .expect("slow failing request should send completion");
    }

    #[tokio::test]
    async fn fanout_fast_failure_then_success_does_not_wait_for_slow_timeout() {
        let (medium_release, medium_wait) = tokio::sync::oneshot::channel();
        let (slow_release, slow_wait) = tokio::sync::oneshot::channel();
        let (slow_done, slow_done_wait) = tokio::sync::oneshot::channel();

        let fast =
            tokio::spawn(async { ("NODE_FAST_FAILURE".to_owned(), Err("rejected".to_owned())) });
        let (failure_seen_tx, failure_seen_rx) = tokio::sync::oneshot::channel();
        let fast = tokio::spawn(async move {
            let result = fast.await.expect("fast failure task should run");
            let _ = failure_seen_tx.send(());
            result
        });
        let medium = tokio::spawn(async move {
            let _ = medium_wait.await;
            ("NODE_MEDIUM_SUCCESS".to_owned(), Ok(()))
        });
        let slow = tokio::spawn(async move {
            let _ = slow_wait.await;
            let _ = slow_done.send(());
            ("NODE_SLOW_TIMEOUT".to_owned(), Err("timeout".to_owned()))
        });

        // Releasing the medium response after the fast failure future has had
        // a scheduling turn makes the expected acceptance order deterministic.
        tokio::spawn(async move {
            let _ = failure_seen_rx.await;
            let _ = medium_release.send(());
        });
        let attempts = vec![fast, medium, slow];

        assert_eq!(
            wait_for_first_fanout_success(attempts)
                .await
                .expect("medium node should accept after fast failure"),
            "NODE_MEDIUM_SUCCESS"
        );
        slow_release.send(()).expect("slow task should be detached");
        timeout(Duration::from_secs(1), slow_done_wait)
            .await
            .expect("slow fan-out task should be allowed to complete")
            .expect("slow task should signal completion");
    }

    fn fee_policy_through_epoch(valid_through_epoch: u32) -> TransactionFeePolicy {
        TransactionFeePolicy {
            horizon: TransactionFeeHorizon {
                prepared_at_tip: HeaderId::from([0; 32]),
                prepared_at_epoch: Epoch::new(2),
                valid_through_epoch: Epoch::new(valid_through_epoch),
                live_prices: GasPrices::default(),
                ceiling_prices: GasPrices::default(),
            },
            priority_fee_percent: 0,
        }
    }

    #[test]
    fn submission_fee_horizon_allows_current_epoch() {
        let policy = fee_policy_through_epoch(4);

        validate_submission_epoch(4, Some(&policy))
            .expect("submission should remain valid through the horizon");
    }

    #[test]
    fn submission_fee_horizon_rejects_late_current_epoch() {
        let policy = fee_policy_through_epoch(4);

        assert!(matches!(
            validate_submission_epoch(5, Some(&policy)),
            Err(StepError::FeeHorizonExceeded {
                current_epoch: 5,
                prepared_at_epoch: 2,
                valid_through_epoch: 4,
            })
        ));
    }

    #[test]
    fn removes_a_reserved_single_input_from_its_known_wallet_and_index() {
        let mut cache = HashMap::from([
            (
                WalletId::from("sender"),
                vec![utxo(10, 0), utxo(30, 1), utxo(20, 2)],
            ),
            (WalletId::from("other"), vec![utxo(40, 3), utxo(50, 4)]),
        ]);
        let reserved_input = cache["sender"][1];

        apply_reserved_inputs_to_wallet_utxo_cache(
            &mut cache,
            "sender",
            Some(1),
            WalletReservedInputs::new(vec![reserved_input], Vec::new()),
        );

        assert_eq!(
            cache["sender"]
                .iter()
                .map(|utxo| utxo.note.value)
                .collect::<Vec<_>>(),
            vec![10, 20]
        );
        assert_eq!(
            cache["other"]
                .iter()
                .map(|utxo| utxo.note.value)
                .collect::<Vec<_>>(),
            vec![40, 50]
        );
    }

    fn workload_account() -> WalletAccount {
        WalletAccount::deterministic(100, 1_000_000, false)
            .expect("workload test account should build")
    }

    fn workload_utxo(value: u64, output_index: usize) -> Utxo {
        let account = workload_account();
        Utxo::new(
            [output_index as u8; 32],
            output_index,
            Note::new(value, account.public_key()),
        )
    }

    fn prepare_all_provided(
        utxos: &[Utxo],
        output_value: u64,
        gas_prices: GasPrices,
        priority_fee_percent: u64,
    ) -> Result<PreparedWalletTransactionWorkItem, WalletTransactionError> {
        let account = workload_account();
        let public_key = account.public_key();
        let source = WalletFundingSource::with_change_pk_and_strategy(
            account,
            utxos.to_vec(),
            public_key,
            WalletInputSelectionStrategy::AllProvided,
        );
        let intent = WalletTransactionIntent::transfer(&[(ZkPublicKey::zero(), output_value)])?
            .with_gas_prices(gas_prices);
        prepare_wallet_transaction_work_item(
            intent,
            WalletFundingResources::new(source),
            priority_fee_percent,
        )
    }

    fn prepare_fee_sponsored_all_provided(
        sender_utxos: &[Utxo],
        fee_sponsor_utxos: &[Utxo],
        output_value: u64,
    ) -> Result<PreparedWalletTransactionWorkItem, WalletTransactionError> {
        let sender_account = workload_account();
        let sender_public_key = sender_account.public_key();
        let sender = WalletFundingSource::with_change_pk_and_strategy(
            sender_account,
            sender_utxos.to_vec(),
            sender_public_key,
            WalletInputSelectionStrategy::AllProvided,
        );
        let fee_sponsor_account = WalletAccount::deterministic(101, 1_000_000, false)
            .expect("fee sponsor test account should build");
        let fee_sponsor = WalletFundingSource::new(fee_sponsor_account, fee_sponsor_utxos.to_vec());
        let intent = WalletTransactionIntent::transfer(&[(ZkPublicKey::zero(), output_value)])?
            .with_gas_prices(GasPrices::default());

        prepare_wallet_transaction_work_item(
            intent,
            WalletFundingResources::fee_sponsored(sender, fee_sponsor),
            0,
        )
    }

    fn input_values(work_item: &PreparedWalletTransactionWorkItem) -> Vec<u64> {
        let (sender_inputs, fee_sponsor_inputs) = work_item
            .reserved_inputs()
            .into_sender_and_fee_sponsor_inputs();
        assert_eq!(fee_sponsor_inputs, Vec::new());
        sender_inputs
            .into_iter()
            .map(|utxo| utxo.note.value)
            .collect()
    }

    #[test]
    fn finalization_completion_order_is_restored_before_dependent_shuffle() {
        let preparation_order = vec![10, 20, 30, 40, 50, 60];
        let first_completion_order = vec![(3, 40), (0, 10), (5, 60), (2, 30), (1, 20), (4, 50)];
        let second_completion_order = vec![(4, 50), (5, 60), (1, 20), (0, 10), (3, 40), (2, 30)];

        let mut first = restore_preparation_order(6, first_completion_order)
            .expect("all first results should be restored by preparation index");
        let mut second = restore_preparation_order(6, second_completion_order)
            .expect("all second results should be restored by preparation index");
        assert_eq!(first, preparation_order);
        assert_eq!(second, preparation_order);

        first.shuffle(&mut ChaCha8Rng::seed_from_u64(42));
        second.shuffle(&mut ChaCha8Rng::seed_from_u64(42));
        assert_eq!(first, second);
    }

    #[test]
    fn workload_selection_uses_only_the_primary_when_no_dust_qualifies() {
        let pool = WorkloadUtxoPool::new(&[
            workload_utxo(10_000, 0),
            workload_utxo(9_000, 1),
            workload_utxo(8_000, 2),
        ]);
        let candidates = pool.candidates(8_000, 0).expect("primary should exist");
        let inputs = candidates.inputs(0);

        assert_eq!(
            inputs
                .iter()
                .map(|utxo| utxo.note.value)
                .collect::<Vec<_>>(),
            vec![10_000]
        );
        let work_item = prepare_all_provided(&inputs, 8_000, GasPrices::default(), 0)
            .expect("primary should fund the transfer");
        assert_eq!(input_values(&work_item), vec![10_000]);
    }

    #[test]
    fn workload_selection_prefers_one_sufficient_large_primary() {
        let pool = WorkloadUtxoPool::new(&[
            workload_utxo(20_000, 0),
            workload_utxo(8_000, 1),
            workload_utxo(8_000, 2),
            workload_utxo(1, 3),
            workload_utxo(1, 4),
        ]);
        let primary_candidates = pool.primary_candidates();
        let selected_primary_count = (1..=primary_candidates.len())
            .find(|count| {
                prepare_all_provided(
                    &primary_candidates[..*count],
                    8_000,
                    GasPrices::default(),
                    0,
                )
                .is_ok()
            })
            .expect("large primary should fund the transaction");
        let base_fee = 0;
        let candidates = pool.candidates_for_primary(
            &primary_candidates[..selected_primary_count],
            8_000,
            base_fee,
        );

        assert_eq!(selected_primary_count, 1);
        assert_eq!(candidates.primary_inputs[0].note.value, 20_000);
        assert!(!candidates.dust.iter().any(|utxo| utxo.note.value == 8_000));
    }

    #[test]
    fn workload_selection_uses_two_or_three_primary_inputs_only_when_needed() {
        let two_input_utxos = [workload_utxo(8_000, 0), workload_utxo(8_000, 1)];
        let two_input_pool = WorkloadUtxoPool::new(&two_input_utxos);
        let two_input_candidates = two_input_pool.primary_candidates();
        let two_input_count = (1..=two_input_candidates.len())
            .find(|count| {
                prepare_all_provided(
                    &two_input_candidates[..*count],
                    13_000,
                    GasPrices::default(),
                    0,
                )
                .is_ok()
            })
            .expect("two 8,000 inputs should fund the transfer");
        assert_eq!(two_input_count, 2);

        let three_input_utxos = [
            workload_utxo(8_000, 10),
            workload_utxo(8_000, 11),
            workload_utxo(8_000, 12),
        ];
        let three_input_pool = WorkloadUtxoPool::new(&three_input_utxos);
        let three_input_candidates = three_input_pool.primary_candidates();
        let two_input_max_output = {
            let mut low = 0u64;
            let mut high = 16_000u64;
            while low < high {
                let output_value = low + (high - low).div_ceil(2);
                if prepare_all_provided(
                    &three_input_candidates[..2],
                    output_value,
                    GasPrices::default(),
                    0,
                )
                .is_ok()
                {
                    low = output_value;
                } else {
                    high = output_value - 1;
                }
            }
            low
        };
        let output_value = two_input_max_output + 1;
        assert!(
            prepare_all_provided(
                &three_input_candidates[..2],
                output_value,
                GasPrices::default(),
                0,
            )
            .is_err()
        );
        assert!(
            prepare_all_provided(
                &three_input_candidates[..3],
                output_value,
                GasPrices::default(),
                0,
            )
            .is_ok()
        );
        let three_input_count = (1..=three_input_candidates.len())
            .find(|count| {
                prepare_all_provided(
                    &three_input_candidates[..*count],
                    output_value,
                    GasPrices::default(),
                    0,
                )
                .is_ok()
            })
            .expect("three 8,000 inputs should fund the transfer");
        assert_eq!(three_input_count, 3);
    }

    #[test]
    fn workload_primary_inputs_remain_eligible_below_the_dust_threshold() {
        let utxos = [
            workload_utxo(8_000, 0),
            workload_utxo(8_000, 1),
            workload_utxo(8_000, 2),
            workload_utxo(1, 3),
        ];
        let pool = WorkloadUtxoPool::new(&utxos);
        let primary_candidates = pool.primary_candidates();
        let candidates = pool.candidates_for_primary(&primary_candidates[..3], 21_000, 9_000);

        assert_eq!(candidates.dust_threshold, 9_000);
        assert_eq!(candidates.primary_inputs.len(), 3);
        assert!(
            candidates
                .primary_inputs
                .iter()
                .all(|utxo| utxo.note.value == 8_000)
        );
        assert!(candidates.dust.iter().all(|utxo| utxo.note.value == 1));
    }

    #[test]
    fn workload_fee_diagnostic_exposes_insufficient_headroom_for_11000_primary() {
        let primary = workload_utxo(11_000, 0);
        let intent = WalletTransactionIntent::transfer(&[(ZkPublicKey::zero(), 8_000)])
            .expect("round-robin transfer intent")
            .with_gas_prices(GasPrices::new(1, 3));

        let (fee_without_change, fee_with_change) =
            estimate_workload_fee_requirements(&intent, &[primary], 200)
                .expect("workload fees should be estimable");

        assert!(fee_without_change > 3_000);
        assert!(fee_with_change >= fee_without_change);
    }

    #[test]
    fn workload_selection_adds_smallest_dust_and_excludes_medium_inputs() {
        let pool = WorkloadUtxoPool::new(&[
            workload_utxo(10_000, 0),
            workload_utxo(9_000, 1),
            workload_utxo(500, 2),
            workload_utxo(100, 3),
            workload_utxo(10, 4),
            workload_utxo(1, 5),
        ]);
        let candidates = pool.candidates(8_000, 0).expect("primary should exist");
        let inputs = candidates.inputs(candidates.dust.len());

        assert_eq!(
            inputs
                .iter()
                .map(|utxo| utxo.note.value)
                .collect::<Vec<_>>(),
            vec![10_000, 1, 10, 100, 500]
        );
        let work_item = prepare_all_provided(&inputs, 8_000, GasPrices::default(), 0)
            .expect("bounded primary and dust set should fund the transfer");
        assert_eq!(input_values(&work_item), vec![10_000, 1, 10, 100, 500]);
    }

    #[test]
    fn workload_selection_caps_dust_inputs_at_ten() {
        let mut utxos = vec![workload_utxo(10_000, 0)];
        utxos.extend((1..=20).map(|index| workload_utxo(index as u64, index)));
        let pool = WorkloadUtxoPool::new(&utxos);
        let candidates = pool.candidates(8_000, 0).expect("primary should exist");

        assert_eq!(candidates.dust.len(), MAX_CONTINUOUS_WORKLOAD_DUST_INPUTS);
        assert_eq!(candidates.inputs(candidates.dust.len()).len(), 11);
        assert_eq!(candidates.dust_value(candidates.dust.len()), 55);
    }

    #[test]
    fn workload_primary_prefix_is_bounded_by_transaction_input_limit() {
        let utxos = (0..300)
            .map(|index| workload_utxo(1, index))
            .collect::<Vec<_>>();
        let pool = WorkloadUtxoPool::new(&utxos);
        let primary_candidates = pool.primary_candidates();

        assert_eq!(primary_candidates.len(), MAX_TRANSACTION_INPUTS);
        assert_eq!(pool.len(), 300);
        assert!(
            prepare_all_provided(&primary_candidates, 1_000, GasPrices::default(), 0,).is_err()
        );
    }

    #[test]
    fn workload_selection_measures_dust_against_transfer_value() {
        let mut utxos = vec![
            workload_utxo(1_200_000, 0),
            workload_utxo(1_200_000, 1),
            workload_utxo(200_000, 2),
            workload_utxo(1_000, 3),
            workload_utxo(500, 4),
            workload_utxo(1, 5),
        ];
        utxos.extend((0..50).map(|index| workload_utxo(20_000, index + 6)));
        let pool = WorkloadUtxoPool::new(&utxos);
        let candidates = pool.candidates(8_000, 0).expect("primary should exist");

        assert_eq!(candidates.primary_inputs[0].note.value, 1_200_000);
        assert_eq!(
            candidates.dust_threshold,
            8_000 / CONTINUOUS_WORKLOAD_DUST_RATIO
        );
        assert_eq!(
            candidates
                .dust
                .iter()
                .map(|utxo| utxo.note.value)
                .collect::<Vec<_>>(),
            vec![1, 500]
        );
        assert_eq!(candidates.inputs(candidates.dust.len()).len(), 3);
        assert_eq!(pool.len(), utxos.len());
    }

    #[test]
    fn workload_dust_threshold_uses_the_larger_transfer_fraction_or_base_fee() {
        assert_eq!(continuous_workload_dust_threshold(8_000, 700), 700);
        assert_eq!(continuous_workload_dust_threshold(15_000, 700), 1_000);
    }

    #[test]
    fn sponsored_workload_does_not_charge_sender_dust_threshold_for_fees() {
        assert_eq!(continuous_workload_sender_fee_requirement(700, true), 0);
        assert_eq!(continuous_workload_sender_fee_requirement(700, false), 700);
    }

    #[test]
    fn workload_selection_drops_dust_until_actual_funding_succeeds() {
        let primary = workload_utxo(1_000_000_000, 0);
        let dust = (1..=10)
            .map(|index| workload_utxo(1, index))
            .collect::<Vec<_>>();
        let mut all_utxos = vec![primary];
        all_utxos.extend_from_slice(&dust);
        let pool = WorkloadUtxoPool::new(&all_utxos);

        let gas_prices = GasPrices::new(1, 1);
        let mut low = 0;
        let mut high = primary.note.value;
        while low < high {
            let output_value = low + (high - low).div_ceil(2);
            if prepare_all_provided(&[primary], output_value, gas_prices.clone(), 200).is_ok() {
                low = output_value;
            } else {
                high = output_value - 1;
            }
        }
        assert!(low > 0, "primary-only transfer should be fundable");
        let candidates = pool.candidates(low, 0).expect("primary should exist");
        assert!(
            prepare_all_provided(
                &candidates.inputs(candidates.dust.len()),
                low,
                gas_prices.clone(),
                200,
            )
            .is_err(),
            "all dust should exceed the fee headroom at the primary-only limit"
        );

        let mut successful_dust_count = None;
        let mut reserved_inputs = None;
        for dust_count in (0..=candidates.dust.len()).rev() {
            let inputs = candidates.inputs(dust_count);
            if let Ok(work_item) = prepare_all_provided(&inputs, low, gas_prices.clone(), 200) {
                successful_dust_count = Some(dust_count);
                reserved_inputs = Some(work_item.reserved_inputs());
                break;
            }
        }

        let successful_dust_count =
            successful_dust_count.expect("primary-only attempt should eventually succeed");
        assert!(successful_dust_count < candidates.dust.len());
        let dropped_dust_ids = candidates.dust[successful_dust_count..]
            .iter()
            .map(Utxo::id)
            .collect::<HashSet<_>>();

        let mut cache = HashMap::from([(WalletId::from("sender"), all_utxos)]);
        let mut pools = WorkloadUtxoPools::from_cache(&cache);
        pools
            .remove_reserved_inputs(
                &mut cache,
                reserved_inputs.expect("successful work item should have reservations"),
            )
            .expect("reserved inputs should be removed from indexed cache");

        let remaining_ids = cache["sender"].iter().map(Utxo::id).collect::<HashSet<_>>();
        assert!(dropped_dust_ids.is_subset(&remaining_ids));
        assert_eq!(pools.candidate_count("sender"), remaining_ids.len());
    }

    #[test]
    fn workload_selection_fails_after_bounded_candidates_when_primary_is_insufficient() {
        let utxos = [
            workload_utxo(10_000, 0),
            workload_utxo(9_000, 1),
            workload_utxo(100, 2),
            workload_utxo(10, 3),
            workload_utxo(1, 4),
        ];
        let pool = WorkloadUtxoPool::new(&utxos);
        let candidates = pool.candidates(15_000, 0).expect("primary should exist");
        let mut attempts = 0;

        for dust_count in (0..=candidates.dust.len()).rev() {
            attempts += 1;
            assert!(
                prepare_all_provided(
                    &candidates.inputs(dust_count),
                    15_000,
                    GasPrices::default(),
                    0,
                )
                .is_err()
            );
        }

        assert_eq!(attempts, candidates.dust.len() + 1);
        assert_eq!(
            pool.candidates(15_000, 0)
                .expect("failed attempts do not mutate the pool")
                .inputs(candidates.dust.len())
                .iter()
                .map(|utxo| utxo.note.value)
                .collect::<Vec<_>>(),
            vec![10_000, 1, 10, 100]
        );
        assert!(pool.value_by_note_id.contains_key(&utxos[1].id()));
    }

    #[test]
    fn repeated_workload_reservations_use_unique_primary_and_dust_inputs() {
        let mut utxos = vec![
            workload_utxo(400_000, 0),
            workload_utxo(400_000, 1),
            workload_utxo(400_000, 2),
        ];
        utxos.extend((1..=15).map(|index| workload_utxo(1, index + 2)));
        let mut cache = HashMap::from([(WalletId::from("sender"), utxos)]);
        let mut pools = WorkloadUtxoPools::from_cache(&cache);
        let mut reserved_ids = HashSet::new();

        for expected_primary in [400_000, 400_000] {
            let primary_candidates = pools.primary_candidates("sender");
            let candidates = pools
                .candidates("sender", &primary_candidates[..1], 8_000, 0)
                .expect("primary should exist");
            let inputs = candidates.inputs(candidates.dust.len());
            assert_eq!(inputs[0].note.value, expected_primary);
            let work_item = prepare_all_provided(&inputs, 8_000, GasPrices::default(), 0)
                .expect("large primary should fund the workload transaction");
            let reserved = work_item.reserved_inputs();
            let (sender_inputs, fee_sponsor_inputs) = reserved.into_sender_and_fee_sponsor_inputs();
            assert_eq!(fee_sponsor_inputs, Vec::new());
            for input in &sender_inputs {
                assert!(reserved_ids.insert(input.id()), "workload input was reused");
            }
            pools
                .remove_reserved_inputs(
                    &mut cache,
                    WalletReservedInputs::new(sender_inputs, Vec::new()),
                )
                .expect("reserved inputs should be removed incrementally");
        }

        assert_eq!(pools.candidate_count("sender"), cache["sender"].len());
        let next_primary_candidates = pools.primary_candidates("sender");
        let next_candidates = pools
            .candidates("sender", &next_primary_candidates[..1], 8_000, 0)
            .expect("next primary remains");
        assert_eq!(next_candidates.primary_inputs[0].note.value, 400_000);
        assert_eq!(next_candidates.dust.len(), 0);
    }

    #[test]
    fn shared_workload_pools_keep_indexes_valid_across_wallet_batches() {
        let mut cache = HashMap::from([
            (
                WalletId::from("sender_a"),
                vec![workload_utxo(10_000, 0), workload_utxo(100, 1)],
            ),
            (
                WalletId::from("sender_b"),
                vec![workload_utxo(20_000, 2), workload_utxo(200, 3)],
            ),
        ]);
        let mut pools = WorkloadUtxoPools::from_cache(&cache);
        let sender_a_primary = pools.primary_candidates("sender_a")[0];
        pools
            .remove_reserved_inputs(
                &mut cache,
                WalletReservedInputs::new(vec![sender_a_primary], Vec::new()),
            )
            .expect("sender A input should be removed through the shared index");

        let sender_b_primary_candidates = pools.primary_candidates("sender_b");
        assert_eq!(sender_b_primary_candidates[0].note.value, 20_000);
        assert_eq!(pools.candidate_count("sender_a"), cache["sender_a"].len());
        assert_eq!(pools.candidate_count("sender_b"), cache["sender_b"].len());

        pools
            .remove_reserved_inputs(
                &mut cache,
                WalletReservedInputs::new(vec![sender_b_primary_candidates[0]], Vec::new()),
            )
            .expect("sender B input should be removed through the same shared index");
        assert_eq!(pools.candidate_count("sender_b"), cache["sender_b"].len());
        assert_eq!(pools.primary_candidates("sender_b")[0].note.value, 200);
    }

    #[test]
    fn non_sponsored_one_lgo_workload_uses_base_fee_as_the_dust_threshold() {
        let mut utxos = vec![workload_utxo(430_000, 0), workload_utxo(410_000, 1)];
        utxos.extend((1..=12).map(|index| workload_utxo(1, index + 1)));
        let pool = WorkloadUtxoPool::new(&utxos);

        let primary = pool.primary().expect("primary should exist");
        let intent = WalletTransactionIntent::transfer(&[(ZkPublicKey::zero(), 1)])
            .expect("one-LGO workload transfer intent")
            .with_gas_prices(GasPrices::default());
        let (_, base_tx_fee) = estimate_workload_fee_requirements(&intent, &[primary], 0)
            .expect("base transaction fee should be estimable");
        let primary_candidates = pool.primary_candidates();
        let candidates = pool.candidates_for_primary(&primary_candidates[..1], 1, base_tx_fee);
        let inputs = candidates.inputs(candidates.dust.len());

        assert_eq!(inputs[0].note.value, 430_000);
        assert!(base_tx_fee >= 1);
        assert_eq!(candidates.dust_threshold, base_tx_fee);
        assert_eq!(candidates.dust.len(), MAX_CONTINUOUS_WORKLOAD_DUST_INPUTS);
        assert!(candidates.dust.iter().all(|utxo| utxo.note.value == 1));
        let work_item = prepare_all_provided(&inputs, 1, GasPrices::default(), 0)
            .expect("large primary plus bounded dust should fund the transfer");
        assert_eq!(
            input_values(&work_item).len(),
            MAX_CONTINUOUS_WORKLOAD_DUST_INPUTS + 1
        );
        assert_eq!(input_values(&work_item)[0], 430_000);
        assert_eq!(pool.len(), utxos.len());
    }

    #[test]
    fn sponsored_one_lgo_batch_preserves_each_output_for_a_transfer() {
        let utxos = (0..11)
            .map(|index| workload_utxo(1, index))
            .collect::<Vec<_>>();
        let mut pool = WorkloadUtxoPool::new(&utxos);
        let intent = WalletTransactionIntent::transfer(&[(ZkPublicKey::zero(), 1)])
            .expect("one-LGO workload transfer intent")
            .with_gas_prices(GasPrices::default());
        let largest = pool.primary().expect("a sender output should be available");
        let (_, base_tx_fee) = estimate_workload_fee_requirements(&intent, &[largest], 0)
            .expect("base transaction fee should be estimable");
        assert!(base_tx_fee >= 1, "test must exercise fee-sized dust");
        let sender_fee_requirement = continuous_workload_sender_fee_requirement(base_tx_fee, true);
        assert_eq!(sender_fee_requirement, 0);

        let fee_sponsor_account = WalletAccount::deterministic(101, 1_000_000, false)
            .expect("fee sponsor test account should build");
        let mut remaining_sponsor_utxos = (0..11).map(|index| {
            Utxo::new(
                [0xFE; 32],
                index,
                Note::new(10_000_000, fee_sponsor_account.public_key()),
            )
        });

        for transaction_index in 0..11 {
            let primary = pool
                .primary()
                .expect("each transfer needs one sender input");
            let candidates = pool.candidates_for_primary(&[primary], 1, sender_fee_requirement);
            assert_eq!(candidates.dust_threshold, 0);
            assert_eq!(candidates.dust.len(), 0);
            let selected_inputs = candidates.inputs(candidates.dust.len());
            assert_eq!(selected_inputs.len(), 1);

            let fee_sponsor_utxo = remaining_sponsor_utxos
                .next()
                .expect("each transaction has a unique fee sponsor input");
            let work_item =
                prepare_fee_sponsored_all_provided(&selected_inputs, &[fee_sponsor_utxo], 1)
                    .expect("one sender LGO plus sponsored fees should fund one transfer");
            let (sender_inputs, fee_sponsor_inputs) = work_item
                .reserved_inputs()
                .into_sender_and_fee_sponsor_inputs();
            assert_eq!(sender_inputs.len(), 1, "transaction {transaction_index}");
            assert_eq!(sender_inputs[0].note.value, 1);
            assert_ne!(fee_sponsor_inputs.len(), 0);
            pool.remove(sender_inputs[0].id());
        }

        assert_eq!(pool.len(), 0, "all eleven sender outputs funded a transfer");
    }
}
