use lb_core::{
    header::HeaderId,
    mantle::{
        gas::{GasCost, GasPrice, GasProfile},
        transactions::MantleTxBuilder,
    },
};
use serde::{Deserialize, Serialize};

/// Current gas prices from the ledger state at the given tip.
#[derive(Serialize, Deserialize)]
pub struct GasPricesResponseBody {
    pub tip: HeaderId,
    pub execution_base_gas_price: GasPrice,
    pub storage_gas_price: GasPrice,
}

/// What a transaction must fit in the era of the current tip, and what its
/// Operations cost there.
#[derive(Serialize, Deserialize)]
pub struct LimitsResponseBody {
    /// The most bytes of data an Operation may carry: an inscription, or a
    /// deposit's metadata.
    pub max_op_data_size: u32,
    /// The most bytes a block's transactions may take in their canonical
    /// encoding.
    pub max_transactions_size: u32,
    /// The most execution gas a block may consume.
    pub max_gas_per_block: u64,
    /// The execution gas of the era's Operations: the context a transaction
    /// is priced under, with the gas prices of a tip.
    pub gas_profile: GasProfile,
}

/// A transaction to price as a wallet would fund it, at `tip` or the current
/// one.
#[derive(Serialize, Deserialize)]
pub struct MantleTxFeeRequestBody {
    pub tip: Option<HeaderId>,
    pub tx_builder: MantleTxBuilder,
}

/// What the transaction costs once funded at `tip`.
///
/// Mandatory fee only, before any priority reserve: the builder as it stands,
/// or with one input and a change note when it has no input, which is what a
/// wallet whose note covers the fee adds.
#[derive(Serialize, Deserialize)]
pub struct MantleTxFeeResponseBody {
    pub tip: HeaderId,
    pub tx_fee: GasCost,
}
