use std::fmt::{self, Display};

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
}

impl From<Value> for Gas {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

pub trait GasCalculator {
    type Context;

    /// Returns the gas cost of this operation.
    fn total_gas_cost<Constants: GasConstants>(
        &self,
        context: &Self::Context,
    ) -> Result<GasCost, GasOverflow>;
    fn storage_gas_cost(&self, context: &Self::Context) -> Result<GasCost, GasOverflow>;
    fn execution_gas_consumption<Constants: GasConstants>(
        &self,
        context: &Self::Context,
    ) -> Result<Gas, GasOverflow>;
    fn storage_gas_consumption(&self, context: &Self::Context) -> Result<Gas, GasOverflow>;
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("Gas overflow")]
pub struct GasOverflow;

impl<T: GasCalculator> GasCalculator for &T {
    type Context = T::Context;

    fn total_gas_cost<Constants: GasConstants>(
        &self,
        context: &Self::Context,
    ) -> Result<GasCost, GasOverflow> {
        T::total_gas_cost::<Constants>(self, context)
    }

    fn storage_gas_cost(&self, context: &Self::Context) -> Result<GasCost, GasOverflow> {
        T::storage_gas_cost(self, context)
    }

    fn execution_gas_consumption<Constants: GasConstants>(
        &self,
        context: &Self::Context,
    ) -> Result<Gas, GasOverflow> {
        T::execution_gas_consumption::<Constants>(self, context)
    }

    fn storage_gas_consumption(&self, context: &Self::Context) -> Result<Gas, GasOverflow> {
        T::storage_gas_consumption(self, context)
    }
}

pub trait GasConstants {
    /// Verify the proof of ownership and relative balance.
    const TRANSFER: Gas;

    /// Verify the inscription signature.
    const CHANNEL_INSCRIBE: Gas;

    /// Verify the administrator signature.
    const CHANNEL_SET_KEYS: Gas;

    /// Verify the deposit signature.
    const CHANNEL_DEPOSIT: Gas;

    /// Verify the withdrawal signature.
    const CHANNEL_WITHDRAW: Gas;

    /// Verify the proof of ownership.
    const SDP_DECLARE: Gas;

    /// Verify the proof of ownership.
    const SDP_WITHDRAW: Gas;

    /// Store the active message.
    const SDP_ACTIVE: Gas;

    /// Consume a reward ticket.
    const LEADER_CLAIM: Gas;
}

pub struct MainnetGasConstants;

impl GasConstants for MainnetGasConstants {
    const TRANSFER: Gas = Gas(2705);
    const CHANNEL_INSCRIBE: Gas = Gas(22);
    const CHANNEL_SET_KEYS: Gas = Gas(22);
    const CHANNEL_DEPOSIT: Gas = Gas(0);
    const CHANNEL_WITHDRAW: Gas = Gas(22);
    const SDP_DECLARE: Gas = Gas(2727);
    const SDP_WITHDRAW: Gas = Gas(2705);
    const SDP_ACTIVE: Gas = Gas(2705);
    const LEADER_CLAIM: Gas = Gas(1150);
}
