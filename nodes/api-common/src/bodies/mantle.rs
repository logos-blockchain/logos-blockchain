use lb_core::{header::HeaderId, mantle::gas::GasPrice};
use serde::{Deserialize, Serialize};

/// Current gas prices from the ledger state at the given tip.
#[derive(Serialize, Deserialize)]
pub struct GasPricesResponseBody {
    pub tip: HeaderId,
    pub execution_base_gas_price: GasPrice,
    pub storage_gas_price: GasPrice,
}

/// What a transaction must fit in the era of the node's current slot.
#[derive(Serialize, Deserialize)]
pub struct LimitsResponseBody {
    /// The most bytes of data an Operation may carry: an inscription, or a
    /// deposit's metadata.
    pub max_op_data_size: u32,
}
