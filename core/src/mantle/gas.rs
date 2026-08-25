use std::{
    fmt::{self, Display},
    ops::Add,
};

use serde::{Deserialize, Serialize};

use crate::mantle::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Gas(Value);

impl Gas {
    #[must_use]
    pub const fn new(value: Value) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn into_inner(self) -> Value {
        self.0
    }

    pub fn checked_add(self, rhs: Self) -> Result<Self, GasOverflow> {
        self.0.checked_add(rhs.0).ok_or(GasOverflow).map(Self)
    }

    pub fn checked_mul(self, rhs: Value) -> Result<Self, GasOverflow> {
        self.0.checked_mul(rhs).ok_or(GasOverflow).map(Self)
    }
}

impl From<Value> for Gas {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GasPrice(Value);

impl GasPrice {
    #[must_use]
    pub const fn new(value: Value) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn into_inner(self) -> Value {
        self.0
    }
}

impl Add for GasPrice {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl From<Value> for GasPrice {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GasCost(Value);

impl GasCost {
    #[must_use]
    pub const fn new(value: Value) -> Self {
        Self(value)
    }

    pub fn calculate(gas: Gas, price: GasPrice) -> Result<Self, GasOverflow> {
        gas.into_inner()
            .checked_mul(price.into_inner())
            .ok_or(GasOverflow)
            .map(Self)
    }

    #[must_use]
    pub const fn into_inner(self) -> Value {
        self.0
    }

    pub fn checked_add(self, rhs: Self) -> Result<Self, GasOverflow> {
        self.0.checked_add(rhs.0).ok_or(GasOverflow).map(Self)
    }

    pub fn checked_sub(self, rhs: Self) -> Result<Self, GasOverflow> {
        self.0.checked_sub(rhs.0).ok_or(GasOverflow).map(Self)
    }
}

impl From<Value> for GasCost {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

impl Display for GasCost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

pub trait TxGasCalculator {
    type Context;

    /// Returns the gas cost of this operation.
    fn total_gas_cost<Profile: GasProfile>(
        &self,
        context: &Self::Context,
    ) -> Result<GasCost, GasOverflow>;

    fn storage_gas_cost(&self, context: &Self::Context) -> Result<GasCost, GasOverflow>;

    fn execution_gas_consumption<Profile: GasProfile>(
        &self,
        context: &Self::Context,
    ) -> Result<Gas, GasOverflow>;

    fn storage_gas_consumption(&self, context: &Self::Context) -> Result<Gas, GasOverflow>;
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("Gas overflow")]
pub struct GasOverflow;

mod private {
    pub trait Sealed {}
}

pub trait GasProfile: private::Sealed {}

pub struct MainnetGasProfile;
impl private::Sealed for MainnetGasProfile {}
impl GasProfile for MainnetGasProfile {}

pub trait OperationGas<Profile: GasProfile> {
    const GAS_COST: Gas;
}

impl<T: OperationGas<Profile>, Profile: GasProfile> OperationGas<Profile> for &T {
    const GAS_COST: Gas = T::GAS_COST;
}

pub trait SignedOperationExecutionGas {
    /// The factor `execution_gas` scales the operation's base gas cost by.
    fn gas_multiplier(&self) -> Value;

    /// Calculates the execution gas.
    fn execution_gas<Profile: GasProfile>(&self) -> Result<Gas, GasOverflow>
    where
        Self: OperationGas<Profile>,
    {
        Self::GAS_COST.checked_mul(self.gas_multiplier())
    }
}

#[cfg(test)]
mod tests {
    use super::{Gas, GasOverflow, MainnetGasProfile, OperationGas, SignedOperationExecutionGas};
    use crate::mantle::Value;

    struct ScalingOp(Value);

    impl OperationGas<MainnetGasProfile> for ScalingOp {
        const GAS_COST: Gas = Gas::new(7);
    }

    impl SignedOperationExecutionGas for ScalingOp {
        fn gas_multiplier(&self) -> Value {
            self.0
        }
    }

    struct MaxCostOp;

    impl OperationGas<MainnetGasProfile> for MaxCostOp {
        const GAS_COST: Gas = Gas::new(Value::MAX);
    }

    impl SignedOperationExecutionGas for MaxCostOp {
        fn gas_multiplier(&self) -> Value {
            2
        }
    }

    #[test]
    fn execution_gas_scales_the_base_cost() {
        assert_eq!(
            ScalingOp(0).execution_gas::<MainnetGasProfile>().unwrap(),
            Gas::new(0)
        );
        assert_eq!(
            ScalingOp(1).execution_gas::<MainnetGasProfile>().unwrap(),
            Gas::new(7)
        );
        assert_eq!(
            ScalingOp(2).execution_gas::<MainnetGasProfile>().unwrap(),
            Gas::new(14)
        );
        assert_eq!(
            ScalingOp(3).execution_gas::<MainnetGasProfile>().unwrap(),
            Gas::new(21)
        );
    }

    #[test]
    fn execution_gas_reports_overflow() {
        assert_eq!(
            MaxCostOp.execution_gas::<MainnetGasProfile>(),
            Err(GasOverflow)
        );
    }
}
