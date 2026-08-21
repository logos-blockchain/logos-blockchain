use lb_core::mantle::{
    gas::{
        FeeHorizonHours, GasCost, GasOverflow, GasPrice, MainnetGasProfile,
        max_storage_price_after_epoch,
    },
    transactions::{MantleTxBuilder, MantleTxContext},
};
use thiserror::Error;

const MILLIS_PER_HOUR: u128 = 3_600_000;

#[derive(Debug, Error)]
pub enum FeeProjectionError {
    #[error("slot duration must be greater than zero")]
    InvalidSlotDuration,
    #[error("fee horizon slot arithmetic overflow")]
    SlotOverflow,
    #[error("fee horizon epoch arithmetic overflow")]
    EpochOverflow,
    #[error("fee horizon storage-price arithmetic overflow")]
    PriceOverflow,
    #[error("fee target arithmetic overflow")]
    FeeOverflow,
    #[error(transparent)]
    Gas(#[from] GasOverflow),
    #[error(transparent)]
    TxBuilder(#[from] lb_core::mantle::transactions::TxBuilderError),
}

pub fn horizon_slots(
    horizon: FeeHorizonHours,
    slot_duration_ms: u64,
) -> Result<u64, FeeProjectionError> {
    if slot_duration_ms == 0 {
        return Err(FeeProjectionError::InvalidSlotDuration);
    }

    let numerator = u128::from(horizon.tenths())
        .checked_mul(MILLIS_PER_HOUR)
        .ok_or(FeeProjectionError::SlotOverflow)?;
    let denominator = u128::from(slot_duration_ms)
        .checked_mul(10)
        .ok_or(FeeProjectionError::SlotOverflow)?;
    u64::try_from(numerator.div_ceil(denominator)).map_err(|_| FeeProjectionError::SlotOverflow)
}

pub fn horizon_end_slot(
    preparation_slot: u64,
    current_slot: u64,
    horizon_slots: u64,
) -> Result<u64, FeeProjectionError> {
    preparation_slot
        .max(current_slot)
        .checked_add(horizon_slots)
        .ok_or(FeeProjectionError::SlotOverflow)
}

pub fn storage_boundaries(
    preparation_epoch: u32,
    horizon_end_epoch: u32,
) -> Result<u64, FeeProjectionError> {
    horizon_end_epoch
        .checked_sub(preparation_epoch)
        .map(u64::from)
        .ok_or(FeeProjectionError::EpochOverflow)
}

pub fn project_storage_price(
    mut storage_price: GasPrice,
    boundaries: u64,
) -> Result<GasPrice, FeeProjectionError> {
    for _ in 0..boundaries {
        storage_price = max_storage_price_after_epoch(storage_price)
            .map_err(|_| FeeProjectionError::PriceOverflow)?;
    }
    Ok(storage_price)
}

pub fn projected_context(
    current_context: &MantleTxContext,
    storage_price: GasPrice,
) -> MantleTxContext {
    let mut prices = current_context.gas_context.get_gas_prices();
    prices.storage_gas_price = storage_price;
    MantleTxContext {
        gas_context: current_context.gas_context.with_gas_prices(prices),
        leader_reward_amount: current_context.leader_reward_amount,
    }
}

pub fn target_fee_from_mandatory_fees(
    mandatory_current: GasCost,
    mandatory_horizon: GasCost,
    priority_fee_percent: u64,
) -> Result<GasCost, FeeProjectionError> {
    let priority_reserve = u128::from(mandatory_current.into_inner())
        .checked_mul(u128::from(priority_fee_percent))
        .and_then(|value| value.checked_add(99))
        .ok_or(FeeProjectionError::FeeOverflow)?
        / 100;
    let priority_reserve =
        u64::try_from(priority_reserve).map_err(|_| FeeProjectionError::FeeOverflow)?;
    mandatory_horizon
        .checked_add(GasCost::new(priority_reserve))
        .map_err(|_| FeeProjectionError::FeeOverflow)
}

pub fn target_fee(
    tx_builder: &MantleTxBuilder,
    current_context: &MantleTxContext,
    horizon_context: &MantleTxContext,
    priority_fee_percent: u64,
) -> Result<GasCost, FeeProjectionError> {
    let mandatory_current = tx_builder.minimum_gas_cost::<MainnetGasProfile>(current_context)?;
    let mandatory_horizon = tx_builder.minimum_gas_cost::<MainnetGasProfile>(horizon_context)?;
    target_fee_from_mandatory_fees(mandatory_current, mandatory_horizon, priority_fee_percent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point_hours_round_up_to_slots() {
        assert_eq!(
            horizon_slots(FeeHorizonHours::from_tenths(0), 1_000).unwrap(),
            0
        );
        assert_eq!(
            horizon_slots(FeeHorizonHours::from_tenths(1), 1_000).unwrap(),
            360
        );
        assert_eq!(
            horizon_slots(FeeHorizonHours::from_tenths(2), 1_000).unwrap(),
            720
        );
        assert_eq!(
            horizon_slots(FeeHorizonHours::from_tenths(1), 333).unwrap(),
            1_082
        );
    }

    #[test]
    fn stale_tip_includes_ticker_elapsed_epochs_in_storage_horizon() {
        let horizon = horizon_slots(FeeHorizonHours::from_tenths(1), 1_000).unwrap();
        let preparation_slot = 100;
        let current_ticker_slot = 150;
        let end = horizon_end_slot(preparation_slot, current_ticker_slot, horizon).unwrap();
        assert_eq!(end, 510);

        let preparation_epoch = u32::try_from(preparation_slot / 50).unwrap();
        let current_ticker_epoch = u32::try_from(current_ticker_slot / 50).unwrap();
        let horizon_end_epoch = u32::try_from(end / 50).unwrap();
        assert_eq!(preparation_epoch, 2);
        assert_eq!(current_ticker_epoch, 3);
        assert_eq!(
            storage_boundaries(preparation_epoch, horizon_end_epoch).unwrap(),
            8
        );
    }

    #[test]
    fn storage_projection_rounds_each_boundary() {
        assert_eq!(
            project_storage_price(GasPrice::new(1), 3).unwrap(),
            GasPrice::new(4)
        );
        assert_eq!(
            project_storage_price(GasPrice::new(9), 1).unwrap(),
            GasPrice::new(11)
        );
    }

    #[test]
    fn priority_is_based_on_current_mandatory_fee_only() {
        let target =
            target_fee_from_mandatory_fees(GasCost::new(794), GasCost::new(998), 12).unwrap();
        assert_eq!(target, GasCost::new(1_094));
    }

    #[test]
    fn zero_horizon_and_zero_priority_are_independent() {
        assert_eq!(
            target_fee_from_mandatory_fees(GasCost::new(794), GasCost::new(794), 12).unwrap(),
            GasCost::new(890)
        );
        assert_eq!(
            target_fee_from_mandatory_fees(GasCost::new(794), GasCost::new(794), 0).unwrap(),
            GasCost::new(794)
        );
        assert_eq!(
            target_fee_from_mandatory_fees(GasCost::new(794), GasCost::new(998), 0).unwrap(),
            GasCost::new(998)
        );
    }

    #[test]
    fn zero_priority_horizon_reserve_is_immediate_tip_until_consumed() {
        let funded_fee =
            target_fee_from_mandatory_fees(GasCost::new(794), GasCost::new(998), 0).unwrap();

        assert_eq!(funded_fee, GasCost::new(998));
        assert_eq!(funded_fee.into_inner() - 794, 204);
        assert_eq!(funded_fee.into_inner() - 998, 0);
    }

    #[test]
    fn several_boundaries_do_not_reapply_priority_to_projected_fee() {
        let projected_storage = project_storage_price(GasPrice::new(1), 3).unwrap();
        assert_eq!(projected_storage, GasPrice::new(4));

        // The fixture mandatory fee is 590 + 204 * storage price.
        let target =
            target_fee_from_mandatory_fees(GasCost::new(794), GasCost::new(1_406), 12).unwrap();
        assert_eq!(target, GasCost::new(1_502));
        assert_eq!(target.into_inner() - 1_406, 96);
    }

    #[test]
    fn arithmetic_overflow_is_reported() {
        assert!(matches!(
            target_fee_from_mandatory_fees(
                GasCost::new(u64::MAX),
                GasCost::new(u64::MAX),
                u64::MAX
            ),
            Err(FeeProjectionError::FeeOverflow)
        ));
        assert!(matches!(
            project_storage_price(GasPrice::new(u64::MAX), 1),
            Err(FeeProjectionError::PriceOverflow)
        ));
        assert!(matches!(
            horizon_end_slot(u64::MAX, u64::MAX, 1),
            Err(FeeProjectionError::SlotOverflow)
        ));
    }
}
