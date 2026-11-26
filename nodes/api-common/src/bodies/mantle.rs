use lb_core::{header::HeaderId, mantle::gas::GasPrice};
use serde::{Deserialize, Serialize};

/// Current gas prices from the ledger state at the given tip.
#[derive(Serialize, Deserialize)]
pub struct GasPricesResponseBody {
    pub tip: HeaderId,
    pub execution_base_gas_price: GasPrice,
    pub storage_gas_price: GasPrice,
}

/// What a transaction must fit in the era of a slot: the node's current one,
/// or the one the request names.
#[derive(Serialize, Deserialize)]
pub struct LimitsResponseBody {
    /// The slot whose era the limits are of.
    pub slot: u64,
    /// The most transactions a block may carry.
    pub max_transactions: u16,
    /// The most bytes the transactions of a block may take together, which
    /// bounds every single transaction too.
    pub max_transactions_size: u32,
    /// The most bytes of data an Operation may carry: an inscription, or a
    /// deposit's metadata.
    pub max_op_data_size: u32,
}
