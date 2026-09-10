use super::{
    CommonHttpClient, CucumberWorld, DiscardedPayloads, DriveMode, Elapsed, FundingConfig, GasCost,
    NodeHttpClient, PolicyRuntime, SEQUENCER_READY_HEIGHT_ADVANCE_TIMEOUT,
    SEQUENCER_READY_POLL_TIMEOUT, SEQUENCER_READY_TIMEOUT, SequencerCheckpoint,
    StartedSequencerRuntime, Step, StepError, StepResult, ZONE_TEST_PRIORITY_FEE_PERCENT,
    ZoneNodeHttpClient, ZoneSequencer, initialize_zone_indexer, log_step_error, sequencer_config,
    sequencer_config_with_pending_submit_depth, start_balance_aware_policy,
    start_custom_republish_policy, start_deposit_lifecycle_policy, start_deposit_withdraw_policy,
    start_republish_lineage_policy, start_sequencer_event_loop, start_sorted_conflict_policy,
    timeout, wait_for_height,
};

pub(in super::super) async fn start_named_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
    checkpoint: Option<SequencerCheckpoint>,
    mode: DriveMode,
) -> StepResult {
    let funding = log_step_error(step, sequencer_funding(world, sequencer_alias.as_ref()))?;
    start_named_sequencer_with_config(
        world,
        step,
        sequencer_alias,
        checkpoint,
        mode,
        sequencer_config(funding),
    )
    .await
}

/// Fund sequencer transactions from the node's own funding wallet.
fn sequencer_funding(
    world: &CucumberWorld,
    sequencer_alias: &str,
) -> Result<FundingConfig, StepError> {
    let node_name = world.zone.sequencer_node_name(sequencer_alias)?;
    let funding_pk = world.funding_wallet(node_name)?.public_key()?;
    Ok(FundingConfig {
        funding_pk,
        change_pk: None,
        max_tx_fee: GasCost::new(u64::MAX),
        priority_fee_percent: ZONE_TEST_PRIORITY_FEE_PERCENT,
    })
}

pub(in super::super) async fn start_named_sequencer_with_pending_submit_depth(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
    checkpoint: Option<SequencerCheckpoint>,
    mode: DriveMode,
    max_pending_publish_depth: usize,
) -> StepResult {
    let funding = log_step_error(step, sequencer_funding(world, sequencer_alias.as_ref()))?;
    let config = sequencer_config_with_pending_submit_depth(max_pending_publish_depth, funding);

    start_named_sequencer_with_config(world, step, sequencer_alias, checkpoint, mode, config).await
}

/// Start `sequencer_alias` with the deposit-lifecycle policy and an indexer.
pub(in super::super) async fn start_deposit_reaction_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    withdraw_outputs: Vec<u64>,
) -> StepResult {
    let recipient = log_step_error(step, sequencer_funding(world, sequencer_alias))?.funding_pk;
    start_named_sequencer(
        world,
        step,
        sequencer_alias,
        None,
        DriveMode::DepositReaction {
            withdraw_outputs,
            recipient,
        },
    )
    .await?;
    initialize_zone_indexer(world, step, sequencer_alias)
}

/// Start `sequencer_alias` with the deposit-withdraw policy and an indexer.
pub(in super::super) async fn start_deposit_withdraw_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    target_amount: u64,
    withdraw_outputs: Vec<u64>,
) -> StepResult {
    let recipient = log_step_error(step, sequencer_funding(world, sequencer_alias))?.funding_pk;
    start_named_sequencer(
        world,
        step,
        sequencer_alias,
        None,
        DriveMode::DepositWithdraw {
            target_amount,
            withdraw_outputs,
            recipient,
        },
    )
    .await?;
    initialize_zone_indexer(world, step, sequencer_alias)
}

async fn start_named_sequencer_with_config(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
    checkpoint: Option<SequencerCheckpoint>,
    mode: DriveMode,
    config: lb_zone_sdk::sequencer::SequencerConfig,
) -> StepResult {
    let sequencer_alias = sequencer_alias.as_ref().to_owned();
    let signing_key =
        log_step_error(step, world.zone.sequencer_signing_key(&sequencer_alias))?.clone();
    let node_client = log_step_error(
        step,
        world.zone_node_http_client_for_sequencer(&sequencer_alias),
    )?;
    let node_url = log_step_error(step, world.zone_node_url_for_sequencer(&sequencer_alias))?;
    let sequencer = ZoneSequencer::init_with_config(
        world.zone.sequencer_channel_id(&sequencer_alias)?,
        signing_key,
        ZoneNodeHttpClient::new(CommonHttpClient::new(None), node_url),
        config,
        checkpoint,
    );

    let runtime = start_sequencer_runtime(sequencer, mode);
    let mut ready_rx = runtime.ready_rx.clone();

    if let Err(error) =
        wait_for_sequencer_ready(&sequencer_alias, &node_client, &mut ready_rx).await
    {
        runtime.task.abort();
        return Err(error);
    }

    world.zone.set_sequencer_runtime(
        sequencer_alias,
        runtime.client,
        runtime.task,
        runtime.events,
        runtime.checkpoint_rx,
        runtime.channel_view_rx,
        runtime.turn_to_write_rx,
        runtime.tx_status_rx,
        runtime.discarded_payloads,
    );

    Ok(())
}

async fn wait_for_sequencer_ready(
    sequencer_alias: &str,
    node_client: &NodeHttpClient,
    ready_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> StepResult {
    timeout(SEQUENCER_READY_TIMEOUT, async {
        let mut last_height = node_client.consensus_info().await?.cryptarchia_info.height;

        loop {
            let poll = timeout(SEQUENCER_READY_POLL_TIMEOUT, async {
                loop {
                    if ready_rx.changed().await.is_err() {
                        return Err(());
                    }
                    if *ready_rx.borrow() {
                        return Ok(());
                    }
                }
            })
            .await;
            if matches!(poll, Ok(Ok(()))) {
                return Ok(());
            }

            let _ = wait_for_height(
                node_client,
                last_height.saturating_add(1),
                SEQUENCER_READY_HEIGHT_ADVANCE_TIMEOUT,
            )
            .await;

            last_height = node_client
                .consensus_info()
                .await?
                .cryptarchia_info
                .height
                .max(last_height);
        }
    })
    .await
    .map_err(|_: Elapsed| StepError::Timeout {
        message: format!(
            "Sequencer `{sequencer_alias}` did not become ready within {} seconds",
            SEQUENCER_READY_TIMEOUT.as_secs()
        ),
    })?
}

fn from_policy_runtime(
    rt: PolicyRuntime,
    discarded_payloads: Option<DiscardedPayloads>,
) -> StartedSequencerRuntime {
    StartedSequencerRuntime {
        task: rt.task,
        client: rt.client,
        events: rt.events,
        checkpoint_rx: rt.checkpoint_rx,
        ready_rx: rt.ready_rx,
        channel_view_rx: rt.channel_view_rx,
        turn_to_write_rx: rt.turn_to_write_rx,
        tx_status_rx: rt.tx_status_rx,
        discarded_payloads,
    }
}

fn start_sequencer_runtime(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    mode: DriveMode,
) -> StartedSequencerRuntime {
    match mode {
        DriveMode::Passive { republish_orphans } => from_policy_runtime(
            start_sequencer_event_loop(sequencer, republish_orphans),
            None,
        ),
        DriveMode::RepublishLineage { planned } => {
            from_policy_runtime(start_republish_lineage_policy(sequencer, planned), None)
        }
        DriveMode::Sorted { discarded } => from_policy_runtime(
            start_sorted_conflict_policy(sequencer, &discarded),
            Some(discarded),
        ),
        DriveMode::BalanceAware {
            initial_balances,
            planned_payloads,
        } => from_policy_runtime(
            start_balance_aware_policy(sequencer, initial_balances, planned_payloads),
            None,
        ),
        DriveMode::CustomRepublish { deps } => {
            from_policy_runtime(start_custom_republish_policy(sequencer, *deps), None)
        }
        DriveMode::DepositReaction {
            withdraw_outputs,
            recipient,
        } => from_policy_runtime(
            start_deposit_lifecycle_policy(sequencer, withdraw_outputs, recipient),
            None,
        ),
        DriveMode::DepositWithdraw {
            target_amount,
            withdraw_outputs,
            recipient,
        } => from_policy_runtime(
            start_deposit_withdraw_policy(sequencer, target_amount, withdraw_outputs, recipient),
            None,
        ),
    }
}
