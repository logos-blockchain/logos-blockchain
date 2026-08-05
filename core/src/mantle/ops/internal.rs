use serde::{Deserialize, Serialize};

use super::{
    Op,
    channel::{config::ChannelConfigOp, deposit::DepositOp, inscribe::InscriptionOp},
    leader_claim::LeaderClaimOp,
    op_codes,
    sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
    serde_::OpWire,
    transfer::TransferOp,
};
use crate::mantle::ops::{
    channel::{channel_transfer::ChannelTransferOp, withdraw::ChannelWithdrawOp},
    pow::ClaimPowRewardOp,
};

/// Core set of supported Mantle operations and their serialization behaviour.
#[derive(Serialize)]
#[serde(untagged)]
pub enum OpSer<'a> {
    ChannelInscribe(OpWire<{ op_codes::INSCRIBE }, &'a InscriptionOp>),
    ChannelConfig(OpWire<{ op_codes::CHANNEL_CONFIG }, &'a ChannelConfigOp>),
    ChannelDeposit(OpWire<{ op_codes::CHANNEL_DEPOSIT }, &'a DepositOp>),
    ChannelWithdraw(OpWire<{ op_codes::CHANNEL_WITHDRAW }, &'a ChannelWithdrawOp>),
    ChannelTransfer(OpWire<{ op_codes::CHANNEL_TRANSFER }, &'a ChannelTransferOp>),
    SDPDeclare(OpWire<{ op_codes::SDP_DECLARE }, &'a SDPDeclareOp>),
    SDPWithdraw(OpWire<{ op_codes::SDP_WITHDRAW }, &'a SDPWithdrawOp>),
    SDPActive(OpWire<{ op_codes::SDP_ACTIVE }, &'a SDPActiveOp>),
    LeaderClaim(OpWire<{ op_codes::LEADER_CLAIM }, &'a LeaderClaimOp>),
    Transfer(OpWire<{ op_codes::TRANSFER }, &'a TransferOp>),
    ClaimPowReward(OpWire<{ op_codes::CLAIM_POW_REWARD }, &'a ClaimPowRewardOp>),
}

impl<'a> From<&'a Op> for OpSer<'a> {
    fn from(value: &'a Op) -> Self {
        match value {
            Op::ChannelInscribe(op) => Self::ChannelInscribe(OpWire::new(op)),
            Op::ChannelConfig(op) => Self::ChannelConfig(OpWire::new(op)),
            Op::ChannelDeposit(op) => Self::ChannelDeposit(OpWire::new(op)),
            Op::ChannelWithdraw(op) => Self::ChannelWithdraw(OpWire::new(op)),
            Op::ChannelTransfer(op) => Self::ChannelTransfer(OpWire::new(op)),
            Op::SDPDeclare(op) => Self::SDPDeclare(OpWire::new(op)),
            Op::SDPWithdraw(op) => Self::SDPWithdraw(OpWire::new(op)),
            Op::SDPActive(op) => Self::SDPActive(OpWire::new(op)),
            Op::LeaderClaim(op) => Self::LeaderClaim(OpWire::new(op)),
            Op::Transfer(op) => Self::Transfer(OpWire::new(op)),
            Op::ClaimPowReward(op) => Self::ClaimPowReward(OpWire::new(op)),
        }
    }
}

/// Core set of supported Mantle operations and their deserialization behaviour.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum OpDe {
    ChannelInscribe(OpWire<{ op_codes::INSCRIBE }, InscriptionOp>),
    ChannelConfig(OpWire<{ op_codes::CHANNEL_CONFIG }, ChannelConfigOp>),
    ChannelDeposit(OpWire<{ op_codes::CHANNEL_DEPOSIT }, DepositOp>),
    ChannelWithdraw(OpWire<{ op_codes::CHANNEL_WITHDRAW }, ChannelWithdrawOp>),
    ChannelTransfer(OpWire<{ op_codes::CHANNEL_TRANSFER }, ChannelTransferOp>),
    SDPDeclare(OpWire<{ op_codes::SDP_DECLARE }, SDPDeclareOp>),
    SDPWithdraw(OpWire<{ op_codes::SDP_WITHDRAW }, SDPWithdrawOp>),
    SDPActive(OpWire<{ op_codes::SDP_ACTIVE }, SDPActiveOp>),
    LeaderClaim(OpWire<{ op_codes::LEADER_CLAIM }, LeaderClaimOp>),
    Transfer(OpWire<{ op_codes::TRANSFER }, TransferOp>),
    ClaimPoWReward(OpWire<{ op_codes::CLAIM_POW_REWARD }, ClaimPowRewardOp>),
}

impl From<OpDe> for Op {
    fn from(value: OpDe) -> Self {
        match value {
            OpDe::ChannelInscribe(w) => Self::ChannelInscribe(w.into_op()),
            OpDe::ChannelConfig(w) => Self::ChannelConfig(w.into_op()),
            OpDe::ChannelDeposit(w) => Self::ChannelDeposit(w.into_op()),
            OpDe::ChannelWithdraw(w) => Self::ChannelWithdraw(w.into_op()),
            OpDe::ChannelTransfer(w) => Self::ChannelTransfer(w.into_op()),
            OpDe::SDPDeclare(w) => Self::SDPDeclare(w.into_op()),
            OpDe::SDPWithdraw(w) => Self::SDPWithdraw(w.into_op()),
            OpDe::SDPActive(w) => Self::SDPActive(w.into_op()),
            OpDe::LeaderClaim(w) => Self::LeaderClaim(w.into_op()),
            OpDe::Transfer(w) => Self::Transfer(w.into_op()),
            OpDe::ClaimPoWReward(w) => Self::ClaimPowReward(w.into_op()),
        }
    }
}
