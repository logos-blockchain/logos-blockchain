use super::{
    CucumberWorld, GasPrices, MAX_TEST_EPOCH_HEADROOM, StepError, TARGET, TransactionFeePolicy,
    build_fee_horizon, get_best_node_info, info,
};

pub async fn build_cycle_fee_policy(
    world: &CucumberWorld,
    step: &str,
    representative_wallet: &str,
    epochs_headroom: u32,
) -> Result<TransactionFeePolicy, StepError> {
    if epochs_headroom > MAX_TEST_EPOCH_HEADROOM {
        return Err(StepError::InvalidArgument {
            message: format!("epochs headroom per cycle must be at most {MAX_TEST_EPOCH_HEADROOM}"),
        });
    }

    let best = get_best_node_info(world, representative_wallet, None).await?;
    let best_node = best.best_node_for_wallet(world, representative_wallet)?;
    let client = world.resolve_node_http_client(&best_node)?;
    let consensus = client
        .consensus_info()
        .await
        .map_err(|source| StepError::StepFail {
            message: format!("Step `{step}` error: consensus query failed: {source}"),
        })?;
    let tip = consensus.cryptarchia_info.tip;
    let prices = client
        .gas_prices(Some(tip))
        .await
        .map_err(|source| StepError::StepFail {
            message: format!("Step `{step}` error: gas prices query failed: {source}"),
        })?;
    if prices.tip != tip {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{step}` error: gas prices response referenced tip {:?}, requested {:?}",
                prices.tip, tip
            ),
        });
    }
    let live_prices = GasPrices {
        execution_base_gas_price: prices.execution_base_gas_price,
        storage_gas_price: prices.storage_gas_price,
    };
    let horizon = build_fee_horizon(
        tip,
        u64::from(consensus.cryptarchia_info.slot),
        world.chain.slots_per_epoch,
        epochs_headroom,
        live_prices,
    )
    .map_err(|source| StepError::LogicalError {
        message: format!("failed to build transaction fee horizon: {source}"),
    })?;
    let policy = TransactionFeePolicy::new(horizon).map_err(|source| StepError::LogicalError {
        message: format!("failed to build transaction fee policy: {source}"),
    })?;
    info!(
        target: TARGET,
        "Cycle fee horizon: prepared tip {:?}, slot {:?}, epoch {:?}, valid through {:?}, execution {}->{}, storage {}->{}, priority reserve {}%, representative wallet `{representative_wallet}`",
        policy.horizon.prepared_at_tip,
        consensus.cryptarchia_info.slot,
        policy.horizon.prepared_at_epoch,
        policy.horizon.valid_through_epoch,
        policy.horizon.live_prices.execution_base_gas_price.into_inner(),
        policy.horizon.ceiling_prices.execution_base_gas_price.into_inner(),
        policy.horizon.live_prices.storage_gas_price.into_inner(),
        policy.horizon.ceiling_prices.storage_gas_price.into_inner(),
        policy.priority_fee_percent,
    );
    Ok(policy)
}
