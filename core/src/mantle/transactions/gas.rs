use serde::{Deserialize, Serialize};

use crate::mantle::gas::GasPrice;

/// Initial storage gas price at genesis
///
/// [Spec](https://www.notion.so/nomos-tech/v1-1-Storage-Markets-Specification-326261aa09df804ab483f573f522baf5?source=copy_link#326261aa09df804280b1fd5da1120a14):
/// `P_STR(0)` = 1 LGO/gas
pub const GENESIS_STORAGE_GAS_PRICE: GasPrice = GasPrice::new(1);

/// Initial execution gas price at genesis
pub const GENESIS_EXECUTION_GAS_PRICE: GasPrice = GasPrice::new(1);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GasPrices {
    pub execution_base_gas_price: GasPrice,
    pub storage_gas_price: GasPrice,
}

impl GasPrices {
    #[must_use]
    pub fn new(execution: u64, storage: u64) -> Self {
        Self {
            execution_base_gas_price: execution.into(),
            storage_gas_price: storage.into(),
        }
    }
}

impl Default for GasPrices {
    fn default() -> Self {
        Self {
            execution_base_gas_price: GENESIS_EXECUTION_GAS_PRICE,
            storage_gas_price: GENESIS_STORAGE_GAS_PRICE,
        }
    }
}
