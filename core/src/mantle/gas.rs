use std::{
    fmt::{self, Display, Formatter},
    ops::Add,
};

use lb_cryptarchia_engine::{Epoch, Slot};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Visitor};

use crate::{
    header::HeaderId,
    mantle::{Value, transactions::GasPrices},
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
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

/// Shared integer factors for the execution-market recurrence.
///
/// These values are public so consensus and wallet projection use one source
/// for the protocol factors.
///
/// Denominator of the execution EMA recurrence.
pub const EXECUTION_MARKET_EMA_DENOMINATOR: u128 = 10;
/// Previous EMA weight.
pub const EXECUTION_MARKET_EMA_PREVIOUS_WEIGHT: u128 = 9;
/// Target execution gas used by the execution market.
pub const EXECUTION_GAS_TARGET: Gas = Gas::new(1_596_730);
/// Numerator component of the execution base-fee recurrence.
pub const EXECUTION_MARKET_BASE_FEE_NUMERATOR: u128 =
    7 * (EXECUTION_GAS_TARGET.into_inner() as u128);
/// Denominator of the execution base-fee recurrence.
pub const EXECUTION_MARKET_BASE_FEE_DENOMINATOR: u128 =
    8 * (EXECUTION_GAS_TARGET.into_inner() as u128);

/// The execution-market recurrence used by the ledger and by fee projection.
///
/// Keeping this arithmetic here prevents the estimator from drifting away from
/// consensus arithmetic. The returned pair is `(new_base_fee, new_average)`.
/// Applies one produced-block execution-market update with checked arithmetic.
pub fn update_execution_market(
    previous_base_fee: GasPrice,
    previous_average: Gas,
    block_execution_gas: Gas,
) -> Result<(GasPrice, Gas), GasOverflow> {
    let average = (u128::from(block_execution_gas.into_inner())
        + EXECUTION_MARKET_EMA_PREVIOUS_WEIGHT * u128::from(previous_average.into_inner()))
        / EXECUTION_MARKET_EMA_DENOMINATOR;
    let average = Value::try_from(average).map_err(|_| GasOverflow)?;
    let fee = (u128::from(previous_base_fee.into_inner())
        * (EXECUTION_MARKET_BASE_FEE_NUMERATOR + u128::from(average)))
    .div_ceil(EXECUTION_MARKET_BASE_FEE_DENOMINATOR);
    let fee = Value::try_from(fee).map_err(|_| GasOverflow)?;

    Ok((GasPrice::new(fee), Gas::new(average)))
}

/// Applies the same execution-market recurrence as [`update_execution_market`]
/// using the ledger's historical `u128 as u64` conversion semantics.
///
/// Consensus state has historically relied on this infallible conversion. The
/// checked variant is used by fee projection so an input that exceeds the
/// representable gas-price range is reported instead of silently truncated.
#[must_use]
pub fn update_execution_market_for_ledger(
    previous_base_fee: GasPrice,
    previous_average: Gas,
    block_execution_gas: Gas,
) -> (GasPrice, Gas) {
    let average = (u128::from(block_execution_gas.into_inner())
        + EXECUTION_MARKET_EMA_PREVIOUS_WEIGHT * u128::from(previous_average.into_inner()))
        / EXECUTION_MARKET_EMA_DENOMINATOR;
    let fee = (u128::from(previous_base_fee.into_inner())
        * (EXECUTION_MARKET_BASE_FEE_NUMERATOR + average))
        .div_ceil(EXECUTION_MARKET_BASE_FEE_DENOMINATOR);

    (GasPrice::new(fee as Value), Gas::new(average as Value))
}

/// Maximum protocol storage-price progression for one epoch boundary.
///
/// Storage projection applies this operation independently at each boundary;
/// it must not exponentiate a single rounded result.
pub fn max_storage_price_after_epoch(price: GasPrice) -> Result<GasPrice, GasOverflow> {
    let price = (u128::from(price.into_inner()) * STORAGE_PRICE_MAX_INCREASE_NUMERATOR)
        .div_ceil(STORAGE_PRICE_MAX_INCREASE_DENOMINATOR);
    Value::try_from(price)
        .map(GasPrice::new)
        .map_err(|_| GasOverflow)
}

/// Shared storage-market maximum upward transition factors.
/// Numerator of the maximum storage-price increase.
pub const STORAGE_PRICE_MAX_INCREASE_NUMERATOR: u128 = 9;
/// Denominator of the maximum storage-price increase.
pub const STORAGE_PRICE_MAX_INCREASE_DENOMINATOR: u128 = 8;

/// A non-negative number of epochs represented exactly in tenths.
///
/// The public JSON form is a decimal number such as `0.8` or `3.0`, but all
/// stored and calculated values are integer tenths. One hundred epochs is an
/// explicit input limit: it prevents accidental unbounded simulation while
/// leaving ample room for fee planning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EpochHeadroom {
    tenths: u16,
}

impl EpochHeadroom {
    /// Maximum accepted headroom, in tenths: `100.0` epochs.
    pub const MAX_TENTHS: u16 = 1_000;

    /// Constructs headroom from its exact fixed-point tenths representation.
    pub const fn from_tenths(tenths: u16) -> Result<Self, EpochHeadroomError> {
        if tenths <= Self::MAX_TENTHS {
            Ok(Self { tenths })
        } else {
            Err(EpochHeadroomError::TooLarge)
        }
    }

    /// Returns the exact fixed-point tenths representation.
    #[must_use]
    pub const fn tenths(self) -> u16 {
        self.tenths
    }
}

/// Validation failures for [`EpochHeadroom`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EpochHeadroomError {
    /// The supplied value was not a decimal number.
    #[error("epoch_headroom must be a decimal number")]
    Invalid,
    /// The supplied value was negative.
    #[error("epoch_headroom cannot be negative")]
    Negative,
    /// The supplied numeric value was NaN or infinite.
    #[error("epoch_headroom must be finite")]
    NonFinite,
    /// The supplied value exceeded the explicit input bound.
    #[error("epoch_headroom exceeds the maximum of 100.0 epochs")]
    TooLarge,
}

/// Parses decimal epoch headroom into exact integer tenths, truncating rather
/// than rounding any precision beyond the first decimal digit.
///
/// This intentionally avoids a floating-point parser because the public
/// semantics require decimal truncation (`1.39 -> 1.3`) and post-truncation
/// range checks, including scientific notation, without binary floating-point
/// rounding changing the result.
fn parse_decimal_tenths(value: &str) -> Result<u16, EpochHeadroomError> {
    if value.is_empty() {
        return Err(EpochHeadroomError::Invalid);
    }
    if value.starts_with('-') {
        return Err(EpochHeadroomError::Negative);
    }
    if value.starts_with('+') {
        return Err(EpochHeadroomError::Invalid);
    }

    let (mantissa, exponent) = if let Some(index) = value.find(['e', 'E']) {
        let (mantissa, exponent) = value.split_at(index);
        let exponent = exponent
            .get(1..)
            .ok_or(EpochHeadroomError::Invalid)?
            .parse::<i64>()
            .map_err(|_| EpochHeadroomError::Invalid)?;
        (mantissa, exponent)
    } else {
        (value, 0)
    };

    let (whole, fraction) = mantissa.find('.').map_or((mantissa, ""), |index| {
        let (whole, fraction) = mantissa.split_at(index);
        (whole, fraction.strip_prefix('.').unwrap_or(""))
    });

    if whole.is_empty()
        || (fraction.is_empty() && mantissa.contains('.'))
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(EpochHeadroomError::Invalid);
    }

    let decimal_position = (whole.len() as i64).checked_add(exponent).ok_or_else(|| {
        if exponent.is_negative() {
            EpochHeadroomError::Invalid
        } else {
            EpochHeadroomError::TooLarge
        }
    })?;

    if decimal_position < 0 {
        return Ok(0);
    }

    let mut digits = Vec::with_capacity(whole.len() + fraction.len());
    digits.extend_from_slice(whole.as_bytes());
    digits.extend_from_slice(fraction.as_bytes());

    let decimal_position = decimal_position as usize;
    let integer_end = decimal_position.min(digits.len());
    let integer_digits = &digits[..integer_end];
    let integer_digits = integer_digits
        .iter()
        .position(|digit| *digit != b'0')
        .map_or(&[][..], |start| &integer_digits[start..]);

    if decimal_position > digits.len() && !digits.iter().all(|byte| *byte == b'0') {
        return Err(EpochHeadroomError::TooLarge);
    }

    let whole_epochs = if integer_digits.is_empty() {
        0
    } else if integer_digits.len() > 3 {
        return Err(EpochHeadroomError::TooLarge);
    } else {
        integer_digits.iter().try_fold(0u16, |value, digit| {
            value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u16::from(*digit - b'0')))
                .ok_or(EpochHeadroomError::TooLarge)
        })?
    };

    let tenths_digit = digits.get(decimal_position).map_or(0, |digit| digit - b'0');
    let tenths = whole_epochs
        .checked_mul(10)
        .and_then(|tenths| tenths.checked_add(u16::from(tenths_digit)))
        .ok_or(EpochHeadroomError::TooLarge)?;

    EpochHeadroom::from_tenths(tenths).map(|_| tenths)
}

impl Serialize for EpochHeadroom {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // JSON has no decimal type. This is only the wire representation; the
        // value has already been reduced to exact tenths before it reaches any
        // fee arithmetic.
        serializer.serialize_f64(f64::from(self.tenths) / 10.0)
    }
}

impl<'de> Deserialize<'de> for EpochHeadroom {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EpochHeadroomVisitor;

        impl Visitor<'_> for EpochHeadroomVisitor {
            type Value = EpochHeadroom;

            fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str("a non-negative decimal epoch count")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let tenths = value
                    .checked_mul(10)
                    .ok_or_else(|| E::custom("epoch_headroom overflow"))?;
                let tenths =
                    u16::try_from(tenths).map_err(|_| E::custom(EpochHeadroomError::TooLarge))?;
                EpochHeadroom::from_tenths(tenths).map_err(E::custom)
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value < 0 {
                    return Err(E::custom(EpochHeadroomError::Negative));
                }
                self.visit_u64(value as u64)
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if !value.is_finite() {
                    return Err(E::custom(EpochHeadroomError::NonFinite));
                }
                if value < 0.0 {
                    return Err(E::custom(EpochHeadroomError::Negative));
                }
                parse_decimal_tenths(&value.to_string())
                    .and_then(EpochHeadroom::from_tenths)
                    .map_err(E::custom)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                parse_decimal_tenths(value)
                    .and_then(EpochHeadroom::from_tenths)
                    .map_err(E::custom)
            }
        }

        deserializer.deserialize_any(EpochHeadroomVisitor)
    }
}

/// User-facing fee policy. `epoch_headroom` provisions projected mandatory
/// fees; `priority_fee` remains an independent explicit inclusion incentive.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeePolicy {
    /// Optional elapsed-time horizon for projected mandatory fees.
    #[serde(default)]
    pub epoch_headroom: Option<EpochHeadroom>,
    /// Explicit inclusion incentive added independently of the horizon.
    #[serde(default)]
    pub priority_fee: Value,
}

impl FeePolicy {
    /// Creates a policy with legacy live-price funding and an explicit tip.
    #[must_use]
    pub const fn legacy(priority_fee: Value) -> Self {
        Self {
            epoch_headroom: None,
            priority_fee,
        }
    }
}

/// Diagnostic metadata returned for a fee-horizon-funded transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeeHorizonQuote {
    /// The requested elapsed-time headroom.
    pub epoch_headroom: EpochHeadroom,
    /// Header tip from which every quote input was read.
    pub prepared_at_tip: HeaderId,
    /// Slot of the preparation state.
    pub prepared_at_slot: Slot,
    /// Epoch of the preparation state.
    pub prepared_at_epoch: Epoch,
    /// Active number of slots in one epoch.
    pub slots_per_epoch: u64,
    /// Authoritative inclusive slot horizon.
    pub valid_until_slot: Slot,
    /// Epoch containing `valid_until_slot`.
    pub valid_until_epoch: Epoch,
    /// Number of storage epoch boundaries in the slot interval.
    pub storage_boundaries_crossed: u64,
    /// Expected produced blocks used for execution simulation.
    pub expected_execution_blocks: u64,
    /// Gas prices at the preparation state.
    pub live_prices: GasPrices,
    /// Prices used to fund the transaction.
    pub projected_prices: GasPrices,
    /// Assumptions and starting state for execution projection.
    pub execution_projection: ExecutionProjection,
    /// Mandatory fee at live prices.
    pub mandatory_fee_live: GasCost,
    /// Mandatory fee at projected prices.
    pub mandatory_fee_projected: GasCost,
    /// Explicit priority fee requested by the caller.
    pub explicit_priority_fee: Value,
    /// Projected mandatory fee plus explicit priority fee.
    pub total_fee: GasCost,
}

/// Execution-market assumptions recorded alongside a fee-horizon quote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionProjection {
    /// Execution EMA at the preparation state.
    pub starting_ema: Gas,
    /// Assumed execution gas in each expected future block.
    pub assumed_future_execution_gas: Gas,
    /// Stable identifier for the estimator model used by this quote.
    pub estimation_model: ExecutionProjectionModel,
    /// Expected slot interval between produced blocks used by the estimator.
    pub average_slots_per_block: u64,
}

/// Versioned execution fee-estimation model identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionProjectionModel {
    /// Simulates produced blocks at target execution utilisation.
    #[serde(rename = "target_load_v1")]
    TargetLoadV1,
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
    const CHANNEL_CONFIG: Gas;

    /// Verify the deposit signature.
    const CHANNEL_DEPOSIT: Gas;

    /// Verify the withdrawal signature.
    const CHANNEL_WITHDRAW: Gas;

    /// Verify the transfer signature.
    const CHANNEL_TRANSFER: Gas;

    /// Verify the proof of ownership.
    const SDP_DECLARE: Gas;

    /// Verify the proof of ownership.
    const SDP_WITHDRAW: Gas;

    /// Store the active message.
    const SDP_ACTIVE: Gas;

    /// Consume a reward ticket.
    const LEADER_CLAIM: Gas;

    /// Claim a `PoW` reward
    const CLAIM_POW_REWARD: Gas;
}

pub struct MainnetGasConstants;

impl GasConstants for MainnetGasConstants {
    const TRANSFER: Gas = Gas(590);
    const CHANNEL_INSCRIBE: Gas = Gas(56);
    const CHANNEL_CONFIG: Gas = Gas(56);
    const CHANNEL_DEPOSIT: Gas = Gas(590);
    const CHANNEL_WITHDRAW: Gas = Gas(56);
    const CHANNEL_TRANSFER: Gas = Gas(56);
    const SDP_DECLARE: Gas = Gas(646);
    const SDP_WITHDRAW: Gas = Gas(590);
    const SDP_ACTIVE: Gas = Gas(590);
    const LEADER_CLAIM: Gas = Gas(580);
    // TODO: Fix this value once decided
    const CLAIM_POW_REWARD: Gas = Gas(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_headroom_truncates_extra_decimal_precision() {
        for (input, expected_tenths) in [
            ("0.04", 0),
            ("0.09", 0),
            ("0.10", 1),
            ("0.19", 1),
            ("0.89", 8),
            ("1.25", 12),
            ("1.29", 12),
            ("1.30", 13),
            ("1.39", 13),
            ("3.99", 39),
            ("100.09", 1_000),
        ] {
            let parsed: EpochHeadroom = serde_json::from_str(input).unwrap();
            assert_eq!(parsed.tenths(), expected_tenths, "input: {input}");
        }

        assert!(serde_json::from_str::<EpochHeadroom>("100.10").is_err());
        assert!(serde_json::from_str::<EpochHeadroom>("-0.1").is_err());
        assert_eq!(
            serde_json::to_string(&EpochHeadroom::from_tenths(30).unwrap()).unwrap(),
            "3.0"
        );
    }

    #[test]
    fn storage_projection_rounds_each_transition() {
        let mut price = GasPrice::new(1);
        for expected in [2, 3, 4] {
            price = max_storage_price_after_epoch(price).unwrap();
            assert_eq!(price.into_inner(), expected);
        }
    }

    #[test]
    fn storage_projection_reports_overflow() {
        assert!(max_storage_price_after_epoch(GasPrice::new(u64::MAX)).is_err());
    }

    #[test]
    fn execution_projection_matches_ledger_integer_recurrence() {
        let (price, average) = update_execution_market(
            GasPrice::new(10_000),
            Gas::new(1_596_730),
            Gas::new(1_700_000),
        )
        .unwrap();
        assert_eq!(average.into_inner(), 1_607_057);
        assert_eq!(price.into_inner(), 10_009);
    }
}
