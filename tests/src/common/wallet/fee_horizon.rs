use std::num::NonZero;

use lb_chain_service::Epoch;
use lb_core::{
    header::HeaderId,
    mantle::{gas::GasPrice, transactions::GasPrices},
};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FeeProjectionError {
    #[error("projected storage gas price overflowed u64")]
    PriceOverflow,
    #[error("priority fee percentage overflowed u64")]
    PriorityFeeOverflow,
    #[error("epoch slot length must be greater than zero")]
    InvalidEpochLength,
    #[error("epoch {prepared_at:?} plus {headroom} epochs overflowed")]
    EpochOverflow { prepared_at: Epoch, headroom: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionFeeHorizon {
    pub prepared_at_tip: HeaderId,
    pub prepared_at_epoch: Epoch,
    pub valid_through_epoch: Epoch,
    pub live_prices: GasPrices,
    pub ceiling_prices: GasPrices,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionFeePolicy {
    pub horizon: TransactionFeeHorizon,
    pub priority_fee_percent: u64,
}

impl TransactionFeePolicy {
    pub fn new(horizon: TransactionFeeHorizon) -> Result<Self, FeeProjectionError> {
        let priority_fee_percent = priority_fee_percent_for_storage_prices(
            horizon.live_prices.storage_gas_price,
            horizon.ceiling_prices.storage_gas_price,
        )?;

        Ok(Self {
            horizon,
            priority_fee_percent,
        })
    }
}

/// Converts the storage-price projection into the percentage reserve accepted
/// by wallet funding.
///
/// The wallet's percentage is applied to the complete mandatory fee. Since
/// execution gas is unchanged by the test horizon and storage gas is the only
/// projected component, the storage-price ratio is a conservative reserve for
/// the projected mandatory fee:
///
/// `priority_fee_percent = ceil(projected_storage * 100 / live_storage) - 100`
///
/// The projection is already rounded at each epoch boundary, so this uses the
/// actual projected price rather than approximating `(9 / 8)^epochs_headroom`.
pub fn priority_fee_percent_for_storage_prices(
    live_storage_price: GasPrice,
    projected_storage_price: GasPrice,
) -> Result<u64, FeeProjectionError> {
    let live = u128::from(live_storage_price.into_inner());
    let projected = u128::from(projected_storage_price.into_inner());

    if live == 0 || projected <= live {
        return Ok(0);
    }

    let gross_percent = projected
        .checked_mul(100)
        .ok_or(FeeProjectionError::PriorityFeeOverflow)?
        .div_ceil(live);
    u64::try_from(
        gross_percent
            .checked_sub(100)
            .ok_or(FeeProjectionError::PriorityFeeOverflow)?,
    )
    .map_err(|_| FeeProjectionError::PriorityFeeOverflow)
}

pub fn project_storage_price(
    mut price: GasPrice,
    epochs_headroom: u32,
) -> Result<GasPrice, FeeProjectionError> {
    for _ in 0..epochs_headroom {
        let current = u128::from(price.into_inner());
        let projected = current
            .checked_mul(9)
            .ok_or(FeeProjectionError::PriceOverflow)?
            .div_ceil(8);
        price =
            GasPrice::new(u64::try_from(projected).map_err(|_| FeeProjectionError::PriceOverflow)?);
    }
    Ok(price)
}

pub fn build_fee_horizon(
    prepared_at_tip: HeaderId,
    slot: u64,
    slots_per_epoch: NonZero<u64>,
    epochs_headroom: u32,
    live_prices: GasPrices,
) -> Result<TransactionFeeHorizon, FeeProjectionError> {
    let prepared_at_epoch =
        Epoch::new(u32::try_from(slot / slots_per_epoch.get()).map_err(|_| {
            FeeProjectionError::EpochOverflow {
                prepared_at: Epoch::new(0),
                headroom: epochs_headroom,
            }
        })?);
    let valid_through_epoch = prepared_at_epoch
        .into_inner()
        .checked_add(epochs_headroom)
        .map(Epoch::new)
        .ok_or(FeeProjectionError::EpochOverflow {
            prepared_at: prepared_at_epoch,
            headroom: epochs_headroom,
        })?;
    let ceiling_prices = GasPrices {
        execution_base_gas_price: live_prices.execution_base_gas_price,
        storage_gas_price: project_storage_price(live_prices.storage_gas_price, epochs_headroom)?,
    };

    Ok(TransactionFeeHorizon {
        prepared_at_tip,
        prepared_at_epoch,
        valid_through_epoch,
        live_prices,
        ceiling_prices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_price_rounds_at_each_epoch_boundary() {
        assert_eq!(
            project_storage_price(GasPrice::new(1), 2),
            Ok(GasPrice::new(3))
        );
    }

    #[test]
    fn zero_headroom_preserves_storage_price() {
        assert_eq!(
            project_storage_price(GasPrice::new(7), 0),
            Ok(GasPrice::new(7))
        );

        assert_eq!(
            priority_fee_percent_for_storage_prices(GasPrice::new(7), GasPrice::new(7)),
            Ok(0)
        );
    }

    #[test]
    fn priority_fee_percent_covers_projected_storage_increase() {
        assert_eq!(
            priority_fee_percent_for_storage_prices(GasPrice::new(100), GasPrice::new(113)),
            Ok(13)
        );
        assert_eq!(
            priority_fee_percent_for_storage_prices(GasPrice::new(3), GasPrice::new(4)),
            Ok(34)
        );
    }

    #[test]
    fn priority_fee_percent_uses_integer_projected_prices() {
        assert_eq!(
            priority_fee_percent_for_storage_prices(GasPrice::new(1), GasPrice::new(3)),
            Ok(200)
        );
    }

    #[test]
    fn overflow_is_reported() {
        assert_eq!(
            project_storage_price(GasPrice::new(u64::MAX), 1),
            Err(FeeProjectionError::PriceOverflow)
        );
    }

    #[test]
    fn horizon_uses_current_epoch_and_future_bound() {
        let horizon = build_fee_horizon(
            HeaderId::from([7; 32]),
            70,
            NonZero::new(10).expect("non-zero epoch length"),
            2,
            GasPrices::new(1, 1),
        )
        .expect("horizon should be valid");

        assert_eq!(horizon.prepared_at_epoch, Epoch::new(7));
        assert_eq!(horizon.valid_through_epoch, Epoch::new(9));
        assert_eq!(horizon.ceiling_prices.storage_gas_price, GasPrice::new(3));
        assert_eq!(
            horizon.ceiling_prices.execution_base_gas_price,
            GasPrice::new(1)
        );

        let policy = TransactionFeePolicy::new(horizon).expect("policy should be valid");
        assert_eq!(policy.priority_fee_percent, 200);
    }
}
