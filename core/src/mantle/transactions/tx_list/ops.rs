use std::collections::HashMap;

use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(any(test, feature = "test-utils"))]
use crate::mantle::ops::{
    channel::{
        channel_transfer::ChannelTransferOp, config::ChannelConfigOp, deposit::DepositOp,
        inscribe::InscriptionOp, withdraw::ChannelWithdrawOp,
    },
    leader_claim::LeaderClaimOp,
    pow::ClaimPowRewardOp,
    sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
    transfer::TransferOp,
};
use crate::{
    block::MAX_BLOCK_TRANSACTIONS_SIZE,
    mantle::{
        GasProfile, Op, OpRef, TxHash, Value,
        channel::Channels,
        gas::{Gas, GasCost, GasOverflow, TxGasCalculator},
        ops::channel::{ChannelId, ChannelKeyIndex},
        traits::{Hashable, MantleTx, StorageSize, hashable},
        transactions::{
            GasPrices,
            codec::minimum_signed_transaction_size,
            thresholds::RunningThresholds,
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

pub type Ops = TxList<Op>;

impl Ops {
    #[must_use]
    pub fn by_ref(&self) -> OpRefs<'_> {
        TxList(self.0.map_ref(OpRef::from))
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self::from([
            Op::Transfer(TransferOp::sample()),               // 0x00
            Op::ChannelConfig(ChannelConfigOp::sample()),     // 0x10
            Op::ChannelInscribe(InscriptionOp::sample()),     // 0x11
            Op::ChannelDeposit(DepositOp::sample()),          // 0x12
            Op::ChannelWithdraw(ChannelWithdrawOp::sample()), // 0x13
            Op::ChannelTransfer(ChannelTransferOp::sample()), // 0x14
            Op::SDPDeclare(SDPDeclareOp::sample()),           // 0x20
            Op::SDPWithdraw(SDPWithdrawOp::sample()),         // 0x21
            Op::SDPActive(SDPActiveOp::sample()),             // 0x22
            Op::LeaderClaim(LeaderClaimOp::sample()),         // 0x30
            Op::ClaimPowReward(ClaimPowRewardOp::sample()),   // 0x40
        ])
    }
}

impl TxGasCalculator for OpRefs<'_> {
    type Context = OpsGasContext;

    fn total_gas_cost<Profile: GasProfile>(
        &self,
        context: &Self::Context,
    ) -> Result<GasCost, GasOverflow> {
        let execution_gas = self.execution_gas_consumption::<Profile>(context)?;
        let execution_gas_cost =
            GasCost::calculate(execution_gas, context.gas_prices.execution_base_gas_price)?;
        let storage_gas_cost = self.storage_gas_cost(context)?;

        execution_gas_cost.checked_add(storage_gas_cost)
    }

    fn storage_gas_cost(&self, context: &Self::Context) -> Result<GasCost, GasOverflow> {
        GasCost::calculate(
            self.storage_gas_consumption(context)?,
            context.gas_prices.storage_gas_price,
        )
    }

    fn execution_gas_consumption<Profile: GasProfile>(
        &self,
        context: &Self::Context,
    ) -> Result<Gas, GasOverflow> {
        // The thresholds carry across the fold: an Operation is priced against
        // the ones in force before it, then moves them for the ones after.
        self.iter()
            .try_fold(
                (RunningThresholds::new(context), Gas::new(0)),
                |(mut thresholds, total), op| {
                    let total = total.checked_add(op.execution_gas::<Profile>(&thresholds)?)?;
                    thresholds.apply(*op);
                    Ok((thresholds, total))
                },
            )
            .map(|(_, total)| total)
    }

    fn storage_gas_consumption(&self, context: &Self::Context) -> Result<Gas, GasOverflow> {
        Ok(Gas::new(
            minimum_signed_transaction_size(self, context) as u64
        ))
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
    use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};

    use super::*;
    use crate::mantle::gas::MainnetGasProfile;

    const CONFIGURATION_THRESHOLD: ChannelKeyIndex = 3;
    const TRANSFER_THRESHOLD: ChannelKeyIndex = 2;

    #[test]
    fn total_gas_cost_sums_execution_and_storage() {
        let ops = Ops::from([Op::ChannelInscribe(InscriptionOp::sample())]);
        let context = OpsGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(2, 3));

        assert_eq!(
            ops.by_ref().total_gas_cost::<MainnetGasProfile>(&context),
            Ok(GasCost::new(643))
        );
    }

    #[test]
    fn execution_gas_consumption_uses_channel_thresholds() {
        let ops = Ops::from([
            Op::ChannelConfig(ChannelConfigOp::sample()),
            Op::ChannelDeposit(DepositOp::sample()),
            Op::ChannelWithdraw(ChannelWithdrawOp::sample()),
        ]);
        let context = OpsGasContext::new(
            [(ChannelWithdrawOp::sample().channel_id, TRANSFER_THRESHOLD)].into(),
            [(ChannelConfigOp::sample().channel, CONFIGURATION_THRESHOLD)].into(),
            GasPrices::new(1, 0),
        );

        assert_eq!(
            ops.by_ref()
                .execution_gas_consumption::<MainnetGasProfile>(&context),
            Ok(Gas::from(168 + 590 + 112))
        );
    }

    #[test]
    fn execution_gas_consumption_charges_nothing_for_a_channel_the_context_does_not_hold() {
        let ops = Ops::from([
            Op::ChannelConfig(ChannelConfigOp::sample()),
            Op::ChannelWithdraw(ChannelWithdrawOp::sample()),
            Op::ChannelTransfer(ChannelTransferOp::sample()),
        ]);

        assert_eq!(
            ops.by_ref()
                .execution_gas_consumption::<MainnetGasProfile>(&OpsGasContext::default()),
            Ok(Gas::from(0))
        );
    }

    #[test]
    fn execution_gas_consumption_charges_the_flat_cost_of_an_op_without_a_channel() {
        let ops = Ops::from([Op::ChannelDeposit(DepositOp::sample())]);

        assert_eq!(
            ops.by_ref()
                .execution_gas_consumption::<MainnetGasProfile>(&OpsGasContext::default()),
            Ok(Gas::from(590))
        );
    }

    #[test]
    fn storage_gas_cost_prices_the_signed_size() {
        let ops = Ops::from([Op::ChannelInscribe(InscriptionOp::sample())]);
        let context = OpsGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(2, 3));

        assert_eq!(
            ops.by_ref().storage_gas_cost(&context),
            Ok(GasCost::new(531))
        );
    }

    #[test]
    fn storage_gas_consumption_counts_the_length_prefix_of_an_empty_column() {
        assert_eq!(
            Ops::empty()
                .by_ref()
                .storage_gas_consumption(&OpsGasContext::default()),
            Ok(Gas::new(1))
        );
    }

    #[test]
    fn storage_gas_consumption_adds_the_proof_each_op_will_carry() {
        let ops = Ops::from([Op::ChannelInscribe(InscriptionOp::sample())]);

        assert_eq!(
            ops.by_ref()
                .storage_gas_consumption(&OpsGasContext::default()),
            Ok(Gas::new(177))
        );
    }

    #[test]
    fn serialize_to_json() {
        let ops = Ops::sample();

        assert_eq!(
            serde_json::to_value(&ops).expect("the human-readable arm serializes"),
            serde_json::to_value(ops.inner()).expect("the inner column serializes")
        );
    }

    #[test]
    fn serialize_to_binary() {
        let ops = Ops::sample();

        assert_eq!(
            ops.to_bytes().expect("the binary arm serializes"),
            bincode::serialize(&ops.encode().into_vec()).expect("the envelope serializes")
        );
    }

    #[test]
    fn deserialize_from_json() {
        let ops = Ops::sample();
        let json = serde_json::to_value(ops.inner()).expect("the inner column serializes");

        assert_eq!(
            serde_json::from_value::<Ops>(json).expect("the human-readable arm deserializes"),
            ops
        );
    }

    #[test]
    fn deserialize_from_binary() {
        let ops = Ops::sample();
        let envelope =
            bincode::serialize(&ops.encode().into_vec()).expect("the envelope serializes");

        assert_eq!(
            Ops::from_bytes(&envelope).expect("the binary arm deserializes"),
            ops
        );
    }

    #[test]
    fn deserialize_from_binary_rejects_trailing_bytes() {
        let mut encoded_ops = Ops::empty().encode().into_vec();
        encoded_ops.push(0);
        let envelope = bincode::serialize(&encoded_ops).expect("the envelope serializes");

        assert!(bincode::deserialize::<Ops>(&envelope).is_err());
    }

    #[test]
    fn deserialize_from_binary_rejects_an_oversized_envelope() {
        let oversized = vec![0u8; MAX_BLOCK_TRANSACTIONS_SIZE + 1];
        let envelope = bincode::serialize(&oversized).expect("the envelope serializes");

        assert!(bincode::deserialize::<Ops>(&envelope).is_err());
    }

    #[test]
    fn mantle_spec_serialize_wraps_the_column_in_an_ops_field() {
        let ops = Ops::sample();

        assert_eq!(
            mantle_spec::serialize(&ops, serde_json::value::Serializer)
                .expect("the human-readable arm serializes"),
            serde_json::json!({ "ops": serde_json::to_value(&ops).expect("the column serializes") })
        );
    }

    #[test]
    fn mantle_spec_deserialize_reads_the_ops_field() {
        let ops = Ops::sample();
        let json = serde_json::json!({ "ops": serde_json::to_value(&ops).expect("the column serializes") });

        assert_eq!(
            mantle_spec::deserialize(json).expect("the human-readable arm deserializes"),
            ops
        );
    }
}
