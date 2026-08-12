use lb_codec::{BinaryDecode, BinaryEncode, DecodeError};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    crypto::{Digest as _, Hash, Hasher},
    mantle::{
        GasProfile,
        gas::{Gas, OperationGas},
        ledger::ProvableOperation as _,
        ops::{
            OPERATION_ID_V1,
            channel::{
                channel_transfer::ChannelTransferOp, config::ChannelConfigOp, deposit::DepositOp,
                inscribe::InscriptionOp, withdraw::ChannelWithdrawOp,
            },
            internal::{OpDe, OpSer},
            leader_claim::LeaderClaimOp,
            pow::ClaimPowRewardOp,
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
            transfer::TransferOp,
        },
    },
};

pub trait OpId {
    fn op_id(&self) -> Hash {
        let mut encoded_bytes = OPERATION_ID_V1.clone();
        encoded_bytes.extend(self.op_bytes());
        Hasher::digest(&encoded_bytes).into()
    }

    fn op_bytes(&self) -> Vec<u8>;
}

/// Core set of supported Mantle operations.
///
/// This type serves as the public-facing representation of [`OpSer`] and
/// [`OpDe`], delegating default serialization and deserialization to them.
///
/// Serialization and deserialization share a single [`serde_::OpWire`] wire
/// shape, which carries an `opcode` tag used to identify the correct variant.
/// Due to limitations in [`bincode`] and [`serde`]'s `#[serde(untagged)]`
/// enums, binary deserialization is routed through [`decode_op`] instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Op {
    ChannelInscribe(InscriptionOp),
    ChannelConfig(ChannelConfigOp),
    ChannelDeposit(DepositOp),
    ChannelWithdraw(ChannelWithdrawOp),
    ChannelTransfer(ChannelTransferOp),
    SDPDeclare(SDPDeclareOp),
    SDPWithdraw(SDPWithdrawOp),
    SDPActive(SDPActiveOp),
    LeaderClaim(LeaderClaimOp),
    Transfer(TransferOp),
    ClaimPowReward(ClaimPowRewardOp),
}

/// Delegates serialization through the [`OpInternal`] representation.
impl Serialize for Op {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            let op_ser = OpSer::from(self);
            op_ser.serialize(serializer)
        } else {
            let bytes = self.encode();
            serializer.serialize_bytes(&bytes)
        }
    }
}

/// Delegates deserialization through the [`OpDe`] representation.
///
/// If the deserializer is non-human-readable it falls back into custom
/// decoding via [`decode_op`]. Otherwise, it deserializes via [`OpDe`]'s
/// default behaviour.
impl<'de> Deserialize<'de> for Op {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            OpDe::deserialize(deserializer).map(Self::from)
        } else {
            let bytes = <Vec<u8>>::deserialize(deserializer)?;
            Self::decode(&bytes, &())
                .map(|(_, op)| op)
                .map_err(serde::de::Error::custom)
        }
    }
}

// Op = Opcode OpPayload
impl BinaryEncode for Op {
    fn encoded_length(&self) -> usize {
        let payload = match self {
            Self::ChannelInscribe(op) => op.encoded_length(),
            Self::ChannelConfig(op) => op.encoded_length(),
            Self::ChannelDeposit(op) => op.encoded_length(),
            Self::ChannelWithdraw(op) => op.encoded_length(),
            Self::ChannelTransfer(op) => op.encoded_length(),
            Self::SDPDeclare(op) => op.encoded_length(),
            Self::SDPWithdraw(op) => op.encoded_length(),
            Self::SDPActive(op) => op.encoded_length(),
            Self::LeaderClaim(op) => op.encoded_length(),
            Self::Transfer(op) => op.encoded_length(),
            Self::ClaimPowReward(op) => op.encoded_length(),
        };
        self.code().encoded_length().checked_add(payload).unwrap()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.code().encode_into(out);
        match self {
            Self::ChannelInscribe(op) => op.encode_into(out),
            Self::ChannelConfig(op) => op.encode_into(out),
            Self::ChannelDeposit(op) => op.encode_into(out),
            Self::ChannelWithdraw(op) => op.encode_into(out),
            Self::ChannelTransfer(op) => op.encode_into(out),
            Self::SDPDeclare(op) => op.encode_into(out),
            Self::SDPWithdraw(op) => op.encode_into(out),
            Self::SDPActive(op) => op.encode_into(out),
            Self::LeaderClaim(op) => op.encode_into(out),
            Self::Transfer(op) => op.encode_into(out),
            Self::ClaimPowReward(op) => op.encode_into(out),
        }
    }
}

impl BinaryDecode for Op {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, opcode) = u8::decode(input, &())?;

        match opcode {
            InscriptionOp::CODE => InscriptionOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ChannelInscribe(op))),
            ChannelConfigOp::CODE => ChannelConfigOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ChannelConfig(op))),
            DepositOp::CODE => {
                DepositOp::decode(input, &()).map(|(rest, op)| (rest, Self::ChannelDeposit(op)))
            }
            ChannelWithdrawOp::CODE => ChannelWithdrawOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ChannelWithdraw(op))),
            ChannelTransferOp::CODE => ChannelTransferOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ChannelTransfer(op))),
            SDPDeclareOp::CODE => {
                SDPDeclareOp::decode(input, &()).map(|(rest, op)| (rest, Self::SDPDeclare(op)))
            }
            SDPWithdrawOp::CODE => {
                SDPWithdrawOp::decode(input, &()).map(|(rest, op)| (rest, Self::SDPWithdraw(op)))
            }
            SDPActiveOp::CODE => {
                SDPActiveOp::decode(input, &()).map(|(rest, op)| (rest, Self::SDPActive(op)))
            }
            LeaderClaimOp::CODE => {
                LeaderClaimOp::decode(input, &()).map(|(rest, op)| (rest, Self::LeaderClaim(op)))
            }
            TransferOp::CODE => {
                TransferOp::decode(input, &()).map(|(rest, op)| (rest, Self::Transfer(op)))
            }
            ClaimPowRewardOp::CODE => ClaimPowRewardOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ClaimPowReward(op))),
            other => Err(DecodeError::unknown_discriminant::<Self>(u64::from(other))),
        }
    }
}

const fn gas_cost_of<Op, Profile>(_op: &Op) -> Gas
where
    Op: OperationGas<Profile>,
    Profile: GasProfile,
{
    Op::GAS_COST
}

// We just check that the enum discriminant tag is encoded correctly, so a
// single fixture is fine here.
// TODO: Remove once the `BinaryCodec` macro supports enums.

impl Op {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ChannelInscribe(_) => "ChannelInscribe",
            Self::ChannelConfig(_) => "ChannelConfig",
            Self::ChannelDeposit(_) => "ChannelDeposit",
            Self::ChannelWithdraw(_) => "ChannelWithdraw",
            Self::ChannelTransfer(_) => "ChannelTransfer",
            Self::SDPDeclare(_) => "SDPDeclare",
            Self::SDPWithdraw(_) => "SDPWithdraw",
            Self::SDPActive(_) => "SDPActive",
            Self::LeaderClaim(_) => "LeaderClaim",
            Self::Transfer(_) => "Transfer",
            Self::ClaimPowReward(_) => "ClaimPowReward",
        }
    }

    #[must_use]
    pub const fn gas_cost<Profile: GasProfile>(&self) -> Gas {
        match self {
            Self::ChannelInscribe(op) => gas_cost_of(op),
            Self::ChannelConfig(op) => gas_cost_of(op),
            Self::ChannelDeposit(op) => gas_cost_of(op),
            Self::ChannelWithdraw(op) => gas_cost_of(op),
            Self::ChannelTransfer(op) => gas_cost_of(op),
            Self::SDPDeclare(op) => gas_cost_of(op),
            Self::SDPWithdraw(op) => gas_cost_of(op),
            Self::SDPActive(op) => gas_cost_of(op),
            Self::LeaderClaim(op) => gas_cost_of(op),
            Self::Transfer(op) => gas_cost_of(op),
            Self::ClaimPowReward(op) => gas_cost_of(op),
        }
    }

    const fn code(&self) -> u8 {
        match self {
            Self::ChannelInscribe(_) => InscriptionOp::CODE,
            Self::ChannelConfig(_) => ChannelConfigOp::CODE,
            Self::ChannelDeposit(_) => DepositOp::CODE,
            Self::ChannelWithdraw(_) => ChannelWithdrawOp::CODE,
            Self::ChannelTransfer(_) => ChannelTransferOp::CODE,
            Self::SDPDeclare(_) => SDPDeclareOp::CODE,
            Self::SDPWithdraw(_) => SDPWithdrawOp::CODE,
            Self::SDPActive(_) => SDPActiveOp::CODE,
            Self::LeaderClaim(_) => LeaderClaimOp::CODE,
            Self::Transfer(_) => TransferOp::CODE,
            Self::ClaimPowReward(_) => ClaimPowRewardOp::CODE,
        }
    }
}
