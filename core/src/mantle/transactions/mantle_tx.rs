use std::{collections::HashMap, sync::LazyLock};

use lb_core_macros::NomCodec;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    crypto::{Digest as _, Hasher},
    mantle::{
        GasConstants, Op, SignedMantleTx, TxHash, Value,
        channel::Channels,
        gas::{Gas, GasCost, GasOverflow},
        nom::{NomDecode as _, NomEncode as _},
        ops::{
            channel::{ChannelId, ChannelKeyIndex},
            transfer::TransferOp,
        },
        traits::{Hashable, StorageSize, hashable},
        transactions::{
            GasPrices, Ops, codec::minimum_signed_mantle_tx_size, states::VerificationState,
        },
    },
};

static MANTLE_TX_HASH_V1_BYTES: LazyLock<Vec<u8>> = LazyLock::new(|| b"MANTLE_TXHASH_V1".to_vec());

#[derive(Clone, Debug, PartialEq, Eq, NomCodec)]
pub struct MantleTx(pub Ops);

impl MantleTx {
    /// Predicts the minimum total gas cost of the transaction once signed.
    ///
    /// See [`minimum_signed_mantle_tx_size`] for why this doesn't implement
    /// [`crate::mantle::GasCalculator`] which calculates an exact gas cost.
    pub fn minimum_total_gas_cost<Constants: GasConstants>(
        &self,
        context: &MantleTxGasContext,
    ) -> Result<GasCost, GasOverflow> {
        let execution_gas = self.minimum_execution_gas_consumption::<Constants>(context)?;
        let execution_gas_cost =
            GasCost::calculate(execution_gas, context.gas_prices.execution_base_gas_price)?;
        let storage_gas_cost = self.minimum_storage_gas_cost(context)?;

        execution_gas_cost.checked_add(storage_gas_cost)
    }

    /// Predicts the minimum execution gas the transaction will consume once
    /// signed.
    pub fn minimum_execution_gas_consumption<Constants: GasConstants>(
        &self,
        context: &MantleTxGasContext,
    ) -> Result<Gas, GasOverflow> {
        self.ops()
            .iter()
            .map(|op| contextual_op_execution_gas::<Constants>(op, context))
            .try_fold(Gas::from(0), |total, gas| total.checked_add(gas?))
    }

    /// Predicts the minimum storage gas cost of the transaction once signed.
    /// See [`minimum_signed_mantle_tx_size`] for why this is a
    /// minimum, not an exact value.
    fn minimum_storage_gas_cost(
        &self,
        context: &MantleTxGasContext,
    ) -> Result<GasCost, GasOverflow> {
        GasCost::calculate(
            self.minimum_signed_serialized_size(context).into(),
            context.gas_prices.storage_gas_price,
        )
    }

    /// Predicts the minimum serialized size of the transaction once signed.
    #[must_use]
    fn minimum_signed_serialized_size(&self, context: &MantleTxGasContext) -> u64 {
        minimum_signed_mantle_tx_size(self, context) as u64
    }

    #[must_use]
    pub fn transfers(&self) -> Vec<TransferOp> {
        let mut transfers: Vec<TransferOp> = vec![];
        for op in self.ops() {
            if let Op::Transfer(transfer_op) = op {
                transfers.push(transfer_op.clone());
            }
        }
        transfers
    }

    #[must_use]
    pub const fn ops(&self) -> &Ops {
        &self.0
    }
}

impl Hashable for MantleTx {
    //noinspection RsTypeCheck: The type is correct, but the linter is confused by
    // the closure.
    const HASHER: hashable::Hasher<Self> = |tx| {
        let bytes: [u8; 32] = Hasher::digest(tx.as_signing()).into();
        TxHash::from(bytes)
    };
    type Hash = TxHash;

    fn as_signing(&self) -> Vec<u8> {
        // constant and structure as defined in the Mantle specification:
        // https://lip.logos.co/blockchain/raw/bedrock-v1.1-mantle-specification.html
        let mut buffer = MANTLE_TX_HASH_V1_BYTES.to_vec();
        buffer.extend(self.encode());
        buffer
    }
}

impl StorageSize for MantleTx {
    fn storage_size(&self) -> usize {
        self.encode().len()
    }
}

fn contextual_op_execution_gas<Constants: GasConstants>(
    op: &Op,
    context: &MantleTxGasContext,
) -> Result<Gas, GasOverflow> {
    let multiplier = match op {
        // Existing channels require the `configuration_threshold` proofs.
        // For new channels, the ledger skips proof verification. So, use 0.
        Op::ChannelConfig(operation) => context
            .configuration_threshold(&operation.channel)
            .unwrap_or(0),
        Op::ChannelWithdraw(operation) => context
            .transfer_threshold(&operation.channel_id)
            .unwrap_or(0),
        Op::ChannelTransfer(operation) => context
            .transfer_threshold(&operation.channel_id)
            .unwrap_or(0),
        _ => return Ok(op.execution_gas::<Constants>()),
    };

    op.execution_gas::<Constants>()
        .checked_mul(Value::from(multiplier))
}

impl<State: VerificationState> From<SignedMantleTx<State>> for MantleTx {
    fn from(signed_tx: SignedMantleTx<State>) -> Self {
        signed_tx.mantle_tx
    }
}

#[derive(Serialize, Deserialize)]
struct MantleTxSerde {
    pub ops: Ops,
}

impl From<MantleTxSerde> for MantleTx {
    fn from(MantleTxSerde { ops }: MantleTxSerde) -> Self {
        Self(ops)
    }
}

impl From<MantleTx> for MantleTxSerde {
    fn from(MantleTx(ops): MantleTx) -> Self {
        Self { ops }
    }
}

impl Serialize for MantleTx {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            let tx_deser: MantleTxSerde = self.clone().into();
            tx_deser.serialize(serializer)
        } else {
            let bytes = self.encode();
            serializer.serialize_bytes(&bytes)
        }
    }
}

impl<'de> Deserialize<'de> for MantleTx {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            <MantleTxSerde as Deserialize>::deserialize(deserializer).map(Into::into)
        } else {
            let bytes: Vec<u8> = <Vec<u8>>::deserialize(deserializer)?;
            Self::decode(&bytes)
                .map(|(_, tx)| tx)
                .map_err(serde::de::Error::custom)
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MantleTxContext {
    pub gas_context: MantleTxGasContext,
    pub leader_reward_amount: Value,
}

#[derive(Debug, Clone, Default)]
pub struct MantleTxGasContext {
    transfer_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
    configuration_thresholds: HashMap<ChannelId, ChannelKeyIndex>,
    gas_prices: GasPrices,
}

impl MantleTxGasContext {
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
    pub fn from_channels(channels: &Channels, base_prices: GasPrices) -> Self {
        let (transfer_thresholds, configuration_thresholds) = channels
            .into_iter()
            .map(|(channel_id, channel)| {
                (
                    (*channel_id, channel.transfer_threshold),
                    (*channel_id, channel.configuration_threshold),
                )
            })
            .unzip();
        Self::new(transfer_thresholds, configuration_thresholds, base_prices)
    }

    #[must_use]
    pub fn get_gas_prices(&self) -> GasPrices {
        self.gas_prices.clone()
    }
}
