use std::collections::HashMap;

use lb_codec::{BinaryDecode, BinaryEncode, DecodeError};
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    block::MAX_BLOCK_TRANSACTIONS_SIZE,
    mantle::{
        GasProfile, Op, OpRef, TxHash, Value,
        channel::Channels,
        gas::{Gas, GasCost, GasOverflow},
        ops::channel::{ChannelId, ChannelKeyIndex},
        traits::{Hashable, MantleTx, StorageSize, hashable},
        transactions::{
            GasPrices,
            codec::minimum_signed_transaction_size,
            tx_list::{
                OpRefs,
                common::{TxBoundedVec, TxList},
                hash::tx_hasher,
            },
        },
    },
};

#[derive(Debug, Clone, Default)]
pub struct OpsGasContext {
    transfer_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
    configuration_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
    gas_prices: GasPrices,
}

impl OpsGasContext {
    #[must_use]
    pub const fn new(
        transfer_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
        configuration_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
        gas_prices: GasPrices,
    ) -> Self {
        Self {
            transfer_thresholds,
            configuration_thresholds,
            gas_prices,
        }
    }

    #[must_use]
    pub fn transfer_threshold(&self, channel_id: &ChannelId) -> Option<ChannelKeyIndex> {
        self.transfer_thresholds.get(channel_id).copied()
    }

    #[must_use]
    pub fn configuration_threshold(&self, channel_id: &ChannelId) -> Option<ChannelKeyIndex> {
        self.configuration_thresholds.get(channel_id).copied()
    }

    #[must_use]
    pub fn from_channels(value: &Channels, base_prices: GasPrices) -> Self {
        let transfer_thresholds = value
            .channels
            .iter()
            .map(|(channel_id, channel)| (*channel_id, channel.transfer_threshold))
            .collect();
        let configuration_thresholds = value
            .channels
            .iter()
            .map(|(channel_id, channel)| (*channel_id, channel.configuration_threshold))
            .collect();
        Self::new(transfer_thresholds, configuration_thresholds, base_prices)
    }

    #[must_use]
    pub fn get_gas_prices(&self) -> GasPrices {
        self.gas_prices.clone()
    }
}

#[derive(Debug, Clone, Default)]
pub struct OpsContext {
    pub gas_context: OpsGasContext,
    pub leader_reward_amount: Value,
}

/// The Execution Gas of an Operation, derived from the Operation and the
/// channel state it is validated against. The proof is never an input.
pub fn op_execution_gas<Profile: GasProfile>(
    op: OpRef<'_>,
    channels: &Channels,
) -> Result<Gas, GasOverflow> {
    // A channel that does not exist yet has no accredited key, so its
    // threshold is 0.
    let threshold = match op {
        OpRef::ChannelConfig(operation) => channels
            .channels
            .get(&operation.channel)
            .map_or(0, |channel| channel.configuration_threshold),
        OpRef::ChannelWithdraw(operation) => channels
            .channels
            .get(&operation.channel_id)
            .map_or(0, |channel| channel.transfer_threshold),
        OpRef::ChannelTransfer(operation) => channels
            .channels
            .get(&operation.channel_id)
            .map_or(0, |channel| channel.transfer_threshold),
        _ => return Ok(op.gas_cost::<Profile>()),
    };

    op.gas_cost::<Profile>().checked_mul(Value::from(threshold))
}

// The thresholds an Operation is verified against
pub struct RunningThresholds<'a> {
    context: &'a OpsGasContext,
    transfer_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
    configuration_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
}

impl<'a> RunningThresholds<'a> {
    #[must_use]
    pub fn new(context: &'a OpsGasContext) -> Self {
        Self {
            context,
            transfer_thresholds: HashMap::new(),
            configuration_thresholds: HashMap::new(),
        }
    }

    // A channel that does not exist yet has no accredited key, so its threshold
    // is 0.
    #[must_use]
    pub fn transfer(&self, channel_id: &ChannelId) -> ChannelKeyIndex {
        self.transfer_thresholds
            .get(channel_id)
            .copied()
            .or_else(|| self.context.transfer_threshold(channel_id))
            .unwrap_or(0)
    }

    #[must_use]
    pub fn configuration(&self, channel_id: &ChannelId) -> ChannelKeyIndex {
        self.configuration_thresholds
            .get(channel_id)
            .copied()
            .or_else(|| self.context.configuration_threshold(channel_id))
            .unwrap_or(0)
    }

    // Call once the Operation has been priced: a `ChannelConfig` is itself
    // verified against the thresholds in force before it.
    pub fn apply(&mut self, op: OpRef<'_>) {
        if let OpRef::ChannelConfig(operation) = op {
            self.transfer_thresholds
                .insert(operation.channel, operation.transfer_threshold);
            self.configuration_thresholds
                .insert(operation.channel, operation.configuration_threshold);
        }
    }
}

fn contextual_op_execution_gas<Profile: GasProfile>(
    op: OpRef<'_>,
    thresholds: &RunningThresholds,
) -> Result<Gas, GasOverflow> {
    let multiplier = match op {
        OpRef::ChannelConfig(operation) => thresholds.configuration(&operation.channel),
        OpRef::ChannelWithdraw(operation) => thresholds.transfer(&operation.channel_id),
        OpRef::ChannelTransfer(operation) => thresholds.transfer(&operation.channel_id),
        _ => return Ok(op.gas_cost::<Profile>()),
    };

    op.gas_cost::<Profile>()
        .checked_mul(Value::from(multiplier))
}

pub type Ops = TxList<Op>;

impl Ops {
    #[must_use]
    pub fn by_ref(&self) -> OpRefs<'_> {
        TxList(self.0.map_ref(OpRef::from))
    }
}

impl OpRefs<'_> {
    /// Predicts the minimum total gas cost of the transaction once signed.
    ///
    /// See [`minimum_signed_transaction_size`] for why this is a minimum, not
    /// an exact gas cost.
    pub fn minimum_total_gas_cost<Profile: GasProfile>(
        &self,
        context: &OpsGasContext,
    ) -> Result<GasCost, GasOverflow> {
        let execution_gas = self.minimum_execution_gas_consumption::<Profile>(context)?;
        let execution_gas_cost =
            GasCost::calculate(execution_gas, context.gas_prices.execution_base_gas_price)?;
        let storage_gas_cost = self.minimum_storage_gas_cost(context)?;

        execution_gas_cost.checked_add(storage_gas_cost)
    }

    /// Predicts the minimum execution gas the transaction will consume once
    /// signed.
    pub fn minimum_execution_gas_consumption<Profile: GasProfile>(
        &self,
        context: &OpsGasContext,
    ) -> Result<Gas, GasOverflow> {
        let mut thresholds = RunningThresholds::new(context);
        let mut total = Gas::new(0);
        for op in self.iter() {
            total = total.checked_add(contextual_op_execution_gas::<Profile>(*op, &thresholds)?)?;
            thresholds.apply(*op);
        }
        Ok(total)
    }

    /// Predicts the minimum storage gas cost of the transaction once signed.
    /// See [`minimum_signed_transaction_size`] for why this is a
    /// minimum, not an exact value.
    fn minimum_storage_gas_cost(&self, context: &OpsGasContext) -> Result<GasCost, GasOverflow> {
        GasCost::calculate(
            self.minimum_signed_serialized_size(context).into(),
            context.gas_prices.storage_gas_price,
        )
    }

    /// Predicts the minimum serialized size of the transaction once signed.
    #[must_use]
    fn minimum_signed_serialized_size(&self, context: &OpsGasContext) -> u64 {
        minimum_signed_transaction_size(self, context) as u64
    }
}

impl BinaryEncode for Ops {
    fn encoded_length(&self) -> usize {
        self.0.encoded_length()
    }
    fn encode_into(&self, out: &mut Vec<u8>) {
        self.0.encode_into(out);
    }
}

impl BinaryDecode for Ops {
    type Context = <Op as BinaryDecode>::Context;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        TxBoundedVec::decode(input, context).map(|(rest, ops)| (rest, Self(ops)))
    }
}

impl Hashable for Ops {
    //noinspection RsTypeCheck: The type is correct, but the linter is confused by
    // the closure.
    const HASHER: hashable::Hasher<Self> = tx_hasher;
    type Hash = TxHash;

    fn as_signing(&self) -> Vec<u8> {
        self.by_ref().as_signing()
    }
}

impl StorageSize for Ops {
    fn storage_size(&self) -> usize {
        self.encode().len()
    }
}

impl MantleTx for Ops {
    fn op_refs(&self) -> OpRefs<'_> {
        self.by_ref()
    }

    fn op_refs_iter(&self) -> impl Iterator<Item = OpRef<'_>> {
        self.iter().map(Op::by_ref)
    }
}

impl Serialize for Ops {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            self.inner().serialize(serializer)
        } else {
            let bytes = self.encode();
            serializer.serialize_bytes(&bytes)
        }
    }
}

impl<'de> Deserialize<'de> for Ops {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            TxBoundedVec::deserialize(deserializer).map(Self)
        } else {
            let bytes = deserialize_bounded_bytes::<MAX_BLOCK_TRANSACTIONS_SIZE, D>(deserializer)?;
            let (remaining, tx) = Self::decode(&bytes, &()).map_err(serde::de::Error::custom)?;
            if remaining.is_empty() {
                Ok(tx)
            } else {
                Err(serde::de::Error::custom(
                    "MantleTx binary encoding contains trailing bytes",
                ))
            }
        }
    }
}

fn deserialize_bounded_bytes<'de, const MAX: usize, D>(
    deserializer: D,
) -> Result<UpperBoundedVec<u8, MAX>, D::Error>
where
    D: Deserializer<'de>,
{
    struct Visitor<const MAX: usize>;

    impl<const MAX: usize> serde::de::Visitor<'_> for Visitor<MAX> {
        type Value = UpperBoundedVec<u8, MAX>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "at most {MAX} encoded MantleTx bytes")
        }

        fn visit_bytes<E>(self, bytes: &[u8]) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            if bytes.len() > MAX {
                return Err(E::custom(format_args!(
                    "encoded MantleTx contains {} bytes, maximum is {MAX}",
                    bytes.len()
                )));
            }

            Ok(UpperBoundedVec::new_unchecked(bytes.to_vec()))
        }

        fn visit_byte_buf<E>(self, bytes: Vec<u8>) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            let byte_len = bytes.len();

            UpperBoundedVec::try_from(bytes).map_err(|_| {
                E::custom(format_args!(
                    "encoded MantleTx contains {byte_len} bytes, maximum is {MAX}"
                ))
            })
        }
    }

    deserializer.deserialize_bytes(Visitor::<MAX>)
}

pub mod mantle_spec {
    //! Mantle specification serde definition for the spec's *unsigned*
    //! transaction, in the shape of:
    //!
    //! ```json
    //! { "ops": [ ... ] }
    //! ```

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::Ops;

    /// Mantle specification shape of the *unsigned* transaction
    #[derive(Serialize, Deserialize)]
    struct MantleTxSerde<Column> {
        ops: Column,
    }

    pub fn serialize<Column, S>(ops: &Column, serializer: S) -> Result<S::Ok, S::Error>
    where
        Column: Serialize,
        S: Serializer,
    {
        MantleTxSerde { ops }.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Ops, D::Error>
    where
        D: Deserializer<'de>,
    {
        MantleTxSerde::<Ops>::deserialize(deserializer).map(|mantle_tx| mantle_tx.ops)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_serde_rejects_trailing_bytes_inside_transaction_envelope() {
        let tx = Ops::empty();
        let mut encoded_tx = tx.encode().into_vec();
        encoded_tx.push(0);
        let envelope = bincode::serialize(&encoded_tx).unwrap();

        assert!(bincode::deserialize::<Ops>(&envelope).is_err());
    }

    #[test]
    fn binary_serde_rejects_oversized_transaction_envelope() {
        let oversized = vec![0u8; MAX_BLOCK_TRANSACTIONS_SIZE + 1];
        let envelope = bincode::serialize(&oversized).unwrap();

        assert!(bincode::deserialize::<Ops>(&envelope).is_err());
    }
}
