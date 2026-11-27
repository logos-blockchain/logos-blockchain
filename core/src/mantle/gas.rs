use std::{
    fmt::{self, Display},
    ops::Add,
};

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};
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

// An amount of gas: the `Value` it counts.
impl BinaryEncode for Gas {
    fn encoded_length(&self) -> usize {
        self.0.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.0.encode_into(out);
    }
}

codec_fixtures!(Gas, encode_only, Gas::new(0x0807_0605_0403_0201) => "0102030405060708");

/// The Execution Gas of an era: the base cost of each Operation, in opcode
/// order, and the measured components of RFC-506 an Operation and a
/// transaction add to them for what they carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GasProfile {
    pub transfer: Gas,
    pub channel_config: Gas,
    pub channel_inscribe: Gas,
    pub channel_deposit: Gas,
    pub channel_withdraw: Gas,
    pub sdp_declare: Gas,
    pub sdp_withdraw: Gas,
    pub sdp_active: Gas,
    pub claim_pow_reward: Gas,
    /// Per kilobyte of the transaction, floored.
    pub kilobyte: Gas,
    /// Per channel or SDP note set a transaction changes.
    pub note_set: Gas,
    /// Per spend proof.
    pub proof: Gas,
    /// Per commitment root a transaction extends over its buffer.
    pub root: Gas,
    /// Per buffered commitment a root is extended over.
    pub buffered: Gas,
    /// Per nullifier a spend inserts.
    pub nullifier: Gas,
    /// Per commitment a spend adds.
    pub commitment: Gas,
    /// Per signature check.
    pub signature: Gas,
    /// Per user step of an inscription, beyond its spend.
    pub user_step: Gas,
}

impl GasProfile {
    /// The Execution Gas of extending a root over `buffered` commitments of the
    /// buffer, which a transaction pays once for each distinct note set,
    /// referenced root and `buffered` of its proofs.
    pub fn root_gas(&self, buffered: usize) -> Result<Gas, GasOverflow> {
        self.buffered
            .checked_mul(Value::try_from(buffered).map_err(|_| GasOverflow)?)?
            .checked_add(self.root)
    }

    /// The Execution Gas of a spend of `inputs` nullifiers and `outputs`
    /// commitments.
    pub fn spend_gas(&self, inputs: usize, outputs: usize) -> Result<Gas, GasOverflow> {
        self.proof
            .checked_add(
                self.nullifier
                    .checked_mul(Value::try_from(inputs).map_err(|_| GasOverflow)?)?,
            )?
            .checked_add(
                self.commitment
                    .checked_mul(Value::try_from(outputs).map_err(|_| GasOverflow)?)?,
            )
    }

    /// The Execution Gas of the transaction itself, on top of its Operations:
    /// the kilobytes it carries, floored, and one note set update per channel
    /// or SDP note set it changed.
    pub fn tx_terms_gas(&self, bytes: usize, changed_note_sets: usize) -> Result<Gas, GasOverflow> {
        let bytes = Value::try_from(bytes).map_err(|_| GasOverflow)?;
        let kilobytes_gas = Gas::new(
            self.kilobyte
                .checked_mul(bytes)?
                .into_inner()
                .div_euclid(1000),
        );
        kilobytes_gas.checked_add(
            self.note_set
                .checked_mul(Value::try_from(changed_note_sets).map_err(|_| GasOverflow)?)?,
        )
    }
}

// Every cost, in declaration order. A node's era digest commits to them through
// this encoding.
impl BinaryEncode for GasProfile {
    fn encoded_length(&self) -> usize {
        let Self {
            transfer,
            channel_config,
            channel_inscribe,
            channel_deposit,
            channel_withdraw,
            sdp_declare,
            sdp_withdraw,
            sdp_active,
            claim_pow_reward,
            kilobyte,
            note_set,
            proof,
            root,
            buffered,
            nullifier,
            commitment,
            signature,
            user_step,
        } = self;
        transfer.encoded_length()
            + channel_config.encoded_length()
            + channel_inscribe.encoded_length()
            + channel_deposit.encoded_length()
            + channel_withdraw.encoded_length()
            + sdp_declare.encoded_length()
            + sdp_withdraw.encoded_length()
            + sdp_active.encoded_length()
            + claim_pow_reward.encoded_length()
            + kilobyte.encoded_length()
            + note_set.encoded_length()
            + proof.encoded_length()
            + root.encoded_length()
            + buffered.encoded_length()
            + nullifier.encoded_length()
            + commitment.encoded_length()
            + signature.encoded_length()
            + user_step.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            transfer,
            channel_config,
            channel_inscribe,
            channel_deposit,
            channel_withdraw,
            sdp_declare,
            sdp_withdraw,
            sdp_active,
            claim_pow_reward,
            kilobyte,
            note_set,
            proof,
            root,
            buffered,
            nullifier,
            commitment,
            signature,
            user_step,
        } = self;
        transfer.encode_into(out);
        channel_config.encode_into(out);
        channel_inscribe.encode_into(out);
        channel_deposit.encode_into(out);
        channel_withdraw.encode_into(out);
        sdp_declare.encode_into(out);
        sdp_withdraw.encode_into(out);
        sdp_active.encode_into(out);
        claim_pow_reward.encode_into(out);
        kilobyte.encode_into(out);
        note_set.encode_into(out);
        proof.encode_into(out);
        root.encode_into(out);
        buffered.encode_into(out);
        nullifier.encode_into(out);
        commitment.encode_into(out);
        signature.encode_into(out);
        user_step.encode_into(out);
    }
}

/// The encoding of the fixture below, one cost per group.
const GAS_PROFILE_HEX: &str = "
    0100000000000000 0300000000000000 0400000000000000 0500000000000000 0600000000000000
    0700000000000000 0800000000000000 0900000000000000 0a00000000000000 0b00000000000000
    0c00000000000000 0d00000000000000 0e00000000000000 0f00000000000000 1000000000000000
    1100000000000000 1200000000000000 1300000000000000
";

codec_fixtures!(
    GasProfile,
    encode_only,
    GasProfile {
        transfer: Gas::new(1),
        channel_config: Gas::new(3),
        channel_inscribe: Gas::new(4),
        channel_deposit: Gas::new(5),
        channel_withdraw: Gas::new(6),
        sdp_declare: Gas::new(7),
        sdp_withdraw: Gas::new(8),
        sdp_active: Gas::new(9),
        claim_pow_reward: Gas::new(10),
        kilobyte: Gas::new(11),
        note_set: Gas::new(12),
        proof: Gas::new(13),
        root: Gas::new(14),
        buffered: Gas::new(15),
        nullifier: Gas::new(16),
        commitment: Gas::new(17),
        signature: Gas::new(18),
        user_step: Gas::new(19),
    } => GAS_PROFILE_HEX
);

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
    fn total_gas_cost(&self, context: &Self::Context) -> Result<GasCost, GasOverflow>;

    fn storage_gas_cost(&self, context: &Self::Context) -> Result<GasCost, GasOverflow>;

    fn execution_gas_consumption(&self, context: &Self::Context) -> Result<Gas, GasOverflow>;

    fn storage_gas_consumption(&self, context: &Self::Context) -> Result<Gas, GasOverflow>;
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("Gas overflow")]
pub struct GasOverflow;

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

// The state an Operation is priced against: the thresholds in force.
pub trait OpGasContext: ThresholdSource {}

// The Execution Gas of an Operation under the era's `profile`, derived from the
// Operation and the state it is validated against EXCLUSIVELY.
pub trait OpGasCalculator {
    fn execution_gas(
        &self,
        profile: &GasProfile,
        context: &impl OpGasContext,
    ) -> Result<Gas, GasOverflow>;
}

#[cfg(test)]
pub mod test_utils {
    use crate::mantle::{
        gas::{Gas, GasProfile, OpGasContext, ThresholdSource},
        ops::channel::{ChannelId, ChannelKeyIndex},
    };

    /// The costs of the deployments' current gas profile.
    pub const GAS_PROFILE: GasProfile = GasProfile {
        transfer: Gas::new(10),
        channel_config: Gas::new(24),
        channel_inscribe: Gas::new(26),
        channel_deposit: Gas::new(26),
        channel_withdraw: Gas::new(33),
        sdp_declare: Gas::new(1693),
        sdp_withdraw: Gas::new(0),
        sdp_active: Gas::new(15117),
        claim_pow_reward: Gas::new(1506),
        kilobyte: Gas::new(30),
        note_set: Gas::new(8961),
        proof: Gas::new(4177),
        root: Gas::new(2332),
        buffered: Gas::new(60),
        nullifier: Gas::new(1758),
        commitment: Gas::new(58),
        signature: Gas::new(557),
        user_step: Gas::new(28),
    };

    pub struct FixedThresholds(pub ChannelKeyIndex);

    impl ThresholdSource for FixedThresholds {
        fn configuration_threshold(&self, _channel: &ChannelId) -> ChannelKeyIndex {
            self.0
        }
    }

    impl OpGasContext for FixedThresholds {}
}

#[cfg(test)]
mod tests {
    use super::{Gas, GasOverflow, test_utils::GAS_PROFILE};

    #[test]
    fn spend_gas_charges_the_proof_and_the_notes() {
        assert_eq!(
            GAS_PROFILE.spend_gas(1, 2),
            Ok(Gas::new(4177 + 1758 + 2 * 58))
        );
    }

    #[test]
    fn root_gas_charges_the_root_and_each_commitment() {
        assert_eq!(GAS_PROFILE.root_gas(3), Ok(Gas::new(2332 + 3 * 60)));
    }

    #[test]
    fn tx_terms_gas_floors_the_bytes() {
        assert_eq!(GAS_PROFILE.tx_terms_gas(999, 0), Ok(Gas::new(29)));
        assert_eq!(GAS_PROFILE.tx_terms_gas(1000, 1), Ok(Gas::new(30 + 8961)));
    }

    #[test]
    fn tx_terms_gas_overflow_is_an_error() {
        assert_eq!(GAS_PROFILE.tx_terms_gas(0, usize::MAX), Err(GasOverflow));
    }

    #[test]
    fn spend_gas_overflow_is_an_error() {
        assert_eq!(GAS_PROFILE.spend_gas(usize::MAX, 0), Err(GasOverflow));
    }
}
