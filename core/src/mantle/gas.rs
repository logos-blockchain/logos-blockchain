use std::{
    fmt::{self, Display, Formatter},
    ops::Add,
};

use serde::{Deserialize, Serialize, Serializer};
use thiserror::Error;

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

/// Maximum upward storage-price transition applied at an epoch boundary.
pub const STORAGE_PRICE_MAX_INCREASE_NUMERATOR: u128 = 9;
pub const STORAGE_PRICE_MAX_INCREASE_DENOMINATOR: u128 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
enum FeeHorizonParseError {
    #[error("fee_horizon_hours must be non-negative")]
    NonNegative,
    #[error("fee_horizon_hours must be a plain decimal number")]
    InvalidDecimal,
    #[error("fee_horizon_hours exceeds the supported maximum of 168 hours (7 days)")]
    ExceedsMaximum,
}

/// Applies the protocol's maximum upward storage-price transition with checked
/// arithmetic and rounding at this boundary.
pub fn max_storage_price_after_epoch(price: GasPrice) -> Result<GasPrice, GasOverflow> {
    let projected = u128::from(price.into_inner())
        .checked_mul(STORAGE_PRICE_MAX_INCREASE_NUMERATOR)
        .ok_or(GasOverflow)?
        .div_ceil(STORAGE_PRICE_MAX_INCREASE_DENOMINATOR);
    Value::try_from(projected)
        .map(GasPrice::new)
        .map_err(|_| GasOverflow)
}

/// A non-negative, deployment-independent duration in hours stored internally
/// in tenths of an hour.
///
/// The wallet resolves this elapsed-time policy to protocol slots using the
/// deployment's configured slot duration; tenths are only the normalized
/// representation of the public 0.1-hour precision.
///
/// User-facing decimal values are rounded up to the next 0.1 hour so funded
/// coverage is never shorter than requested. The maximum supported horizon is
/// 168 hours (7 days); larger values are rejected.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct FeeHorizonHours {
    tenths: u16,
}

impl FeeHorizonHours {
    pub const MAX_TENTHS: u16 = 1_680;

    #[must_use]
    pub const fn from_tenths(tenths: u16) -> Self {
        Self { tenths }
    }

    #[must_use]
    pub const fn tenths(self) -> u16 {
        self.tenths
    }

    fn parse_decimal(value: &str) -> Result<Self, FeeHorizonParseError> {
        if value.starts_with('-') {
            return Err(FeeHorizonParseError::NonNegative);
        }
        if value.is_empty() {
            return Err(FeeHorizonParseError::InvalidDecimal);
        }

        let (whole, fraction) = value
            .split_once('.')
            .map_or((value, None), |(whole, fraction)| (whole, Some(fraction)));
        if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(FeeHorizonParseError::InvalidDecimal);
        }
        if let Some(fraction) = fraction
            && (fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err(FeeHorizonParseError::InvalidDecimal);
        }

        let whole = whole
            .parse::<u128>()
            .map_err(|_| FeeHorizonParseError::ExceedsMaximum)?;
        let first_fractional_digit = fraction
            .and_then(|fraction| fraction.as_bytes().first().copied())
            .map_or(0, |digit| u128::from(digit - b'0'));
        let fractional_remainder_is_nonzero = fraction
            .is_some_and(|fraction| fraction.as_bytes()[1..].iter().any(|&digit| digit != b'0'));

        let tenths = whole
            .checked_mul(10)
            .and_then(|value| value.checked_add(first_fractional_digit))
            .and_then(|value| {
                if fractional_remainder_is_nonzero {
                    value.checked_add(1)
                } else {
                    Some(value)
                }
            })
            .ok_or(FeeHorizonParseError::ExceedsMaximum)?;
        if tenths > u128::from(Self::MAX_TENTHS) {
            return Err(FeeHorizonParseError::ExceedsMaximum);
        }
        Ok(Self::from_tenths(tenths as u16))
    }
}

impl std::str::FromStr for FeeHorizonHours {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse_decimal(value).map_err(|error| error.to_string())
    }
}

impl Serialize for FeeHorizonHours {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(f64::from(self.tenths) / 10.0)
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
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
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
    use super::*;

    #[test]
    fn maximum_storage_transition_rounds_at_each_boundary() {
        assert_eq!(
            max_storage_price_after_epoch(GasPrice::new(1)),
            Ok(GasPrice::new(2))
        );
        assert_eq!(
            max_storage_price_after_epoch(GasPrice::new(9)),
            Ok(GasPrice::new(11))
        );
    }

    #[test]
    fn fee_horizon_hours_decimal_input_rounds_up_to_tenths() {
        let cases = [
            ("0", 0),
            ("0.01", 1),
            ("0.0000", 0),
            ("0.09", 1),
            ("0.1", 1),
            ("0.10", 1),
            ("0.1000", 1),
            ("0.1001", 2),
            ("0.11", 2),
            ("0.25", 3),
            ("1.01", 11),
            ("1", 10),
            ("1.0", 10),
            ("1.00000", 10),
            ("1.5", 15),
            ("1.50", 15),
            ("1.5000", 15),
            ("1.5001", 16),
            ("1.50001", 16),
            ("1.999", 20),
            ("167.999", 1_680),
            ("168", 1_680),
            ("168.0", 1_680),
            ("168.00000", 1_680),
        ];

        for (input, expected_tenths) in cases {
            assert_eq!(
                input.parse::<FeeHorizonHours>().unwrap().tenths(),
                expected_tenths,
                "input: {input}"
            );
        }
        for input in ["", "-0.1", ".1", "1.", "1.2.3", "1e2", "alphabetic"] {
            assert!(
                input.parse::<FeeHorizonHours>().is_err(),
                "input should be rejected: {input}"
            );
        }
        for input in ["168.00001", "168.1", "169", "1000"] {
            assert_eq!(
                input.parse::<FeeHorizonHours>().unwrap_err(),
                FeeHorizonParseError::ExceedsMaximum.to_string(),
                "input: {input}"
            );
        }
    }

    #[test]
    fn fee_horizon_hours_serializes_normalized_values_canonically() {
        assert_eq!(
            serde_json::to_string(&FeeHorizonHours::from_tenths(0)).unwrap(),
            "0.0"
        );
        assert_eq!(
            serde_json::to_string(&FeeHorizonHours::from_tenths(3)).unwrap(),
            "0.3"
        );
        assert_eq!(
            serde_json::to_string(&FeeHorizonHours::from_tenths(15)).unwrap(),
            "1.5"
        );
    }
}
