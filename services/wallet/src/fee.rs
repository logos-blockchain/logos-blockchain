//! Fee-horizon resolution for wallet funding.
//!
//! Storage and execution are deliberately modeled separately. Storage changes
//! only at epoch boundaries, so its projection is a deterministic protocol
//! ceiling: count the boundaries in the slot interval and apply the maximum
//! `ceil(price * 9 / 8)` transition independently at each boundary. Decimal
//! headroom is elapsed time, not a number of storage updates; consequently a
//! headroom of `0.8` can cross zero or one boundary depending on the starting
//! slot.
//!
//! Execution changes after produced blocks, not after slots or epochs. Future
//! demand is unknowable, so this estimator assumes every expected future block
//! consumes `G_target` (1,596,730 gas), updates the 90/10 EMA, and applies the
//! same integer base-fee recurrence as the ledger. The maximum price observed
//! during the simulated interval is selected. This is an estimate, not a
//! protocol-guaranteed execution ceiling: sustained above-target demand or a
//! different actual block-production rate can exceed it.

use lb_core::{
    header::HeaderId,
    mantle::{
        EpochHeadroom, ExecutionProjection, ExecutionProjectionModel, FeeHorizonQuote, FeePolicy,
        gas::{self, EXECUTION_GAS_TARGET, GasCost, GasPrice, MainnetGasConstants},
        transactions::{GasPrices, MantleTxBuilder, MantleTxContext},
    },
};
use lb_cryptarchia_engine::{
    Config as ConsensusConfig, Epoch, EpochConfig, Slot, average_slots_for_blocks,
};
use lb_ledger::LedgerState;
use thiserror::Error;

/// Errors raised while resolving a fee horizon from one ledger state.
#[derive(Debug, Error)]
pub enum FeeProjectionError {
    /// The slot horizon could not be represented as a slot.
    #[error("fee-horizon slot arithmetic overflow")]
    SlotOverflow,
    /// The projected slot could not be represented as an epoch.
    #[error("fee-horizon epoch arithmetic overflow")]
    EpochOverflow,
    /// A projected price or fee exceeded the supported integer range.
    #[error("fee projection arithmetic overflow")]
    ArithmeticOverflow,
    /// The transaction builder could not calculate its mandatory gas cost.
    #[error("transaction fee calculation failed: {0}")]
    TxBuilder(#[from] lb_core::mantle::transactions::TxBuilderError),
}

/// Projected context and quote awaiting final transaction fee calculation.
pub struct FeeProjection {
    /// Transaction context containing the projected gas prices.
    pub projected_context: MantleTxContext,
    /// Quote metadata populated before funding and finalized after funding.
    pub quote: FeeHorizonQuote,
}

impl FeeProjection {
    /// Completes fee metadata using the funded transaction and explicit tip.
    pub fn finalize(
        mut self,
        tx_builder: &MantleTxBuilder,
        priority_fee: u64,
    ) -> Result<FeeHorizonQuote, FeeProjectionError> {
        let live_context = self
            .projected_context
            .clone_with_live_prices(&self.quote.live_prices);
        let mandatory_fee_live =
            tx_builder.minimum_gas_cost::<MainnetGasConstants>(&live_context)?;
        let mandatory_fee_projected =
            tx_builder.minimum_gas_cost::<MainnetGasConstants>(&self.projected_context)?;
        let total_fee = mandatory_fee_projected
            .checked_add(GasCost::new(priority_fee))
            .map_err(|_| FeeProjectionError::ArithmeticOverflow)?;
        self.quote.mandatory_fee_live = mandatory_fee_live;
        self.quote.mandatory_fee_projected = mandatory_fee_projected;
        self.quote.explicit_priority_fee = priority_fee;
        self.quote.total_fee = total_fee;
        Ok(self.quote)
    }
}

/// Resolve all horizon data from the one ledger state identified by `tip`.
///
/// Storage is projected as a deterministic boundary ceiling. Execution is
/// simulated using target-load produced blocks and the ledger's integer market
/// recurrence, so the resulting execution value is an estimate.
pub fn resolve(
    tip: HeaderId,
    ledger: &LedgerState,
    epoch_config: &EpochConfig,
    consensus_config: &ConsensusConfig,
    policy: &FeePolicy,
) -> Result<FeeProjection, FeeProjectionError> {
    let headroom = policy
        .epoch_headroom
        .unwrap_or_else(|| EpochHeadroom::from_tenths(0).expect("zero is valid"));
    let slots_per_epoch = epoch_config.epoch_length(consensus_config.base_period_length());
    let prepared_at_slot = ledger.slot();
    let prepared_at_epoch = ledger.epoch_state().epoch;
    let horizon_slots = horizon_slots(headroom, slots_per_epoch)?;
    let valid_until_slot = Slot::from(
        u64::from(prepared_at_slot)
            .checked_add(horizon_slots)
            .ok_or(FeeProjectionError::SlotOverflow)?,
    );
    let valid_until_epoch: Epoch = (u64::from(valid_until_slot) / slots_per_epoch)
        .try_into()
        .map_err(|_| FeeProjectionError::EpochOverflow)?;
    let storage_boundaries_crossed = count_storage_boundaries(
        u64::from(prepared_at_slot),
        u64::from(valid_until_slot),
        slots_per_epoch,
    );

    let live_prices = ledger.get_gas_prices();
    let mut projected_storage = live_prices.storage_gas_price;
    for _ in 0..storage_boundaries_crossed {
        projected_storage = gas::max_storage_price_after_epoch(projected_storage)
            .map_err(|_| FeeProjectionError::ArithmeticOverflow)?;
    }

    let average_slots_per_block = average_slots_for_blocks(
        std::num::NonZeroU32::new(1).expect("one is non-zero"),
        consensus_config.slot_activation_coeff(),
    )
    .get();
    let expected_execution_blocks =
        expected_execution_blocks(horizon_slots, average_slots_per_block);
    let (projected_execution, _, _) = project_execution_market(
        live_prices.execution_base_gas_price,
        ledger.average_execution_gas(),
        expected_execution_blocks,
    )?;

    let projected_prices = GasPrices {
        execution_base_gas_price: projected_execution,
        storage_gas_price: projected_storage,
    };
    let live_context = ledger.tx_context();
    let projected_context = MantleTxContext {
        gas_context: live_context
            .gas_context
            .with_gas_prices(projected_prices.clone()),
        leader_reward_amount: live_context.leader_reward_amount,
    };
    let quote = FeeHorizonQuote {
        epoch_headroom: headroom,
        prepared_at_tip: tip,
        prepared_at_slot,
        prepared_at_epoch,
        slots_per_epoch,
        valid_until_slot,
        valid_until_epoch,
        storage_boundaries_crossed,
        expected_execution_blocks,
        live_prices,
        projected_prices,
        execution_projection: ExecutionProjection {
            starting_ema: ledger.average_execution_gas(),
            assumed_future_execution_gas: EXECUTION_GAS_TARGET,
            estimation_model: ExecutionProjectionModel::TargetLoadV1,
            average_slots_per_block,
        },
        mandatory_fee_live: GasCost::new(0),
        mandatory_fee_projected: GasCost::new(0),
        explicit_priority_fee: 0,
        total_fee: GasCost::new(0),
    };
    Ok(FeeProjection {
        projected_context,
        quote,
    })
}

fn horizon_slots(headroom: EpochHeadroom, slots_per_epoch: u64) -> Result<u64, FeeProjectionError> {
    let slots = u128::from(headroom.tenths())
        .checked_mul(u128::from(slots_per_epoch))
        .ok_or(FeeProjectionError::SlotOverflow)?
        .div_ceil(10);
    u64::try_from(slots).map_err(|_| FeeProjectionError::SlotOverflow)
}

const fn expected_execution_blocks(horizon_slots: u64, average_slots_per_block: u64) -> u64 {
    horizon_slots.div_ceil(average_slots_per_block)
}

/// Simulates the target-load execution model and returns `(maximum, final,
/// final_ema)`. The maximum is retained because the estimator's fee context
/// must cover every simulated price, not only the final state.
fn project_execution_market(
    initial_price: GasPrice,
    initial_ema: gas::Gas,
    expected_blocks: u64,
) -> Result<(GasPrice, GasPrice, gas::Gas), FeeProjectionError> {
    let mut execution_price = initial_price;
    let mut execution_ema = initial_ema;
    let mut maximum_price = initial_price;
    for _ in 0..expected_blocks {
        let (new_price, new_ema) =
            gas::update_execution_market(execution_price, execution_ema, EXECUTION_GAS_TARGET)
                .map_err(|_| FeeProjectionError::ArithmeticOverflow)?;
        execution_price = new_price;
        execution_ema = new_ema;
        maximum_price = maximum_price.max(execution_price);
    }
    Ok((maximum_price, execution_price, execution_ema))
}

const fn count_storage_boundaries(
    prepared_slot: u64,
    valid_until_slot: u64,
    slots_per_epoch: u64,
) -> u64 {
    valid_until_slot / slots_per_epoch - prepared_slot / slots_per_epoch
}

trait MantleTxContextExt {
    fn clone_with_live_prices(&self, prices: &GasPrices) -> MantleTxContext;
}

impl MantleTxContextExt for MantleTxContext {
    fn clone_with_live_prices(&self, prices: &GasPrices) -> MantleTxContext {
        Self {
            gas_context: self.gas_context.with_gas_prices(prices.clone()),
            leader_reward_amount: self.leader_reward_amount,
        }
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::gas::Gas;

    use super::*;

    #[test]
    fn fractional_headroom_rounds_slot_horizon_up() {
        let headroom = EpochHeadroom::from_tenths(1).unwrap();
        assert_eq!(horizon_slots(headroom, 21).unwrap(), 3);
    }

    #[test]
    fn storage_boundaries_are_counted_from_actual_slots() {
        assert_eq!(count_storage_boundaries(57, 1_657, 2_000), 0);
        assert_eq!(count_storage_boundaries(57, 2_657, 2_000), 1);
        assert_eq!(count_storage_boundaries(57, 6_057, 2_000), 3);
        assert_eq!(count_storage_boundaries(1_900, 3_500, 2_000), 1);
        assert_eq!(count_storage_boundaries(2_000, 2_000, 2_000), 0);
    }

    #[test]
    fn expected_blocks_round_up_from_active_consensus_rate() {
        assert_eq!(expected_execution_blocks(0, 7), 0);
        assert_eq!(expected_execution_blocks(1, 7), 1);
        assert_eq!(expected_execution_blocks(14, 7), 2);
        assert_eq!(expected_execution_blocks(15, 7), 3);
    }

    #[test]
    fn target_load_preserves_execution_equilibrium() {
        let (maximum, final_price, final_ema) =
            project_execution_market(GasPrice::new(10_000), EXECUTION_GAS_TARGET, 10).unwrap();

        assert_eq!(maximum, GasPrice::new(10_000));
        assert_eq!(final_price, GasPrice::new(10_000));
        assert_eq!(final_ema, EXECUTION_GAS_TARGET);
    }

    #[test]
    fn above_target_ema_can_raise_price_under_target_load() {
        let (maximum, final_price, final_ema) = project_execution_market(
            GasPrice::new(10_000),
            Gas::new(EXECUTION_GAS_TARGET.into_inner() * 2),
            2,
        )
        .unwrap();

        assert!(final_ema > EXECUTION_GAS_TARGET);
        assert!(final_price > GasPrice::new(10_000));
        assert_eq!(maximum, final_price);
    }

    #[test]
    fn below_target_ema_reduces_price_but_maximum_keeps_live_price() {
        let (maximum, final_price, final_ema) =
            project_execution_market(GasPrice::new(10_000), Gas::new(0), 1).unwrap();

        assert!(final_ema < EXECUTION_GAS_TARGET);
        assert!(final_price < GasPrice::new(10_000));
        assert_eq!(maximum, GasPrice::new(10_000));
    }
}
