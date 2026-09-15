use std::{
    fmt::{self, Display},
    ops::Add,
};

use serde::{Deserialize, Serialize};

use crate::mantle::{
    Value,
    ops::channel::{ChannelId, ChannelKeyIndex},
};

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

// The gas cost of a whole transaction, summed from its Operations. How exact it
// is follows the context: a live state answers what the ledger will charge, a
// snapshot only what it would charge were the transaction included right now.
pub trait TxGasCalculator {
    type Context;

    /// Returns the gas cost of this transaction.
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

// Where an Operation reads the threshold it is verified against. The ledger
// answers from the live channels, the wallet from what it predicts them to be.
pub trait ThresholdSource {
    // A channel that does not exist yet accredits no key, so both thresholds
    // are 0.
    fn configuration_threshold(&self, channel: &ChannelId) -> ChannelKeyIndex;

    fn transfer_threshold(&self, channel: &ChannelId) -> ChannelKeyIndex;
}

// The Execution Gas of an Operation, derived from the Operation and the state
// it is validated against EXCLUSIVELY.
pub trait OpGasCalculator<Profile: GasProfile>: OperationGas<Profile> {
    fn execution_gas(&self, _thresholds: &impl ThresholdSource) -> Result<Gas, GasOverflow> {
        Ok(Self::GAS_COST)
    }
}

#[cfg(test)]
pub mod test_utils {
    use crate::mantle::{
        gas::ThresholdSource,
        ops::channel::{ChannelId, ChannelKeyIndex},
    };

    pub struct FixedThresholds(pub ChannelKeyIndex);

    impl ThresholdSource for FixedThresholds {
        fn configuration_threshold(&self, _channel: &ChannelId) -> ChannelKeyIndex {
            self.0
        }

        fn transfer_threshold(&self, _channel: &ChannelId) -> ChannelKeyIndex {
            self.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Gas, GasOverflow, MainnetGasProfile, OpGasCalculator, OperationGas, ThresholdSource,
    };
    use crate::mantle::{Value, gas::test_utils::FixedThresholds, ops::channel::ChannelId};

    struct ScalingOp;

    impl OperationGas<MainnetGasProfile> for ScalingOp {
        const GAS_COST: Gas = Gas::new(7);
    }

    impl OpGasCalculator<MainnetGasProfile> for ScalingOp {
        fn execution_gas(&self, thresholds: &impl ThresholdSource) -> Result<Gas, GasOverflow> {
            Self::GAS_COST.checked_mul(Value::from(
                thresholds.transfer_threshold(&ChannelId::from([0; 32])),
            ))
        }
    }

    struct FlatOp;

    impl OperationGas<MainnetGasProfile> for FlatOp {
        const GAS_COST: Gas = Gas::new(7);
    }

    impl OpGasCalculator<MainnetGasProfile> for FlatOp {}

    struct MaxCostOp;

    impl OperationGas<MainnetGasProfile> for MaxCostOp {
        const GAS_COST: Gas = Gas::new(Value::MAX);
    }

    impl OpGasCalculator<MainnetGasProfile> for MaxCostOp {
        fn execution_gas(&self, thresholds: &impl ThresholdSource) -> Result<Gas, GasOverflow> {
            Self::GAS_COST.checked_mul(Value::from(
                thresholds.transfer_threshold(&ChannelId::from([0; 32])),
            ))
        }
    }

    #[test]
    fn execution_gas_scales_the_base_cost_by_the_threshold() {
        assert_eq!(
            ScalingOp.execution_gas(&FixedThresholds(0)),
            Ok(Gas::new(0))
        );
        assert_eq!(
            ScalingOp.execution_gas(&FixedThresholds(1)),
            Ok(Gas::new(7))
        );
        assert_eq!(
            ScalingOp.execution_gas(&FixedThresholds(2)),
            Ok(Gas::new(14))
        );
    }

    #[test]
    fn execution_gas_defaults_to_the_base_cost_whatever_the_threshold() {
        assert_eq!(FlatOp.execution_gas(&FixedThresholds(0)), Ok(Gas::new(7)));
        assert_eq!(FlatOp.execution_gas(&FixedThresholds(3)), Ok(Gas::new(7)));
    }

    #[test]
    fn execution_gas_reports_overflow() {
        assert_eq!(
            MaxCostOp.execution_gas(&FixedThresholds(2)),
            Err(GasOverflow)
        );
    }
}
