use lb_chain_service::{
    ChainServiceInfo,
    api::{ApiError as ChainApiError, CryptarchiaServiceApi},
};
use lb_core::header::HeaderId;
use lb_http_api_common::bodies::mantle::{GasPricesResponseBody, LimitsResponseBody};
use lb_ledger::LedgerState;
use lb_time_service::TimeServiceMessage;

use super::{Error, Handle, request};
use crate::{CryptarchiaService, TimeService};

impl From<ChainApiError> for Error {
    fn from(error: ChainApiError) -> Self {
        match error {
            ChainApiError::CommsFailure(_) => Self::unavailable(error),
            _ => Self::internal(error),
        }
    }
}

pub(super) async fn chain_api(handle: &Handle) -> CryptarchiaServiceApi<CryptarchiaService> {
    CryptarchiaServiceApi::from_overwatch_handle(handle).await
}

pub async fn info(handle: &Handle) -> Result<ChainServiceInfo, Error> {
    Ok(chain_api(handle).await.info().await?)
}

pub(crate) async fn ledger_state(handle: &Handle) -> Result<LedgerState, Error> {
    let chain_api = chain_api(handle).await;
    let tip = chain_api.info().await?.cryptarchia_info.tip;

    chain_api
        .get_ledger_state(tip)
        .await?
        .ok_or_else(|| Error::internal("ledger state for tip must exist"))
}

/// The gas prices at `tip`, or at the node's current tip when `None`.
pub async fn gas_prices(
    handle: &Handle,
    tip: Option<HeaderId>,
) -> Result<GasPricesResponseBody, Error> {
    let chain_api = chain_api(handle).await;
    let tip = match tip {
        Some(tip) => tip,
        None => chain_api.info().await?.cryptarchia_info.tip,
    };
    let gas_prices = chain_api
        .get_ledger_state(tip)
        .await?
        .ok_or_else(|| Error::NotFound("Ledger state not found for block".into()))?
        .get_gas_prices();
    Ok(GasPricesResponseBody {
        tip,
        execution_base_gas_price: gas_prices.execution_base_gas_price,
        storage_gas_price: gas_prices.storage_gas_price,
    })
}

pub async fn limits(handle: &Handle) -> Result<LimitsResponseBody, Error> {
    let slot =
        request::<TimeService, _>(handle, |sender| TimeServiceMessage::CurrentSlot { sender })
            .await?
            .slot;
    let config = chain_api(handle).await.get_ledger_config(slot).await?;
    Ok(LimitsResponseBody {
        max_op_data_size: config.max_op_data_size,
    })
}
