use lb_codec::{BinaryDecode, BinaryEncode, DecodeError};
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};
use serde::{Deserialize, Serialize};

use crate::{
    mantle::{
        Op,
        ledger::ProvableOperation,
        ops::{NoOpProof, OpProofRef, ZkAndEd25519Proof},
    },
    proofs::{
        channel_multi_sig_proof::ChannelMultiSigProof, leader_claim_proof::Groth16LeaderClaimProof,
    },
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpProof {
    Ed25519Sig(Ed25519Signature),
    ZkSig(ZkSignature),
    ZkAndEd25519Sigs(ZkAndEd25519Proof),
    PoC(Groth16LeaderClaimProof),
    ChannelMultiSigProof(ChannelMultiSigProof),
    None(NoOpProof),
}

impl OpProof {
    #[must_use]
    pub fn by_ref(&self) -> OpProofRef<'_> {
        self.into()
    }

    /// Helper to decode an `OpProof` for a given `Op`
    pub fn decode_for_op<'i>(input: &'i [u8], op: &Op) -> Result<(&'i [u8], Self), DecodeError> {
        match op {
            Op::ChannelInscribe(op) => decode_proof_for(op, input),
            Op::ChannelConfig(op) => decode_proof_for(op, input),
            Op::ChannelDeposit(op) => decode_proof_for(op, input),
            Op::ChannelWithdraw(op) => decode_proof_for(op, input),
            Op::ChannelTransfer(op) => decode_proof_for(op, input),
            Op::SDPDeclare(op) => decode_proof_for(op, input),
            Op::SDPWithdraw(op) => decode_proof_for(op, input),
            Op::SDPActive(op) => decode_proof_for(op, input),
            Op::LeaderClaim(op) => decode_proof_for(op, input),
            Op::Transfer(op) => decode_proof_for(op, input),
            Op::ClaimPowReward(op) => decode_proof_for(op, input),
        }
    }
}

impl BinaryEncode for OpProof {
    fn encoded_length(&self) -> usize {
        self.by_ref().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.by_ref().encode_into(out);
    }
}

fn decode_proof_for<'a, T>(_op: &T, input: &'a [u8]) -> Result<(&'a [u8], OpProof), DecodeError>
where
    T: ProvableOperation,
    T::Proof: BinaryDecode<Context = ()> + Into<OpProof>,
{
    T::Proof::decode(input, &()).map(|(rest, proof)| (rest, proof.into()))
}

pub struct OpProofDecodeContext<'op> {
    pub op: &'op Op,
}

// impl<'context> BinaryDecode for OpProof {
//     type Context = OpProofDecodeContext<'context>;
//
//     fn decode<'input>(
//         input: &'input [u8],
//         context: &Self::Context,
//     ) -> Result<(&'input [u8], Self), DecodeError> {
//         match context.op {
//             Op::ChannelInscribe(op) => decode_proof_for(op, input),
//             Op::ChannelConfig(op) => decode_proof_for(op, input),
//             Op::ChannelDeposit(op) => decode_proof_for(op, input),
//             Op::ChannelWithdraw(op) => decode_proof_for(op, input),
//             Op::ChannelTransfer(op) => decode_proof_for(op, input),
//             Op::SDPDeclare(op) => decode_proof_for(op, input),
//             Op::SDPWithdraw(op) => decode_proof_for(op, input),
//             Op::SDPActive(op) => decode_proof_for(op, input),
//             Op::LeaderClaim(op) => decode_proof_for(op, input),
//             Op::Transfer(op) => decode_proof_for(op, input),
//             Op::ClaimPowReward(op) => decode_proof_for(op, input),
//         }
//     }
// }

macro_rules! impl_from_proof {
    ($($proof:ty => $variant:ident),* $(,)?) => {
        $(
            impl From<$proof> for crate::mantle::ops::OpProof {
                fn from(proof: $proof) -> Self {
                    crate::mantle::ops::OpProof::$variant(proof)
                }
            }
        )*
    };
}

impl_from_proof! {
    Ed25519Signature => Ed25519Sig,
    ZkSignature => ZkSig,
    ZkAndEd25519Proof => ZkAndEd25519Sigs,
    Groth16LeaderClaimProof => PoC,
    ChannelMultiSigProof => ChannelMultiSigProof,
    NoOpProof => None,
}

macro_rules! impl_try_from_op_proof_for_proof {
    ($($variant:ident => $proof:ty),* $(,)?) => {
        $(
            impl TryFrom<crate::mantle::ops::OpProof> for $proof {
                type Error = crate::mantle::ops::OpProof;

                fn try_from(op_proof: crate::mantle::ops::OpProof) -> Result<Self, Self::Error> {
                    match op_proof {
                        crate::mantle::ops::OpProof::$variant(inner) => Ok(inner),
                        other => Err(other),
                    }
                }
            }
        )*
    };
}

impl_try_from_op_proof_for_proof! {
    Ed25519Sig => Ed25519Signature,
    ZkSig => ZkSignature,
    ZkAndEd25519Sigs => ZkAndEd25519Proof,
    PoC => Groth16LeaderClaimProof,
    ChannelMultiSigProof => ChannelMultiSigProof,
    None => NoOpProof,
}

#[cfg(feature = "test-utils")]
pub mod placeholders {
    use lb_groth16::{COMPRESSED_PROOF_SIZE, CompressedGroth16Proof};
    use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};

    use crate::{
        mantle::{
            OpProof,
            ledger::ProvableOperation,
            ops::{NoOpProof, ZkAndEd25519Proof},
        },
        proofs::{
            channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignatures},
            leader_claim_proof::Groth16LeaderClaimProof,
        },
    };

    pub trait PlaceholderProof {
        fn placeholder() -> Self;
    }

    impl PlaceholderProof for Ed25519Signature {
        fn placeholder() -> Self {
            Self::zero()
        }
    }

    impl PlaceholderProof for ZkSignature {
        fn placeholder() -> Self {
            Self::new(CompressedGroth16Proof::from_bytes(
                &[0u8; COMPRESSED_PROOF_SIZE],
            ))
        }
    }

    impl PlaceholderProof for ZkAndEd25519Proof {
        fn placeholder() -> Self {
            Self::new(ZkSignature::placeholder(), Ed25519Signature::placeholder())
        }
    }

    impl PlaceholderProof for Groth16LeaderClaimProof {
        fn placeholder() -> Self {
            Self::new(CompressedGroth16Proof::from_bytes(
                &[0u8; COMPRESSED_PROOF_SIZE],
            ))
        }
    }

    impl PlaceholderProof for ChannelMultiSigProof {
        fn placeholder() -> Self {
            Self::new_unchecked(IndexedSignatures::empty())
        }
    }

    impl PlaceholderProof for NoOpProof {
        fn placeholder() -> Self {
            Self
        }
    }

    pub fn placeholder_proof_for<T>(_op: &T) -> OpProof
    where
        T: ProvableOperation,
        T::Proof: PlaceholderProof + Into<OpProof>,
    {
        T::Proof::placeholder().into()
    }
}

#[cfg(test)]
mod tests {
    use lb_codec::BinaryEncode as _;
    use lb_groth16::{COMPRESSED_PROOF_SIZE, CompressedGroth16Proof};
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use num_bigint::BigUint;

    use crate::{
        mantle::{
            Op, OpProof,
            ops::leader_claim::{LeaderClaimOp, RewardsRoot, VoucherNullifier},
        },
        proofs::leader_claim_proof::Groth16LeaderClaimProof,
    };

    #[test]
    fn test_encode_decode_roundtrip_leader_claim_op_proof() {
        let op = Op::LeaderClaim(LeaderClaimOp {
            rewards_root: RewardsRoot::default(),
            voucher_nullifier: VoucherNullifier::default(),
            pk: ZkPublicKey::from(BigUint::from(0u64)),
        });

        let proof_bytes: [u8; 128] = core::array::from_fn(|i| i as u8);
        let proof = OpProof::PoC(Groth16LeaderClaimProof::new(
            CompressedGroth16Proof::from_bytes(&proof_bytes),
        ));

        let encoded = proof.encode();
        assert_eq!(encoded.len(), COMPRESSED_PROOF_SIZE);

        let (remaining, decoded) = OpProof::decode_for_op(&encoded, &op).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(decoded, proof);
    }
}
