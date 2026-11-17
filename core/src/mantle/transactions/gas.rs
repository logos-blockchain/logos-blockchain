use serde::{Deserialize, Serialize};

use crate::mantle::gas::GasPrice;

/// Units of `TokenValue` in 1 LGO
pub const UNITS_PER_LGO: u64 = 1_000_000_000;

/// Initial storage gas price at genesis
///
/// [Spec](https://lip.logos.co/blockchain/raw/storage-markets.html):
/// `P_STR(0)` = 1 LGO/gas, 10^9 units of `TokenValue`
pub const GENESIS_STORAGE_GAS_PRICE: GasPrice = GasPrice::new(1_000_000_000);

/// Initial execution gas price at genesis
///
/// [Spec](https://lip.logos.co/blockchain/raw/execution-market.html):
/// `b_exec` for the first block = 1 LGO/gas, 10^9 units of `TokenValue`
pub const GENESIS_EXECUTION_GAS_PRICE: GasPrice = GasPrice::new(1_000_000_000);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genesis_prices_are_one_lgo_per_gas() {
        assert_eq!(
            GasPrices::default(),
            GasPrices::new(1_000_000_000, 1_000_000_000)
        );
    }
}
