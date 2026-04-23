pub mod channel;
pub(crate) mod internal;
pub mod leader_claim;
pub mod opcode;
pub mod sdp;
mod serde_;
pub mod transfer;

use std::sync::LazyLock;

use channel::{
    deposit::DepositOp, inscribe::InscriptionOp, set_keys::SetKeysOp, withdraw::ChannelWithdrawOp,
};
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{
    gas::{Gas, GasConstants},
    ops::{
        leader_claim::LeaderClaimOp,
        opcode::{
            INSCRIBE, LEADER_CLAIM, SDP_ACTIVE, SDP_DECLARE, SDP_WITHDRAW, SET_CHANNEL_KEYS,
            TRANSFER,
        },
        sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
    },
};
use crate::{
    crypto::{Digest as _, Hash, Hasher},
    mantle::{
        encoding::{decode_op, encode_op},
        ops::{
            internal::{OpDe, OpSer},
            opcode::{CHANNEL_DEPOSIT, CHANNEL_WITHDRAW},
            transfer::TransferOp,
        },
    },
    proofs::{
        channel_withdraw_proof::ChannelWithdrawProof, leader_claim_proof::Groth16LeaderClaimProof,
    },
};

static OPERATION_ID_V1: LazyLock<Vec<u8>> = LazyLock::new(|| b"OPERATION_ID_V1".to_vec());

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
/// Serialization and deserialization are performed using [`serde_::WireOpSer`]
/// and [`serde_::WireOpDe`], which introduce a custom `opcode` tag to identify
/// the correct variant. Due to limitations in [`bincode`] and [`serde`]'s
/// `#[serde(untagged)]` enums, binary deserialization is routed through
/// [`OpWireVisitor`], which correctly handles `opcode` to select the
/// appropriate variant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Op {
    ChannelInscribe(InscriptionOp),
    ChannelSetKeys(SetKeysOp),
    ChannelDeposit(DepositOp),
    ChannelWithdraw(ChannelWithdrawOp),
    SDPDeclare(SDPDeclareOp),
    SDPWithdraw(SDPWithdrawOp),
    SDPActive(SDPActiveOp),
    LeaderClaim(LeaderClaimOp),
    Transfer(TransferOp),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpProof {
    Ed25519Sig(Ed25519Signature),
    ZkSig(ZkSignature),
    ZkAndEd25519Sigs {
        zk_sig: ZkSignature,
        ed25519_sig: Ed25519Signature,
    },
    PoC(Groth16LeaderClaimProof),
    ChannelWithdrawProof(ChannelWithdrawProof),
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
            let bytes = encode_op(self);
            serializer.serialize_bytes(&bytes)
        }
    }
}

/// Delegates deserialization through the [`OpInternal`] representation.
///
/// If the deserializer is non-human-readable it falls back into custom
/// decoding. Otherwise, it falls back to deserializing via [`OpInternal`]'s
/// default behaviour.
///
/// # Notes
/// - When using the `wire` format, the tuple must contain the exact number of
///   fields expected by [`WireOpDes`](serde_::WireOpDes), or unexpected
///   behaviour may occur.
impl<'de> Deserialize<'de> for Op {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            OpDe::deserialize(deserializer).map(Self::from)
        } else {
            let bytes = <Vec<u8>>::deserialize(deserializer)?;
            decode_op(&bytes)
                .map(|(_, op)| op)
                .map_err(serde::de::Error::custom)
        }
    }
}

impl Op {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ChannelInscribe(_) => "ChannelInscribe",
            Self::ChannelSetKeys(_) => "ChannelSetKeys",
            Self::ChannelDeposit(_) => "ChannelDeposit",
            Self::ChannelWithdraw(_) => "ChannelWithdraw",
            Self::SDPDeclare(_) => "SDPDeclare",
            Self::SDPWithdraw(_) => "SDPWithdraw",
            Self::SDPActive(_) => "SDPActive",
            Self::LeaderClaim(_) => "LeaderClaim",
            Self::Transfer(_) => "Transfer",
        }
    }
    #[must_use]
    pub const fn opcode(&self) -> u8 {
        match self {
            Self::ChannelInscribe(_) => INSCRIBE,
            Self::ChannelSetKeys(_) => SET_CHANNEL_KEYS,
            Self::ChannelDeposit(_) => CHANNEL_DEPOSIT,
            Self::ChannelWithdraw(_) => CHANNEL_WITHDRAW,
            Self::SDPDeclare(_) => SDP_DECLARE,
            Self::SDPWithdraw(_) => SDP_WITHDRAW,
            Self::SDPActive(_) => SDP_ACTIVE,
            Self::LeaderClaim(_) => LEADER_CLAIM,
            Self::Transfer(_) => TRANSFER,
        }
    }

    #[must_use]
    pub const fn execution_gas<Constants: GasConstants>(&self) -> Gas {
        match self {
            Self::ChannelInscribe(_) => Constants::CHANNEL_INSCRIBE,
            Self::ChannelSetKeys(_) => Constants::CHANNEL_SET_KEYS,
            Self::ChannelDeposit(_) => Constants::CHANNEL_DEPOSIT,
            Self::ChannelWithdraw(_) => Constants::CHANNEL_WITHDRAW,
            Self::SDPDeclare(_) => Constants::SDP_DECLARE,
            Self::SDPWithdraw(_) => Constants::SDP_WITHDRAW,
            Self::SDPActive(_) => Constants::SDP_ACTIVE,
            Self::LeaderClaim(_) => Constants::LEADER_CLAIM,
            Self::Transfer(_) => Constants::TRANSFER,
        }
    }
}
