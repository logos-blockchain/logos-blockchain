use serde::{Serialize, Serializer};

use crate::mantle::{OpProofRef, transactions::tx_list::common::TxList};

pub type OpProofRefs<'a> = TxList<OpProofRef<'a>>;

/// A bare sequence, mirroring [`OpProofs`]' `Deserialize`, which is what reads
/// it back.
///
/// Human-readable only; binary is not supported.
/// The wire encoding of this column belongs to `SignedOps`, which holds both.
///
/// A proof's variant is not recoverable from the proof column alone, it comes
/// from the [`Op`] at the same index.
/// The human-readable form carries the variant in the payload, so it
/// round-trips fine.
///
/// Refusing binary also neuters the blanket `SerializeOp` impl in
/// [`crate::codec`]: `OpProofRefs::to_bytes` still exists, but fails instead of
/// emitting a column that carries no way to type itself.
impl Serialize for OpProofRefs<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            self.as_slice().serialize(serializer)
        } else {
            Err(serde::ser::Error::custom(
                "OpProofRefs has no standalone binary form: proofs are typed by their ops, so only SignedOps can encode them",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codec::SerializeOp as _,
        mantle::{OpProof, ops::NoOpProof, transactions::OpProofs},
    };

    #[test]
    fn serialize_to_json() {
        let op_proofs = OpProofs::sample();
        let op_proof_refs =
            OpProofRefs::try_from(op_proofs.iter().map(OpProof::by_ref).collect::<Vec<_>>())
                .expect("the sample is within bounds");

        assert_eq!(
            serde_json::to_value(&op_proof_refs).expect("the human-readable arm serializes"),
            serde_json::to_value(op_proof_refs.as_slice()).expect("the inner column serializes")
        );
    }

    #[test]
    fn serialize_to_json_round_trips_into_owned_op_proofs() {
        let op_proofs = OpProofs::sample();
        let op_proof_refs =
            OpProofRefs::try_from(op_proofs.iter().map(OpProof::by_ref).collect::<Vec<_>>())
                .expect("the sample is within bounds");

        let json =
            serde_json::to_string(&op_proof_refs).expect("the human-readable arm serializes");

        assert_eq!(
            serde_json::from_str::<OpProofs>(&json).expect("the human-readable arm deserializes"),
            op_proofs
        );
    }

    #[test]
    fn serialize_rejects_binary() {
        let op_proof = OpProof::None(NoOpProof);
        let op_proof_refs = OpProofRefs::from([op_proof.by_ref()]);

        op_proof_refs
            .to_bytes()
            .expect_err("Context-less binary encoding is unsupported.");
    }
}
