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
}

// A note set a transaction can buffer commitments in or change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoteSetKey {
    Ledger,
    Sdp,
    Channel(ChannelId),
}

// The state an Operation is priced against: the thresholds in force and the
// commitments the transaction has buffered in each note set before it.
pub trait OpGasContext: ThresholdSource {
    fn buffered(&self, set: &NoteSetKey) -> usize;
}

// The Execution Gas of an Operation, derived from the Operation and the state
// it is validated against EXCLUSIVELY.
pub trait OpGasCalculator<Profile: GasProfile>: OperationGas<Profile> {
    fn execution_gas(&self, _context: &impl OpGasContext) -> Result<Gas, GasOverflow> {
        Ok(Self::GAS_COST)
    }
}

// The measured components of the Execution Gas of RFC-506.
pub const EXECUTION_KB_GAS: Gas = Gas::new(26);
pub const EXECUTION_NOTE_SET_GAS: Gas = Gas::new(4285);
pub const EXECUTION_PROOF_GAS: Gas = Gas::new(1946);
pub const EXECUTION_ROOT_GAS: Gas = Gas::new(887);
pub const EXECUTION_BUFFERED_GAS: Gas = Gas::new(23);
pub const EXECUTION_NULLIFIER_GAS: Gas = Gas::new(207);
pub const EXECUTION_COMMITMENT_GAS: Gas = Gas::new(25);
pub const EXECUTION_SIGNATURE_GAS: Gas = Gas::new(167);
pub const EXECUTION_USER_STEP_GAS: Gas = Gas::new(137);
pub const EXECUTION_PAYMENT_GAS: Gas = Gas::new(211);
pub const EXECUTION_PROGRAM_STEP_GAS: Gas = Gas::new(60);
pub const EXECUTION_OPENING_GAS: Gas = Gas::new(157);
pub const EXECUTION_PROGRAM_PROOF_GAS: Gas = Gas::new(6305);

/// The Execution Gas of extending the root of a spend over `buffered`
/// commitments of the buffer, nothing when the buffer is empty.
pub fn root_gas(buffered: usize) -> Result<Gas, GasOverflow> {
    if buffered == 0 {
        return Ok(Gas::new(0));
    }
    EXECUTION_BUFFERED_GAS
        .checked_mul(Value::try_from(buffered).map_err(|_| GasOverflow)?)?
        .checked_add(EXECUTION_ROOT_GAS)
}

/// The Execution Gas of a spend of `inputs` nullifiers and `outputs`
/// commitments, its root extended over `buffered` commitments.
pub fn spend_gas(inputs: usize, outputs: usize, buffered: usize) -> Result<Gas, GasOverflow> {
    EXECUTION_PROOF_GAS
        .checked_add(root_gas(buffered)?)?
        .checked_add(
            EXECUTION_NULLIFIER_GAS
                .checked_mul(Value::try_from(inputs).map_err(|_| GasOverflow)?)?,
        )?
        .checked_add(
            EXECUTION_COMMITMENT_GAS
                .checked_mul(Value::try_from(outputs).map_err(|_| GasOverflow)?)?,
        )
}

/// The Execution Gas of the transaction itself, on top of its Operations: the
/// kilobytes it carries, floored, and one note set update per channel or SDP
/// note set it changed.
pub fn tx_terms_gas(bytes: usize, changed_note_sets: usize) -> Result<Gas, GasOverflow> {
    let bytes = Value::try_from(bytes).map_err(|_| GasOverflow)?;
    let kilobytes_gas = Gas::new(
        EXECUTION_KB_GAS
            .checked_mul(bytes)?
            .into_inner()
            .div_euclid(1000),
    );
    kilobytes_gas.checked_add(
        EXECUTION_NOTE_SET_GAS
            .checked_mul(Value::try_from(changed_note_sets).map_err(|_| GasOverflow)?)?,
    )
}

#[cfg(test)]
pub mod test_utils {
    use crate::mantle::{
        gas::{NoteSetKey, OpGasContext, ThresholdSource},
        ops::channel::{ChannelId, ChannelKeyIndex},
    };

    pub struct FixedThresholds(pub ChannelKeyIndex);

    impl ThresholdSource for FixedThresholds {
        fn configuration_threshold(&self, _channel: &ChannelId) -> ChannelKeyIndex {
            self.0
        }
    }

    impl OpGasContext for FixedThresholds {
        fn buffered(&self, _set: &NoteSetKey) -> usize {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Gas, GasOverflow, MainnetGasProfile, OpGasCalculator, OpGasContext, OperationGas, spend_gas,
        tx_terms_gas,
    };
    use crate::mantle::{Value, gas::test_utils::FixedThresholds, ops::channel::ChannelId};

    struct ScalingOp;

    impl OperationGas<MainnetGasProfile> for ScalingOp {
        const GAS_COST: Gas = Gas::new(7);
    }

    impl OpGasCalculator<MainnetGasProfile> for ScalingOp {
        fn execution_gas(&self, thresholds: &impl OpGasContext) -> Result<Gas, GasOverflow> {
            Self::GAS_COST.checked_mul(Value::from(
                thresholds.configuration_threshold(&ChannelId::from([0; 32])),
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
        fn execution_gas(&self, thresholds: &impl OpGasContext) -> Result<Gas, GasOverflow> {
            Self::GAS_COST.checked_mul(Value::from(
                thresholds.configuration_threshold(&ChannelId::from([0; 32])),
            ))
        }
    }

    #[test]
    fn spend_gas_without_buffer_has_no_root() {
        assert_eq!(spend_gas(1, 2, 0), Ok(Gas::new(1946 + 207 + 2 * 25)));
    }

    #[test]
    fn spend_gas_with_buffer_extends_the_root() {
        assert_eq!(
            spend_gas(1, 1, 3),
            Ok(Gas::new(1946 + 887 + 3 * 23 + 207 + 25))
        );
    }

    #[test]
    fn tx_terms_gas_floors_the_bytes() {
        assert_eq!(tx_terms_gas(999, 0), Ok(Gas::new(25)));
        assert_eq!(tx_terms_gas(1000, 1), Ok(Gas::new(26 + 4285)));
    }

    #[test]
    fn tx_terms_gas_overflow_is_an_error() {
        assert_eq!(tx_terms_gas(0, usize::MAX), Err(GasOverflow));
    }

    #[test]
    fn spend_gas_overflow_is_an_error() {
        assert_eq!(spend_gas(usize::MAX, 0, 0), Err(GasOverflow));
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
