use lb_codec::codec_fixtures;

use crate::mantle::{
    OpRef,
    fixtures::ops::op_values::{
        ALL_OPS_COLUMN_HEX, CHANNEL_CONFIG, CHANNEL_TRANSFER, CHANNEL_WITHDRAW, CLAIM_POW_REWARD,
        DEPOSIT, EMPTY_COLUMN_HEX, INSCRIPTION, LEADER_CLAIM, SDP_ACTIVE, SDP_DECLARE,
        SDP_WITHDRAW, TRANSFER, TRANSFER_AND_INSCRIPTION_COLUMN_HEX, TRANSFER_COLUMN_HEX,
    },
    transactions::OpRefs,
};

codec_fixtures!(
    OpRefs<'_>,
    encode_only,
    Self::empty() => EMPTY_COLUMN_HEX,
    Self::from([OpRef::Transfer(&TRANSFER)]) => TRANSFER_COLUMN_HEX,
    Self::from([
        OpRef::Transfer(&TRANSFER),
        OpRef::ChannelInscribe(&INSCRIPTION),
    ]) => TRANSFER_AND_INSCRIPTION_COLUMN_HEX,
    Self::from([
        OpRef::Transfer(&TRANSFER),
        OpRef::ChannelConfig(&CHANNEL_CONFIG),
        OpRef::ChannelInscribe(&INSCRIPTION),
        OpRef::ChannelDeposit(&DEPOSIT),
        OpRef::ChannelWithdraw(&CHANNEL_WITHDRAW),
        OpRef::ChannelTransfer(&CHANNEL_TRANSFER),
        OpRef::SDPDeclare(&SDP_DECLARE),
        OpRef::SDPWithdraw(&SDP_WITHDRAW),
        OpRef::SDPActive(&SDP_ACTIVE),
        OpRef::LeaderClaim(&LEADER_CLAIM),
        OpRef::ClaimPowReward(&CLAIM_POW_REWARD),
    ]) => ALL_OPS_COLUMN_HEX
);
