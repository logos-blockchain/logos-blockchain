use serde::{Deserialize, Serialize};

use super::{
    Op, OpRef,
    channel::{config::ChannelConfigOp, deposit::DepositOp, inscribe::InscriptionOp},
    leader_claim::LeaderClaimOp,
    sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
    serde_::OpWire,
    transfer::TransferOp,
};
use crate::mantle::{
    ledger::ProvableOperation as _,
    ops::{
        channel::{channel_transfer::ChannelTransferOp, withdraw::ChannelWithdrawOp},
        pow::ClaimPowRewardOp,
    },
};

/// Core set of supported Mantle operations and their serialization behaviour.
#[derive(Serialize)]
#[serde(untagged)]
pub enum OpSer<'a> {
    Transfer(OpWire<{ TransferOp::CODE }, &'a TransferOp>),
    ChannelInscribe(OpWire<{ InscriptionOp::CODE }, &'a InscriptionOp>),
    ChannelConfig(OpWire<{ ChannelConfigOp::CODE }, &'a ChannelConfigOp>),
    ChannelDeposit(OpWire<{ DepositOp::CODE }, &'a DepositOp>),
    ChannelWithdraw(OpWire<{ ChannelWithdrawOp::CODE }, &'a ChannelWithdrawOp>),
    ChannelTransfer(OpWire<{ ChannelTransferOp::CODE }, &'a ChannelTransferOp>),
    SDPDeclare(OpWire<{ SDPDeclareOp::CODE }, &'a SDPDeclareOp>),
    SDPWithdraw(OpWire<{ SDPWithdrawOp::CODE }, &'a SDPWithdrawOp>),
    SDPActive(OpWire<{ SDPActiveOp::CODE }, &'a SDPActiveOp>),
    LeaderClaim(OpWire<{ LeaderClaimOp::CODE }, &'a LeaderClaimOp>),
    ClaimPowReward(OpWire<{ ClaimPowRewardOp::CODE }, &'a ClaimPowRewardOp>),
}

impl<'a> From<&'a Op> for OpSer<'a> {
    fn from(value: &'a Op) -> Self {
        match value {
            Op::Transfer(op) => Self::Transfer(OpWire::new(op)),
            Op::ChannelInscribe(op) => Self::ChannelInscribe(OpWire::new(op)),
            Op::ChannelConfig(op) => Self::ChannelConfig(OpWire::new(op)),
            Op::ChannelDeposit(op) => Self::ChannelDeposit(OpWire::new(op)),
            Op::ChannelWithdraw(op) => Self::ChannelWithdraw(OpWire::new(op)),
            Op::ChannelTransfer(op) => Self::ChannelTransfer(OpWire::new(op)),
            Op::SDPDeclare(op) => Self::SDPDeclare(OpWire::new(op)),
            Op::SDPWithdraw(op) => Self::SDPWithdraw(OpWire::new(op)),
            Op::SDPActive(op) => Self::SDPActive(OpWire::new(op)),
            Op::LeaderClaim(op) => Self::LeaderClaim(OpWire::new(op)),
            Op::ClaimPowReward(op) => Self::ClaimPowReward(OpWire::new(op)),
        }
    }
}

impl<'a> From<OpRef<'a>> for OpSer<'a> {
    fn from(value: OpRef<'a>) -> Self {
        match value {
            OpRef::ChannelInscribe(op) => Self::ChannelInscribe(OpWire::new(op)),
            OpRef::ChannelConfig(op) => Self::ChannelConfig(OpWire::new(op)),
            OpRef::ChannelDeposit(op) => Self::ChannelDeposit(OpWire::new(op)),
            OpRef::ChannelWithdraw(op) => Self::ChannelWithdraw(OpWire::new(op)),
            OpRef::ChannelTransfer(op) => Self::ChannelTransfer(OpWire::new(op)),
            OpRef::SDPDeclare(op) => Self::SDPDeclare(OpWire::new(op)),
            OpRef::SDPWithdraw(op) => Self::SDPWithdraw(OpWire::new(op)),
            OpRef::SDPActive(op) => Self::SDPActive(OpWire::new(op)),
            OpRef::LeaderClaim(op) => Self::LeaderClaim(OpWire::new(op)),
            OpRef::Transfer(op) => Self::Transfer(OpWire::new(op)),
            OpRef::ClaimPowReward(op) => Self::ClaimPowReward(OpWire::new(op)),
        }
    }
}

/// Core set of supported Mantle operations and their deserialization behaviour.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum OpDe {
    Transfer(OpWire<{ TransferOp::CODE }, TransferOp>),
    ChannelConfig(OpWire<{ ChannelConfigOp::CODE }, ChannelConfigOp>),
    ChannelInscribe(OpWire<{ InscriptionOp::CODE }, InscriptionOp>),
    ChannelDeposit(OpWire<{ DepositOp::CODE }, DepositOp>),
    ChannelWithdraw(OpWire<{ ChannelWithdrawOp::CODE }, ChannelWithdrawOp>),
    ChannelTransfer(OpWire<{ ChannelTransferOp::CODE }, ChannelTransferOp>),
    SDPDeclare(OpWire<{ SDPDeclareOp::CODE }, SDPDeclareOp>),
    SDPWithdraw(OpWire<{ SDPWithdrawOp::CODE }, SDPWithdrawOp>),
    SDPActive(OpWire<{ SDPActiveOp::CODE }, SDPActiveOp>),
    LeaderClaim(OpWire<{ LeaderClaimOp::CODE }, LeaderClaimOp>),
    ClaimPowReward(OpWire<{ ClaimPowRewardOp::CODE }, ClaimPowRewardOp>),
}

impl From<OpDe> for Op {
    fn from(value: OpDe) -> Self {
        match value {
            OpDe::Transfer(w) => Self::Transfer(w.into_op()),
            OpDe::ChannelConfig(w) => Self::ChannelConfig(w.into_op()),
            OpDe::ChannelInscribe(w) => Self::ChannelInscribe(w.into_op()),
            OpDe::ChannelDeposit(w) => Self::ChannelDeposit(w.into_op()),
            OpDe::ChannelWithdraw(w) => Self::ChannelWithdraw(w.into_op()),
            OpDe::ChannelTransfer(w) => Self::ChannelTransfer(w.into_op()),
            OpDe::SDPDeclare(w) => Self::SDPDeclare(w.into_op()),
            OpDe::SDPWithdraw(w) => Self::SDPWithdraw(w.into_op()),
            OpDe::SDPActive(w) => Self::SDPActive(w.into_op()),
            OpDe::LeaderClaim(w) => Self::LeaderClaim(w.into_op()),
            OpDe::ClaimPowReward(w) => Self::ClaimPowReward(w.into_op()),
        }
    }
}
