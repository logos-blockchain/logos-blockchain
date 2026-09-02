use super::*;

/// Creates a scenario-local sequencer key.
#[must_use]
pub fn keygen() -> Ed25519Key {
    let mut key_bytes = [0u8; 32];
    thread_rng().fill(&mut key_bytes);
    Ed25519Key::from_bytes(&key_bytes)
}

/// Encodes a balance-affecting zone payload used by balance-aware sequencer
/// scenarios.
#[must_use]
pub fn balance_update_payload(uuid: &str, account: &str, delta: i64) -> Inscription {
    make_inscription(&format!("{uuid}:{account}:{delta}"))
}

/// Parses a balance-affecting payload in the same format produced by
/// [`balance_update_payload`].
pub fn parse_balance_payload(payload: &Inscription) -> Option<(String, String, i64)> {
    let payload = std::str::from_utf8(payload.as_slice()).ok()?;
    let parts = payload.splitn(3, ':').collect::<Vec<_>>();
    let [uuid, account, delta] = parts.as_slice() else {
        return None;
    };

    Some((
        (*uuid).to_owned(),
        (*account).to_owned(),
        delta.parse().ok()?,
    ))
}

/// Uses a short resubmit interval so retry-sensitive zone scenarios settle
/// quickly enough for CI.
#[must_use]
pub const fn sequencer_config(funding: FundingConfig) -> SequencerConfig {
    SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        min_slots_remaining_in_turn: 2,
        ..SequencerConfig::new(funding)
    }
}

/// Uses the same retry profile while overriding pending publish submit depth.
#[must_use]
pub const fn sequencer_config_with_pending_submit_depth(
    max_pending_publish_depth: usize,
    funding: FundingConfig,
) -> SequencerConfig {
    SequencerConfig {
        max_pending_publish_depth,
        ..sequencer_config(funding)
    }
}

/// Publishes a zone payload through the runner and returns the SDK's
/// [`PublishResult`] inline. Retries transient publish errors until the
/// deadline elapses. No "wait for event" — the SDK accepts the publish
/// inline (funding it via the node when configured) and the runner forwards
/// the call through the drive task.
pub async fn publish_message_with_retry(
    client: &SequencerClient,
    data: &Inscription,
    deadline: PublishDeadline,
) -> Result<PublishResult, ZoneTestError> {
    loop {
        if deadline.is_expired() {
            return Err(ZoneTestError::PublishTimeout);
        }
        match client.publish(data.clone()).await {
            Ok((result, _cp)) => return Ok(result),
            Err(error) => {
                warn!(error = %error, "Zone sequencer publish failed, retrying");
                sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Waits until every tx in `tx_hashes` reports [`TxStatus::OnChain`] on the
/// sequencer's status stream, collecting the tx hashes seen as
/// [`TxStatus::PendingMempool`] along the way. Own publishes don't echo in
/// [`ChannelUpdate::adopted`] on chain extension (the sequencer already
/// tracks them), so the per-tx status stream is where "landed on chain, not
/// yet finalized" is observable.
pub async fn wait_for_on_chain_statuses_and_collect_mempool_pending(
    statuses: &mut tokio::sync::broadcast::Receiver<TxStatusUpdate>,
    tx_hashes: &[InscriptionId],
    duration: Duration,
) -> Result<HashSet<InscriptionId>, ZoneTestError> {
    timeout(duration, async {
        let mut on_chain: HashSet<InscriptionId> = HashSet::new();
        let mut mempool_pending = HashSet::new();

        while on_chain.len() < tx_hashes.len() {
            let update = match statuses.recv().await {
                Ok(update) => update,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("status subscriber lagged by {n}, recovering");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(ZoneTestError::SequencerStopped);
                }
            };
            match update.status {
                TxStatus::PendingMempool => {
                    mempool_pending.insert(update.tx_hash);
                }
                TxStatus::OnChain(_) if tx_hashes.contains(&update.tx_hash) => {
                    on_chain.insert(update.tx_hash);
                }
                _ => {}
            }
        }

        Ok(mempool_pending)
    })
    .await
    .map_err(|_| ZoneTestError::PublishTimeout)?
}

pub async fn wait_for_tx_status_lifecycle(
    tx_status_rx: &mut tokio::sync::broadcast::Receiver<TxStatusUpdate>,
    tx_hashes: &[InscriptionId],
    statuses: &[TxStatus],
    duration: Duration,
) -> Result<(), ZoneTestError> {
    let mut remaining: HashSet<(InscriptionId, TxStatus)> = tx_hashes
        .iter()
        .flat_map(|tx_hash| statuses.iter().map(move |status| (*tx_hash, *status)))
        .collect();

    timeout(duration, async {
        while !remaining.is_empty() {
            let update = match tx_status_rx.recv().await {
                Ok(update) => update,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("tx-status subscriber lagged by {n}, recovering");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(ZoneTestError::SequencerStopped);
                }
            };
            remaining.remove(&(update.tx_hash, update.status));
            if remaining.is_empty() {
                return Ok(());
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| ZoneTestError::IndexerTimeout)?
}

/// Waits until the subscribed channel view satisfies the supplied predicate.
pub async fn wait_for_channel_view(
    view_rx: &mut tokio::sync::watch::Receiver<SequencerChannelView>,
    duration: Duration,
    predicate: impl Fn(&SequencerChannelView) -> bool + Send + Sync,
) -> Result<SequencerChannelView, ZoneTestError> {
    timeout(duration, async {
        loop {
            let current = view_rx.borrow().clone();
            if predicate(&current) {
                return Ok(current);
            }

            view_rx
                .changed()
                .await
                .map_err(|error| ZoneTestError::Indexer {
                    message: format!("channel view sender closed: {error}"),
                })?;
        }
    })
    .await
    .map_err(|_| ZoneTestError::ChannelViewTimeout {
        message: format!(
            "condition not reached within {} seconds",
            duration.as_secs()
        ),
    })?
}

/// Waits until the sequencer emits a turn-to-write notification.
pub async fn wait_for_turn_to_write(
    turn_rx: &mut tokio::sync::watch::Receiver<TurnNotification>,
    duration: Duration,
) -> Result<TurnNotification, ZoneTestError> {
    timeout(duration, async {
        loop {
            let current = turn_rx.borrow().clone();
            if current.our_turn_to_write {
                return Ok(current);
            }

            turn_rx
                .changed()
                .await
                .map_err(|error| ZoneTestError::Indexer {
                    message: format!("turn-to-write sender closed: {error}"),
                })?;
        }
    })
    .await
    .map_err(|_| ZoneTestError::ChannelViewTimeout {
        message: format!(
            "turn to write not reached within {} seconds",
            duration.as_secs()
        ),
    })?
}

/// Replays the channel's finalized history by cold-starting a fresh
/// read-only sequencer: a random signing key that is not part of the channel
/// rotation, so the instance can never publish or repost anything —
/// inscription posting is turn-gated. Finalized txs are collected from the
/// backfill events until the sequencer reports `Ready`, then the instance is
/// dropped; each call observes a fresh snapshot up to the LIB at connect
/// time.
pub async fn replay_finalized_history(
    reader: &ZoneReaderConfig,
) -> Result<Vec<FinalizedTx>, ZoneTestError> {
    let node = ZoneNodeHttpClient::new(CommonHttpClient::new(None), reader.node_url.clone());
    // Placeholder funding: the reader never publishes (random key, posting is
    // turn-gated), so the funding wallet is never exercised.
    let funding = FundingConfig {
        funding_pk: lb_groth16::Fr::from(1u64).into(),
        change_pk: None,
        max_tx_fee: GasCost::new(u64::MAX),
        priority_fee_percent: FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT,
    };
    let mut sequencer = ZoneSequencer::init(reader.channel_id, keygen(), node, funding, None);

    timeout(Duration::from_mins(3), async {
        let mut finalized = Vec::new();
        loop {
            match sequencer.next_event().await {
                Event::BlocksProcessed {
                    finalized: batch, ..
                } => finalized.extend(batch),
                Event::Ready => return finalized,
                Event::MempoolPending(_) | Event::TurnNotification { .. } => {}
            }
        }
    })
    .await
    .map_err(|_| ZoneTestError::IndexerTimeout)
}

/// Ordered inscription payloads within a finalized-history replay.
pub fn replayed_inscription_payloads(history: &[FinalizedTx]) -> Vec<Inscription> {
    finalized_inscriptions(history)
        .map(|info| info.payload.clone())
        .collect()
}

/// Collects indexed block payloads until all expected messages have appeared.
///
/// The returned order is the finalized on-chain order, which lets assertions
/// decide whether ordering matters for the scenario.
pub async fn collect_indexed_messages(
    reader: &ZoneReaderConfig,
    expected_messages: &[Inscription],
    duration: Duration,
) -> Result<Vec<Inscription>, ZoneTestError> {
    let expected: HashSet<Inscription> = expected_messages.iter().cloned().collect();

    timeout(duration, async {
        loop {
            let payloads = replayed_inscription_payloads(&replay_finalized_history(reader).await?);
            let mut seen: HashSet<Inscription> = HashSet::new();
            let mut ordered: Vec<Inscription> = Vec::new();
            for payload in payloads {
                if expected.contains(&payload) && seen.insert(payload.clone()) {
                    ordered.push(payload);
                }
            }

            if seen == expected {
                return Ok(ordered);
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::IndexerTimeout)?
}

/// Replays the finalized history until it exactly matches the expected
/// message sequence without duplicates.
pub async fn collect_indexed_messages_exactly_once(
    reader: &ZoneReaderConfig,
    expected_messages: &[Inscription],
    duration: Duration,
) -> Result<Vec<Inscription>, ZoneTestError> {
    let expected: HashSet<Inscription> = expected_messages.iter().cloned().collect();

    timeout(duration, async {
        loop {
            let ordered: Vec<Inscription> =
                replayed_inscription_payloads(&replay_finalized_history(reader).await?)
                    .into_iter()
                    .filter(|payload| expected.contains(payload))
                    .collect();

            if ordered == expected_messages {
                return Ok(ordered);
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::IndexerTimeout)?
}

/// Waits until the finalized history contains exactly `expected_count` copies
/// of one payload after a short settle period.
///
/// This intentionally counts duplicate payload bytes, which is required for
/// shared-payload zone tests where each inscription has the same data but a
/// distinct transaction lineage.
pub async fn wait_for_exact_indexed_payload_count(
    reader: &ZoneReaderConfig,
    expected_payload: Inscription,
    expected_count: usize,
    duration: Duration,
) -> Result<(), ZoneTestError> {
    timeout(duration, async {
        loop {
            let count = count_indexed_payload(reader, &expected_payload).await?;

            if count >= expected_count {
                sleep(Duration::from_secs(30)).await;

                let final_count = count_indexed_payload(reader, &expected_payload).await?;
                if final_count == expected_count {
                    return Ok(());
                }

                return Err(ZoneTestError::IndexedPayloadCountMismatch {
                    payload: String::from_utf8_lossy(expected_payload.as_slice()).to_string(),
                    expected: expected_count,
                    actual: final_count,
                });
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::IndexerTimeout)?
}

async fn count_indexed_payload(
    reader: &ZoneReaderConfig,
    expected_payload: &Inscription,
) -> Result<usize, ZoneTestError> {
    Ok(
        replayed_inscription_payloads(&replay_finalized_history(reader).await?)
            .iter()
            .filter(|payload| *payload == expected_payload)
            .count(),
    )
}

/// Polls until the wallet holds exactly the given number of finalized and
/// unfinalized notes — an exact-count check that catches double-counting.
pub async fn wait_for_channel_wallet_counts(
    client: &SequencerClient,
    finalized: usize,
    unfinalized: usize,
    duration: Duration,
) -> Result<(), ZoneTestError> {
    timeout(duration, async {
        loop {
            let view =
                client
                    .channel_wallet()
                    .await
                    .map_err(|error| ZoneTestError::ChannelWallet {
                        message: error.to_string(),
                    })?;
            if view.finalized.len() == finalized && view.unfinalized.len() == unfinalized {
                return Ok(());
            }
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::ChannelWalletTimeout)?
}

/// Polls the sequencer's channel wallet until a note of `value` is present.
/// With `finalized_only`, only the finalized layer counts.
pub async fn wait_for_channel_wallet_note(
    client: &SequencerClient,
    value: Value,
    finalized_only: bool,
    duration: Duration,
) -> Result<(), ZoneTestError> {
    timeout(duration, async {
        loop {
            let view =
                client
                    .channel_wallet()
                    .await
                    .map_err(|error| ZoneTestError::ChannelWallet {
                        message: error.to_string(),
                    })?;
            let unfinalized = (!finalized_only)
                .then_some(view.unfinalized.iter())
                .into_iter()
                .flatten();
            if view
                .finalized
                .iter()
                .chain(unfinalized)
                .any(|note| note.value == value)
            {
                return Ok(());
            }
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::ChannelWalletTimeout)?
}

/// Waits until the finalized channel history contains the expected channel
/// deposit, including its amount.
pub async fn wait_for_deposit(
    reader: &ZoneReaderConfig,
    expected: &DepositOp,
    expected_amount: Value,
    duration: Duration,
) -> Result<(), ZoneTestError> {
    poll_replayed_history_until(reader, duration, ZoneTestError::IndexerTimeout, |op| {
        matches!(op, FinalizedOp::Deposit(deposit)
            if deposit.inputs == expected.inputs
                && deposit.amount == expected_amount
                && deposit.metadata == expected.metadata)
    })
    .await
}

/// Waits until the finalized channel history contains the expected withdraw.
pub async fn wait_for_withdraw(
    reader: &ZoneReaderConfig,
    expected: &ChannelWithdrawOp,
    timeout_duration: Duration,
) -> Result<(), ZoneTestError> {
    poll_replayed_history_until(
        reader,
        timeout_duration,
        ZoneTestError::WithdrawTimeout,
        |op| matches!(op, FinalizedOp::Withdraw(withdraw) if withdraw.op.inputs == expected.inputs),
    )
    .await
}

/// Waits until the finalized channel history contains a channel transfer whose
/// input set has exactly `expected_inputs` notes.
///
/// This is the on-chain record of the withdrawal's note selection: in the
/// dust-flood scenario it proves best-fit largest-first consumed a single
/// covering note rather than sweeping the >255-note dust flood into a transfer
/// the ledger would reject.
pub async fn wait_for_channel_transfer_input_count(
    reader: &ZoneReaderConfig,
    expected_inputs: usize,
    timeout_duration: Duration,
) -> Result<(), ZoneTestError> {
    poll_replayed_history_until(
        reader,
        timeout_duration,
        ZoneTestError::IndexerTimeout,
        move |op| {
            matches!(op, FinalizedOp::ChannelTransfer(transfer)
                if transfer.op.inputs.len() == expected_inputs)
        },
    )
    .await
}

async fn poll_replayed_history_until(
    reader: &ZoneReaderConfig,
    duration: Duration,
    timeout_error: ZoneTestError,
    mut predicate: impl FnMut(&FinalizedOp) -> bool,
) -> Result<(), ZoneTestError> {
    timeout(duration, async {
        loop {
            let history = replay_finalized_history(reader).await?;
            if history
                .iter()
                .flat_map(|tx| tx.ops.iter())
                .any(&mut predicate)
            {
                return Ok(());
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| timeout_error)?
}

/// Waits until the sequencer's event stream surfaces the expected deposit
/// in [`Event::BlocksProcessed::finalized`] (matched by `inputs`, `amount`,
/// and `metadata`) while collecting any mempool-pending events. Drains the
/// events channel as it goes — call this after any earlier event consumers in
/// the scenario have moved past the relevant publish events.
pub async fn wait_for_finalized_deposit_via_sequencer_and_collect_mempool_pending(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    expected: &DepositOp,
    expected_amount: Value,
    duration: Duration,
) -> Result<HashSet<InscriptionId>, ZoneTestError> {
    poll_sequencer_finalized_until_and_collect_mempool_pending(
        events,
        duration,
        ZoneTestError::IndexerTimeout,
        |op| {
            matches!(op, FinalizedOp::Deposit(d)
            if d.inputs == expected.inputs
                && d.amount == expected_amount
                && d.metadata == expected.metadata)
        },
    )
    .await
}

/// Waits until the sequencer's event stream surfaces the expected withdraw
/// (matched by `outputs`) while collecting any mempool-pending events. Drains
/// the events channel as it goes.
pub async fn wait_for_finalized_withdraw_via_sequencer_and_collect_mempool_pending(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    expected: &ChannelWithdrawOp,
    duration: Duration,
) -> Result<HashSet<InscriptionId>, ZoneTestError> {
    poll_sequencer_finalized_until_and_collect_mempool_pending(
        events,
        duration,
        ZoneTestError::WithdrawTimeout,
        |op| matches!(op, FinalizedOp::Withdraw(w) if w.op.inputs == expected.inputs),
    )
    .await
}

async fn poll_sequencer_finalized_until_and_collect_mempool_pending(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    duration: Duration,
    timeout_error: ZoneTestError,
    mut predicate: impl FnMut(&FinalizedOp) -> bool,
) -> Result<HashSet<InscriptionId>, ZoneTestError> {
    timeout(duration, async {
        let mut mempool_pending = HashSet::new();
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("event subscriber lagged by {n}, recovering");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(ZoneTestError::SequencerStopped);
                }
            };
            if let Event::MempoolPending(tx_hash) = event {
                mempool_pending.insert(tx_hash);
                continue;
            }
            let Event::BlocksProcessed { finalized, .. } = event else {
                continue;
            };
            for tx in finalized {
                if tx.ops.iter().any(&mut predicate) {
                    return Ok(mempool_pending);
                }
            }
        }
    })
    .await
    .map_err(|_| timeout_error)?
}

/// Waits until node mempool/chain observation confirms the submitted zone
/// transactions reached the canonical chain.
pub async fn ensure_zone_transactions_included(
    client: &NodeHttpClient,
    tx_hashes: &[InscriptionId],
    duration: Duration,
) -> Result<(), ZoneTestError> {
    let included = wait_for_transactions_inclusion(client, tx_hashes, duration).await;

    if included {
        return Ok(());
    }

    Err(ZoneTestError::InclusionTimeout)
}

/// Walks back from LIB until every expected zone transaction is found in the
/// finalized chain.
pub async fn wait_for_transactions_finalized(
    node_url: Url,
    tx_hashes: &[InscriptionId],
    duration: Duration,
) -> Result<(), ZoneTestError> {
    let client = CommonHttpClient::new(None);
    let expected: HashSet<_> = tx_hashes.iter().copied().collect();

    timeout(duration, async {
        loop {
            let info = client
                .consensus_info(node_url.clone())
                .await
                .map_err(|error| ZoneTestError::Consensus {
                    message: error.to_string(),
                })?;

            let mut found = HashSet::new();
            let mut current = info.cryptarchia_info.lib;

            while let Some(block) = client
                .get_block_by_id(node_url.clone(), current)
                .await
                .map_err(|error| ZoneTestError::Block {
                    message: error.to_string(),
                })?
            {
                for tx in &block.transactions {
                    let hash = tx.hash();
                    if expected.contains(&hash) {
                        found.insert(hash);
                    }
                }

                current = block.header.parent_block;
            }

            if found == expected {
                return Ok(());
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::FinalizationTimeout)?
}

/// Waits for LIB movement after a restart so stale-checkpoint scenarios can
/// distinguish old local state from new canonical chain progress.
pub async fn wait_for_lib_advance(
    client: &NodeHttpClient,
    initial_lib_slot: Slot,
    duration: Duration,
) -> Result<(), ZoneTestError> {
    timeout(duration, async {
        loop {
            let info = client
                .consensus_info()
                .await
                .map_err(|error| ZoneTestError::Consensus {
                    message: error.to_string(),
                })?;

            if info.cryptarchia_info.lib_slot > initial_lib_slot {
                return Ok(());
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::LibAdvanceTimeout)?
}
