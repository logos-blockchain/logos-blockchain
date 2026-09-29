use std::{collections::HashMap, time::Instant};

use lb_core::mantle::{
    Note,
    gas::MainnetGasProfile,
    transactions::{
        MantleTxBuilder,
        tx_list::ops::{OpsContext, OpsGasContext},
    },
};

use super::{
    BTreeMap, BestNodeInfo, CucumberWorld, GasPrices, HashSet, ManualCommand, NonZero,
    SignedUserWalletSubmission, StepError, TxHash, WalletError, WalletInfo, WalletSendReadiness,
    WalletUtxos, ZkPublicKey, dependent::DependentTransactionLoadState, sync, utils,
};
use crate::cucumber::steps::nodes::diagnostics::BlendDiagnosticEventLogger;

pub(super) async fn handle_verify_command(
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

pub(super) fn request_faucet_funds_all_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
    rounds: usize,
) -> Result<(), StepError> {
    let number_of_rounds = NonZero::new(rounds).ok_or_else(|| StepError::InvalidArgument {
        message: "Invalid value for 'rounds': '0'".to_owned(),
    })?;
    let all_wallets_pk_hex = world
        .wallet_registry
        .wallet_info
        .values()
        .filter(|w| w.is_user_wallet())
        .map(WalletInfo::public_key_hex)
        .collect::<Vec<_>>();
    utils::request_faucet_funds(world, step, number_of_rounds, &all_wallets_pk_hex)
}

pub(super) fn request_faucet_funds_all_funding_wallets(
    world: &mut CucumberWorld,
    step: &str,
    rounds: usize,
) -> Result<(), StepError> {
    let number_of_rounds = NonZero::new(rounds).ok_or_else(|| StepError::InvalidArgument {
        message: "Invalid value for 'rounds': '0'".to_owned(),
    })?;
    let all_wallets_pk_hex = world
        .wallet_registry
        .wallet_info
        .values()
        .filter(|wallet| wallet.is_node_funding_wallet())
        .map(WalletInfo::public_key_hex)
        .collect::<Vec<_>>();
    utils::request_faucet_funds(world, step, number_of_rounds, &all_wallets_pk_hex)
}

pub(super) async fn execute_coin_split(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    outputs: usize,
    value: u64,
) -> Result<Vec<TxHash>, StepError> {
    let wallet = world.resolve_wallet(wallet_name)?;
    let self_pk = wallet.public_key()?;
    let receivers = vec![(self_pk, value); outputs];

    let mut available_utxos = WalletUtxos::new();
    let best_node_info = sync::wait_wallet_send_ready(
        world,
        step,
        wallet_name,
        180,
        outputs as u64 * value,
        WalletSendReadiness::TotalValueOnly,
        &mut available_utxos,
        &HashSet::new(),
    )
    .await?;

    utils::create_and_submit_transaction_hashes_with_utxo_cache(
        world,
        step,
        wallet_name,
        &receivers,
        Some(&best_node_info),
        Some(&mut available_utxos),
    )
    .await
}

pub(super) async fn execute_coin_split_with_utxo_cache(
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

pub async fn execute_mempool_diagnostic_coin_splits(
    world: &mut CucumberWorld,
    step: &str,
    outputs_per_wallet: usize,
    epochs_headroom: u32,
) -> Result<(), StepError> {
    if outputs_per_wallet == 0 {
        return Err(StepError::InvalidArgument {
            message: "mempool diagnostic split output count must be greater than zero".to_owned(),
        });
    }
    let mut wallets = super::all_user_wallets(world)?;
    if wallets.len() < 2 {
        return Err(StepError::InvalidArgument {
            message: "mempool diagnostic splitting requires at least two user wallets".to_owned(),
        });
    }
    wallets.sort();

    let policy = super::build_cycle_fee_policy(world, step, &wallets[0], epochs_headroom).await?;
    let mut available_utxos = super::current_available_utxos_for_user_wallets(world, step).await?;
    let mut requests = Vec::with_capacity(wallets.len());

    for wallet_name in &wallets {
        sync::wait_wallet_send_ready(
            world,
            step,
            wallet_name,
            180,
            1,
            WalletSendReadiness::TotalValueOnly,
            &mut available_utxos,
            &HashSet::new(),
        )
        .await?;

        let wallet = world.resolve_wallet(wallet_name)?;
        let public_key = wallet.public_key()?;
        let available_value = available_utxos
            .get(wallet_name.as_str())
            .into_iter()
            .flatten()
            .try_fold(0u64, |total, utxo| total.checked_add(utxo.note.value))
            .ok_or_else(|| StepError::LogicalError {
                message: format!("available value overflowed for wallet `{wallet_name}`"),
            })?;
        let required_fee = estimate_split_required_fee(
            outputs_per_wallet,
            public_key,
            policy.horizon.ceiling_prices.clone(),
            policy.priority_fee_percent,
        )?;
        let split_value =
            approximate_equal_split_value(available_value, outputs_per_wallet, required_fee)?;
        tracing::info!(
            target: super::TARGET,
            wallet = wallet_name,
            available_value,
            split_outputs = outputs_per_wallet,
            split_value,
            estimated_fee_reserve = required_fee,
            "Prepared fee-aware approximately equal mempool diagnostic split"
        );
        BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
            "mempool_wallet_split_prepared",
            &serde_json::json!({
                "wallet": wallet_name,
                "available_value": available_value,
                "output_count": outputs_per_wallet,
                "output_value": split_value,
                "estimated_fee_reserve": required_fee,
            }),
        );

        requests.push((
            wallet_name.clone(),
            vec![(public_key, split_value); outputs_per_wallet],
        ));
    }

    let signed_submissions = prepare_signed_submissions_with_utxo_cache(
        world,
        step,
        requests,
        &mut available_utxos,
        Some(policy.horizon.ceiling_prices.clone()),
        policy.priority_fee_percent,
    )
    .await?;
    let expected_transactions = wallets.len();
    let submitted = utils::submit_signed_user_wallet_submissions_concurrently(
        world,
        signed_submissions,
        Some(&policy),
    )
    .await?;
    if submitted.len() != expected_transactions {
        return Err(StepError::StepFail {
            message: format!(
                "mempool diagnostic split submitted {} transaction(s), expected {expected_transactions}",
                submitted.len(),
            ),
        });
    }
    Ok(())
}

fn estimate_split_required_fee(
    outputs_per_wallet: usize,
    public_key: ZkPublicKey,
    gas_prices: GasPrices,
    priority_fee_percent: u64,
) -> Result<u64, StepError> {
    let mut builder = MantleTxBuilder::new();
    for _ in 0..outputs_per_wallet.saturating_add(1) {
        builder = builder
            .add_ledger_output(Note::new(1, public_key))
            .map_err(|error| StepError::LogicalError {
                message: format!("could not build split fee estimate: {error}"),
            })?;
    }
    let context = OpsContext {
        gas_context: OpsGasContext::new(HashMap::new(), HashMap::new(), gas_prices),
        ..OpsContext::default()
    };
    let mandatory_fee = builder
        .minimum_gas_cost::<MainnetGasProfile>(&context)
        .map_err(|error| StepError::LogicalError {
            message: format!("could not estimate split transaction fee: {error}"),
        })?
        .into_inner();
    let priority_fee =
        crate::common::fee_spec::priority_fee_amount(mandatory_fee, priority_fee_percent)
            .map_err(|message| StepError::LogicalError { message })?;
    mandatory_fee
        .checked_add(priority_fee)
        .and_then(|fee| fee.checked_add(1))
        .ok_or_else(|| StepError::LogicalError {
            message: "split transaction fee reserve overflowed".to_owned(),
        })
}

fn approximate_equal_split_value(
    available_value: u64,
    outputs_per_wallet: usize,
    required_fee_reserve: u64,
) -> Result<u64, StepError> {
    if outputs_per_wallet == 0 {
        return Err(StepError::InvalidArgument {
            message: "mempool diagnostic split output count must be greater than zero".to_owned(),
        });
    }
    let output_and_change_count = outputs_per_wallet
        .checked_add(1)
        .and_then(|count| u64::try_from(count).ok())
        .ok_or_else(|| StepError::InvalidArgument {
            message: "mempool diagnostic split output count is too large".to_owned(),
        })?;
    let chunk_value = available_value
        .checked_sub(required_fee_reserve)
        .map(|remaining| remaining / output_and_change_count)
        .filter(|value| *value > 0)
        .ok_or(StepError::FundsDeficit {
            available: available_value,
            num_utxos_required: outputs_per_wallet,
            value_per_utxos_required: 1,
        })?;
    Ok(chunk_value)
}

async fn prepare_signed_submissions_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    requests: Vec<(String, Vec<(ZkPublicKey, u64)>)>,
    available_utxos: &mut WalletUtxos,
    gas_prices: Option<GasPrices>,
    priority_fee_percent: u64,
) -> Result<Vec<SignedUserWalletSubmission>, StepError> {
    let mut reserved_submissions = Vec::with_capacity(requests.len());

    for (sender, receivers) in requests {
        let reserved_submission =
            utils::reserve_user_wallet_transaction_submission_with_utxo_cache(
                world,
                step,
                &sender,
                &receivers,
                available_utxos,
                gas_prices.clone(),
                priority_fee_percent,
            )
            .await?;
        reserved_submissions.push(reserved_submission);
    }

    utils::finalize_reserved_user_wallet_submissions_concurrently(step, reserved_submissions).await
}

#[expect(clippy::too_many_arguments, reason = "Coin-split preparation inputs")]
pub(super) async fn prepare_coin_splits_all_wallets_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    wallet_names: &[String],
    outputs: usize,
    value: u64,
    available_utxos: &mut WalletUtxos,
    gas_prices: Option<GasPrices>,
    priority_fee_percent: u64,
) -> Result<(Vec<SignedUserWalletSubmission>, BTreeMap<String, usize>), StepError> {
    let mut requests = Vec::with_capacity(wallet_names.len());
    let mut prepared_counts = BTreeMap::new();

    for wallet_name in wallet_names {
        let wallet = world.resolve_wallet(wallet_name)?;
        let self_pk = wallet.public_key()?;
        let receivers = vec![(self_pk, value); outputs];
        *prepared_counts.entry(wallet_name.clone()).or_insert(0usize) += 1;
        requests.push((wallet_name.clone(), receivers));
    }

    let signed_submissions = prepare_signed_submissions_with_utxo_cache(
        world,
        step,
        requests,
        available_utxos,
        gas_prices,
        priority_fee_percent,
    )
    .await?;
    Ok((signed_submissions, prepared_counts))
}

pub(super) async fn execute_send(
    world: &mut CucumberWorld,
    step: &str,
    number_of_transactions: usize,
    value: u64,
    from: &str,
    to: &str,
) -> Result<(), StepError> {
    let receiver = world.resolve_recipient(to)?;
    let receiver_pk = receiver.public_key;

    let mut available_utxos = WalletUtxos::new();
    let best_node_info = sync::wait_wallet_send_ready(
        world,
        step,
        from,
        180,
        number_of_transactions as u64 * value,
        WalletSendReadiness::EligibleUtxoBatch {
            min_required_outputs: number_of_transactions,
            min_value_per_transaction: value,
        },
        &mut available_utxos,
        &HashSet::new(),
    )
    .await?;

    for i in 0..number_of_transactions {
        let result = utils::create_and_submit_transaction(
            world,
            step,
            from,
            &[(receiver_pk, value)],
            Some(&best_node_info),
            Some(&mut available_utxos),
        )
        .await;

        if let Err(StepError::WalletError(WalletError::InsufficientFunds { available })) = result {
            return Err(StepError::FundsDeficit {
                available,
                num_utxos_required: number_of_transactions - i,
                value_per_utxos_required: value,
            });
        }
        result?;
    }
    Ok(())
}

#[expect(clippy::too_many_arguments, reason = "Transaction preparation inputs")]
pub(super) async fn prepare_ring_send_round_send_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    transactions: usize,
    round_number: usize,
    value: u64,
    from: &str,
    to: &str,
    available_utxos: &mut WalletUtxos,
    workload_pools: &mut utils::WorkloadUtxoPools,
    gas_prices: Option<GasPrices>,
    priority_fee_percent: u64,
    dependent_state: Option<&DependentTransactionLoadState>,
    mempool_diagnostics: bool,
) -> Result<Vec<SignedUserWalletSubmission>, StepError> {
    let preparation_started = Instant::now();
    let receiver = world.resolve_recipient(to)?;
    let receiver_pk = receiver.public_key;
    let mut reserved_submissions = Vec::with_capacity(transactions);
    let reservation_started = Instant::now();

    for _ in 0..transactions {
        let sender_utxo_count_before = available_utxos.get(from).map_or(0usize, Vec::len);

        let receivers = vec![(receiver_pk, value)];
        let transfer_intent = crate::common::wallet::WalletTransactionIntent::transfer(&receivers)
            .map_err(|error| StepError::LogicalError {
                message: error.to_string(),
            })?;
        let transaction_intent = if let Some(dependent_state) = dependent_state {
            dependent_state.prepare_hybrid_intent(transfer_intent)?
        } else {
            transfer_intent
        };
        let reserved_submission = utils::reserve_workload_transaction_intent_with_primary_and_dust(
            world,
            step,
            from,
            transaction_intent,
            value,
            available_utxos,
            workload_pools,
            gas_prices.clone(),
            priority_fee_percent,
        )
        .await?;
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

    let reservation_duration = reservation_started.elapsed();
    let finalization_started = Instant::now();
    let signed_submissions =
        utils::finalize_reserved_user_wallet_submissions_concurrently(step, reserved_submissions)
            .await?;
    let finalization_duration = finalization_started.elapsed();
    let total_duration = preparation_started.elapsed();
    let transactions_per_second = if total_duration.is_zero() {
        0.0
    } else {
        transactions as f64 / total_duration.as_secs_f64()
    };
    tracing::info!(
        target: super::TARGET,
        sender = from,
        burst = round_number,
        workload_mode = if dependent_state.is_some() { "dependent" } else { "independent" },
        transaction_count = transactions,
        reservation_ms = reservation_duration.as_millis(),
        signing_finalization_ms = finalization_duration.as_millis(),
        total_pre_submission_preparation_ms = total_duration.as_millis(),
        transactions_per_second,
        "Prepared next-wallet transaction batch"
    );
    if mempool_diagnostics {
        BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
            "mempool_transaction_wallet_batch_prepared",
            &serde_json::json!({
                "workload_mode": if dependent_state.is_some() { "dependent" } else { "independent" },
                "round": round_number,
                "wallet": from,
                "transaction_count": transactions,
                "reservation_ms": reservation_duration.as_millis(),
                "signing_finalization_ms": finalization_duration.as_millis(),
                "total_pre_submission_preparation_ms": total_duration.as_millis(),
                "transactions_per_second": transactions_per_second,
            }),
        );
    }
    Ok(signed_submissions)
}

#[cfg(test)]
mod mempool_diagnostic_split_tests {
    use super::*;

    #[test]
    fn equal_split_accounts_for_fee_and_leaves_a_reusable_change_chunk() {
        let available_value = 110_000_000;
        let output_count = 250;
        let fee_reserve = 12_345;
        let split_value = approximate_equal_split_value(available_value, output_count, fee_reserve)
            .expect("the wallet has enough value for the requested split");
        let change = available_value - split_value * output_count as u64 - fee_reserve;

        assert_eq!(split_value, (available_value - fee_reserve) / 251);
        assert!(split_value > 400_000, "split outputs should not be dust");
        assert!(change >= split_value);
        assert!(change < split_value + output_count as u64 + 1);
    }

    #[test]
    fn equal_split_rejects_a_fee_reserve_that_consumes_all_funds() {
        assert!(approximate_equal_split_value(100, 10, 100).is_err());
        assert!(approximate_equal_split_value(100, 0, 1).is_err());
    }
}
