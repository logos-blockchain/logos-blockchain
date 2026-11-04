use std::{collections::HashMap, ops::Deref};

use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
use lb_utils::bounded::{BoundedError, UpperBoundedVec};
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
    era::ForkDigest,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ops {
    pub(crate) fork_digest: ForkDigest,
    pub(crate) ops: TxList<Op>,
}

impl Ops {
    #[must_use]
    pub fn new(fork_digest: ForkDigest, ops: impl Into<TxList<Op>>) -> Self {
        Self {
            fork_digest,
            ops: ops.into(),
        }
    }

    pub fn try_push(&mut self, op: Op) -> Result<(), BoundedError> {
        self.ops.try_push(op)
    }

    #[must_use]
    pub fn by_ref(&self) -> OpRefs<'_> {
        OpRefs::new(self.fork_digest, TxList(self.ops.0.map_ref(OpRef::from)))
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self::new(
            ForkDigest::new([0x11; 32]),
            [
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
            ],
        )
    }
}

impl Deref for Ops {
    type Target = TxList<Op>;

    fn deref(&self) -> &Self::Target {
        &self.ops
    }
}

impl IntoIterator for Ops {
    type Item = Op;
    type IntoIter = <TxList<Op> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.ops.into_iter()
    }
}

impl<'a> IntoIterator for &'a Ops {
    type Item = &'a Op;
    type IntoIter = <&'a TxList<Op> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.ops.iter()
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
        self.fork_digest.encoded_length() + self.ops.0.encoded_length()
    }
    fn encode_into(&self, out: &mut Vec<u8>) {
        self.fork_digest.encode_into(out);
        self.ops.0.encode_into(out);
    }
}

impl BinaryDecode for Ops {
    type Context = <Op as BinaryDecode>::Context;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, fork_digest) = ForkDigest::decode(input, &())?;
        let (input, ops) = TxBoundedVec::decode(input, context)?;
        Ok((
            input,
            Self {
                fork_digest,
                ops: TxList(ops),
            },
        ))
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
    fn fork_digest(&self) -> ForkDigest {
        self.fork_digest
    }

    fn op_refs(&self) -> OpRefs<'_> {
        self.by_ref()
    }

    fn op_refs_iter(&self) -> impl Iterator<Item = OpRef<'_>> {
        self.iter().map(Op::by_ref)
    }
}

/// The specification's shape of an unsigned transaction, which [`Ops`] and
/// [`OpRefs`] take in human-readable formats:
///
/// ```json
/// { "fork_digest": "…", "ops": [ ... ] }
/// ```
#[derive(Serialize, Deserialize)]
pub(crate) struct MantleTxSerde<Column> {
    pub(crate) fork_digest: ForkDigest,
    pub(crate) ops: Column,
}

impl Serialize for Ops {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            MantleTxSerde {
                fork_digest: self.fork_digest,
                ops: &self.ops,
            }
            .serialize(serializer)
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
            MantleTxSerde::<TxList<Op>>::deserialize(deserializer)
                .map(|MantleTxSerde { fork_digest, ops }| Self { fork_digest, ops })
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

/// The operations of a transaction, without its fork digest: a sequence in
/// human-readable formats, their canonical encoding otherwise.
impl Serialize for TxList<Op> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            self.inner().serialize(serializer)
        } else {
            let bytes = self.0.encode();
            serializer.serialize_bytes(&bytes)
        }
    }
}

impl<'de> Deserialize<'de> for TxList<Op> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            TxBoundedVec::deserialize(deserializer).map(Self)
        } else {
            let bytes = deserialize_bounded_bytes::<MAX_BLOCK_TRANSACTIONS_SIZE, D>(deserializer)?;
            let (remaining, ops) =
                TxBoundedVec::decode(&bytes, &()).map_err(serde::de::Error::custom)?;
            if remaining.is_empty() {
                Ok(Self(ops))
            } else {
                Err(serde::de::Error::custom(
                    "Operations binary encoding contains trailing bytes",
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

pub(crate) mod unbuilt_tx {
    //! Serde definition for the operations of a transaction not yet built,
    //! which has no fork digest yet, in the shape of:
    //!
    //! ```json
    //! { "ops": [ ... ] }
    //! ```

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::mantle::{Op, transactions::TxList};

    #[derive(Serialize, Deserialize)]
    struct UnbuiltTxSerde<Column> {
        ops: Column,
    }

    pub fn serialize<S>(ops: &TxList<Op>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        UnbuiltTxSerde { ops }.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<TxList<Op>, D::Error>
    where
        D: Deserializer<'de>,
    {
        UnbuiltTxSerde::<TxList<Op>>::deserialize(deserializer).map(|unbuilt_tx| unbuilt_tx.ops)
    }
}

#[cfg(test)]
mod tests {
    use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};

    use super::*;
    use crate::mantle::{fixtures::ops::op_values::FORK_DIGEST, gas::MainnetGasProfile};

    const CONFIGURATION_THRESHOLD: ChannelKeyIndex = 3;
    const TRANSFER_THRESHOLD: ChannelKeyIndex = 2;

    #[test]
    fn total_gas_cost_sums_execution_and_storage() {
        let ops = Ops::new(FORK_DIGEST, [Op::ChannelInscribe(InscriptionOp::sample())]);
        let context = OpsGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(2, 3));

        assert_eq!(
            ops.by_ref().total_gas_cost::<MainnetGasProfile>(&context),
            Ok(GasCost::new(739))
        );
    }

    #[test]
    fn execution_gas_consumption_uses_channel_thresholds() {
        let ops = Ops::new(
            FORK_DIGEST,
            [
                Op::ChannelConfig(ChannelConfigOp::sample()),
                Op::ChannelDeposit(DepositOp::sample()),
                Op::ChannelWithdraw(ChannelWithdrawOp::sample()),
            ],
        );
        let context = OpsGasContext::new(
            [(ChannelWithdrawOp::sample().channel_id, TRANSFER_THRESHOLD)].into(),
            [(ChannelConfigOp::sample().channel(), CONFIGURATION_THRESHOLD)].into(),
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
        let ops = Ops::new(
            FORK_DIGEST,
            [
                Op::ChannelConfig(ChannelConfigOp::sample()),
                Op::ChannelWithdraw(ChannelWithdrawOp::sample()),
                Op::ChannelTransfer(ChannelTransferOp::sample()),
            ],
        );

        assert_eq!(
            ops.by_ref()
                .execution_gas_consumption::<MainnetGasProfile>(&OpsGasContext::default()),
            Ok(Gas::from(0))
        );
    }

    #[test]
    fn execution_gas_consumption_charges_the_flat_cost_of_an_op_without_a_channel() {
        let ops = Ops::new(FORK_DIGEST, [Op::ChannelDeposit(DepositOp::sample())]);

        assert_eq!(
            ops.by_ref()
                .execution_gas_consumption::<MainnetGasProfile>(&OpsGasContext::default()),
            Ok(Gas::from(590))
        );
    }

    #[test]
    fn storage_gas_cost_prices_the_signed_size() {
        let ops = Ops::new(FORK_DIGEST, [Op::ChannelInscribe(InscriptionOp::sample())]);
        let context = OpsGasContext::new(HashMap::new(), HashMap::new(), GasPrices::new(2, 3));

        assert_eq!(
            ops.by_ref().storage_gas_cost(&context),
            Ok(GasCost::new(627))
        );
    }

    #[test]
    fn storage_gas_consumption_counts_the_fork_digest_and_length_prefix_of_an_empty_transaction() {
        assert_eq!(
            Ops::new(FORK_DIGEST, TxList::empty())
                .by_ref()
                .storage_gas_consumption(&OpsGasContext::default()),
            Ok(Gas::new(33))
        );
    }

    #[test]
    fn storage_gas_consumption_adds_the_proof_each_op_will_carry() {
        let ops = Ops::new(FORK_DIGEST, [Op::ChannelInscribe(InscriptionOp::sample())]);

        assert_eq!(
            ops.by_ref()
                .storage_gas_consumption(&OpsGasContext::default()),
            Ok(Gas::new(209))
        );
    }

    #[test]
    fn hash_of_the_empty_transaction_matches_the_specification() {
        // The Mantle specification's empty transaction vector, under its test
        // fork digest.
        let empty = Ops::new(FORK_DIGEST, TxList::empty());

        assert_eq!(
            hex::encode(empty.hash().as_signing_bytes()),
            "ac507795dbdd19eccf42c7006f7719916601c8733d3c0b975bcc589e09fd1436"
        );
    }

    #[test]
    fn serialize_to_json() {
        let ops = Ops::sample();

        assert_eq!(
            serde_json::to_value(&ops).expect("the human-readable arm serializes"),
            serde_json::json!({ "fork_digest": ops.fork_digest, "ops": ops.inner() })
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
        let json = serde_json::json!({ "fork_digest": ops.fork_digest, "ops": ops.inner() });

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
        let mut encoded_ops = Ops::new(FORK_DIGEST, TxList::empty()).encode().into_vec();
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
    fn unbuilt_tx_serialize_wraps_the_column_in_an_ops_field() {
        let ops = Ops::sample().ops;

        assert_eq!(
            unbuilt_tx::serialize(&ops, serde_json::value::Serializer)
                .expect("the human-readable arm serializes"),
            serde_json::json!({ "ops": serde_json::to_value(&ops).expect("the column serializes") })
        );
    }

    #[test]
    fn unbuilt_tx_deserialize_reads_the_ops_field() {
        let ops = Ops::sample().ops;
        let json = serde_json::json!({ "ops": serde_json::to_value(&ops).expect("the column serializes") });

        assert_eq!(
            unbuilt_tx::deserialize(json).expect("the human-readable arm deserializes"),
            ops
        );
    }
}
