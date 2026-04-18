use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Display,
    num::NonZero,
    str::FromStr,
    time::Duration,
};

use hex::ToHex as _;
use lb_core::{
    codec::SerializeOp as _,
    mantle::{
        AuthenticatedMantleTx as _, Note, NoteId, OpProof, SignedMantleTx, Transaction as _,
        TxHash, Utxo,
        gas::MainnetGasConstants,
        tx::{MantleTxContext, MantleTxGasContext},
        tx_builder::MantleTxBuilder,
    },
};
use lb_http_api_common::bodies::wallet::transfer_funds::WalletTransferFundsRequestBody;
use lb_key_management_system_service::keys::{ZkKey, ZkPublicKey};
use lb_testing_framework::{NodeHttpClient, configs::wallet::WalletAccount, is_truthy_env};
use lb_wallet::WalletError;
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

pub(crate) use crate::cucumber::steps::manual_transactions::best_node::BestNodeInfo;
use crate::cucumber::{
    defaults::CUCUMBER_VERBOSE_CONSOLE,
    error::{StepError, StepResult},
    fee_reserve::{DEFAULT_STORAGE_GAS_PRICE, SCENARIO_FEE_ACCOUNT_NAME},
    steps::{
        TARGET,
        manual_transactions::{best_node::sanitize_best_node_info, faucet::FaucetTask},
    },
    world::{CucumberWorld, WalletInfo, WalletTokenMap, WalletType},
};

/// Specifies which subset of wallet UTXOs to consider when checking for
/// expected wallet state.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WalletStateType {
    /// All UTXOs that are on-chain, regardless of whether they are encumbered
    /// or not.
    OnChain,
    /// UTXOs that are currently encumbered by pending transactions.
    Encumbered,
    /// UTXOs that are not encumbered and are available for new transactions.
    Available,
}

impl Display for WalletStateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OnChain => write!(f, "on-chain"),
            Self::Encumbered => write!(f, "encumbered"),
            Self::Available => write!(f, "available"),
        }
    }
}

impl FromStr for WalletStateType {
    type Err = StepError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "on-chain" => Ok(Self::OnChain),
            "encumbered" => Ok(Self::Encumbered),
            "available" => Ok(Self::Available),
            _ => Err(StepError::InvalidArgument {
                message: format!("Unknown WalletStateType: '{s}'"),
            }),
        }
    }
}

struct PreparedUserWalletTransaction {
    funded_builder: MantleTxBuilder,
    newly_encumbered: Vec<Utxo>,
    newly_encumbered_fee: Vec<Utxo>,
    signing_keys: Vec<ZkKey>,
}

pub async fn create_and_submit_transaction(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    receivers: &[(ZkPublicKey, u64)],
    best_node_info: Option<&BestNodeInfo>,
) -> Result<String, StepError> {
    let wallet = world.resolve_wallet(sender_wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    let tx_hashes = match wallet.wallet_type {
        WalletType::User { ref wallet_account } => vec![
            submit_user_wallet_transaction(
                world,
                step,
                sender_wallet_name,
                &wallet,
                wallet_account,
                receivers,
                best_node_info,
            )
            .await?,
        ],
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

    let tx_hashes_hex: String = tx_hashes
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

async fn submit_user_wallet_transaction(
    world: &mut CucumberWorld,
    step: &str,
    sender_wallet_name: &str,
    wallet: &WalletInfo,
    wallet_account: &WalletAccount,
    receivers: &[(ZkPublicKey, u64)],
    best_node_info: Option<&BestNodeInfo>,
) -> Result<TxHash, StepError> {
    let available_utxos =
        update_wallet_balance_all_user_wallets(world, step, best_node_info).await?;
    let sender_available_utxos =
        available_utxos
            .get(sender_wallet_name)
            .cloned()
            .ok_or(StepError::LogicalError {
                message: format!("Wallet '{sender_wallet_name}' not found in updated balances"),
            })?;
    let scenario_fee_state =
        scenario_fee_account_state(world, sender_wallet_name, &available_utxos)?;

    let PreparedUserWalletTransaction {
        funded_builder,
        newly_encumbered,
        newly_encumbered_fee,
        signing_keys,
    } = prepare_user_wallet_transaction(
        step,
        receivers,
        sender_available_utxos,
        scenario_fee_state,
        wallet_account,
    )?;

    let mantle_tx = funded_builder.build();
    let tx_hash = mantle_tx.hash();
    let transfer_proof = ZkKey::multi_sign(&signing_keys, tx_hash.as_ref()).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    let signed_tx = SignedMantleTx::new(mantle_tx, vec![OpProof::ZkSig(transfer_proof)])
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

    let (_, best_node_client, _) =
        sanitize_best_node_info(world, &wallet.wallet_name, best_node_info).await?;
    world
        .submit_transaction(wallet, &signed_tx, best_node_client)
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

    world
        .wallet_runtime_state
        .entry(sender_wallet_name.to_owned())
        .or_default()
        .encumbered_tokens
        .extend(newly_encumbered);

    world
        .fee_state
        .encumbered_tokens_per_wallet
        .entry(sender_wallet_name.to_owned())
        .or_default()
        .extend(newly_encumbered_fee);

    world.record_submitted_tx_hash(sender_wallet_name, tx_hash);
    world.record_tracked_spent_fee(
        sender_wallet_name,
        signed_tx
            .total_gas_cost::<MainnetGasConstants>()
            .map_err(|e| StepError::LogicalError {
                message: format!("Step `{step}` error: failed to compute gas cost: {e}"),
            })
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{}` error: {e}", step);
            })?
            .into_inner(),
    );
    Ok(tx_hash)
}

fn prepare_user_wallet_transaction(
    step: &str,
    receivers: &[(ZkPublicKey, u64)],
    sender_utxos: Vec<Utxo>,
    scenario_fee_state: Option<(WalletAccount, Vec<Utxo>)>,
    wallet_account: &WalletAccount,
) -> Result<PreparedUserWalletTransaction, StepError> {
    let sender_pk = wallet_account.public_key();
    let (funded_builder_result, fee_pk, fee_signing_key) = match scenario_fee_state {
        Some((fee_wallet_account, fee_utxos)) => (
            build_sponsored_user_wallet_transaction(
                receivers,
                sender_utxos,
                fee_utxos,
                wallet_account,
                &fee_wallet_account,
            ),
            Some(fee_wallet_account.public_key()),
            Some(fee_wallet_account.secret_key.clone()),
        ),
        None => (
            build_unsponsored_user_wallet_transaction(receivers, sender_utxos, wallet_account),
            None,
            None,
        ),
    };

    let funded_builder = funded_builder_result.inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    let (newly_encumbered, newly_encumbered_fee) = partition_transaction_inputs(
        funded_builder.ledger_inputs(),
        sender_pk,
        fee_pk.unwrap_or(sender_pk),
    );

    let mut signing_keys = vec![wallet_account.secret_key.clone()];
    if !newly_encumbered_fee.is_empty()
        && let Some(fee_signing_key) = fee_signing_key
    {
        signing_keys.push(fee_signing_key);
    }

    Ok(PreparedUserWalletTransaction {
        funded_builder,
        newly_encumbered,
        newly_encumbered_fee,
        signing_keys,
    })
}

fn partition_transaction_inputs(
    inputs: &[Utxo],
    sender_pk: ZkPublicKey,
    fee_pk: ZkPublicKey,
) -> (Vec<Utxo>, Vec<Utxo>) {
    let mut newly_encumbered = Vec::new();
    let mut newly_encumbered_fee = Vec::new();

    for utxo in inputs.iter().copied() {
        if utxo.note.pk == sender_pk {
            newly_encumbered.push(utxo);
        } else if utxo.note.pk == fee_pk {
            newly_encumbered_fee.push(utxo);
        }
    }

    (newly_encumbered, newly_encumbered_fee)
}

fn scenario_fee_account_state(
    world: &CucumberWorld,
    wallet_name: &str,
    available_utxos: &WalletUtxos,
) -> Result<Option<(WalletAccount, Vec<Utxo>)>, StepError> {
    let Some(fee_wallet_account) = world.fee_state.wallet_account.clone() else {
        return Ok(None);
    };

    let fee_wallet_name =
        scenario_fee_wallet_request_name(&group_key_for_wallet(world, wallet_name)?);

    let mut available_fee_utxos = available_utxos.get(&fee_wallet_name).cloned().ok_or_else(
        || StepError::LogicalError {
            message: format!(
                "Scenario fee account state for wallet '{wallet_name}' not found in grouped scan"
            ),
        },
    )?;

    let all_encumbered_fee_note_ids: HashSet<_> = world
        .fee_state
        .encumbered_tokens_per_wallet
        .values()
        .flat_map(|utxos| utxos.iter().map(Utxo::id))
        .collect();
    available_fee_utxos.retain(|utxo| !all_encumbered_fee_note_ids.contains(&utxo.id()));

    Ok(Some((fee_wallet_account, available_fee_utxos)))
}

pub async fn assert_tracked_wallet_fees_equal_sponsored_fee_account_spend(
    world: &mut CucumberWorld,
    step_value: &str,
) -> StepResult {
    let sponsored_genesis_account =
        world
            .fee_state
            .sponsored_genesis_account
            .ok_or_else(|| StepError::LogicalError {
                message: format!(
                    "Step `{step_value}` error: no sponsored genesis fee account configured"
                ),
            })?;

    let fee_wallet_account =
        world
            .fee_state
            .wallet_account
            .clone()
            .ok_or_else(|| StepError::LogicalError {
                message: format!(
                    "Step `{step_value}` error: sponsored fee wallet account not initialized"
                ),
            })?;

    let query_node_name = world.any_started_node()?.name.clone();

    let initial_sponsored_balance = (sponsored_genesis_account.token_count.get() as u64)
        * sponsored_genesis_account.token_value.get();

    let fee_utxos = collect_wallet_utxos(
        world,
        SCENARIO_FEE_ACCOUNT_NAME,
        &query_node_name,
        &[fee_wallet_account.public_key()],
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step_value);
    })?;

    let current_sponsored_balance = fee_utxos.iter().map(|utxo| utxo.note.value).sum::<u64>();
    let sponsored_fee_account_spent = initial_sponsored_balance.checked_sub(current_sponsored_balance).ok_or_else(|| {
        StepError::LogicalError {
            message: format!(
                "Step `{step_value}` error: sponsored fee account balance increased from {initial_sponsored_balance} to {current_sponsored_balance}"
            ),
        }
    })?;

    let tracked_wallet_fees = world.total_tracked_spent_fees();

    if tracked_wallet_fees != sponsored_fee_account_spent {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{step_value}` error: tracked wallet fees {tracked_wallet_fees} do not equal sponsored fee account spent {sponsored_fee_account_spent}"
            ),
        });
    }

    Ok(())
}

pub async fn wait_for_transactions_inclusion(
    client: &NodeHttpClient,
    tx_hashes: &[TxHash],
    timeout: Duration,
) -> Result<(), StepError> {
    let deadline = Instant::now() + timeout;

    loop {
        let mut current = client.consensus_info().await?.tip;
        let mut found = HashSet::new();

        loop {
            let Some(block) = client.block(&current).await? else {
                break;
            };

            for tx in block.transactions() {
                let hash = tx.hash();
                if tx_hashes.contains(&hash) {
                    found.insert(hash);
                }
            }
            current = block.header().parent();
        }

        if tx_hashes.iter().all(|hash| found.contains(hash)) {
            return Ok(());
        }

        if Instant::now() >= deadline {
            let missing = tx_hashes
                .iter()
                .copied()
                .filter(|hash| !found.contains(hash))
                .collect::<Vec<_>>();

            return Err(StepError::Timeout {
                message: format!(
                    "Missing {} submitted transaction(s): {:?}",
                    missing.len(),
                    missing
                ),
            });
        }

        sleep(Duration::from_millis(500)).await;
    }
}

pub async fn wait_for_exact_settled_wallet_balance(
    world: &mut CucumberWorld,
    step_value: &str,
    wallet_name: &str,
    nominal_token_value: u64,
    time_out_seconds: u64,
) -> StepResult {
    let start = Instant::now();
    let timeout = Duration::from_secs(time_out_seconds);

    loop {
        let (_, value) =
            get_wallet_balances(world, step_value, wallet_name, WalletStateType::OnChain).await?;

        if value == nominal_token_value {
            return Ok(());
        }

        if start.elapsed() >= timeout {
            return Err(StepError::StepFail {
                message: format!(
                    "Step `{step_value}` error: wallet '{wallet_name}' has settled LGO = {value}, expected exactly {nominal_token_value}"
                ),
            });
        }

        sleep(Duration::from_millis(100)).await;
    }
}

pub async fn update_wallet_balance_all_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    let mut requests = build_wallet_utxo_requests(world, step, &world.all_user_wallets())?;
    append_scenario_fee_wallet_requests(world, &mut requests);
    scan_available_utxos(world, step, &requests, best_node_info).await
}

pub async fn update_wallet_balance_all_funding_wallets(
    world: &mut CucumberWorld,
    step: &str,
    _best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    let mut funding_wallet_utxos = WalletUtxos::new();
    for wallet in world.all_funding_wallets() {
        let utxos = update_wallet_balance(world, step, &wallet.wallet_name).await?;
        funding_wallet_utxos.insert(wallet.wallet_name.clone(), utxos);
    }
    Ok(funding_wallet_utxos)
}

pub async fn update_wallet_balance_all_wallets(
    world: &mut CucumberWorld,
    step: &str,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    let mut all_wallet_utxos = update_wallet_balance_multiple_wallets(
        world,
        step,
        world.all_user_wallets(),
        best_node_info,
    )
    .await?;
    for wallet in world.all_funding_wallets() {
        let utxos = update_wallet_balance(world, step, &wallet.wallet_name).await?;
        all_wallet_utxos.insert(wallet.wallet_name.clone(), utxos);
    }
    Ok(all_wallet_utxos)
}

pub async fn update_wallet_balance_multiple_wallets(
    world: &mut CucumberWorld,
    step: &str,
    wallets: Vec<WalletInfo>,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    for wallet in &wallets {
        if wallet.is_funding_wallet() {
            return Err(StepError::LogicalError {
                message: format!(
                    "Funding wallet {} should be updated individually due to their strict \
                    coupling with their node's state.",
                    wallet.wallet_name
                ),
            });
        }
    }
    let requests = build_wallet_utxo_requests(world, step, &wallets)?;
    scan_available_utxos(world, step, &requests, best_node_info).await
}

fn build_wallet_utxo_requests(
    world: &CucumberWorld,
    step: &str,
    wallets: &[WalletInfo],
) -> Result<GroupedUtxoRequests, StepError> {
    let mut requests_by_group = GroupedUtxoRequests::new();

    for wallet in wallets {
        let group_key = world
            .node_to_group
            .get(&wallet.node_name)
            .cloned()
            .unwrap_or_default();
        requests_by_group
            .entry(group_key)
            .or_default()
            .push(UtxosRequest {
                wallet_name: wallet.wallet_name.clone(),
                wallet_pk: wallet.public_key().inspect_err(|e| {
                    warn!(target: TARGET, "Step `{}` error: {e}", step);
                })?,
            });
    }

    Ok(requests_by_group)
}

fn append_scenario_fee_wallet_requests(
    world: &CucumberWorld,
    requests_by_group: &mut GroupedUtxoRequests,
) {
    let Some(fee_wallet_account) = world.fee_state.wallet_account.clone() else {
        return;
    };

    for (group_key, requests) in requests_by_group {
        requests.push(UtxosRequest {
            wallet_name: scenario_fee_wallet_request_name(group_key),
            wallet_pk: fee_wallet_account.public_key(),
        });
    }
}

async fn scan_available_utxos(
    world: &mut CucumberWorld,
    step: &str,
    requests_by_group: &GroupedUtxoRequests,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    let mut on_chain_utxos = WalletUtxos::new();
    for grouped_requests in requests_by_group.values() {
        let grouped_utxos = collect_multiple_wallets_utxos(world, grouped_requests, best_node_info)
            .await
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{}` error: {e}", step);
            })?;
        on_chain_utxos.extend(grouped_utxos);
    }

    let available_utxos = requests_by_group
        .values()
        .flat_map(|requests| requests.iter())
        .map(|UtxosRequest { wallet_name, .. }| {
            let wallet_on_chain_utxos =
                on_chain_utxos.get(wallet_name).cloned().unwrap_or_default();
            let available = if wallet_name.starts_with(SCENARIO_FEE_ACCOUNT_NAME) {
                get_available_scenario_fee_utxos(world, wallet_on_chain_utxos)
            } else {
                get_available_utxos(world, wallet_name, wallet_on_chain_utxos)
            };
            (wallet_name.clone(), available)
        })
        .collect();

    Ok(available_utxos)
}

fn scenario_fee_wallet_request_name(group_key: &str) -> String {
    if group_key.is_empty() {
        SCENARIO_FEE_ACCOUNT_NAME.to_owned()
    } else {
        format!("{SCENARIO_FEE_ACCOUNT_NAME}@{group_key}")
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

/// Helper to count and sum UTXOs for a specific sender key.
fn count_and_sum_sender_utxos(utxos: &[Utxo], sender_pk: ZkPublicKey) -> (usize, u64) {
    let sender_utxos: Vec<_> = utxos
        .iter()
        .filter(|utxo| utxo.note.pk == sender_pk)
        .collect();
    (
        sender_utxos.len(),
        sender_utxos.iter().map(|utxo| utxo.note.value).sum(),
    )
}

/// Logs wallet balance information in a clean format.
fn log_wallet_balance(
    wallet_name: &str,
    available_utxos: &[Utxo],
    encumbered_utxos: &[Utxo],
    on_chain_count: usize,
    on_chain_sum: u64,
    sender_pk: ZkPublicKey,
) {
    let (available_count, available_sum) = count_and_sum_sender_utxos(available_utxos, sender_pk);
    let (encumbered_count, encumbered_sum) =
        count_and_sum_sender_utxos(encumbered_utxos, sender_pk);

    info!(
        target: TARGET,
        "Wallet `{wallet_name}` [Available] {available_count}/{available_sum} LGO, \
        [Encumbered] {encumbered_count}/{encumbered_sum} LGO, \
        [On-chain] {on_chain_count}/{on_chain_sum} LGO"
    );
}

/// Builds and funds a user wallet transaction using a scenario fee sponsor.
fn build_sponsored_user_wallet_transaction(
    receivers: &[(ZkPublicKey, u64)],
    sender_utxos: Vec<Utxo>,
    fee_utxos: Vec<Utxo>,
    wallet_account: &WalletAccount,
    fee_wallet_account: &WalletAccount,
) -> Result<MantleTxBuilder, WalletError> {
    let tx_builder = base_user_wallet_transaction(receivers);
    fund_sponsored_user_wallet_transaction(
        tx_builder,
        receivers.iter().map(|(_, value)| *value).sum(),
        sender_utxos,
        wallet_account.public_key(),
        fee_utxos,
        fee_wallet_account.public_key(),
    )
}

/// Builds and funds a user wallet transaction from the wallet's own UTXOs.
fn build_unsponsored_user_wallet_transaction(
    receivers: &[(ZkPublicKey, u64)],
    sender_utxos: Vec<Utxo>,
    wallet_account: &WalletAccount,
) -> Result<MantleTxBuilder, WalletError> {
    let tx_builder = base_user_wallet_transaction(receivers);
    fund_user_wallet_transaction(&tx_builder, sender_utxos, wallet_account.public_key())
}

fn base_user_wallet_transaction(receivers: &[(ZkPublicKey, u64)]) -> MantleTxBuilder {
    let empty_context = MantleTxContext {
        gas_context: MantleTxGasContext::new(HashMap::new()),
        ..MantleTxContext::default()
    };
    let mut tx_builder =
        MantleTxBuilder::new(empty_context).set_storage_gas_price(DEFAULT_STORAGE_GAS_PRICE.into());

    for (receiver_pk, value) in receivers {
        tx_builder = tx_builder.add_ledger_output(Note::new(*value, *receiver_pk));
    }

    tx_builder
}

fn fund_user_wallet_transaction(
    tx_builder: &MantleTxBuilder,
    mut sender_utxos: Vec<Utxo>,
    sender_change_pk: ZkPublicKey,
) -> Result<MantleTxBuilder, WalletError> {
    sender_utxos.sort_by_key(|utxo| Reverse(utxo.note.value));
    fund_builder_from_ordered_utxos(tx_builder, &sender_utxos, sender_change_pk)
}

fn fund_sponsored_user_wallet_transaction(
    tx_builder: MantleTxBuilder,
    output_total: u64,
    sender_utxos: Vec<Utxo>,
    sender_change_pk: ZkPublicKey,
    fee_utxos: Vec<Utxo>,
    fee_change_pk: ZkPublicKey,
) -> Result<MantleTxBuilder, WalletError> {
    let builder_with_sender =
        add_sender_inputs_and_change(tx_builder, output_total, sender_utxos, sender_change_pk)?;

    fund_with_fee_inputs(builder_with_sender, fee_utxos, fee_change_pk)
}

fn add_sender_inputs_and_change(
    tx_builder: MantleTxBuilder,
    output_total: u64,
    mut sender_utxos: Vec<Utxo>,
    sender_change_pk: ZkPublicKey,
) -> Result<MantleTxBuilder, WalletError> {
    sender_utxos.sort_by_key(|utxo| Reverse(utxo.note.value));

    let mut sender_input_sum = 0u64;
    let mut builder = tx_builder;

    for utxo in sender_utxos.iter().copied() {
        builder = builder.add_ledger_input(utxo);
        sender_input_sum = sender_input_sum.saturating_add(utxo.note.value);
        if sender_input_sum >= output_total {
            break;
        }
    }

    if sender_input_sum < output_total {
        return Err(WalletError::InsufficientFunds {
            available: sender_utxos.iter().map(|utxo| utxo.note.value).sum(),
        });
    }

    let sender_change = sender_input_sum - output_total;
    if sender_change > 0 {
        builder = builder.add_ledger_output(Note::new(sender_change, sender_change_pk));
    }

    Ok(builder)
}

fn fund_with_fee_inputs(
    builder: MantleTxBuilder,
    mut fee_utxos: Vec<Utxo>,
    fee_change_pk: ZkPublicKey,
) -> Result<MantleTxBuilder, WalletError> {
    if fee_utxos.is_empty() {
        return if builder.funding_delta::<MainnetGasConstants>()? == 0 {
            Ok(builder)
        } else {
            Err(WalletError::InsufficientFunds { available: 0 })
        };
    }

    fee_utxos.sort_by_key(|utxo| utxo.note.value);
    fund_builder_from_ordered_utxos(&builder, &fee_utxos, fee_change_pk)
}

fn fund_builder_from_ordered_utxos(
    builder: &MantleTxBuilder,
    ordered_utxos: &[Utxo],
    change_pk: ZkPublicKey,
) -> Result<MantleTxBuilder, WalletError> {
    for i in 0..ordered_utxos.len() {
        let funded_builder = builder
            .clone()
            .extend_ledger_inputs(ordered_utxos[..=i].iter().copied());

        match funded_builder
            .funding_delta::<MainnetGasConstants>()?
            .cmp(&0)
        {
            Ordering::Less => {}
            Ordering::Equal => return Ok(funded_builder),
            Ordering::Greater => {
                if let Some(tx_with_change) =
                    funded_builder.return_change::<MainnetGasConstants>(change_pk)?
                {
                    return Ok(tx_with_change);
                }
            }
        }
    }

    Err(WalletError::InsufficientFunds {
        available: ordered_utxos.iter().map(|utxo| utxo.note.value).sum(),
    })
}

pub async fn update_wallet_balance(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> Result<Vec<Utxo>, StepError> {
    let wallet = world.resolve_wallet(wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    let sender_pk = wallet.public_key()?;
    let on_chain_utxos = collect_wallet_utxos(world, wallet_name, &wallet.node_name, &[sender_pk])
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

    let sender_on_chain_utxo_count = on_chain_utxos.len();
    let sender_on_chain_utxo_sum = on_chain_utxos.iter().map(|u| u.note.value).sum::<u64>();

    let available_utxos = get_available_utxos(world, wallet_name, on_chain_utxos);
    let encumbered_utxos = world
        .wallet_runtime_state
        .get(wallet_name)
        .map_or_else(Vec::new, |state| state.encumbered_tokens.clone());

    log_wallet_balance(
        wallet_name,
        &available_utxos,
        &encumbered_utxos,
        sender_on_chain_utxo_count,
        sender_on_chain_utxo_sum,
        sender_pk,
    );

    Ok(available_utxos)
}

fn get_available_utxos(
    world: &CucumberWorld,
    wallet_name: &str,
    on_chain_utxos: Vec<Utxo>,
) -> Vec<Utxo> {
    let mut available_utxos = on_chain_utxos;
    if let Some(state) = world.wallet_runtime_state.get(wallet_name) {
        available_utxos.retain(|utxo| !state.encumbered_tokens.contains(utxo));
    }

    available_utxos
}

fn get_available_scenario_fee_utxos(world: &CucumberWorld, on_chain_utxos: Vec<Utxo>) -> Vec<Utxo> {
    let all_encumbered_fee_note_ids: HashSet<_> = world
        .fee_state
        .encumbered_tokens_per_wallet
        .values()
        .flat_map(|utxos| utxos.iter().map(Utxo::id))
        .collect();

    on_chain_utxos
        .into_iter()
        .filter(|utxo| !all_encumbered_fee_note_ids.contains(&utxo.id()))
        .collect()
}

async fn get_output_balances(
    world: &mut CucumberWorld,
    step: &str,
    wallet: &WalletInfo,
    wallet_name: &str,
    wallet_state_type: WalletStateType,
) -> Result<(usize, u64), StepError> {
    let on_chain_utxos = collect_wallet_utxos(
        world,
        wallet_name,
        &wallet.node_name,
        &[wallet.public_key()?],
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    match wallet_state_type {
        WalletStateType::OnChain => Ok((
            on_chain_utxos.len(),
            on_chain_utxos.iter().map(|u| u.note.value).sum::<u64>(),
        )),
        WalletStateType::Encumbered => {
            let encumbered_utxos = world
                .wallet_runtime_state
                .get(wallet_name)
                .map(|state| state.encumbered_tokens.clone())
                .unwrap_or_default();
            Ok((
                encumbered_utxos.len(),
                encumbered_utxos.iter().map(|u| u.note.value).sum::<u64>(),
            ))
        }
        WalletStateType::Available => {
            let available_utxos: Vec<_> = on_chain_utxos
                .iter()
                .filter(|utxo| {
                    !world
                        .wallet_runtime_state
                        .get(wallet_name)
                        .is_some_and(|state| state.encumbered_tokens.contains(utxo))
                })
                .collect();
            Ok((
                available_utxos.len(),
                available_utxos.iter().map(|u| u.note.value).sum::<u64>(),
            ))
        }
    }
}

pub async fn get_wallet_balances(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    wallet_state_type: WalletStateType,
) -> Result<(usize, u64), StepError> {
    let wallet = world.resolve_wallet(wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    get_output_balances(world, step, &wallet, wallet_name, wallet_state_type).await
}

pub fn clear_wallet_encumbrances(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> StepResult {
    if world.resolve_wallet(wallet_name).is_err() {
        warn!(target: TARGET, "Step `{}` error: wallet '{wallet_name}' not found in world state", step);
        return Err(StepError::LogicalError {
            message: format!("wallet '{wallet_name}' not found in world state"),
        });
    }

    world.wallet_runtime_state.remove(wallet_name);
    world
        .fee_state
        .encumbered_tokens_per_wallet
        .remove(wallet_name);
    info!(target: TARGET, "Cleared encumbrances for wallet '{wallet_name}'");
    Ok(())
}

pub fn clear_all_wallet_encumbrances(world: &mut CucumberWorld, step: &str) -> StepResult {
    let wallet_names: Vec<String> = world.wallet_info.keys().cloned().collect();

    for wallet_name in wallet_names {
        clear_wallet_encumbrances(world, step, &wallet_name)?;
    }
    info!(target: TARGET, "Cleared encumbrances for all wallets");
    Ok(())
}

fn validate_wait_conditions(
    step: &str,
    min_coin_count: Option<&usize>,
    max_coin_count: Option<&usize>,
    min_token_value: Option<&u64>,
    max_token_value: Option<&u64>,
) -> StepResult {
    if min_coin_count.is_none()
        && min_token_value.is_none()
        && max_coin_count.is_none()
        && max_token_value.is_none()
    {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{step}` error: at least one of 'min_coin_count', 'max_coin_count', \
                'min_token_value' or 'max_token_value' must be provided"
            ),
        });
    }
    if min_coin_count.is_some() && max_coin_count.is_some() {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{step}` error: 'min_coin_count' and 'max_coin_count' cannot be used \
                together"
            ),
        });
    }
    if min_token_value.is_some() && max_token_value.is_some() {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{step}` error: 'min_token_value' and 'max_token_value' cannot be used \
                together"
            ),
        });
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "This function is more readable with explicit arguments rather than packing them into structs or tuples."
)]
pub async fn wait_for_wallet_or_encumbered_state(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: String,
    min_coin_count: Option<&usize>,
    max_coin_count: Option<&usize>,
    min_token_value: Option<&u64>,
    max_token_value: Option<&u64>,
    time_out_seconds: u64,
    wallet_state_type: WalletStateType,
) -> StepResult {
    validate_wait_conditions(
        step,
        min_coin_count,
        max_coin_count,
        min_token_value,
        max_token_value,
    )?;

    let wallet = world
        .wallet_info
        .get(&wallet_name)
        .ok_or(StepError::LogicalError {
            message: format!("wallet '{wallet_name}' not found in world state"),
        })?
        .clone();

    let start = Instant::now();
    let time_out = Duration::from_secs(time_out_seconds);
    let mut poll_count = 0usize;

    loop {
        let (coin_count, value) =
            get_output_balances(world, step, &wallet, &wallet_name, wallet_state_type).await?;

        if conditions_met(
            &wallet_name,
            &wallet.node_name,
            coin_count,
            value,
            min_coin_count,
            max_coin_count,
            min_token_value,
            max_token_value,
            wallet_state_type,
        ) {
            return Ok(());
        }

        if start.elapsed() >= time_out {
            let msg = format!(
                "Step `{step}` error: wallet '{wallet_name}/{}' has `outputs/LGO` = `{coin_count}/ \
                {value}`, expected `outputs/LGO` >= `{min_coin_count:?}/{min_token_value:?}` \
                or `outputs/LGO` <= `{max_coin_count:?}/{max_token_value:?}`",
                wallet.node_name,
            );
            warn!(target: TARGET, "{msg}");
            return Err(StepError::StepFail { message: msg });
        }

        if poll_count.is_multiple_of(25) {
            info!(
                target: TARGET,
                "Waiting for wallet '{wallet_name}/{}' to have required '{wallet_state_type}' \
                coins, current count: {coin_count}, value: {value}",
                wallet.node_name
            );
        }
        poll_count += 1;
        sleep(Duration::from_millis(200)).await;
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "This function is more readable with explicit arguments rather than packing them into structs or tuples."
)]
#[expect(
    clippy::cognitive_complexity,
    reason = "Singular fn with multiple branches to handle different events and futures."
)]
fn conditions_met(
    wallet_name: &str,
    wallet_node_name: &str,
    coin_count: usize,
    value: u64,
    min_coin_count: Option<&usize>,
    max_coin_count: Option<&usize>,
    min_token_value: Option<&u64>,
    max_token_value: Option<&u64>,
    wallet_state_type: WalletStateType,
) -> bool {
    match (
        min_coin_count,
        min_token_value,
        max_coin_count,
        max_token_value,
    ) {
        (Some(min_count), Some(min_value), _, _) => {
            if coin_count >= *min_count && value >= *min_value {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}/{}' has required '{wallet_state_type}' coin count: {coin_count} >= \
                    {min_count}, token value: {value} >= {min_value}",
                    wallet_node_name
                );
                return true;
            }
        }
        (Some(min_count), None, _, _) => {
            if coin_count >= *min_count {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}/{}' has required '{wallet_state_type}' coin count: {coin_count} >= \
                    {min_count}",
                    wallet_node_name
                );
                return true;
            }
        }
        (None, Some(min_value), _, _) => {
            if value >= *min_value {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}/{}' has required '{wallet_state_type}' token value: {value} >= \
                    {min_value}",
                    wallet_node_name
                );
                return true;
            }
        }
        (_, _, Some(max_count), Some(max_value)) => {
            if coin_count <= *max_count && value <= *max_value {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}/{}' has required '{wallet_state_type}' coin count: {coin_count} <= \
                    {max_count}, token value: {value} <= {max_value}",
                    wallet_node_name
                );
                return true;
            }
        }
        (_, _, Some(max_count), None) => {
            if coin_count <= *max_count {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}/{}' has required '{wallet_state_type}' coin count: {coin_count} <= \
                    {max_count}",
                    wallet_node_name
                );
                return true;
            }
        }
        (_, _, None, Some(max_value)) => {
            if value <= *max_value {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}/{}' has required '{wallet_state_type}' token value: {value} <= \
                    {max_value}",
                    wallet_node_name
                );
                return true;
            }
        }
        (None, None, None, None) => unreachable!(),
    }

    false
}

fn record_header_height(
    node_header_heights: &mut HashMap<String, HashMap<String, u64>>,
    node_name: &str,
    header_id: &str,
    height: u64,
) {
    node_header_heights
        .entry(node_name.to_owned())
        .or_default()
        .insert(header_id.to_owned(), height);
}

fn get_last_known_height<'a>(
    node_header_heights: &HashMap<String, HashMap<String, u64>>,
    cached_ancestor_header_id: Option<&String>,
    wallet_node_name: &'a str,
    reached_chain_start: bool,
) -> (u64, &'a str) {
    // Height of the first block in tail_blocks (oldest uncached block)
    cached_ancestor_header_id.as_ref().map_or(
        (1, if reached_chain_start { "" } else { "~" }),
        |&cached_header_id| {
            let cached_height = node_header_heights
                .get(wallet_node_name)
                .and_then(|m| m.get(cached_header_id))
                .copied();

            cached_height.map_or((1, "~"), |h| (h + 1, ""))
        },
    )
}

#[derive(Clone)]
struct UtxosRequest {
    wallet_name: String,
    wallet_pk: ZkPublicKey,
}

type GroupedUtxoRequests = BTreeMap<String, Vec<UtxosRequest>>;

type WalletPkMap = HashMap<String, ZkPublicKey>;
type WalletsByPk = HashMap<ZkPublicKey, String>;

type OwnedUtxos = HashMap<NoteId, Utxo>;
type UtxoWalletMap = HashMap<String, OwnedUtxos>;
type WalletUtxos = HashMap<String, Vec<Utxo>>;

fn collect_multiple_sync_wallet_info(
    requests: &[UtxosRequest],
) -> Result<(WalletPkMap, UtxoWalletMap), StepError> {
    let mut wallet_pks: WalletPkMap = HashMap::new();
    let mut owned_per_wallet: UtxoWalletMap = HashMap::new();
    for UtxosRequest {
        wallet_name,
        wallet_pk,
    } in requests
    {
        let Some(existing_pk) = wallet_pks.get(wallet_name) else {
            wallet_pks.insert(wallet_name.clone(), *wallet_pk);
            owned_per_wallet.insert(wallet_name.clone(), HashMap::new());
            continue;
        };
        if *existing_pk != *wallet_pk {
            return Err(StepError::LogicalError {
                message: format!("Conflicting public keys in requests for wallet '{wallet_name}'",),
            });
        }
    }
    Ok((wallet_pks, owned_per_wallet))
}

fn organize_wallets_by_pk(wallet_pks: &WalletPkMap) -> Result<WalletsByPk, StepError> {
    let mut wallets_by_pk: WalletsByPk = HashMap::new();
    for (wallet_name, pk) in wallet_pks {
        if let Some(existing_wallet) = wallets_by_pk.get(pk) {
            return Err(StepError::LogicalError {
                message: format!(
                    "Multiple wallets have the same public key {pk:?}: '{existing_wallet}' and \
                    '{wallet_name}'"
                ),
            });
        }
        wallets_by_pk.insert(*pk, wallet_name.clone());
    }
    Ok(wallets_by_pk)
}

fn refresh_owned_per_wallet_from_cache(
    world: &CucumberWorld,
    header_id: &str,
    wallet_pks: &WalletPkMap,
    owned_per_wallet: &mut UtxoWalletMap,
) {
    if let Some(wallet_token_map) = world.wallet_tokens_per_block.get(header_id) {
        for wallet_name in wallet_pks.keys() {
            let wallet_owned = owned_per_wallet
                .get_mut(wallet_name)
                .expect("wallet exists");
            update_wallet_owned(wallet_name, wallet_owned, wallet_token_map);
        }
    }
}

fn find_cached_header_ancestor_multi_wallets(
    world: &CucumberWorld,
    header_id: &str,
    wallet_pks: &WalletPkMap,
    node_name: &str,
) -> Option<String> {
    if let Some(wallet_token_map) = world.wallet_tokens_per_block.get(header_id)
        && wallet_pks
            .keys()
            .all(|wallet_name| wallet_token_map.utxos_per_wallet.contains_key(wallet_name))
    {
        if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
            info!(
                target: TARGET,
                "Common header '{header_id}' found for {} wallets on `{node_name}`",
                wallet_pks.len(),
            );
        }

        return Some(header_id.to_owned());
    }
    None
}

fn update_genesis_utxos_multi_wallets(
    world: &CucumberWorld,
    wallets_by_pk: &WalletsByPk,
    owned_per_wallet: &mut UtxoWalletMap,
) {
    let utxo_note_pks = world
        .genesis_block_utxos
        .iter()
        .map(|utxo| utxo.note.pk)
        .collect::<HashSet<_>>();
    for note_pk in &utxo_note_pks {
        if let Some(wallet_name) = wallets_by_pk.get(note_pk)
            && let Some(owned) = owned_per_wallet.get_mut(wallet_name)
            && owned.is_empty()
        {
            for utxo in world
                .genesis_block_utxos
                .iter()
                .filter(|u| u.note.pk == *note_pk)
            {
                owned.insert(utxo.id(), *utxo);
            }
        }
    }
}

async fn collect_multiple_wallets_utxos(
    world: &mut CucumberWorld,
    requests: &[UtxosRequest],
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    if requests.is_empty() {
        return Ok(HashMap::new());
    }

    let (best_node_name, best_node_client, best_consensus) =
        sanitize_best_node_info(world, &requests[0].wallet_name, best_node_info).await?;
    let (wallet_pks, mut owned_per_wallet) = collect_multiple_sync_wallet_info(requests)?;
    let wallets_by_pk = organize_wallets_by_pk(&wallet_pks)?;

    // Walk back from best tip until chain start or a cache hit that contains all
    // wallets.
    let mut tail_blocks = Vec::new();
    let mut reached_chain_start = false;
    let mut cached_ancestor_header_id: Option<String> = None;
    let mut current = best_consensus.tip;
    loop {
        let Some(block) = best_node_client.block(&current).await? else {
            reached_chain_start = true;
            break;
        };

        let header_id = block.header().id().to_string();

        // Cache represents the post-state after evaluating this block.
        refresh_owned_per_wallet_from_cache(world, &header_id, &wallet_pks, &mut owned_per_wallet);

        if let Some(header_id) = find_cached_header_ancestor_multi_wallets(
            world,
            &header_id,
            &wallet_pks,
            &best_node_name,
        ) {
            cached_ancestor_header_id = Some(header_id);
            break;
        }

        let parent = block.header().parent();
        tail_blocks.push(block);
        current = parent;
    }
    tail_blocks.reverse();

    // Add genesis block UTXOs to each owned empty set, as they are not in the
    // blocks stream.
    update_genesis_utxos_multi_wallets(world, &wallets_by_pk, &mut owned_per_wallet);

    // Replay uncached tail once and update all tracked wallets together.
    let (base_height, height_prefix) = get_last_known_height(
        &world.node_header_heights,
        cached_ancestor_header_id.as_ref(),
        best_node_name.as_str(),
        reached_chain_start,
    );
    for (i, block) in tail_blocks.iter().enumerate() {
        let header_id = block.header().id().to_string();
        let height = base_height + i as u64;
        record_header_height(
            &mut world.node_header_heights,
            best_node_name.as_str(),
            &header_id,
            height,
        );

        if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
            info!(
                target: TARGET,
                "Evaluating block {height_prefix}{height} for {} wallets on `{best_node_name}`: \
                {header_id}, transactions len: {}",
                wallet_pks.len(),
                block.transactions().len(),
            );
        }

        for tx in block.transactions() {
            for transfer in tx.mantle_tx.transfers() {
                for utxo in transfer.utxos() {
                    if let Some(wallet_name) = wallets_by_pk.get(&utxo.note.pk)
                        && let Some(owned) = owned_per_wallet.get_mut(wallet_name)
                    {
                        add_new_utxo(owned, utxo, wallet_name);
                    }
                }
            }

            for transfer in tx.mantle_tx.transfers() {
                for spent in &transfer.inputs {
                    for (wallet_name, owned) in &mut owned_per_wallet {
                        remove_spent_utxo(world, owned, spent, wallet_name);
                    }
                }
            }
        }

        let entry = world
            .wallet_tokens_per_block
            .entry(header_id.clone())
            .or_insert_with(|| WalletTokenMap {
                header_id: header_id.clone(),
                utxos_per_wallet: HashMap::new(),
            });
        for (wallet_name, owned) in &owned_per_wallet {
            entry
                .utxos_per_wallet
                .insert(wallet_name.clone(), owned.values().copied().collect());
        }
    }

    Ok(owned_per_wallet
        .into_iter()
        .map(|(wallet_name, owned)| (wallet_name, owned.into_values().collect()))
        .collect())
}

fn add_new_utxo(owned: &mut OwnedUtxos, utxo: Utxo, wallet_name: &str) {
    if owned.insert(utxo.id(), utxo).is_none() && is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
        info!(
            target: TARGET,
            "Found UTXO for `{wallet_name}`: value: {}, id: {:?}",
            utxo.note.value,
            utxo.id(),
        );
    }
}

fn remove_spent_utxo(
    world: &mut CucumberWorld,
    owned: &mut OwnedUtxos,
    spent: &NoteId,
    wallet_name: &str,
) {
    if owned.remove(spent).is_some() {
        if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
            info!(
                target: TARGET,
                "Found spent UTXO for `{wallet_name}`: id: {spent:?}",
            );
        }
        if let Some(state) = world.wallet_runtime_state.get_mut(wallet_name) {
            state.encumbered_tokens.retain(|u| u.id() != *spent);
        }
    }
}

fn update_wallet_owned(
    wallet_name: &str,
    wallet_owned: &mut OwnedUtxos,
    wallet_token_map: &WalletTokenMap,
) {
    if wallet_owned.is_empty()
        && let Some(cached_utxos) = wallet_token_map.utxos_per_wallet.get(wallet_name)
    {
        for utxo in cached_utxos {
            wallet_owned.insert(utxo.id(), *utxo);
        }
    }
}

fn refresh_owned_from_cache_single_wallet(
    world: &CucumberWorld,
    header_id: &str,
    wallet_name: &str,
    wallet_owned: &mut OwnedUtxos,
) {
    if let Some(wallet_token_map) = world.wallet_tokens_per_block.get(header_id)
        && wallet_token_map.utxos_per_wallet.contains_key(wallet_name)
    {
        update_wallet_owned(wallet_name, wallet_owned, wallet_token_map);
    }
}

fn find_cached_header_single_wallet(
    world: &CucumberWorld,
    header_id: &str,
    wallet_name: &str,
) -> Option<String> {
    if let Some(wallet_token_map) = world.wallet_tokens_per_block.get(header_id)
        && wallet_token_map.utxos_per_wallet.contains_key(wallet_name)
    {
        return Some(header_id.to_owned());
    }
    None
}

fn update_genesis_utxos_single_wallet(
    world: &CucumberWorld,
    wallet_pk: &ZkPublicKey,
    wallet_owned: &mut OwnedUtxos,
) {
    if wallet_owned.is_empty() {
        for utxo in &world.genesis_block_utxos {
            if &utxo.note.pk == wallet_pk {
                wallet_owned.insert(utxo.id(), *utxo);
            }
        }
    }
}

async fn collect_wallet_utxos(
    world: &mut CucumberWorld,
    wallet_name: &str,
    wallet_node_name: &str,
    wallet_pks: &[ZkPublicKey],
) -> Result<Vec<Utxo>, StepError> {
    let mut wallet_owned: OwnedUtxos = HashMap::new();
    let wallet_pks = wallet_pks.iter().copied().collect::<HashSet<_>>();

    let node = world
        .nodes_info
        .get(wallet_node_name)
        .ok_or(StepError::LogicalError {
            message: format!("Node '{wallet_node_name}' for wallet '{wallet_name}' not found"),
        })?;
    let consensus = node.started_node.client.consensus_info().await?;
    let mut current = consensus.tip;

    // Get all blocks from the current tip walking backwards, but stop as soon as
    // we hit a header for which we already have cached wallet state.
    let mut tail_blocks = Vec::new();
    let mut reached_chain_start = false;
    let mut cached_ancestor_header_id: Option<String> = None;
    loop {
        let Some(block) = node.started_node.client.block(&current).await? else {
            reached_chain_start = true;
            break;
        };

        let header_id = block.header().id().to_string();

        // Cache represents the post-state after evaluating this block.
        refresh_owned_from_cache_single_wallet(world, &header_id, wallet_name, &mut wallet_owned);

        if let Some(header_id) = find_cached_header_single_wallet(world, &header_id, wallet_name) {
            cached_ancestor_header_id = Some(header_id);
            break;
        }

        let parent = block.header().parent();
        tail_blocks.push(block);
        current = parent;
    }
    tail_blocks.reverse();

    // Add genesis block UTXOs to the owned set, as they are not included in the
    // blocks stream.
    for wallet_pk in &wallet_pks {
        update_genesis_utxos_single_wallet(world, wallet_pk, &mut wallet_owned);
    }

    // Evaluate the tail blocks forward to reconstruct the wallet state at the tip.
    let (base_height, height_prefix) = get_last_known_height(
        &world.node_header_heights,
        cached_ancestor_header_id.as_ref(),
        wallet_node_name,
        reached_chain_start,
    );
    for (i, block) in tail_blocks.iter().enumerate() {
        let header_id = block.header().id().to_string();
        let height = base_height + i as u64;
        record_header_height(
            &mut world.node_header_heights,
            wallet_node_name,
            &header_id,
            height,
        );

        if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
            info!(
                target: TARGET,
                "Evaluating block {height_prefix}{height} for `{wallet_name}/{wallet_node_name}`: \
                {}, transactions len: {}",
                header_id,
                block.transactions().len(),
            );
        }

        for tx in block.transactions() {
            // Unspent outputs
            for transfer in tx.mantle_tx.transfers() {
                for utxo in transfer.utxos() {
                    if wallet_pks.contains(&utxo.note.pk) {
                        add_new_utxo(&mut wallet_owned, utxo, wallet_name);
                    }
                }
            }

            // Spent outputs
            for transfer in tx.mantle_tx.transfers() {
                for spent in &transfer.inputs {
                    remove_spent_utxo(world, &mut wallet_owned, spent, wallet_name);
                }
            }
        }

        // Update wallet tokens cache
        let entry = world
            .wallet_tokens_per_block
            .entry(header_id.clone())
            .or_insert_with(|| WalletTokenMap {
                header_id: header_id.clone(),
                utxos_per_wallet: HashMap::new(),
            });
        entry.utxos_per_wallet.insert(
            wallet_name.to_owned(),
            wallet_owned.values().copied().collect(),
        );
    }

    Ok(wallet_owned.values().copied().collect())
}

pub(crate) fn request_faucet_funds(
    world: &mut CucumberWorld,
    step: &str,
    number_of_rounds: NonZero<usize>,
    wallets: &[String],
) -> StepResult {
    if world.faucet_base_url.is_none()
        || world.faucet_username.is_none()
        || world.faucet_password.is_none()
    {
        warn!(
            target: TARGET,
            "Step `{}` error: Faucet details not set.",
            step
        );
        return Err(StepError::LogicalError {
            message: "Faucet details not set".to_owned(),
        });
    }
    let faucet_task = FaucetTask::new(
        world
            .faucet_base_url
            .clone()
            .expect("checked above")
            .as_ref(),
        world
            .faucet_username
            .clone()
            .expect("checked above")
            .as_ref(),
        world
            .faucet_password
            .clone()
            .expect("checked above")
            .as_ref(),
        wallets,
        number_of_rounds,
    );
    if let Some(handles) = &mut world.faucet_task_handles {
        handles.push(faucet_task.spawn(1000, step));
    } else {
        world.faucet_task_handles = Some(vec![faucet_task.spawn(1000, step)]);
    }

    Ok(())
}
