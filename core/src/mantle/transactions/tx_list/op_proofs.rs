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
