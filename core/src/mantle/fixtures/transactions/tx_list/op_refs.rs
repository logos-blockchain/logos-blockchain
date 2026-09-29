use lb_binary_codec::canonical::codec_fixtures;

use super::FORK_DIGEST_HEX;
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
    Self::empty() => &[FORK_DIGEST_HEX, EMPTY_COLUMN_HEX].concat(),
    Self::from([OpRef::Transfer(&TRANSFER)]) => &[FORK_DIGEST_HEX, TRANSFER_COLUMN_HEX].concat(),
    Self::from([
        OpRef::Transfer(&TRANSFER),
        OpRef::ChannelInscribe(&INSCRIPTION),
    ]) => &[FORK_DIGEST_HEX, TRANSFER_AND_INSCRIPTION_COLUMN_HEX].concat(),
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
    ]) => &[FORK_DIGEST_HEX, ALL_OPS_COLUMN_HEX].concat()
);
