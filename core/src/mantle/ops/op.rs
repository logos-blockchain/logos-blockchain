use lb_codec::{BinaryDecode, BinaryEncode, DecodeError};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(any(test, feature = "samples"))]
use crate::mantle::OpProof;
#[cfg(any(test, feature = "samples"))]
use crate::mantle::ops::op_proof::samples::sample_proof_for;
use crate::{
    crypto::{Digest as _, Hash, Hasher},
    mantle::{
        GasProfile,
        gas::Gas,
        ledger::ProvableOperation as _,
        ops::{
            OPERATION_ID_V1, OpRef,
            channel::{
                channel_transfer::ChannelTransferOp, config::ChannelConfigOp, deposit::DepositOp,
                inscribe::InscriptionOp, withdraw::ChannelWithdrawOp,
            },
            internal::{OpDe, OpSer},
            leader_claim::LeaderClaimOp,
            pow::ClaimPowRewardOp,
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
            transfer::TransferOp,
        },
    },
};

pub trait OpId {
    fn op_id(&self) -> Hash {
        let mut encoded_bytes = OPERATION_ID_V1.clone();
        encoded_bytes.extend(self.op_bytes());
        Hasher::digest(&encoded_bytes).into()
    }

    fn op_bytes(&self) -> Vec<u8>;
}

impl<T: OpId> OpId for &T {
    fn op_id(&self) -> Hash {
        (*self).op_id()
    }

    fn op_bytes(&self) -> Vec<u8> {
        (*self).op_bytes()
    }
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
pub enum Op {
    ChannelInscribe(InscriptionOp),
    ChannelConfig(ChannelConfigOp),
    ChannelDeposit(DepositOp),
    ChannelWithdraw(ChannelWithdrawOp),
    ChannelTransfer(ChannelTransferOp),
    SDPDeclare(SDPDeclareOp),
    SDPWithdraw(SDPWithdrawOp),
    SDPActive(SDPActiveOp),
    LeaderClaim(LeaderClaimOp),
    Transfer(TransferOp),
    ClaimPowReward(ClaimPowRewardOp),
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
            let bytes = self.encode();
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
            Self::decode(&bytes, &())
                .map(|(_, op)| op)
                .map_err(serde::de::Error::custom)
        }
    }
}

// Op = Opcode OpPayload
impl BinaryEncode for Op {
    fn encoded_length(&self) -> usize {
        self.by_ref().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.by_ref().encode_into(out);
    }
}

impl BinaryDecode for Op {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, opcode) = u8::decode(input, &())?;

        match opcode {
            InscriptionOp::CODE => InscriptionOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ChannelInscribe(op))),
            ChannelConfigOp::CODE => ChannelConfigOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ChannelConfig(op))),
            DepositOp::CODE => {
                DepositOp::decode(input, &()).map(|(rest, op)| (rest, Self::ChannelDeposit(op)))
            }
            ChannelWithdrawOp::CODE => ChannelWithdrawOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ChannelWithdraw(op))),
            ChannelTransferOp::CODE => ChannelTransferOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ChannelTransfer(op))),
            SDPDeclareOp::CODE => {
                SDPDeclareOp::decode(input, &()).map(|(rest, op)| (rest, Self::SDPDeclare(op)))
            }
            SDPWithdrawOp::CODE => {
                SDPWithdrawOp::decode(input, &()).map(|(rest, op)| (rest, Self::SDPWithdraw(op)))
            }
            SDPActiveOp::CODE => {
                SDPActiveOp::decode(input, &()).map(|(rest, op)| (rest, Self::SDPActive(op)))
            }
            LeaderClaimOp::CODE => {
                LeaderClaimOp::decode(input, &()).map(|(rest, op)| (rest, Self::LeaderClaim(op)))
            }
            TransferOp::CODE => {
                TransferOp::decode(input, &()).map(|(rest, op)| (rest, Self::Transfer(op)))
            }
            ClaimPowRewardOp::CODE => ClaimPowRewardOp::decode(input, &())
                .map(|(rest, op)| (rest, Self::ClaimPowReward(op))),
            other => Err(DecodeError::unknown_discriminant::<Self>(u64::from(other))),
        }
    }
}

impl Op {
    #[must_use]
    pub fn by_ref(&self) -> OpRef<'_> {
        self.into()
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        self.by_ref().as_str()
    }

    #[must_use]
    pub fn gas_cost<Profile: GasProfile>(&self) -> Gas {
        self.by_ref().gas_cost::<Profile>()
    }

    #[cfg(any(test, feature = "samples"))]
    #[must_use]
    pub fn sample_proof(&self) -> OpProof {
        match self {
            Self::ChannelInscribe(op) => sample_proof_for(op),
            Self::ChannelConfig(op) => sample_proof_for(op),
            Self::ChannelDeposit(op) => sample_proof_for(op),
            Self::ChannelWithdraw(op) => sample_proof_for(op),
            Self::ChannelTransfer(op) => sample_proof_for(op),
            Self::SDPDeclare(op) => sample_proof_for(op),
            Self::SDPWithdraw(op) => sample_proof_for(op),
            Self::SDPActive(op) => sample_proof_for(op),
            Self::LeaderClaim(op) => sample_proof_for(op),
            Self::Transfer(op) => sample_proof_for(op),
            Self::ClaimPowReward(op) => sample_proof_for(op),
        }
    }
}

macro_rules! impl_from_operation {
    ($($variant:ident => $operation:ident),* $(,)?) => {
        $(
            impl From<$operation> for Op {
                fn from(op: $operation) -> Self {
                    Self::$variant(op)
                }
            }
        )*
    };
}

impl_from_operation! {
    ChannelInscribe => InscriptionOp,
    ChannelConfig => ChannelConfigOp,
    ChannelDeposit => DepositOp,
    ChannelWithdraw => ChannelWithdrawOp,
    ChannelTransfer => ChannelTransferOp,
    SDPDeclare => SDPDeclareOp,
    SDPWithdraw => SDPWithdrawOp,
    SDPActive => SDPActiveOp,
    LeaderClaim => LeaderClaimOp,
    Transfer => TransferOp,
    ClaimPowReward => ClaimPowRewardOp,
}

#[cfg(test)]
mod tests {
    use crate::mantle::{Op, OpProof, transactions::Ops};

    #[test]
    fn sample_proof_matches_the_kind_each_op_requires() {
        for op in &Ops::sample() {
            let expected: fn(&OpProof) -> bool = match op {
                Op::ChannelInscribe(_) => |proof| matches!(proof, OpProof::Ed25519Sig(_)),
                Op::ChannelConfig(_) => |proof| matches!(proof, OpProof::ChannelMultiSigProof(_)),
                Op::ChannelDeposit(_) => |proof| matches!(proof, OpProof::ZkSig(_)),
                Op::ChannelWithdraw(_) => |proof| matches!(proof, OpProof::ChannelMultiSigProof(_)),
                Op::ChannelTransfer(_) => |proof| matches!(proof, OpProof::ChannelMultiSigProof(_)),
                Op::SDPDeclare(_) => |proof| matches!(proof, OpProof::ZkAndEd25519Sigs(_)),
                Op::SDPWithdraw(_) => |proof| matches!(proof, OpProof::ZkSig(_)),
                Op::SDPActive(_) => |proof| matches!(proof, OpProof::ZkSig(_)),
                Op::LeaderClaim(_) => |proof| matches!(proof, OpProof::PoC(_)),
                Op::Transfer(_) => |proof| matches!(proof, OpProof::ZkSig(_)),
                Op::ClaimPowReward(_) => |proof| matches!(proof, OpProof::None(_)),
            };

            let proof = op.sample_proof();

            assert!(
                expected(&proof),
                "{} got the wrong proof kind: {proof:?}",
                op.as_str()
            );
        }
    }
}
