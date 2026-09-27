use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
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

#[cfg(feature = "openapi")]
mod openapi {
    use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};
    use utoipa::{
        PartialSchema, ToSchema,
        openapi::{
            RefOr, Type,
            schema::{AdditionalProperties, ObjectBuilder, OneOfBuilder, Schema},
        },
    };

    use super::OpProof;
    use crate::{
        mantle::ops::{OpProofRef, ZkAndEd25519Proof},
        openapi::{Schemas, collect, reference},
        proofs::{
            channel_multi_sig_proof::ChannelMultiSigProof,
            leader_claim_proof::Groth16LeaderClaimProof,
        },
    };

    /// `serde`'s externally tagged form: an object whose only key is the
    /// variant.
    fn variant(tag: &str, value: impl Into<RefOr<Schema>>) -> ObjectBuilder {
        ObjectBuilder::new()
            .schema_type(Type::Object)
            .title(Some(tag))
            .property(tag, value)
            .required(tag)
            .additional_properties(Some(AdditionalProperties::FreeForm(false)))
    }

    impl PartialSchema for OpProof {
        fn schema() -> RefOr<Schema> {
            OneOfBuilder::new()
                .item(variant("Ed25519Sig", reference::<Ed25519Signature>()))
                .item(variant("ZkSig", reference::<ZkSignature>()))
                .item(variant(
                    "ZkAndEd25519Sigs",
                    reference::<ZkAndEd25519Proof>(),
                ))
                .item(variant("PoC", reference::<Groth16LeaderClaimProof>()))
                .item(variant(
                    "ChannelMultiSigProof",
                    reference::<ChannelMultiSigProof>(),
                ))
                .item(variant(
                    "None",
                    ObjectBuilder::new().schema_type(Type::Null),
                ))
                .description(Some(
                    "The proof authorizing the operation at the same index. Which variant is \
                     required is determined by that operation.",
                ))
                .into()
        }
    }

    impl ToSchema for OpProof {
        fn schemas(schemas: &mut Schemas) {
            collect::<Ed25519Signature>(schemas);
            collect::<ZkSignature>(schemas);
            collect::<ZkAndEd25519Proof>(schemas);
            collect::<Groth16LeaderClaimProof>(schemas);
            collect::<ChannelMultiSigProof>(schemas);
        }
    }

    /// Serializes exactly like [`OpProof`], so it shares its component.
    impl PartialSchema for OpProofRef<'_> {
        fn schema() -> RefOr<Schema> {
            OpProof::schema()
        }
    }

    impl ToSchema for OpProofRef<'_> {
        fn name() -> std::borrow::Cow<'static, str> {
            OpProof::name()
        }

        fn schemas(schemas: &mut Schemas) {
            OpProof::schemas(schemas);
        }
    }
}

pub struct OpProofDecodeContext<'op> {
    pub op: &'op Op,
}

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

#[cfg(any(test, feature = "test-utils"))]
pub mod samples {
    use lb_groth16::{COMPRESSED_PROOF_SIZE, CompressedGroth16Proof};
    use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};

    use crate::mantle::{OpProof, ledger::ProvableOperation};

    pub trait SampleProof {
        fn sample() -> Self;
    }

    impl SampleProof for Ed25519Signature {
        fn sample() -> Self {
            Self::zero()
        }
    }

    impl SampleProof for ZkSignature {
        fn sample() -> Self {
            Self::new(CompressedGroth16Proof::from_bytes(
                &[0u8; COMPRESSED_PROOF_SIZE],
            ))
        }
    }

    pub fn sample_proof_for<T>(_op: &T) -> OpProof
    where
        T: ProvableOperation,
        T::Proof: SampleProof + Into<OpProof>,
    {
        T::Proof::sample().into()
    }
}
