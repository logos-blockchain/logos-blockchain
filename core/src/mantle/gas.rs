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

/// The execution gas each Operation consumes in an era.
///
/// The channel Operations its keys authorize cost their entry once per key of
/// the threshold they are verified against, so a channel that does not exist
/// yet prices them at zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GasProfile {
    pub transfer: Gas,
    /// Per configuration key.
    pub channel_config: Gas,
    pub channel_inscribe: Gas,
    pub channel_deposit: Gas,
    /// Per transfer key.
    pub channel_withdraw: Gas,
    /// Per transfer key.
    pub channel_transfer: Gas,
    pub sdp_declare: Gas,
    pub sdp_withdraw: Gas,
    pub sdp_active: Gas,
    pub leader_claim: Gas,
    pub claim_pow_reward: Gas,
}

// The cost of every Operation, in declaration order: the order of their
// opcodes. A node's era digest commits to them through this encoding.
impl BinaryEncode for GasProfile {
    fn encoded_length(&self) -> usize {
        let Self {
            transfer,
            channel_config,
            channel_inscribe,
            channel_deposit,
            channel_withdraw,
            channel_transfer,
            sdp_declare,
            sdp_withdraw,
            sdp_active,
            leader_claim,
            claim_pow_reward,
        } = self;
        transfer.encoded_length()
            + channel_config.encoded_length()
            + channel_inscribe.encoded_length()
            + channel_deposit.encoded_length()
            + channel_withdraw.encoded_length()
            + channel_transfer.encoded_length()
            + sdp_declare.encoded_length()
            + sdp_withdraw.encoded_length()
            + sdp_active.encoded_length()
            + leader_claim.encoded_length()
            + claim_pow_reward.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            transfer,
            channel_config,
            channel_inscribe,
            channel_deposit,
            channel_withdraw,
            channel_transfer,
            sdp_declare,
            sdp_withdraw,
            sdp_active,
            leader_claim,
            claim_pow_reward,
        } = self;
        transfer.encode_into(out);
        channel_config.encode_into(out);
        channel_inscribe.encode_into(out);
        channel_deposit.encode_into(out);
        channel_withdraw.encode_into(out);
        channel_transfer.encode_into(out);
        sdp_declare.encode_into(out);
        sdp_withdraw.encode_into(out);
        sdp_active.encode_into(out);
        leader_claim.encode_into(out);
        claim_pow_reward.encode_into(out);
    }
}

/// The encoding of the fixture below, one Operation per group.
const GAS_PROFILE_HEX: &str = "
    0100000000000000 0200000000000000 0300000000000000 0400000000000000 0500000000000000
    0600000000000000 0700000000000000 0800000000000000 0900000000000000 0a00000000000000
    0b00000000000000
";

codec_fixtures!(
    GasProfile,
    encode_only,
    GasProfile {
        transfer: Gas::new(1),
        channel_config: Gas::new(2),
        channel_inscribe: Gas::new(3),
        channel_deposit: Gas::new(4),
        channel_withdraw: Gas::new(5),
        channel_transfer: Gas::new(6),
        sdp_declare: Gas::new(7),
        sdp_withdraw: Gas::new(8),
        sdp_active: Gas::new(9),
        leader_claim: Gas::new(10),
        claim_pow_reward: Gas::new(11),
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

    fn transfer_threshold(&self, channel: &ChannelId) -> ChannelKeyIndex;
}

#[cfg(test)]
pub mod test_utils {
    use crate::mantle::{
        gas::{Gas, GasProfile, ThresholdSource},
        ops::channel::{ChannelId, ChannelKeyIndex},
    };

    /// The costs of the deployments' current gas profile.
    pub const GAS_PROFILE: GasProfile = GasProfile {
        transfer: Gas::new(590),
        channel_config: Gas::new(56),
        channel_inscribe: Gas::new(56),
        channel_deposit: Gas::new(590),
        channel_withdraw: Gas::new(56),
        channel_transfer: Gas::new(56),
        sdp_declare: Gas::new(646),
        sdp_withdraw: Gas::new(590),
        sdp_active: Gas::new(590),
        leader_claim: Gas::new(580),
        claim_pow_reward: Gas::new(1),
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
