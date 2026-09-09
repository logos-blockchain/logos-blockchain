use super::{
    BTreeMap, BestNodeInfo, CucumberWorld, GasPrices, HashSet, ManualCommand, NonZero,
    SignedUserWalletSubmission, StepError, TxHash, WalletError, WalletInfo, WalletSendReadiness,
    WalletUtxos, ZkPublicKey, sync, utils,
};

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
    value: u64,
    from: &str,
    to: &str,
    available_utxos: &mut WalletUtxos,
    gas_prices: Option<GasPrices>,
    priority_fee_percent: u64,
) -> Result<Vec<SignedUserWalletSubmission>, StepError> {
    let receiver = world.resolve_recipient(to)?;
    let receiver_pk = receiver.public_key;
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
                gas_prices.clone(),
                priority_fee_percent,
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
