use serde::{Deserialize, Serialize};

use super::{
    Op,
    channel::{blob::BlobOp, inscribe::InscriptionOp, set_keys::SetKeysOp},
    leader_claim::LeaderClaimOp,
    opcode::{
        BLOB, INSCRIBE, LEADER_CLAIM, SDP_ACTIVE, SDP_DECLARE, SDP_WITHDRAW, SET_CHANNEL_KEYS,
    },
    sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
    serde_,
};

/// Core set of supported Mantle operations and their serialization behaviour.
#[derive(Serialize)]
#[serde(untagged)]
pub enum OpSer<'a> {
    ChannelInscribe(
        #[serde(serialize_with = "serde_::serialize_op_variant::<{INSCRIBE}, InscriptionOp, _>")]
        &'a InscriptionOp,
    ),
    ChannelBlob(
        #[serde(serialize_with = "serde_::serialize_op_variant::<{BLOB}, BlobOp, _>")] &'a BlobOp,
    ),
    ChannelSetKeys(
        #[serde(
            serialize_with = "serde_::serialize_op_variant::<{SET_CHANNEL_KEYS}, SetKeysOp, _>"
        )]
        &'a SetKeysOp,
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
}

impl<'a> From<&'a Op> for OpSer<'a> {
    fn from(value: &'a Op) -> Self {
        match value {
            Op::ChannelInscribe(op) => OpSer::ChannelInscribe(op),
            Op::ChannelBlob(op) => OpSer::ChannelBlob(op),
            Op::ChannelSetKeys(op) => OpSer::ChannelSetKeys(op),
            Op::SDPDeclare(op) => OpSer::SDPDeclare(op),
            Op::SDPWithdraw(op) => OpSer::SDPWithdraw(op),
            Op::SDPActive(op) => OpSer::SDPActive(op),
            Op::LeaderClaim(op) => OpSer::LeaderClaim(op),
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
    ChannelBlob(
        #[serde(deserialize_with = "serde_::deserialize_op_variant::<{BLOB}, BlobOp, _>")] BlobOp,
    ),
    ChannelSetKeys(
        #[serde(
            deserialize_with = "serde_::deserialize_op_variant::<{SET_CHANNEL_KEYS}, SetKeysOp, _>"
        )]
        SetKeysOp,
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
}

impl From<OpDe> for Op {
    fn from(value: OpDe) -> Self {
        match value {
            OpDe::ChannelInscribe(inscribe) => Self::ChannelInscribe(inscribe),
            OpDe::ChannelBlob(blob) => Self::ChannelBlob(blob),
            OpDe::ChannelSetKeys(channel_set_keys) => Self::ChannelSetKeys(channel_set_keys),
            OpDe::SDPDeclare(sdp_declare) => Self::SDPDeclare(sdp_declare),
            OpDe::SDPWithdraw(sdp_withdraw) => Self::SDPWithdraw(sdp_withdraw),
            OpDe::SDPActive(sdp_active) => Self::SDPActive(sdp_active),
            OpDe::LeaderClaim(leader_claim) => Self::LeaderClaim(leader_claim),
        }
    }
}
