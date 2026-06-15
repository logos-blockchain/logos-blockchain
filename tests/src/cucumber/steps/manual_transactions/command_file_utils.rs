//! This module executes manual commands for Cucumber scenarios.
//!
//! External command controller:
//! - Set `CUCUMBER_MANUAL_COMMAND_FILE=/tmp/cucumber-manual-commands.txt`.
//! - Start the scenario.
//! - Prepare the command file beforehand, or append commands while the test
//!   runs.
//!
//! Supported commands (one per line):
//!
//! ```text
//! COIN_SPLIT, wallet '<wallet_name>', outputs <count>, value <amount>
//! VERIFY, wallet '<wallet_name>', outputs <count>, time_out <duration_seconds>
//! BALANCE, wallet '<wallet_name>'
//! BALANCE_ALL_WALLETS
//! BALANCE_ALL_USER_WALLETS
//! BALANCE_ALL_FUNDING_WALLETS
//! CLEAR_ENCUMBRANCES, wallet '<wallet_name>'
//! CLEAR_ENCUMBRANCES_ALL_WALLETS
//! SEND, transactions <count>, value <amount>, from '<wallet_name>', to '<wallet_name>'
//! VERIFY_MAX, wallet '<wallet_name>', wallet_state_type 'on-chain'/'encumbered'/'available', outputs <count>, value 14000, time_out <duration_seconds>
//! VERIFY_MIN, wallet '<wallet_name>', wallet_state_type 'on-chain'/'encumbered'/'available', outputs <count>, value 14000, time_out <duration_seconds>
//! CONTINUOUS_ROUND_ROBIN_USER_WALLETS, coin_split_outputs <count>, coin_split_value <amount>, transactions <count>, value <amount>, cycles <count>
//! COIN_SPLIT_ALL_USER_WALLETS, splits_per_wallet <count>, outputs <count>, value <amount>
//! VERIFY_MIN_AVAILABLE_OUTPUTS_ALL_USER_WALLETS, min_outputs <count>, timeout_seconds <duration_seconds>
//! CONTINUOUS_NEXT_WALLET_USER_WALLETS, cycles <count>, transactions_per_wallet <count>, value <amount>
//! FAUCET_ALL_USER_WALLETS, rounds <count>
//! FAUCET_ALL_FUNDING_WALLETS, rounds <count>
//! CREATE_BLOCKCHAIN_SNAPSHOT_ALL_NODES, snapshot_name '<snapshot_name>'
//! CREATE_BLOCKCHAIN_SNAPSHOT_NODE, snapshot_name '<snapshot_name>', node_name '<node_name>'
//! RESTART_NODE, node_name '<node_name>'
//! CRYPTARCHIA_INFO_ALL_NODES
//! WAIT_ALL_NODES_SYNCED_TO_CHAIN
//! STOP
//! ```

use std::{
    collections::{BTreeMap, HashSet},
    env,
    num::NonZero,
    path::Path,
    time::Duration,
};

use lb_core::mantle::TxHash;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_wallet::WalletError;
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::{
    common::wallet::WalletUtxos,
    cucumber::{
        error::{StepError, StepResult},
        steps::{
            TARGET, manual_nodes,
            manual_nodes::{
                snapshots::save_named_blockchain_snapshot,
                utils::{
                    create_snapshots_all_nodes, restart_node,
                    wait_for_all_nodes_to_be_synced_to_chain,
                },
            },
            manual_transactions::{
                command_file_parsing::{ManualCommand, take_next_command},
                utils,
                utils::{BestNodeInfo, WalletOutputState, extend_tx_hash_set},
            },
        },
        wallet::{
            best_node::{get_best_node_info, get_best_node_info_choose},
            checks::wait_for_observed_transaction_hashes,
        },
        world::{CucumberWorld, WalletInfo},
    },
};

const MANUAL_COMMAND_FILE_ENV: &str = "CUCUMBER_MANUAL_COMMAND_FILE";
const MANUAL_COMMAND_POLL_INTERVAL_ENV: &str = "CUCUMBER_MANUAL_COMMAND_POLL_INTERVAL_MS";

pub(crate) async fn execute_manual_command(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
) -> Result<bool, StepError> {
    if matches!(command, ManualCommand::Stop) {
        return Ok(true);
    }

    execute_non_stop_manual_command(world, step, command).await?;
    Ok(false)
}

pub(crate) async fn execute_continuous_round_robin_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
    coin_split_outputs: usize,
    coin_split_value: u64,
    transactions: usize,
    value: u64,
    cycles: usize,
) -> Result<(), StepError> {
    let command = ManualCommand::ContinuousRoundRobinUserWallets {
        coin_split_outputs,
        coin_split_value,
        transactions,
        value,
        cycles,
    };

    execute_non_stop_manual_command(world, step, &command).await
}

pub(crate) async fn execute_coin_splits_all_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
    splits_per_wallet: usize,
    outputs: usize,
    value: u64,
) -> Result<(), StepError> {
    let mut wallet_names: Vec<_> = world
        .all_user_wallets()
        .iter()
        .map(|w| w.wallet_name.clone())
        .collect();
    if wallet_names.len() < 2 {
        return Err(StepError::InvalidArgument {
            message: "coin split for all user wallets requires at least two wallets".to_owned(),
        });
    }
    wallet_names.sort();
    let mut available_utxos = utils::current_available_utxos_for_user_wallets(world, step).await?;

    for wallet_name in &wallet_names {
        for _ in 0..splits_per_wallet {
            let best_node_info = get_best_node_info(world, wallet_name).await?;
            execute_coin_split_with_utxo_cache(
                world,
                step,
                wallet_name,
                outputs,
                value,
                Some(&best_node_info),
                &mut available_utxos,
            )
            .await?;
        }
    }

    Ok(())
}

pub(crate) async fn verify_min_outputs_all_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
    min_outputs: usize,
    timeout_seconds: u64,
    wallet_state_type: WalletOutputState,
) -> Result<(), StepError> {
    let mut wallet_names: Vec<_> = world
        .all_user_wallets()
        .iter()
        .map(|w| w.wallet_name.clone())
        .collect();
    wallet_names.sort();

    for wallet_name in &wallet_names {
        utils::wait_for_wallet_output_state(
            world,
            step,
            wallet_name.clone(),
            Some(&min_outputs),
            None,
            None,
            None,
            timeout_seconds,
            wallet_state_type,
        )
        .await?;
    }

    Ok(())
}

fn destructure_next_wallet_command(
    command: &ManualCommand,
) -> Result<(usize, usize, u64), StepError> {
    let ManualCommand::ContinuousNextWalletUserWallets {
        cycles,
        transactions_per_wallet,
        value,
    } = command
    else {
        return Err(StepError::LogicalError {
            message: "expected ContinuousNextWalletUserWallets command".to_owned(),
        });
    };
    Ok((*cycles, *transactions_per_wallet, *value))
}

pub(crate) async fn execute_continuous_next_wallet_user_wallet(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
) -> Result<(), StepError> {
    let (cycles, transactions_per_wallet, value) = destructure_next_wallet_command(command)?;
    let wallet_names = all_user_wallets(world)?;

    let mut all_next_wallet_tx_hashes = HashSet::new();
    for cycle in 0..cycles {
        let mut available_utxos =
            utils::current_available_utxos_for_user_wallets(world, step).await?;

        let cycle_tx_hashes = execute_ring_send_round_with_utxo_cache(
            world,
            step,
            &wallet_names,
            transactions_per_wallet,
            value,
            cycle,
            &mut available_utxos,
        )
        .await?;

        verify_transactions_mined(
            world,
            step,
            &cycle_tx_hashes,
            wallet_names.len() * transactions_per_wallet,
            Some(cycle + 1),
            "CONTINUOUS NEXT WALLET",
            "D",
        )
        .await?;

        extend_tx_hash_set(&mut all_next_wallet_tx_hashes, &cycle_tx_hashes);
    }

    let expected_total = cycles * wallet_names.len() * transactions_per_wallet;
    if all_next_wallet_tx_hashes.len() != expected_total {
        return Err(StepError::StepFail {
            message: format!(
                "CONTINUOUS NEXT WALLET submitted {} unique transaction hash(es), expected \
                {expected_total}",
                all_next_wallet_tx_hashes.len(),
            ),
        });
    }

    info!(
        target: TARGET,
        "CONTINUOUS NEXT WALLET scenario complete: {} unique submitted transaction(s) verified \
        across {} cycle(s)",
        all_next_wallet_tx_hashes.len(),
        cycles,
    );

    Ok(())
}

async fn verify_transactions_mined(
    world: &mut CucumberWorld,
    step: &str,
    tx_hashes: &HashSet<TxHash>,
    expected_tx_count: usize,
    cycle: Option<usize>,
    tag: &str,
    phase: &str,
) -> Result<(), StepError> {
    if tx_hashes.len() != expected_tx_count {
        return Err(StepError::StepFail {
            message: format!(
                "{tag}{} submitted {} transaction hash(es), expected {expected_tx_count}",
                cycle.map_or_else(String::new, |cycle| format!(" cycle {cycle}")),
                tx_hashes.len(),
            ),
        });
    }

    info!(
        target: TARGET,
        "{tag}{} {phase}: Wait for {} submitted transaction hashes to be observed in chain blocks",
        cycle.map_or_else(String::new, |cycle| format!(" cycle {cycle}")),
        tx_hashes.len(),
    );

    wait_for_observed_transaction_hashes(world, step, tx_hashes, Duration::from_mins(3)).await
}

fn log_phase_d_counts(tag: &str, cycle: usize, kind: &str, counts: &BTreeMap<String, usize>) {
    log_phase_counts(tag, cycle, "D", kind, counts);
}

fn log_phase_counts(
    tag: &str,
    cycle: usize,
    phase: &str,
    kind: &str,
    counts: &BTreeMap<String, usize>,
) {
    let counts = counts
        .iter()
        .map(|(wallet, count)| format!("{wallet}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    info!(
        target: TARGET,
        "{tag} cycle {} {phase}: {kind} tx counts by sender wallet: {counts}",
        cycle + 1,
    );
}

#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
async fn execute_ring_send_round_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    wallet_names: &[String],
    transactions_per_wallet: usize,
    value: u64,
    cycle: usize,
    available_utxos: &mut WalletUtxos,
) -> Result<HashSet<TxHash>, StepError> {
    let mut signed_submissions = Vec::with_capacity(wallet_names.len() * transactions_per_wallet);
    let mut prepared_counts = BTreeMap::new();

    for i in 0..wallet_names.len() {
        info!(
            target: TARGET,
            "CONTINUOUS NEXT WALLET cycle {} A: Await funds",
            cycle + 1
        );
        let from = &wallet_names[i];
        let to = &wallet_names[(i + 1) % wallet_names.len()];

        let required_available = transactions_per_wallet as u64 * value;
        wait_wallet_send_ready(
            world,
            from,
            180,
            required_available,
            WalletSendReadiness::EligibleUtxoBatch {
                required_outputs: transactions_per_wallet,
                value_per_transaction: value,
            },
            Some(available_utxos),
        )
        .await?;

        info!(
            target: TARGET,
            "CONTINUOUS NEXT WALLET cycle {} B: Prepare transactions to next wallet concurrently",
            cycle + 1
        );
        let mut prepared = prepare_ring_send_round_send_with_utxo_cache(
            world,
            step,
            transactions_per_wallet,
            value,
            from,
            to,
            available_utxos,
        )
        .await?;
        prepared_counts.insert(from.clone(), prepared.len());
        signed_submissions.append(&mut prepared);
    }

    log_phase_d_counts(
        "CONTINUOUS NEXT WALLET",
        cycle,
        "prepared",
        &prepared_counts,
    );

    info!(
        target: TARGET,
        "CONTINUOUS NEXT WALLET cycle {} C: Submit {} transaction(s) concurrently",
        cycle + 1,
        signed_submissions.len()
    );
    let best_node_info = get_best_node_info_choose(world, wallet_names).await?;
    let submitted_hashes = utils::submit_signed_user_wallet_submissions_concurrently(
        world,
        signed_submissions,
        Some(&best_node_info),
    )
    .await?;
    let mut submitted_counts = BTreeMap::new();
    for (sender, _) in &submitted_hashes {
        *submitted_counts.entry(sender.clone()).or_insert(0usize) += 1;
    }
    log_phase_d_counts(
        "CONTINUOUS NEXT WALLET",
        cycle,
        "submitted",
        &submitted_counts,
    );
    let cycle_tx_hashes = submitted_hashes
        .into_iter()
        .map(|(_, tx_hash)| tx_hash)
        .collect::<HashSet<_>>();

    info!(
        target: TARGET,
        "CONTINUOUS NEXT WALLET cycle {} D: submitted {} transaction(s)",
        cycle + 1,
        cycle_tx_hashes.len()
    );

    Ok(cycle_tx_hashes)
}

#[derive(Clone, Copy, Debug)]
enum WalletSendReadiness {
    EligibleUtxoBatch {
        required_outputs: usize,
        value_per_transaction: u64,
    },
}

async fn wait_wallet_send_ready(
    world: &mut CucumberWorld,
    wallet_name: &str,
    timeout_seconds: u64,
    required_available: u64,
    readiness: WalletSendReadiness,
    mut available_utxos: Option<&mut WalletUtxos>,
) -> Result<(), StepError> {
    let start = Instant::now();

    let mut last_available_value = 0u64;
    let mut last_available_outputs = 0usize;
    let mut last_eligible_value = 0u64;
    let mut last_eligible_outputs = 0usize;
    let WalletSendReadiness::EligibleUtxoBatch {
        required_outputs,
        value_per_transaction,
    } = readiness;

    while start.elapsed() < Duration::from_secs(timeout_seconds) {
        let mut fresh = utils::current_wallet_available_state(world, wallet_name).await?;

        // Prefer smaller UTXOs first. The transaction builder selects sufficient
        // inputs, so this helps consume smaller/dustier outputs before large
        // change-like outputs.
        fresh.available_utxos.sort_by_key(|utxo| utxo.note.value);

        let available = fresh.observation.balance(WalletOutputState::Available);
        last_available_value = available.value;
        last_available_outputs = available.output_count;

        let mut eligible_outputs = 0usize;
        let mut eligible_value = 0u64;

        for utxo in fresh
            .available_utxos
            .iter()
            .filter(|utxo| utxo.note.value >= value_per_transaction)
        {
            eligible_outputs += 1;
            eligible_value += utxo.note.value;
        }

        let is_ready = eligible_outputs >= required_outputs && eligible_value >= required_available;

        if is_ready {
            if let Some(cache) = available_utxos.as_deref_mut() {
                cache.insert(wallet_name.to_owned().into(), fresh.available_utxos);
            }

            return Ok(());
        }

        last_eligible_value = eligible_value;
        last_eligible_outputs = eligible_outputs;

        sleep(Duration::from_millis(300)).await;
    }

    Err(StepError::StepFail {
        message: format!(
            "Timed out waiting for wallet '{wallet_name}' send readiness: \
            required {required_outputs} eligible UTXO(s) with value >= \
            {value_per_transaction} and eligible value >= {required_available}; \
            last observed: available={last_available_outputs}/{last_available_value}, \
            eligible={last_eligible_outputs}/{last_eligible_value}"
        ),
    })
}

async fn execute_non_stop_manual_command(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
) -> Result<(), StepError> {
    match command {
        ManualCommand::CreateBlockchainSnapshotAllNodes { snapshot_name } => {
            execute_create_blockchain_snapshot_all_nodes(world, snapshot_name)
        }
        ManualCommand::CreateBlockchainSnapshotNode {
            snapshot_name,
            node_name,
        } => execute_create_blockchain_snapshot_node(world, snapshot_name, node_name),
        ManualCommand::CoinSplit {
            wallet,
            outputs,
            value,
        } => execute_coin_split(world, step, wallet, *outputs, *value, None)
            .await
            .map(|_| ()),
        ManualCommand::Verify { .. } => handle_verify_command(world, step, command).await,
        ManualCommand::WalletBalance { wallet_name } => {
            log_wallet_balance(world, step, wallet_name).await
        }
        ManualCommand::WalletBalanceAllUserWallets => {
            log_wallet_balances(world, step, world.all_user_wallets()).await
        }
        ManualCommand::WalletBalanceAllFundingWallets => {
            log_wallet_balances(world, step, world.all_funding_wallets()).await
        }
        ManualCommand::WalletBalanceAllWallets => {
            let mut wallets = world.all_user_wallets();
            wallets.extend(world.all_funding_wallets());

            log_wallet_balances(world, step, wallets).await
        }
        ManualCommand::ClearEncumbrances { wallet_name } => {
            clear_wallet_encumbrances(world, step, wallet_name)
        }
        ManualCommand::ClearEncumbrancesAllWallets => clear_all_wallet_encumbrances(world, step),
        ManualCommand::Send {
            transactions,
            value,
            from,
            to,
        } => execute_send(world, step, *transactions, *value, from, to, None).await,
        ManualCommand::ContinuousRoundRobinUserWallets { .. } => {
            execute_continuous_round_robin(world, step, command).await
        }
        ManualCommand::FaucetFundsAllUserWallets { rounds } => {
            request_faucet_funds_all_user_wallets(world, step, *rounds)
        }
        ManualCommand::FaucetFundsAllFundingWallets { rounds } => {
            request_faucet_funds_all_funding_wallets(world, step, *rounds)
        }
        ManualCommand::RestartNode { node_name } => restart_node(world, step, node_name).await,
        ManualCommand::CryptarchiaInfoAllNodes => {
            manual_nodes::utils::get_cryptarchia_info_all_nodes(world, step).await;
            Ok(())
        }
        ManualCommand::WaitAllNodesSyncedToChain => {
            wait_for_all_nodes_to_be_synced_to_chain(world, step).await
        }
        ManualCommand::CoinSplitAllUserWallets {
            splits_per_wallet,
            outputs,
            value,
        } => {
            execute_coin_splits_all_user_wallets(world, step, *splits_per_wallet, *outputs, *value)
                .await
        }
        ManualCommand::VerifyMinAvailableOutputsAllUserWallets {
            min_outputs,
            timeout_seconds,
        } => {
            verify_min_outputs_all_user_wallets(
                world,
                step,
                *min_outputs,
                *timeout_seconds,
                WalletOutputState::Available,
            )
            .await
        }
        ManualCommand::ContinuousNextWalletUserWallets { .. } => {
            execute_continuous_next_wallet_user_wallet(world, step, command).await
        }
        ManualCommand::Stop => Ok(()),
    }
}

async fn log_wallet_balances(
    world: &mut CucumberWorld,
    step: &str,
    wallets: Vec<WalletInfo>,
) -> StepResult {
    for wallet in wallets {
        log_wallet_balance(world, step, &wallet.wallet_name).await?;
    }

    Ok(())
}

async fn log_wallet_balance(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> StepResult {
    let available =
        utils::current_wallet_balance(world, step, wallet_name, WalletOutputState::Available)
            .await?;

    let reserved =
        utils::current_wallet_balance(world, step, wallet_name, WalletOutputState::Reserved)
            .await?;

    let on_chain =
        utils::current_wallet_balance(world, step, wallet_name, WalletOutputState::OnChain).await?;

    info!(
        target: TARGET,
        "Wallet `{wallet_name}` [Available] {}/{} LGO, [Encumbered] {}/{} LGO, \
        [On-chain] {}/{} LGO",
        available.output_count,
        available.value,
        reserved.output_count,
        reserved.value,
        on_chain.output_count,
        on_chain.value,
    );

    Ok(())
}

fn clear_wallet_encumbrances(
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

    world.with_wallets_mut(|wallets| wallets.clear_encumbrances(wallet_name))?;
    world.fee_state.clear_wallet_reservations(wallet_name);
    info!(target: TARGET, "Cleared encumbrances for wallet '{wallet_name}'");
    Ok(())
}

fn clear_all_wallet_encumbrances(world: &mut CucumberWorld, step: &str) -> StepResult {
    let wallet_names: Vec<String> = world.wallet_info.keys().cloned().collect();

    for wallet_name in wallet_names {
        clear_wallet_encumbrances(world, step, &wallet_name)?;
    }
    info!(target: TARGET, "Cleared encumbrances for all wallets");
    Ok(())
}

fn execute_create_blockchain_snapshot_all_nodes(
    world: &CucumberWorld,
    snapshot_name: &str,
) -> Result<(), StepError> {
    if world.nodes_info.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "cannot create snapshot: no running nodes".to_owned(),
        });
    }

    create_snapshots_all_nodes(world, snapshot_name)
}

fn execute_create_blockchain_snapshot_node(
    world: &CucumberWorld,
    snapshot_name: &str,
    node_name: &str,
) -> Result<(), StepError> {
    if world.nodes_info.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "cannot create snapshot: no running nodes".to_owned(),
        });
    }

    if let Some(info) = world.nodes_info.get(node_name) {
        save_named_blockchain_snapshot(snapshot_name, node_name, &info.runtime_dir)?;
        info!(
            target: TARGET,
            "Saved blockchain snapshot `{snapshot_name}` for node {}",
            info.runtime_dir.display()
        );
        Ok(())
    } else {
        Err(StepError::InvalidArgument {
            message: format!("Node {node_name} does not exist"),
        })
    }
}

async fn handle_verify_command(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
) -> Result<(), StepError> {
    let ManualCommand::Verify {
        wallet,
        outputs,
        value,
        time_out,
        wallet_state_type,
        verify_max,
    } = command
    else {
        unreachable!("handle_verify_command must be called with ManualCommand::Verify")
    };

    let verify_min = !*verify_max;
    utils::wait_for_wallet_output_state(
        world,
        step,
        wallet.clone(),
        if verify_min { outputs.as_ref() } else { None },
        if *verify_max { outputs.as_ref() } else { None },
        if verify_min { value.as_ref() } else { None },
        if *verify_max { value.as_ref() } else { None },
        *time_out,
        *wallet_state_type,
    )
    .await
}

fn request_faucet_funds_all_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
    rounds: usize,
) -> Result<(), StepError> {
    let number_of_rounds = NonZero::new(rounds).ok_or_else(|| StepError::InvalidArgument {
        message: "Invalid value for 'rounds': '0'".to_owned(),
    })?;
    let all_wallets_pk_hex = world
        .wallet_info
        .values()
        .filter(|w| w.is_user_wallet())
        .map(WalletInfo::public_key_hex)
        .collect::<Vec<_>>();
    utils::request_faucet_funds(world, step, number_of_rounds, &all_wallets_pk_hex)
}

fn request_faucet_funds_all_funding_wallets(
    world: &mut CucumberWorld,
    step: &str,
    rounds: usize,
) -> Result<(), StepError> {
    let number_of_rounds = NonZero::new(rounds).ok_or_else(|| StepError::InvalidArgument {
        message: "Invalid value for 'rounds': '0'".to_owned(),
    })?;
    let all_wallets_pk_hex = world
        .wallet_info
        .values()
        .filter(|w| w.is_funding_wallet())
        .map(WalletInfo::public_key_hex)
        .collect::<Vec<_>>();
    utils::request_faucet_funds(world, step, number_of_rounds, &all_wallets_pk_hex)
}

async fn execute_coin_split(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    outputs: usize,
    value: u64,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<Vec<TxHash>, StepError> {
    let wallet = world.resolve_wallet(wallet_name)?;
    let self_pk = wallet.public_key()?;
    let receivers = vec![(self_pk, value); outputs];
    utils::create_and_submit_transaction_hashes(
        world,
        step,
        wallet_name,
        &receivers,
        best_node_info,
    )
    .await
}

async fn execute_coin_split_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    outputs: usize,
    value: u64,
    best_node_info: Option<&BestNodeInfo>,
    available_utxos: &mut WalletUtxos,
) -> Result<Vec<TxHash>, StepError> {
    let wallet = world.resolve_wallet(wallet_name)?;
    let self_pk = wallet.public_key()?;
    let receivers = vec![(self_pk, value); outputs];
    utils::create_and_submit_transaction_hashes_with_utxo_cache(
        world,
        step,
        wallet_name,
        &receivers,
        best_node_info,
        Some(available_utxos),
    )
    .await
}

async fn prepare_signed_submissions_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    requests: Vec<(String, Vec<(ZkPublicKey, u64)>)>,
    available_utxos: &mut WalletUtxos,
) -> Result<Vec<utils::SignedUserWalletSubmission>, StepError> {
    let mut reserved_submissions = Vec::with_capacity(requests.len());

    for (sender, receivers) in requests {
        let reserved_submission =
            utils::reserve_user_wallet_transaction_submission_with_utxo_cache(
                world,
                step,
                &sender,
                &receivers,
                available_utxos,
            )
            .await?;
        reserved_submissions.push(reserved_submission);
    }

    utils::finalize_reserved_user_wallet_submissions_concurrently(step, reserved_submissions).await
}

async fn prepare_coin_splits_all_wallets_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    wallet_names: &[String],
    outputs: usize,
    value: u64,
    available_utxos: &mut WalletUtxos,
) -> Result<
    (
        Vec<utils::SignedUserWalletSubmission>,
        BTreeMap<String, usize>,
    ),
    StepError,
> {
    let mut requests = Vec::with_capacity(wallet_names.len());
    let mut prepared_counts = BTreeMap::new();

    for wallet_name in wallet_names {
        let wallet = world.resolve_wallet(wallet_name)?;
        let self_pk = wallet.public_key()?;
        let receivers = vec![(self_pk, value); outputs];
        *prepared_counts.entry(wallet_name.clone()).or_insert(0usize) += 1;
        requests.push((wallet_name.clone(), receivers));
    }

    let signed_submissions =
        prepare_signed_submissions_with_utxo_cache(world, step, requests, available_utxos).await?;
    Ok((signed_submissions, prepared_counts))
}

async fn execute_send(
    world: &mut CucumberWorld,
    step: &str,
    transactions: usize,
    value: u64,
    from: &str,
    to: &str,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<(), StepError> {
    let receiver_wallet = world.resolve_wallet(to)?;
    let receiver_pk = receiver_wallet.public_key()?;
    for i in 0..transactions {
        let result = utils::create_and_submit_transaction(
            world,
            step,
            from,
            &[(receiver_pk, value)],
            best_node_info,
        )
        .await;

        if let Err(StepError::WalletError(WalletError::InsufficientFunds { available })) = result {
            return Err(StepError::FundsDeficit {
                available,
                num_utxos_required: transactions - i,
                value_per_utxos_required: value,
            });
        }
        result?;
    }
    Ok(())
}

async fn prepare_ring_send_round_send_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    transactions: usize,
    value: u64,
    from: &str,
    to: &str,
    available_utxos: &mut WalletUtxos,
) -> Result<Vec<utils::SignedUserWalletSubmission>, StepError> {
    let receiver_wallet = world.resolve_wallet(to)?;
    let receiver_pk = receiver_wallet.public_key()?;
    let mut reserved_submissions = Vec::with_capacity(transactions);

    for i in 0..transactions {
        let sender_utxo_count_before = available_utxos.get(from).map_or(0usize, Vec::len);

        let receivers = vec![(receiver_pk, value)];
        let reserved_submission =
            utils::reserve_user_wallet_transaction_submission_with_utxo_cache(
                world,
                step,
                from,
                &receivers,
                available_utxos,
            )
            .await
            .map_err(|error| match error {
                StepError::WalletError(WalletError::InsufficientFunds { available }) => {
                    StepError::FundsDeficit {
                        available,
                        num_utxos_required: transactions - i,
                        value_per_utxos_required: value,
                    }
                }
                error => error,
            })?;
        let sender_utxo_count_after = available_utxos.get(from).map_or(0usize, Vec::len);

        if transactions > 1 && sender_utxo_count_after >= sender_utxo_count_before {
            return Err(StepError::LogicalError {
                message: format!(
                    "Batch cache accounting failed for '{from}': expected available input count to \
                    decrease between submissions ({sender_utxo_count_before} -> {sender_utxo_count_after})"
                ),
            });
        }

        reserved_submissions.push(reserved_submission);
    }

    utils::finalize_reserved_user_wallet_submissions_concurrently(step, reserved_submissions).await
}

fn destructure_round_robin_command(
    command: &ManualCommand,
) -> Result<(usize, u64, usize, u64, usize), StepError> {
    let ManualCommand::ContinuousRoundRobinUserWallets {
        coin_split_outputs,
        coin_split_value,
        transactions,
        value,
        cycles,
    } = command
    else {
        return Err(StepError::LogicalError {
            message: "expected ContinuousRoundRobinUserWallets command".to_owned(),
        });
    };
    Ok((
        *coin_split_outputs,
        *coin_split_value,
        *transactions,
        *value,
        *cycles,
    ))
}

fn all_user_wallets(world: &CucumberWorld) -> Result<Vec<String>, StepError> {
    let mut wallet_names = world
        .all_user_wallets()
        .iter()
        .map(|w| w.wallet_name.clone())
        .collect::<Vec<_>>();
    if wallet_names.len() < 2 {
        return Err(StepError::InvalidArgument {
            message: "This command requires at least two user wallets".to_owned(),
        });
    }
    wallet_names.sort();
    Ok(wallet_names)
}

#[expect(
    clippy::cognitive_complexity,
    reason = "This function has multiple steps that are logically distinct."
)]
#[expect(
    clippy::too_many_lines,
    reason = "This function has multiple steps that are logically distinct."
)]
async fn execute_continuous_round_robin(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
) -> Result<(), StepError> {
    let (coin_split_outputs, coin_split_value, transactions, value, cycles) =
        destructure_round_robin_command(command)?;
    let wallet_names = all_user_wallets(world)?;
    let required_sum = coin_split_outputs as u64 * coin_split_value;
    let mut all_round_robin_tx_hashes = HashSet::new();

    for cycle in 0..cycles {
        info!(
            target: TARGET,
            "CONTINUOUS ROUND ROBIN cycle {} A: Wait for available funds all wallets",
            cycle + 1
        );

        for sender in &wallet_names {
            if let Err(e) = wait_for_available_value(world, step, sender, required_sum, 300).await {
                warn!(target: TARGET, "Step `{}` error in cycle {}: {e}", step, cycle + 1);
            }
        }

        let split_tx_hashes = coin_splits_for_round_robin(
            world,
            step,
            &wallet_names,
            coin_split_outputs,
            coin_split_value,
            cycle,
        )
        .await?;
        wait_for_n_blocks_or_warn(world, step, &wallet_names, Duration::from_mins(3), 2, cycle)
            .await?;

        verify_transactions_mined(
            world,
            step,
            &split_tx_hashes,
            wallet_names.len(),
            Some(cycle + 1),
            "CONTINUOUS ROUND ROBIN",
            "B",
        )
        .await?;

        info!(
            target: TARGET,
            "CONTINUOUS ROUND ROBIN cycle {} C: Wait all wallets ready and build UTXO cache",
            cycle + 1
        );

        // Build fresh cache snapshot after phase C completes

        // Refresh all sender cache entries before phase D begins and update the
        // available utxo cache
        let best_node_info = get_best_node_info_choose(world, &wallet_names).await?;
        let mut available_utxos = WalletUtxos::new();
        let required_available = transactions as u64 * value;
        for sender in &wallet_names {
            if let Err(e) = wait_wallet_send_ready(
                world,
                sender,
                180,
                required_available,
                WalletSendReadiness::EligibleUtxoBatch {
                    required_outputs: transactions,
                    value_per_transaction: value,
                },
                Some(&mut available_utxos),
            )
            .await
            {
                warn!(target: TARGET, "Step `{}` error in cycle {}: Failed to refresh cache for sender '{}': {e}", step, cycle + 1, sender);
            }
        }

        info!(
            target: TARGET,
            "CONTINUOUS ROUND ROBIN cycle {} D: Send all round robin transactions with UTXO cache",
            cycle + 1
        );

        let mut signed_submissions = Vec::with_capacity(wallet_names.len() * transactions);
        let mut prepared_counts = BTreeMap::new();
        for sender in &wallet_names {
            let recipients = recipient_wallets(&wallet_names, sender)?;
            let prepared = prepare_round_robin_with_utxo_cache(
                world,
                step,
                sender,
                &recipients,
                transactions,
                value,
                &mut available_utxos,
            )
            .await;

            match prepared {
                Ok(mut prepared) => {
                    prepared_counts.insert(sender.clone(), prepared.len());
                    signed_submissions.append(&mut prepared);
                }
                Err(e) => {
                    warn!(target: TARGET, "Step `{}` error in cycle {}: {e}", step, cycle + 1);
                }
            }
        }
        log_phase_d_counts(
            "CONTINUOUS ROUND ROBIN",
            cycle,
            "prepared",
            &prepared_counts,
        );

        let submitted_hashes = utils::submit_signed_user_wallet_submissions_concurrently(
            world,
            signed_submissions,
            Some(&best_node_info),
        )
        .await?;
        let mut submitted_counts = BTreeMap::new();
        for (sender, _) in &submitted_hashes {
            *submitted_counts.entry(sender.clone()).or_insert(0usize) += 1;
        }
        log_phase_d_counts(
            "CONTINUOUS ROUND ROBIN",
            cycle,
            "submitted",
            &submitted_counts,
        );
        let cycle_tx_hashes = submitted_hashes
            .into_iter()
            .map(|(_, tx_hash)| tx_hash)
            .collect::<HashSet<_>>();

        // Assert transaction count
        verify_transactions_mined(
            world,
            step,
            &cycle_tx_hashes,
            wallet_names.len() * transactions,
            Some(cycle + 1),
            "CONTINUOUS ROUND ROBIN",
            "E",
        )
        .await?;

        // Collect hashes for final drain verification
        extend_tx_hash_set(&mut all_round_robin_tx_hashes, &cycle_tx_hashes);
    }

    // Final drain: verify submitted D-phase transaction hashes are observed in
    // chain blocks.
    info!(
        target: TARGET,
        "CONTINUOUS ROUND ROBIN final: Verify {} submitted round-robin transaction(s) were observed \
        in chain blocks",
        all_round_robin_tx_hashes.len(),
    );

    wait_for_observed_transaction_hashes(
        world,
        step,
        &all_round_robin_tx_hashes,
        Duration::from_mins(3),
    )
    .await?;

    info!(
        target: TARGET,
        "CONTINUOUS ROUND ROBIN scenario complete: {} transaction(s) verified from observed chain block transaction hashes across {} cycle(s)",
        all_round_robin_tx_hashes.len(),
        cycles
    );

    Ok(())
}

async fn wait_for_n_blocks_or_warn(
    world: &CucumberWorld,
    step: &str,
    wallet_names: &[String],
    time_out: Duration,
    blocks_to_wait: u64,
    cycle: usize,
) -> Result<(), StepError> {
    if wallet_names.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "No wallet names provided for wait_for_n_blocks".to_owned(),
        });
    }
    let best_node_info = get_best_node_info_choose(world, wallet_names).await?;
    let node = world
        .resolve_node_http_client(&best_node_info.best_node_for_wallet(world, &wallet_names[0])?)?;
    let start_height = node.consensus_info().await?.cryptarchia_info.height;
    let start = Instant::now();
    loop {
        sleep(Duration::from_millis(200)).await;
        let best_node_info = get_best_node_info_choose(world, wallet_names).await?;
        let node = world.resolve_node_http_client(
            &best_node_info.best_node_for_wallet(world, &wallet_names[0])?,
        )?;
        let height = node.consensus_info().await?.cryptarchia_info.height;
        if height >= start_height + blocks_to_wait {
            return Ok(());
        }
        if start.elapsed() > time_out {
            warn!(
                target: TARGET,
                "Step `{step}` cycle {}: Chain could not grow by {blocks_to_wait} blocks in {time_out:.2?}",
                cycle + 1
            );
            return Ok(());
        }
    }
}

async fn coin_splits_for_round_robin(
    world: &mut CucumberWorld,
    step: &str,
    wallet_names: &[String],
    coin_split_outputs: usize,
    coin_split_value: u64,
    cycle: usize,
) -> Result<HashSet<TxHash>, StepError> {
    info!(target: TARGET, "CONTINUOUS ROUND ROBIN cycle {} B: Perform coin splits all wallets", cycle + 1);

    let best_node_info = get_best_node_info_choose(world, wallet_names).await?;
    let mut split_available_utxos =
        utils::current_available_utxos_for_user_wallets(world, step).await?;
    let (signed_split_submissions, prepared_split_counts) =
        prepare_coin_splits_all_wallets_with_utxo_cache(
            world,
            step,
            wallet_names,
            coin_split_outputs,
            coin_split_value,
            &mut split_available_utxos,
        )
        .await?;
    log_phase_counts(
        "CONTINUOUS ROUND ROBIN",
        cycle,
        "B",
        "split prepared",
        &prepared_split_counts,
    );

    let submitted_split_hashes = utils::submit_signed_user_wallet_submissions_concurrently(
        world,
        signed_split_submissions,
        Some(&best_node_info),
    )
    .await?;
    let mut submitted_split_counts = BTreeMap::new();
    for (sender, _) in &submitted_split_hashes {
        *submitted_split_counts
            .entry(sender.clone())
            .or_insert(0usize) += 1;
    }
    log_phase_counts(
        "CONTINUOUS ROUND ROBIN",
        cycle,
        "B",
        "split submitted",
        &submitted_split_counts,
    );
    Ok(submitted_split_hashes
        .into_iter()
        .map(|(_, tx_hash)| tx_hash)
        .collect::<HashSet<_>>())
}

fn recipient_wallets(wallet_names: &[String], sender: &str) -> Result<Vec<String>, StepError> {
    let recipients: Vec<_> = wallet_names
        .iter()
        .filter(|wallet| wallet.as_str() != sender)
        .cloned()
        .collect();
    if recipients.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("No recipient wallets available for sender '{sender}'"),
        });
    }

    Ok(recipients)
}

async fn prepare_round_robin_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    sender: &str,
    recipients: &[String],
    transactions: usize,
    value: u64,
    available_utxos: &mut WalletUtxos,
) -> Result<Vec<utils::SignedUserWalletSubmission>, StepError> {
    let mut reserved_submissions = Vec::with_capacity(transactions);

    for i in 0..transactions {
        let receiver_name = &recipients[i % recipients.len()];
        let receiver_wallet = world.resolve_wallet(receiver_name)?;
        let receiver_pk = receiver_wallet.public_key()?;

        let receivers = vec![(receiver_pk, value)];
        let reserved_submission =
            utils::reserve_user_wallet_transaction_submission_with_utxo_cache(
                world,
                step,
                sender,
                &receivers,
                available_utxos,
            )
            .await?;

        reserved_submissions.push(reserved_submission);
    }

    utils::finalize_reserved_user_wallet_submissions_concurrently(step, reserved_submissions).await
}

async fn wait_for_available_value(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    required_value: u64,
    timeout_seconds: u64,
) -> Result<(), StepError> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(timeout_seconds) {
        let balance =
            utils::current_wallet_balance(world, step, wallet_name, WalletOutputState::Available)
                .await?;
        if balance.value >= required_value {
            return Ok(());
        }
        sleep(Duration::from_millis(200)).await;
    }

    Err(StepError::StepFail {
        message: format!(
            "Timed out waiting for wallet '{wallet_name}' to have at least {required_value} available LGO"
        ),
    })
}

#[expect(
    clippy::cognitive_complexity,
    reason = "Singular fn with multiple branches to handle different events and futures."
)]
pub async fn perform_manual_step_control(
    world: &mut CucumberWorld,
    step: &str,
    timeout_seconds: u64,
) -> Result<(), StepError> {
    let command_file =
        env::var(MANUAL_COMMAND_FILE_ENV).map_err(|_| StepError::InvalidArgument {
            message: format!(
                "Step `{step}` requires environment variable '{MANUAL_COMMAND_FILE_ENV}' to be set",
            ),
        })?;
    let poll_interval_ms = env::var(MANUAL_COMMAND_POLL_INTERVAL_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(300);

    info!(
        target: TARGET,
        "Manual control step started. Monitoring command file: `{command_file}`"
    );

    let time_out = Duration::from_secs(timeout_seconds);
    let start = Instant::now();
    while start.elapsed() < time_out {
        if let Some(command) = take_next_command(Path::new(&command_file))? {
            info!(target: TARGET, "====> manual command: {command:?}");
            if matches!(
                execute_manual_command(world, step, &command).await,
                Ok(true)
            ) {
                info!(
                    target: TARGET,
                   "Manual command loop stopped by STOP command after {:.2?}",
                   start.elapsed()
                );
                return Ok(());
            }
        } else {
            sleep(Duration::from_millis(poll_interval_ms)).await;
        }
    }
    info!(target: TARGET, "Manual command loop stopped by tine-out after {:.2?}", start.elapsed());

    Ok(())
}
