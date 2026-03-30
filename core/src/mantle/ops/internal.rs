use serde::{Deserialize, Serialize};

use super::{
    Op,
    channel::{deposit::DepositOp, inscribe::InscriptionOp, set_keys::SetKeysOp},
    leader_claim::LeaderClaimOp,
    opcode::{
        CHANNEL_DEPOSIT, CHANNEL_WITHDRAW, INSCRIBE, LEADER_CLAIM, SDP_ACTIVE, SDP_DECLARE,
        SDP_WITHDRAW, SET_CHANNEL_KEYS, TRANSFER,
    },
    sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
    serde_,
    transfer::TransferOp,
};
use crate::mantle::ops::channel::withdraw::ChannelWithdrawOp;

/// Core set of supported Mantle operations and their serialization behaviour.
#[derive(Serialize)]
#[serde(untagged)]
pub enum OpSer<'a> {
    ChannelInscribe(
        #[serde(serialize_with = "serde_::serialize_op_variant::<{INSCRIBE}, InscriptionOp, _>")]
        &'a InscriptionOp,
    ),
    ChannelSetKeys(
        #[serde(
            serialize_with = "serde_::serialize_op_variant::<{SET_CHANNEL_KEYS}, SetKeysOp, _>"
        )]
        &'a SetKeysOp,
    ),
    ChannelDeposit(
        #[serde(
            serialize_with = "serde_::serialize_op_variant::<{CHANNEL_DEPOSIT}, DepositOp, _>"
        )]
        &'a DepositOp,
    ),
    ChannelWithdraw(
        #[serde(
            serialize_with = "serde_::serialize_op_variant::<{CHANNEL_WITHDRAW}, ChannelWithdrawOp, _>"
        )]
        &'a ChannelWithdrawOp,
    ),
    SDPDeclare(
        #[serde(serialize_with = "serde_::serialize_op_variant::<{SDP_DECLARE}, SDPDeclareOp, _>")]
        &'a SDPDeclareOp,
    ),
    SDPWithdraw(
        #[serde(
            serialize_with = "serde_::serialize_op_variant::<{SDP_WITHDRAW}, SDPWithdrawOp, _>"
        )]
        &'a SDPWithdrawOp,
    ),
    SDPActive(
        #[serde(serialize_with = "serde_::serialize_op_variant::<{SDP_ACTIVE}, SDPActiveOp, _>")]
        &'a SDPActiveOp,
    ),
    LeaderClaim(
        #[serde(
            serialize_with = "serde_::serialize_op_variant::<{LEADER_CLAIM}, LeaderClaimOp, _>"
        )]
        &'a LeaderClaimOp,
    ),
    Transfer(
        #[serde(serialize_with = "serde_::serialize_op_variant::<{TRANSFER}, TransferOp, _>")]
        &'a TransferOp,
    ),
}

impl<'a> From<&'a Op> for OpSer<'a> {
    fn from(value: &'a Op) -> Self {
        match value {
            Op::ChannelInscribe(op) => OpSer::ChannelInscribe(op),
            Op::ChannelSetKeys(op) => OpSer::ChannelSetKeys(op),
            Op::ChannelDeposit(op) => OpSer::ChannelDeposit(op),
            Op::ChannelWithdraw(op) => OpSer::ChannelWithdraw(op),
            Op::SDPDeclare(op) => OpSer::SDPDeclare(op),
            Op::SDPWithdraw(op) => OpSer::SDPWithdraw(op),
            Op::SDPActive(op) => OpSer::SDPActive(op),
            Op::LeaderClaim(op) => OpSer::LeaderClaim(op),
            Op::Transfer(op) => OpSer::Transfer(op),
        }
    }
}

/// Core set of supported Mantle operations and their deserialization behaviour.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum OpDe {
    ChannelInscribe(
        #[serde(
            deserialize_with = "serde_::deserialize_op_variant::<{INSCRIBE}, InscriptionOp, _>"
        )]
        InscriptionOp,
    ),
    ChannelSetKeys(
        #[serde(
            deserialize_with = "serde_::deserialize_op_variant::<{SET_CHANNEL_KEYS}, SetKeysOp, _>"
        )]
        SetKeysOp,
    ),
    ChannelDeposit(
        #[serde(
            deserialize_with = "serde_::deserialize_op_variant::<{CHANNEL_DEPOSIT}, DepositOp, _>"
        )]
        DepositOp,
    ),
    ChannelWithdraw(
        #[serde(
            deserialize_with = "serde_::deserialize_op_variant::<{CHANNEL_WITHDRAW}, ChannelWithdrawOp, _>"
        )]
        ChannelWithdrawOp,
    ),
    SDPDeclare(
        #[serde(
            deserialize_with = "serde_::deserialize_op_variant::<{SDP_DECLARE}, SDPDeclareOp, _>"
        )]
        SDPDeclareOp,
    ),
    SDPWithdraw(
        #[serde(
            deserialize_with = "serde_::deserialize_op_variant::<{SDP_WITHDRAW}, SDPWithdrawOp, _>"
        )]
        SDPWithdrawOp,
    ),
    SDPActive(
        #[serde(
            deserialize_with = "serde_::deserialize_op_variant::<{SDP_ACTIVE}, SDPActiveOp, _>"
        )]
        SDPActiveOp,
    ),
    LeaderClaim(
        #[serde(
            deserialize_with = "serde_::deserialize_op_variant::<{LEADER_CLAIM}, LeaderClaimOp, _>"
        )]
        LeaderClaimOp,
    ),
    Transfer(
        #[serde(deserialize_with = "serde_::deserialize_op_variant::<{TRANSFER}, TransferOp, _>")]
        TransferOp,
    ),
}

impl From<OpDe> for Op {
    fn from(value: OpDe) -> Self {
        match value {
            OpDe::ChannelInscribe(inscribe) => Self::ChannelInscribe(inscribe),
            OpDe::ChannelSetKeys(channel_set_keys) => Self::ChannelSetKeys(channel_set_keys),
            OpDe::ChannelDeposit(channel_deposit) => Self::ChannelDeposit(channel_deposit),
            OpDe::ChannelWithdraw(channel_withdraw) => Self::ChannelWithdraw(channel_withdraw),
            OpDe::SDPDeclare(sdp_declare) => Self::SDPDeclare(sdp_declare),
            OpDe::SDPWithdraw(sdp_withdraw) => Self::SDPWithdraw(sdp_withdraw),
            OpDe::SDPActive(sdp_active) => Self::SDPActive(sdp_active),
            OpDe::LeaderClaim(leader_claim) => Self::LeaderClaim(leader_claim),
            OpDe::Transfer(transfer) => Self::Transfer(transfer),
        }
    }
}
