use std::borrow::Cow;

use lb_codec::DecodeError;
use serde::{Deserialize, Deserializer};

use crate::mantle::{
    OpProof,
    transactions::tx_list::{Ops, TxBoundedVec, common::TxList},
};

pub type OpProofs = TxList<OpProof>;

impl OpProofs {
    /// Helper to decode an `OpProofs` for a given `Ops`
    pub(crate) fn decode_with_ops<'i>(
        input: &'i [u8],
        ops: &Ops,
    ) -> Result<(&'i [u8], Self), DecodeError> {
        let mut op_proofs_vec = Vec::with_capacity(ops.len());
        let mut remaining_input = input;
        for op in ops {
            let (rest, op_proof) = OpProof::decode_for_op(remaining_input, op)?;
            op_proofs_vec.push(op_proof);
            remaining_input = rest;
        }

        let op_proofs = Self::try_from(op_proofs_vec).map_err(|error| {
            DecodeError::Custom(Cow::from(format!("Failed to decode OpProofs: {error}")))
        })?;

        Ok((remaining_input, op_proofs))
    }

    /// One sample of every [`OpProof`] variant, in declaration order.
    ///
    /// Exhaustive over the enum, so it carries no alignment with any [`Ops`] —
    /// pairing proofs to their ops is `SignedOps`' job, not this column's.
    #[cfg(any(test, feature = "samples"))]
    #[must_use]
    pub fn sample() -> Self {
        use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};

        use crate::{
            mantle::ops::{NoOpProof, ZkAndEd25519Proof, op_proof::samples::SampleProof as _},
            proofs::{
                channel_multi_sig_proof::ChannelMultiSigProof,
                leader_claim_proof::Groth16LeaderClaimProof,
            },
        };

        Self::from([
            OpProof::Ed25519Sig(Ed25519Signature::sample()),
            OpProof::ZkSig(ZkSignature::sample()),
            OpProof::ZkAndEd25519Sigs(ZkAndEd25519Proof::sample()),
            OpProof::PoC(Groth16LeaderClaimProof::sample()),
            OpProof::ChannelMultiSigProof(ChannelMultiSigProof::sample()),
            OpProof::None(NoOpProof::sample()),
        ])
    }
}

/// A bare sequence, mirroring [`OpProofRefs`]' `Serialize`, which is what
/// writes it out.
///
/// Human-readable only; binary is not supported.
/// The wire encoding of this column belongs to `SignedOps`, which holds both.
///
/// A proof's variant is not recoverable from the proof column alone, it comes
/// from the [`Op`] at the same index.
/// The human-readable form carries the variant in the payload, so it
/// round-trips fine.
///
/// Refusing binary also neuters the blanket `DeserializeOp` impl in
/// [`crate::codec`]: `OpProofs::from_bytes` still exists, but fails instead of
/// decoding a column that carries no way to type itself.
impl<'de> Deserialize<'de> for OpProofs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            TxBoundedVec::deserialize(deserializer).map(Self)
        } else {
            Err(serde::de::Error::custom(
                "OpProofs has no standalone binary form: proofs are typed by their ops, so only SignedOps can decode them",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::DeserializeOp as _;

    #[test]
    fn deserialize_from_json() {
        let op_proofs = OpProofs::sample();
        let json = serde_json::to_value(op_proofs.inner()).expect("the inner column serializes");

        assert_eq!(
            serde_json::from_value::<OpProofs>(json).expect("the human-readable arm deserializes"),
            op_proofs
        );
    }

    #[test]
    fn deserialize_rejects_binary() {
        OpProofs::from_bytes(&[0u8; 8]).expect_err("Context-less binary decoding is unsupported.");
    }
}
