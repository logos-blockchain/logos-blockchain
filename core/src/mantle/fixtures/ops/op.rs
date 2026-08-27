use lb_codec::codec_fixtures;

use crate::mantle::{
    Op,
    fixtures::ops::op_values::{
        CHANNEL_CONFIG, CHANNEL_CONFIG_HEX, CHANNEL_TRANSFER, CHANNEL_TRANSFER_HEX,
        CHANNEL_WITHDRAW, CHANNEL_WITHDRAW_HEX, CLAIM_POW_REWARD, CLAIM_POW_REWARD_HEX, DEPOSIT,
        DEPOSIT_HEX, INSCRIPTION, INSCRIPTION_HEX, LEADER_CLAIM, LEADER_CLAIM_HEX, SDP_ACTIVE,
        SDP_ACTIVE_HEX, SDP_DECLARE, SDP_DECLARE_HEX, SDP_WITHDRAW, SDP_WITHDRAW_HEX, TRANSFER,
        TRANSFER_HEX,
    },
};

codec_fixtures!(
    Op,
    Self::Transfer(TRANSFER.clone()) => TRANSFER_HEX,
    Self::ChannelConfig(CHANNEL_CONFIG.clone()) => CHANNEL_CONFIG_HEX,
    Self::ChannelInscribe(INSCRIPTION.clone()) => INSCRIPTION_HEX,
    Self::ChannelDeposit(DEPOSIT.clone()) => DEPOSIT_HEX,
    Self::ChannelWithdraw(CHANNEL_WITHDRAW.clone()) => CHANNEL_WITHDRAW_HEX,
    Self::ChannelTransfer(CHANNEL_TRANSFER.clone()) => CHANNEL_TRANSFER_HEX,
    Self::SDPDeclare(SDP_DECLARE.clone()) => SDP_DECLARE_HEX,
    Self::SDPWithdraw(*SDP_WITHDRAW) => SDP_WITHDRAW_HEX,
    Self::SDPActive(SDP_ACTIVE.clone()) => SDP_ACTIVE_HEX,
    Self::LeaderClaim(LEADER_CLAIM.clone()) => LEADER_CLAIM_HEX,
    Self::ClaimPowReward(CLAIM_POW_REWARD.clone()) => CLAIM_POW_REWARD_HEX
);
