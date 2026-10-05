use lb_core::mantle::gas::{Gas, GasCost};

use crate::LedgerError;

pub const EXECUTION_GAS_LIMIT: Gas = Gas::new(3_193_460);

/// Gas consumed and fees paid by applied transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GasAndFees {
    pub(super) execution_gas: Gas,
    pub(super) storage_gas: Gas,
    pub(super) fee_burned: GasCost,
    pub(super) fee_tip: GasCost,
}

impl Default for GasAndFees {
    fn default() -> Self {
        Self {
            execution_gas: 0.into(),
            storage_gas: 0.into(),
            fee_burned: 0.into(),
            fee_tip: 0.into(),
        }
    }
}

impl GasAndFees {
    /// Adds one transaction's gas and fees, enforcing the execution gas limit.
    /// An oversized transaction is distinguished from exhausted capacity.
    pub fn checked_add<Id>(self, transaction: Self) -> Result<Self, LedgerError<Id>> {
        if transaction.execution_gas > EXECUTION_GAS_LIMIT {
            return Err(LedgerError::TooMuchTransactionExecutionGas {
                gas: transaction.execution_gas,
                limit: EXECUTION_GAS_LIMIT,
            });
        }

        let execution_gas = self.execution_gas.checked_add(transaction.execution_gas)?;
        if execution_gas > EXECUTION_GAS_LIMIT {
            return Err(LedgerError::TooMuchExecutionGas {
                gas: execution_gas,
                limit: EXECUTION_GAS_LIMIT,
            });
        }

        Ok(Self {
            execution_gas,
            storage_gas: self.storage_gas.checked_add(transaction.storage_gas)?,
            fee_burned: self.fee_burned.checked_add(transaction.fee_burned)?,
            fee_tip: self.fee_tip.checked_add(transaction.fee_tip)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_execution_gas(execution_gas: Gas) -> GasAndFees {
        GasAndFees {
            execution_gas,
            ..GasAndFees::default()
        }
    }

    #[test]
    fn rejects_a_transaction_that_exceeds_the_block_limit() {
        let excessive_gas = EXECUTION_GAS_LIMIT
            .checked_add(Gas::new(1))
            .expect("the test gas value should not overflow");
        let transaction = with_execution_gas(excessive_gas);

        assert_eq!(
            GasAndFees::default().checked_add::<()>(transaction),
            Err(LedgerError::TooMuchTransactionExecutionGas {
                gas: excessive_gas,
                limit: EXECUTION_GAS_LIMIT,
            })
        );
    }

    #[test]
    fn distinguishes_exhausted_block_capacity_from_an_oversized_transaction() {
        let accumulated = with_execution_gas(EXECUTION_GAS_LIMIT);
        let transaction = with_execution_gas(Gas::new(1));

        let cumulative_gas = EXECUTION_GAS_LIMIT
            .checked_add(transaction.execution_gas)
            .expect("the test gas value should not overflow");

        assert_eq!(
            accumulated.checked_add::<()>(transaction),
            Err(LedgerError::TooMuchExecutionGas {
                gas: cumulative_gas,
                limit: EXECUTION_GAS_LIMIT,
            })
        );
    }
}
