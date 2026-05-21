use serde::{Deserialize, Serialize};

use super::{
    CHANNEL_CONFIG, CHANNEL_DEPOSIT, CHANNEL_WITHDRAW, INSCRIBE, LEADER_CLAIM, Op, SDP_ACTIVE,
    SDP_DECLARE, SDP_WITHDRAW, TRANSFER,
    channel::{config::ChannelConfigOp, deposit::DepositOp, inscribe::InscriptionOp},
    leader_claim::LeaderClaimOp,
    sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
    serde_::OpWire,
    transfer::TransferOp,
};
use crate::mantle::ops::channel::withdraw::ChannelWithdrawOp;

/// Core set of supported Mantle operations and their serialization behaviour.
#[derive(Serialize)]
#[serde(untagged)]
pub enum OpSer<'a> {
    ChannelInscribe(OpWire<INSCRIBE, &'a InscriptionOp>),
    ChannelConfig(OpWire<CHANNEL_CONFIG, &'a ChannelConfigOp>),
    ChannelDeposit(OpWire<CHANNEL_DEPOSIT, &'a DepositOp>),
    ChannelWithdraw(OpWire<CHANNEL_WITHDRAW, &'a ChannelWithdrawOp>),
    SDPDeclare(OpWire<SDP_DECLARE, &'a SDPDeclareOp>),
    SDPWithdraw(OpWire<SDP_WITHDRAW, &'a SDPWithdrawOp>),
    SDPActive(OpWire<SDP_ACTIVE, &'a SDPActiveOp>),
    LeaderClaim(OpWire<LEADER_CLAIM, &'a LeaderClaimOp>),
    Transfer(OpWire<TRANSFER, &'a TransferOp>),
}

impl<'a> From<&'a Op> for OpSer<'a> {
    fn from(value: &'a Op) -> Self {
        match value {
            Op::ChannelInscribe(op) => Self::ChannelInscribe(OpWire::new(op)),
            Op::ChannelConfig(op) => Self::ChannelConfig(OpWire::new(op)),
            Op::ChannelDeposit(op) => Self::ChannelDeposit(OpWire::new(op)),
            Op::ChannelWithdraw(op) => Self::ChannelWithdraw(OpWire::new(op)),
            Op::SDPDeclare(op) => Self::SDPDeclare(OpWire::new(op)),
            Op::SDPWithdraw(op) => Self::SDPWithdraw(OpWire::new(op)),
            Op::SDPActive(op) => Self::SDPActive(OpWire::new(op)),
            Op::LeaderClaim(op) => Self::LeaderClaim(OpWire::new(op)),
            Op::Transfer(op) => Self::Transfer(OpWire::new(op)),
        }
    }
}

/// Core set of supported Mantle operations and their deserialization behaviour.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum OpDe {
    ChannelInscribe(OpWire<INSCRIBE, InscriptionOp>),
    ChannelConfig(OpWire<CHANNEL_CONFIG, ChannelConfigOp>),
    ChannelDeposit(OpWire<CHANNEL_DEPOSIT, DepositOp>),
    ChannelWithdraw(OpWire<CHANNEL_WITHDRAW, ChannelWithdrawOp>),
    SDPDeclare(OpWire<SDP_DECLARE, SDPDeclareOp>),
    SDPWithdraw(OpWire<SDP_WITHDRAW, SDPWithdrawOp>),
    SDPActive(OpWire<SDP_ACTIVE, SDPActiveOp>),
    LeaderClaim(OpWire<LEADER_CLAIM, LeaderClaimOp>),
    Transfer(OpWire<TRANSFER, TransferOp>),
}

impl From<OpDe> for Op {
    fn from(value: OpDe) -> Self {
        match value {
            OpDe::ChannelInscribe(w) => Self::ChannelInscribe(w.into_op()),
            OpDe::ChannelConfig(w) => Self::ChannelConfig(w.into_op()),
            OpDe::ChannelDeposit(w) => Self::ChannelDeposit(w.into_op()),
            OpDe::ChannelWithdraw(w) => Self::ChannelWithdraw(w.into_op()),
            OpDe::SDPDeclare(w) => Self::SDPDeclare(w.into_op()),
            OpDe::SDPWithdraw(w) => Self::SDPWithdraw(w.into_op()),
            OpDe::SDPActive(w) => Self::SDPActive(w.into_op()),
            OpDe::LeaderClaim(w) => Self::LeaderClaim(w.into_op()),
            OpDe::Transfer(w) => Self::Transfer(w.into_op()),
        }
    }
}
