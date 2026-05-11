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

use std::{env, num::NonZero, path::Path, time::Duration};

use lb_wallet::WalletError;
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::cucumber::{
    error::StepError,
    steps::{
        TARGET, manual_nodes,
        manual_nodes::{
            snapshots::save_named_blockchain_snapshot,
            utils::{
                create_snapshots_all_nodes, restart_node, wait_for_all_nodes_to_be_synced_to_chain,
            },
        },
        manual_transactions::{
            best_node::get_best_node_info,
            command_file_parsing::{ManualCommand, take_next_command},
            utils,
            utils::{BestNodeInfo, WalletStateType},
        },
    },
    world::{CucumberWorld, WalletInfo},
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

    for wallet_name in &wallet_names {
        for _ in 0..splits_per_wallet {
            let best_node_info = get_best_node_info(world, wallet_name).await?;
            execute_coin_split(
                world,
                step,
                wallet_name,
                outputs,
                value,
                Some(&best_node_info),
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
    wallet_state_type: WalletStateType,
) -> Result<(), StepError> {
    let mut wallet_names: Vec<_> = world
        .all_user_wallets()
        .iter()
        .map(|w| w.wallet_name.clone())
        .collect();
    wallet_names.sort();

    for wallet_name in &wallet_names {
        utils::wait_for_wallet_or_encumbered_state(
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

#[expect(clippy::cognitive_complexity, reason = "Function has been simplified.")]
pub(crate) async fn execute_continuous_next_wallet_user_wallet(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
) -> Result<(), StepError> {
    let (cycles, transactions_per_wallet, value) = destructure_next_wallet_command(command)?;
    let wallet_names = all_user_wallets(world)?;

    let required_value = transactions_per_wallet as u64 * value;
    for cycle in 0..cycles {
        info!(
            target: TARGET,
            "CONTINUOUS NEXT WALLET cycle {} A: Await funds & send transactions to next wallet",
            cycle + 1
        );
        execute_ring_send_round(world, step, &wallet_names, transactions_per_wallet, value).await?;

        info!(
            target: TARGET,
            "CONTINUOUS NEXT WALLET cycle {} B: Verify available funds reverse order",
            cycle + 1
        );
        verify_reverse_wallet_available_value(world, step, &wallet_names, required_value, 300)
            .await?;

        info!(
            target: TARGET,
            "CONTINUOUS NEXT WALLET cycle {} C: Refresh user wallet balances",
            cycle + 1
        );
        utils::update_wallet_balance_all_user_wallets(world, step, None).await?;
    }

    Ok(())
}

async fn execute_ring_send_round(
    world: &mut CucumberWorld,
    step: &str,
    wallet_names: &[String],
    transactions_per_wallet: usize,
    value: u64,
) -> Result<(), StepError> {
    for i in 0..wallet_names.len() {
        let from = &wallet_names[i];
        let to = &wallet_names[(i + 1) % wallet_names.len()];

        wait_wallet_send_ready(world, from, 180).await?;

        let best_node_info = get_best_node_info(world, from).await?;
        let send_result = execute_send(
            world,
            step,
            transactions_per_wallet,
            value,
            from,
            to,
            Some(&best_node_info),
        )
        .await;

        if let Err(err) = send_result {
            match err {
                StepError::WalletError(WalletError::InsufficientFunds { .. }) => {
                    // Wait for all ongoing transactions to be mined so that any change may be
                    // returned, then submit repeated coin split transactions until the balance
                    // cannot be split anymore. These coin splits will consume all available UTXOs.
                    info!(
                        target: TARGET,
                        "Wallet '{}' has insufficient funds for sending all required, performing \
                        coin split(s) to refresh UTXOs",
                        from
                    );
                    wait_wallet_send_ready(world, from, 180).await?;
                    loop {
                        let (_, available) = utils::get_wallet_balances(
                            world,
                            "execute_ring_send_round",
                            from,
                            WalletStateType::Available,
                        )
                        .await?;
                        if available > value * 2 {
                            execute_coin_split(
                                world,
                                step,
                                from,
                                usize::try_from(available / value).unwrap().min(250),
                                value,
                                Some(&best_node_info),
                            )
                            .await?;
                        } else {
                            return Ok(());
                        }
                    }
                }
                _ => return Err(err),
            }
        }
    }

    Ok(())
}

async fn wait_wallet_send_ready(
    world: &mut CucumberWorld,
    wallet_name: &str,
    timeout_seconds: u64,
) -> Result<(), StepError> {
    let start = Instant::now();
    let mut last_encumbered = 0usize;

    while start.elapsed() < Duration::from_secs(timeout_seconds) {
        let (encumbered_count, _) = utils::get_wallet_balances(
            world,
            "wait_wallet_send_ready",
            wallet_name,
            WalletStateType::Encumbered,
        )
        .await?;

        last_encumbered = encumbered_count;

        if encumbered_count == 0 {
            return Ok(());
        }

        sleep(Duration::from_millis(300)).await;
    }

    Err(StepError::StepFail {
        message: format!(
            "Timed out waiting for wallet '{wallet_name}' send readiness: required encumbered \
            count == 0 ({last_encumbered})"
        ),
    })
}

async fn verify_reverse_wallet_available_value(
    world: &mut CucumberWorld,
    step: &str,
    wallet_names: &[String],
    required_value: u64,
    timeout_seconds: u64,
) -> Result<(), StepError> {
    for wallet_name in wallet_names.iter().rev() {
        wait_for_available_value(world, step, wallet_name, required_value, timeout_seconds).await?;
    }

    Ok(())
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
        } => execute_coin_split(world, step, wallet, *outputs, *value, None).await,
        ManualCommand::Verify { .. } => handle_verify_command(world, step, command).await,
        ManualCommand::WalletBalance { wallet_name } => {
            utils::update_wallet_balance(world, step, wallet_name).await?;
            Ok(())
        }
        ManualCommand::WalletBalanceAllUserWallets => {
            utils::update_wallet_balance_all_user_wallets(world, step, None).await?;
            Ok(())
        }
        ManualCommand::WalletBalanceAllFundingWallets => {
            utils::update_wallet_balance_all_funding_wallets(world, step, None).await?;
            Ok(())
        }
        ManualCommand::WalletBalanceAllWallets => {
            utils::update_wallet_balance_all_wallets(world, step, None).await?;
            Ok(())
        }
        ManualCommand::ClearEncumbrances { wallet_name } => {
            utils::clear_wallet_encumbrances(world, step, wallet_name)
        }
        ManualCommand::ClearEncumbrancesAllWallets => {
            utils::clear_all_wallet_encumbrances(world, step)
        }
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
                WalletStateType::Available,
            )
            .await
        }
        ManualCommand::ContinuousNextWalletUserWallets { .. } => {
            execute_continuous_next_wallet_user_wallet(world, step, command).await
        }
        ManualCommand::Stop => Ok(()),
    }
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
    utils::wait_for_wallet_or_encumbered_state(
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
) -> Result<(), StepError> {
    let wallet = world.resolve_wallet(wallet_name)?;
    let self_pk = wallet.public_key()?;
    let receivers = vec![(self_pk, value); outputs];
    utils::create_and_submit_transaction(world, step, wallet_name, &receivers, best_node_info)
        .await?;
    Ok(())
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
    for _ in 0..transactions {
        utils::create_and_submit_transaction(
            world,
            step,
            from,
            &[(receiver_pk, value)],
            best_node_info,
        )
        .await?;
    }
    Ok(())
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
        info!(target: TARGET, "CONTINUOUS ROUND ROBIN cycle {} B: Perform coin splits all wallets", cycle + 1);
        for sender in &wallet_names {
            let best_node_info = get_best_node_info(world, sender).await?;
            if let Err(e) = execute_coin_split(
                world,
                step,
                sender,
                coin_split_outputs,
                coin_split_value,
                Some(&best_node_info),
            )
            .await
            {
                warn!(target: TARGET, "Step `{}` error in cycle {}: {e}", step, cycle + 1);
            }
        }
        info!(
            target: TARGET,
            "CONTINUOUS ROUND ROBIN cycle {} C: Wait for coin splits to be mined all wallets",
            cycle + 1
        );
        for sender in &wallet_names {
            if let Err(e) = utils::wait_for_wallet_or_encumbered_state(
                world,
                step,
                sender.clone(),
                None,
                Some(&0),
                None,
                None,
                300,
                WalletStateType::Encumbered,
            )
            .await
            {
                warn!(target: TARGET, "Step `{}` error in cycle {}: {e}", step, cycle + 1);
            }
        }
        info!(
            target: TARGET,
            "CONTINUOUS ROUND ROBIN cycle {} D: Send transactions to peers all wallets",
            cycle + 1
        );
        for sender in &wallet_names {
            let best_node_info = get_best_node_info(world, sender).await?;
            let recipients = recipient_wallets(&wallet_names, sender)?;
            if let Err(e) = send_round_robin(
                world,
                step,
                sender,
                &recipients,
                transactions,
                value,
                Some(&best_node_info),
            )
            .await
            {
                warn!(target: TARGET, "Step `{}` error in cycle {}: {e}", step, cycle + 1);
            }
        }
        info!(
            target: TARGET,
            "CONTINUOUS ROUND ROBIN cycle {} E: Wait for transactions to be mined all wallets",
            cycle + 1
        );
        for sender in &wallet_names {
            if let Err(e) = utils::wait_for_wallet_or_encumbered_state(
                world,
                step,
                sender.clone(),
                None,
                Some(&0),
                None,
                None,
                300,
                WalletStateType::Encumbered,
            )
            .await
            {
                warn!(target: TARGET, "Step `{}` error in cycle {}: {e}", step, cycle + 1);
            }
        }
    }

    Ok(())
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

async fn send_round_robin(
    world: &mut CucumberWorld,
    step: &str,
    sender: &str,
    recipients: &[String],
    transactions: usize,
    value: u64,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<(), StepError> {
    for i in 0..transactions {
        let receiver_name = &recipients[i % recipients.len()];
        let receiver_wallet = world.resolve_wallet(receiver_name)?;
        let receiver_pk = receiver_wallet.public_key()?;
        utils::create_and_submit_transaction(
            world,
            step,
            sender,
            &[(receiver_pk, value)],
            best_node_info,
        )
        .await?;
    }
    Ok(())
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
        let (_, value) =
            utils::get_wallet_balances(world, step, wallet_name, WalletStateType::Available)
                .await?;
        if value >= required_value {
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
