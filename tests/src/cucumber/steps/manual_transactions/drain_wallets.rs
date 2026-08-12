use std::{
    collections::{HashMap, HashSet},
    num::NonZeroUsize,
    time::Duration,
};

use lb_core::mantle::{
    Note, Utxo,
    gas::{GasCost, MainnetGasProfile},
    ledger::MAX_TRANSACTION_INPUTS,
    transactions::{
        GENESIS_EXECUTION_GAS_PRICE, GasPrices, MantleTxBuilder, MantleTxContext,
        MantleTxGasContext, OpsProofs,
    },
};
use lb_key_management_system_service::keys::ZkPublicKey;
use tracing::{info, warn};

use crate::{
    common::wallet::{
        WalletId, WalletInputSelectionStrategy, WalletTransactionIntent,
        extend_wallet_funding_inputs,
    },
    cucumber::{
        error::{StepError, StepResult},
        fee_reserve::DEFAULT_STORAGE_GAS_PRICE,
        steps::TARGET,
        utils::{pk_to_hex, tx_hash_to_hex},
        wallet::{
            checks::wait_for_observed_transaction_hashes,
            submissions::{
                prepare_user_wallet_transaction_submission_with_change_and_strategy,
                submit_node_wallet_transfer, submit_prepared_user_wallet_transaction,
                wait_for_transactions_inclusion,
            },
            sync::current_available_utxos_for_user_wallets,
        },
        world::{CucumberWorld, WalletInfo},
    },
};

const DRAIN_MAX_INPUTS_PER_TRANSACTION: usize = MAX_TRANSACTION_INPUTS;
const DRAIN_TRANSACTION_TIMEOUT: Duration = Duration::from_mins(3);

// Select the smallest low-value-first tranche that can pay its own fee,
// otherwise best effort.
fn select_tranche_utxos(utxos: &[Utxo]) -> Result<(Vec<Utxo>, u64, u64), StepError> {
    let mut fee_estimate;
    if utxos.len() <= DRAIN_MAX_INPUTS_PER_TRANSACTION {
        fee_estimate = estimate_drain_fee_with_inputs(utxos)?;
        return Ok((
            utxos.to_vec(),
            utxos.iter().map(|utxo| utxo.note.value).sum::<u64>(),
            fee_estimate,
        ));
    }

    let mut sorted_utxos = utxos.to_vec();
    sorted_utxos.sort_by_key(|utxo| utxo.note.value);

    let split_index = DRAIN_MAX_INPUTS_PER_TRANSACTION;

    let mut selected_set = sorted_utxos[..split_index].to_vec();
    let unselected_set = &sorted_utxos[split_index..];

    let mut selected_value = selected_set
        .iter()
        .fold(0u64, |total, utxo| total.saturating_add(utxo.note.value));

    fee_estimate = estimate_drain_fee_with_inputs(&selected_set)?;
    if selected_value > fee_estimate {
        return Ok((selected_set, selected_value, fee_estimate));
    }

    // Replace selected UTXOs progressively with every unselected UTXO.
    for (swap_offset, replacement) in unselected_set.iter().enumerate() {
        let selected_index = selected_set.len() - 1 - (swap_offset % selected_set.len());

        selected_value = selected_value
            .saturating_sub(selected_set[selected_index].note.value)
            .saturating_add(replacement.note.value);

        selected_set[selected_index] = *replacement;

        fee_estimate = estimate_drain_fee_with_inputs(&selected_set)?;
        if selected_value > fee_estimate {
            return Ok((selected_set, selected_value, fee_estimate));
        }
    }

    Ok((selected_set, selected_value, fee_estimate))
}

#[expect(
    clippy::cognitive_complexity,
    reason = "Singular fn with multiple branches to handle drain logic."
)]
#[expect(clippy::too_many_lines, reason = "Test function.")]
pub async fn drain_user_wallet(
    world: &mut CucumberWorld,
    step: &str,
    sender: &WalletInfo,
    receiver_pk: ZkPublicKey,
) -> StepResult {
    let mut available = current_available_utxos_for_user_wallets(world, step).await?;
    let mut submitted = 0usize;
    let mut tx_hashes = HashSet::new();

    'drain: loop {
        let sender_utxos = available
            .get(sender.wallet_name.as_str())
            .cloned()
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Wallet '{}' has no tracked UTXO state", sender.wallet_name),
            })?;
        if sender_utxos.is_empty() {
            info!(target: TARGET, "Drain wallet: `{}` has no outputs to drain", sender.wallet_name);
            break 'drain;
        }

        let mut fee_estimation_deficit = 0;
        'fee_retry: loop {
            let (tranche_utxos, tranche_total, fee_estimate) = select_tranche_utxos(&sender_utxos)?;

            let fee = fee_estimate.saturating_add(fee_estimation_deficit);
            if fee >= tranche_total {
                info!(
                    target: TARGET,
                    "Drain wallet: `{}` has insufficient funds to cover fees, {fee}>={tranche_total}, {}",
                    sender.wallet_name,
                    sender.public_key_hex(),
                );
                break 'drain;
            }
            let amount = tranche_total - fee;
            info!(
                target: TARGET,
                "Drain wallet: `{}`, selected count: {}, amount: {amount}, estimated fee: {fee}",
                sender.wallet_name,
                tranche_utxos.len(),
            );

            let intent = WalletTransactionIntent::transfer(
                &[(receiver_pk, amount)],
                DEFAULT_STORAGE_GAS_PRICE,
            )
            .map_err(|error| StepError::LogicalError {
                message: error.to_string(),
            })?;
            let mut tranche_available = HashMap::new();
            tranche_available.insert(WalletId::new(sender.wallet_name.clone()), tranche_utxos);

            let prepared_response =
                prepare_user_wallet_transaction_submission_with_change_and_strategy(
                    world,
                    step,
                    &sender.wallet_name,
                    intent,
                    Some(&tranche_available),
                    Some(receiver_pk),
                    WalletInputSelectionStrategy::AllProvided,
                )
                .await;

            let prepared = match prepared_response {
                Ok(prepared) => prepared,
                Err(error) if is_wallet_funds_deficit(&error) => {
                    fee_estimation_deficit = fee_estimation_deficit.saturating_add(1);
                    info!(
                        target: TARGET,
                        "Drain wallet: `{}` fee estimate was too low during preparation; \
                         retrying with fee estimation deficit {}",
                        sender.wallet_name,
                        fee_estimation_deficit,
                    );
                    continue 'fee_retry;
                }
                Err(error) => return Err(error),
            };

            let submit_response = submit_prepared_user_wallet_transaction(
                world,
                step,
                prepared,
                OpsProofs::empty(),
                None,
                Some(&mut available),
            )
            .await;
            match submit_response {
                Ok(tx_hash) => {
                    submitted += 1;
                    if !tx_hashes.insert(tx_hash) {
                        return Err(StepError::LogicalError {
                            message: format!(
                                "Drain wallet '{}' produced duplicate transaction {}",
                                sender.wallet_name,
                                tx_hash_to_hex(&tx_hash),
                            ),
                        });
                    }
                    info!(
                        target: TARGET,
                        "Drain wallet: Submitted tranche {submitted} from `{}` to `{}`: \
                        amount={amount}, fee={fee}, {}tx={}",
                        sender.wallet_name,
                        pk_to_hex(&receiver_pk),
                        if fee_estimation_deficit > 0 {
                            format!("fee_estimation_deficit={fee_estimation_deficit}, ")
                        } else {
                            String::new()
                        },
                        tx_hash_to_hex(&tx_hash),
                    );
                    break 'fee_retry;
                }
                Err(error) if is_wallet_funds_deficit(&error) => {
                    fee_estimation_deficit = fee_estimation_deficit.saturating_add(1);
                    info!(
                        target: TARGET,
                        "Drain wallet: `{}` fee estimate was too low during submission; \
                         retrying with fee estimation deficit {}",
                        sender.wallet_name,
                        fee_estimation_deficit,
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }

    wait_for_observed_transaction_hashes(world, step, &tx_hashes, Duration::from_mins(10)).await
}

fn is_wallet_funds_deficit(error: &StepError) -> bool {
    let is_deficit = match error {
        StepError::WalletError(lb_wallet::WalletError::InsufficientFunds { .. })
        | StepError::FundsDeficit { .. } => true,

        StepError::CommonHttpError(lb_common_http_client::Error::Server(message)) => {
            message.contains("Wallet does not have enough funds")
        }

        _ => false,
    };

    if !is_deficit {
        warn!(target: TARGET, "Drain wallet: submission is not retryable: {error}");
    }
    is_deficit
}

#[expect(
    clippy::cognitive_complexity,
    reason = "The drain flow keeps balance, fee, submission, and confirmation handling together"
)]
#[expect(clippy::too_many_lines, reason = "Test function.")]
pub async fn drain_node_wallet(
    world: &CucumberWorld,
    sender: &WalletInfo,
    receiver_pk: ZkPublicKey,
) -> StepResult {
    let node = world
        .nodes_info
        .get(&sender.node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!(
                "Node '{}' for wallet '{}' not found",
                sender.node_name, sender.wallet_name
            ),
        })?;
    let balance_response = node
        .started_node
        .client
        .wallet_balance(sender.public_key()?, None)
        .await;
    let Ok(mut balance) = balance_response else {
        info!(
            target: TARGET,
            "Drain wallet: `{}` has no funds yet, {}",
            sender.wallet_name,
            sender.public_key_hex(),
        );
        return Ok(());
    };
    info!(
        target: TARGET, "Drain wallet: `{}`, has {}/{} LGO",
        sender.wallet_name, balance.notes.len(), balance.balance
    );
    let mut remaining_tranches = balance
        .notes
        .len()
        .div_ceil(DRAIN_MAX_INPUTS_PER_TRANSACTION);
    let mut submitted = 0usize;

    while remaining_tranches > 0 {
        let input_count = balance.notes.len().min(DRAIN_MAX_INPUTS_PER_TRANSACTION);
        if input_count == 0 {
            break;
        }
        let mut fee = estimate_drain_fee_with_values(
            NonZeroUsize::new(input_count).expect("may not be zero"),
            balance.balance,
        )?;
        if fee >= balance.balance {
            info!(
                target: TARGET,
                "Drain wallet: `{}` has insufficient funds to cover fees, {fee}>={}, {}",
                sender.wallet_name,
                balance.balance,
                sender.public_key_hex(),
            );
            break;
        }
        let (tx_hash, amount) = loop {
            let amount = drain_amount(balance.balance, remaining_tranches, fee)?;
            info!(
                target: TARGET, "Drain wallet: `{}`, amount: {amount}, estimated fee: {fee}",
                sender.wallet_name,
            );
            let submit_response = submit_node_wallet_transfer(
                world,
                &sender.wallet_name,
                receiver_pk,
                amount,
                receiver_pk,
            )
            .await;
            match submit_response {
                Ok(hash) => break (hash, amount),
                Err(e) => {
                    if is_wallet_funds_deficit(&e) {
                        info!(
                            target: TARGET,
                            "Drain wallet: `{}` fee estimate {fee} was too low during transfer; \
                             retrying with higher fee {}",
                            sender.wallet_name,
                            fee + 1,
                        );
                        fee += 1;
                    } else {
                        return Err(e);
                    }
                }
            }
        };
        wait_for_transactions_inclusion(
            &node.started_node.client,
            &[tx_hash],
            DRAIN_TRANSACTION_TIMEOUT,
        )
        .await?;

        submitted += 1;
        remaining_tranches = remaining_tranches.saturating_sub(1);
        let balance_response = node
            .started_node
            .client
            .wallet_balance(sender.public_key()?, None)
            .await;
        let Ok(refreshed_balance) = balance_response else {
            info!(
                target: TARGET,
                "Drain wallet: `{}` has no funds remaining, {}",
                sender.wallet_name,
                sender.public_key_hex(),
            );
            break;
        };
        balance = refreshed_balance;
        info!(
            target: TARGET,
            "Drain wallet: Drained tranche {submitted} from `{}` to `{}`: amount={amount}, tx={}",
            sender.wallet_name,
            pk_to_hex(&receiver_pk),
            tx_hash_to_hex(&tx_hash),
        );
    }

    Ok(())
}

pub async fn drain_all_node_wallets(
    world: &CucumberWorld,
    node_name: &str,
    receiver_wallet_name: &str,
) -> StepResult {
    if world.fee_state.sponsored_genesis_account.is_some() {
        return Err(StepError::LogicalError {
            message: "Draining user wallets with sponsored fee accounts not supported".to_owned(),
        });
    }

    let receiver_pk = world.resolve_recipient(receiver_wallet_name)?.public_key;
    let node_wallets = world
        .all_node_wallets()
        .into_iter()
        .filter(|wallet| {
            wallet.node_name == node_name && wallet.wallet_name != receiver_wallet_name
        })
        .collect::<Vec<_>>();

    if node_wallets.is_empty() {
        return Err(StepError::LogicalError {
            message: format!("No wallets found for node `{node_name}`"),
        });
    }

    info!(target: TARGET, "Drain node wallets...");
    for wallet in node_wallets {
        drain_node_wallet(world, &wallet, receiver_pk).await?;
    }

    Ok(())
}

fn drain_amount(total: u64, tranches: usize, fee: u64) -> Result<u64, StepError> {
    let fee_total = fee
        .checked_mul(tranches as u64)
        .ok_or_else(|| StepError::LogicalError {
            message: "Drain fee total overflowed".to_owned(),
        })?;
    let distributable = total.checked_sub(fee_total).ok_or_else(|| {
        StepError::WalletError(lb_wallet::WalletError::InsufficientFunds { available: total })
    })?;
    let amount = distributable / tranches as u64;
    if amount == 0 {
        return Err(StepError::LogicalError {
            message: "Drain amount after fees is zero".to_owned(),
        });
    }
    Ok(amount)
}

fn estimate_drain_fee_with_values(input_count: NonZeroUsize, value: u64) -> Result<u64, StepError> {
    let input_value = value / input_count.get() as u64;
    let input_utxos = (0..input_count.get())
        .map(|index| {
            Utxo::new(
                [index as u8; 32],
                index,
                Note::new(input_value, ZkPublicKey::zero()),
            )
        })
        .collect::<Vec<_>>();

    estimate_drain_fee_with_inputs(&input_utxos)
}

fn estimate_drain_fee_with_inputs(input_utxos: &[Utxo]) -> Result<u64, StepError> {
    let builder = fee_estimate_builder()?;
    let builder = extend_wallet_funding_inputs(&builder, input_utxos).map_err(|error| {
        StepError::LogicalError {
            message: error.to_string(),
        }
    })?;
    finalize_fee(&builder)
}

fn fee_estimate_builder() -> Result<MantleTxBuilder, StepError> {
    MantleTxBuilder::new()
        // Simulate the receiver output. A correctly estimated drain consumes
        // the remaining input value as its fee and therefore has no change.
        .add_ledger_output(Note::new(0, ZkPublicKey::zero()))
        .map_err(|error| StepError::LogicalError {
            message: error.to_string(),
        })
}

fn finalize_fee(builder: &MantleTxBuilder) -> Result<u64, StepError> {
    let context = MantleTxContext {
        gas_context: MantleTxGasContext::new(
            HashMap::new(),
            HashMap::new(),
            GasPrices::new(
                GENESIS_EXECUTION_GAS_PRICE.into_inner(),
                DEFAULT_STORAGE_GAS_PRICE,
            ),
        ),
        ..MantleTxContext::default()
    };
    builder
        .minimum_gas_cost::<MainnetGasProfile>(&context)
        .map(GasCost::into_inner)
        .map_err(|error| StepError::LogicalError {
            message: error.to_string(),
        })
}
