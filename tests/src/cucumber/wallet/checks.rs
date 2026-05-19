use std::{fmt::Display, time::Duration};

use thiserror::Error;
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::{
    common::wallet::{WalletBalance, WalletOutputState},
    cucumber::{
        error::{StepError, StepResult},
        fee_reserve::SCENARIO_FEE_ACCOUNT_NAME,
        wallet::{
            TARGET,
            sync::{sync_wallet_output_balance, sync_wallet_state_from_chain},
            wallet_output_state_label,
        },
        world::CucumberWorld,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WalletBalanceBounds {
    min_output_count: Option<usize>,
    max_output_count: Option<usize>,
    min_value: Option<u64>,
    max_value: Option<u64>,
}

impl WalletBalanceBounds {
    #[must_use]
    const fn new(
        min_output_count: Option<usize>,
        max_output_count: Option<usize>,
        min_value: Option<u64>,
        max_value: Option<u64>,
    ) -> Self {
        Self {
            min_output_count,
            max_output_count,
            min_value,
            max_value,
        }
    }

    fn validate(&self) -> Result<(), WalletBalanceBoundsError> {
        if self.min_output_count.is_none()
            && self.min_value.is_none()
            && self.max_output_count.is_none()
            && self.max_value.is_none()
        {
            return Err(WalletBalanceBoundsError::NoBounds);
        }

        if u8::from(self.min_output_count.is_some())
            + u8::from(self.max_output_count.is_some())
            + u8::from(self.min_value.is_some())
            + u8::from(self.max_value.is_some())
            == 3
        {
            return Err(WalletBalanceBoundsError::MissingBound);
        }

        Ok(())
    }

    #[must_use]
    fn matches(&self, balance: WalletBalance) -> bool {
        bounds_match(
            balance.output_count,
            self.min_output_count,
            self.max_output_count,
        ) && bounds_match(balance.value, self.min_value, self.max_value)
    }

    #[must_use]
    fn exact_output_count(&self) -> bool {
        self.min_output_count
            .zip(self.max_output_count)
            .is_some_and(|(min, max)| min == max)
    }

    #[must_use]
    fn exact_value(&self) -> bool {
        self.min_value
            .zip(self.max_value)
            .is_some_and(|(min, max)| min == max)
    }

    #[must_use]
    const fn min_output_count(&self) -> Option<usize> {
        self.min_output_count
    }

    #[must_use]
    const fn max_output_count(&self) -> Option<usize> {
        self.max_output_count
    }

    #[must_use]
    const fn min_value(&self) -> Option<u64> {
        self.min_value
    }

    #[must_use]
    const fn max_value(&self) -> Option<u64> {
        self.max_value
    }
}

#[derive(Debug, Clone, Copy, Eq, Error, PartialEq)]
enum WalletBalanceBoundsError {
    #[error("at least one balance bound must be provided")]
    NoBounds,
    #[error("balance output-count and value bounds must be complete")]
    MissingBound,
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

    let fee_state = sync_wallet_state_from_chain(
        world,
        SCENARIO_FEE_ACCOUNT_NAME,
        &query_node_name,
        fee_wallet_account.public_key(),
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step_value);
    })?;

    let current_sponsored_balance = fee_state.balance(WalletOutputState::OnChain).value;
    let sponsored_fee_account_spent = initial_sponsored_balance.checked_sub(current_sponsored_balance).ok_or_else(|| {
        StepError::LogicalError {
            message: format!(
                "Step `{step_value}` error: sponsored fee account balance increased from {initial_sponsored_balance} to {current_sponsored_balance}"
            ),
        }
    })?;

    let tracked_wallet_fees = world.wallets.total_tracked_spent_fees();

    if tracked_wallet_fees != sponsored_fee_account_spent {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{step_value}` error: tracked wallet fees {tracked_wallet_fees} do not equal sponsored fee account spent {sponsored_fee_account_spent}"
            ),
        });
    }

    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "This function is more readable with explicit arguments rather than packing them into structs or tuples."
)]
pub async fn wait_for_wallet_output_state(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: String,
    min_coin_count: Option<&usize>,
    max_coin_count: Option<&usize>,
    min_token_value: Option<&u64>,
    max_token_value: Option<&u64>,
    time_out_seconds: u64,
    wallet_state_type: WalletOutputState,
) -> StepResult {
    let expected_bounds = wallet_balance_bounds(
        step,
        min_coin_count,
        max_coin_count,
        min_token_value,
        max_token_value,
    )
    .inspect_err(|error| {
        warn!(target: TARGET, "Step `{}` error: {error}", step);
    })?;

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
    let wallet_state_label = wallet_output_state_label(wallet_state_type);

    loop {
        let balance =
            sync_wallet_output_balance(world, step, &wallet, &wallet_name, wallet_state_type)
                .await?;

        if conditions_met(
            &wallet_name,
            &wallet.node_name,
            balance,
            expected_bounds,
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
                coin_count = balance.output_count,
                value = balance.value,
            );
            warn!(target: TARGET, "{msg}");
            return Err(StepError::StepFail { message: msg });
        }

        if poll_count.is_multiple_of(25) {
            info!(
                target: TARGET,
                "Waiting for wallet '{wallet_name}/{}' to have required '{wallet_state_label}' \
                coins, current count: {coin_count}, value: {value}",
                wallet.node_name,
                coin_count = balance.output_count,
                value = balance.value,
            );
        }
        poll_count += 1;
        sleep(Duration::from_millis(200)).await;
    }
}

fn bounds_match<T: Copy + Ord>(actual: T, min: Option<T>, max: Option<T>) -> bool {
    min.is_none_or(|min| actual >= min) && max.is_none_or(|max| actual <= max)
}

fn bound_message<T: Display>(actual: T, min: Option<T>, max: Option<T>) -> Option<String> {
    match (min, max) {
        (Some(min), Some(max)) => Some(format!("{actual} >= {min} and <= {max}")),
        (Some(min), None) => Some(format!("{actual} >= {min}")),
        (None, Some(max)) => Some(format!("{actual} <= {max}")),
        (None, None) => None,
    }
}

fn wallet_balance_bounds(
    step: &str,
    min_coin_count: Option<&usize>,
    max_coin_count: Option<&usize>,
    min_token_value: Option<&u64>,
    max_token_value: Option<&u64>,
) -> Result<WalletBalanceBounds, StepError> {
    let bounds = WalletBalanceBounds::new(
        min_coin_count.copied(),
        max_coin_count.copied(),
        min_token_value.copied(),
        max_token_value.copied(),
    );

    bounds.validate().map_err(|error| StepError::LogicalError {
        message: match error {
            WalletBalanceBoundsError::NoBounds => {
                format!(
                    "Step `{step}` error: at least one of 'min_coin_count', 'max_coin_count', \
                    'min_token_value' or 'max_token_value' must be provided"
                )
            }
            WalletBalanceBoundsError::MissingBound => {
                format!(
                    "Step `{step}` error: missing one of 'min_coin_count', 'max_coin_count', \
                    'min_token_value' or 'max_token_value'"
                )
            }
        },
    })?;

    Ok(bounds)
}

fn conditions_met(
    wallet_name: &str,
    wallet_node_name: &str,
    observed_state: WalletBalance,
    expected_bounds: WalletBalanceBounds,
    wallet_state_type: WalletOutputState,
) -> bool {
    if !expected_bounds.matches(observed_state) {
        return false;
    }

    log_condition_match(
        wallet_name,
        wallet_node_name,
        observed_state,
        expected_bounds,
        wallet_state_type,
    );

    true
}

fn log_condition_match(
    wallet_name: &str,
    wallet_node_name: &str,
    observed_state: WalletBalance,
    expected_bounds: WalletBalanceBounds,
    wallet_state_type: WalletOutputState,
) {
    let exact_coin_count = expected_bounds.exact_output_count();
    let exact_value = expected_bounds.exact_value();

    if exact_coin_count || exact_value {
        log_exact_condition_match(
            wallet_name,
            wallet_node_name,
            observed_state,
            wallet_state_type,
            exact_coin_count,
            exact_value,
        );
    } else {
        log_ranged_condition_match(
            wallet_name,
            wallet_node_name,
            observed_state,
            expected_bounds,
            wallet_state_type,
        );
    }
}

fn log_exact_condition_match(
    wallet_name: &str,
    wallet_node_name: &str,
    observed_state: WalletBalance,
    wallet_state_type: WalletOutputState,
    exact_coin_count: bool,
    exact_value: bool,
) {
    let wallet_state_label = wallet_output_state_label(wallet_state_type);

    match (exact_coin_count, exact_value) {
        (true, true) => info!(
            target: TARGET,
            "Wallet '{wallet_name}/{wallet_node_name}' has required '{wallet_state_label}' coin count: \
            {} and token value: {}",
            observed_state.output_count,
            observed_state.value,
        ),
        (true, false) => info!(
            target: TARGET,
            "Wallet '{wallet_name}/{wallet_node_name}' has required '{wallet_state_label}' coin count: \
            {}",
            observed_state.output_count,
        ),
        (false, true) => info!(
            target: TARGET,
            "Wallet '{wallet_name}/{wallet_node_name}' has required '{wallet_state_label}' token value: \
            {}",
            observed_state.value,
        ),
        (false, false) => unreachable!(),
    }
}

fn log_ranged_condition_match(
    wallet_name: &str,
    wallet_node_name: &str,
    observed_state: WalletBalance,
    expected_bounds: WalletBalanceBounds,
    wallet_state_type: WalletOutputState,
) {
    let coin_count_message = bound_message(
        observed_state.output_count,
        expected_bounds.min_output_count(),
        expected_bounds.max_output_count(),
    );
    let value_message = bound_message(
        observed_state.value,
        expected_bounds.min_value(),
        expected_bounds.max_value(),
    );
    let wallet_state_label = wallet_output_state_label(wallet_state_type);

    match (coin_count_message, value_message) {
        (Some(coin_count_message), Some(value_message)) => info!(
            target: TARGET,
            "Wallet '{wallet_name}/{wallet_node_name}' has required '{wallet_state_label}' coin count: \
            {coin_count_message}, token value: {value_message}",
        ),
        (Some(coin_count_message), None) => info!(
            target: TARGET,
            "Wallet '{wallet_name}/{wallet_node_name}' has required '{wallet_state_label}' coin count: \
            {coin_count_message}",
        ),
        (None, Some(value_message)) => info!(
            target: TARGET,
            "Wallet '{wallet_name}/{wallet_node_name}' has required '{wallet_state_label}' token value: \
            {value_message}",
        ),
        (None, None) => unreachable!(),
    }
}
