use lb_core::{header::HeaderId, mantle::gas::GasPrice};
use serde::{Deserialize, Serialize};

/// Current gas prices from the ledger state at the given tip.
#[derive(Serialize, Deserialize)]
pub struct GasPricesResponseBody {
    pub tip: HeaderId,
    pub execution_base_gas_price: GasPrice,
    pub storage_gas_price: GasPrice,
}
