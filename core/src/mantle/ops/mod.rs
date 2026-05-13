pub mod channel;
pub(crate) mod internal;
pub mod leader_claim;
pub mod opcode;
pub mod sdp;
mod serde_;
pub mod transfer;

use std::sync::LazyLock;

use channel::{
    config::ChannelConfigOp, deposit::DepositOp, inscribe::InscriptionOp,
    withdraw::ChannelWithdrawOp,
};
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{
    gas::{Gas, GasConstants},
    ops::{
        leader_claim::LeaderClaimOp,
        opcode::{
            CHANNEL_CONFIG, INSCRIBE, LEADER_CLAIM, SDP_ACTIVE, SDP_DECLARE, SDP_WITHDRAW, TRANSFER,
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
        channel_multi_sig_proof::ChannelMultiSigProof, leader_claim_proof::Groth16LeaderClaimProof,
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
/// Serialization and deserialization share a single [`serde_::OpWire`] wire
/// shape, which carries an `opcode` tag used to identify the correct variant.
/// Due to limitations in [`bincode`] and [`serde`]'s `#[serde(untagged)]`
/// enums, binary deserialization is routed through [`decode_op`] instead.
#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Op {
    ChannelInscribe(InscriptionOp) = INSCRIBE,
    ChannelConfig(ChannelConfigOp) = CHANNEL_CONFIG,
    ChannelDeposit(DepositOp) = CHANNEL_DEPOSIT,
    ChannelWithdraw(ChannelWithdrawOp) = CHANNEL_WITHDRAW,
    SDPDeclare(SDPDeclareOp) = SDP_DECLARE,
    SDPWithdraw(SDPWithdrawOp) = SDP_WITHDRAW,
    SDPActive(SDPActiveOp) = SDP_ACTIVE,
    LeaderClaim(LeaderClaimOp) = LEADER_CLAIM,
    Transfer(TransferOp) = TRANSFER,
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

/// Delegates deserialization through the [`OpDe`] representation.
///
/// If the deserializer is non-human-readable it falls back into custom
/// decoding via [`decode_op`]. Otherwise, it deserializes via [`OpDe`]'s
/// default behaviour.
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
            Self::ChannelConfig(_) => "ChannelConfig",
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
    pub const fn execution_gas<Constants: GasConstants>(&self) -> Gas {
        match self {
            Self::ChannelInscribe(_) => Constants::CHANNEL_INSCRIBE,
            Self::ChannelConfig(_) => Constants::CHANNEL_CONFIG,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpProof {
    Ed25519Sig(Ed25519Signature),
    ZkSig(ZkSignature),
    ZkAndEd25519Sigs {
        zk_sig: ZkSignature,
        ed25519_sig: Ed25519Signature,
    },
    PoC(Groth16LeaderClaimProof),
    ChannelMultiSigProof(ChannelMultiSigProof),
}
