use lb_binary_codec::canonical::codec_fixtures;

use super::FORK_DIGEST_HEX;
use crate::mantle::{
    Op,
    fixtures::ops::op_values::{
        ALL_OPS_COLUMN_HEX, CHANNEL_CONFIG, CHANNEL_TRANSFER, CHANNEL_WITHDRAW, CLAIM_POW_REWARD,
        DEPOSIT, EMPTY_COLUMN_HEX, INSCRIPTION, LEADER_CLAIM, SDP_ACTIVE, SDP_DECLARE,
        SDP_WITHDRAW, TRANSFER, TRANSFER_AND_INSCRIPTION_COLUMN_HEX, TRANSFER_COLUMN_HEX,
    },
    transactions::Ops,
};

codec_fixtures!(
    Ops,
    Self::empty() => &[FORK_DIGEST_HEX, EMPTY_COLUMN_HEX].concat(),
    Self::from([Op::Transfer(TRANSFER.clone())]) => &[FORK_DIGEST_HEX, TRANSFER_COLUMN_HEX].concat(),
    Self::from([
        Op::Transfer(TRANSFER.clone()),
        Op::ChannelInscribe(INSCRIPTION.clone()),
    ]) => &[FORK_DIGEST_HEX, TRANSFER_AND_INSCRIPTION_COLUMN_HEX].concat(),
    Self::from([
        Op::Transfer(TRANSFER.clone()),
        Op::ChannelConfig(CHANNEL_CONFIG.clone()),
        Op::ChannelInscribe(INSCRIPTION.clone()),
        Op::ChannelDeposit(DEPOSIT.clone()),
        Op::ChannelWithdraw(CHANNEL_WITHDRAW.clone()),
        Op::ChannelTransfer(CHANNEL_TRANSFER.clone()),
        Op::SDPDeclare(SDP_DECLARE.clone()),
        Op::SDPWithdraw(*SDP_WITHDRAW),
        Op::SDPActive(SDP_ACTIVE.clone()),
        Op::LeaderClaim(LEADER_CLAIM.clone()),
        Op::ClaimPowReward(CLAIM_POW_REWARD.clone()),
    ]) => &[FORK_DIGEST_HEX, ALL_OPS_COLUMN_HEX].concat()
);
