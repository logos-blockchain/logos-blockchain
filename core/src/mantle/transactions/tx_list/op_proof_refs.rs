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
