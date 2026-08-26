use lb_codec::BinaryEncode;
use serde::{Serialize, Serializer};

use crate::mantle::{
    OpRef, TxHash,
    traits::{Hashable, hashable},
    transactions::{
        MANTLE_TX_HASH_V1_BYTES,
        tx_list::{Ops, common::TxList, hash::tx_hasher},
    },
};

pub type OpRefs<'a> = TxList<OpRef<'a>>;

impl<'a> OpRefs<'a> {
    #[must_use]
    pub fn get(&self, index: usize) -> Option<OpRef<'a>> {
        self.as_slice().get(index).copied()
    }
}

impl<'a> From<&'a Ops> for OpRefs<'a> {
    fn from(ops: &'a Ops) -> Self {
        ops.by_ref()
    }
}

impl BinaryEncode for OpRefs<'_> {
    fn encoded_length(&self) -> usize {
        self.0.encoded_length()
    }
    fn encode_into(&self, out: &mut Vec<u8>) {
        self.0.encode_into(out);
    }
}

impl Hashable for OpRefs<'_> {
    //noinspection RsTypeCheck: The type is correct, but the linter is confused by
    // the closure.
    const HASHER: hashable::Hasher<Self> = tx_hasher;
    type Hash = TxHash;

    fn as_signing(&self) -> Vec<u8> {
        // Constant and structure as defined in the Mantle specification:
        // https://lip.logos.co/blockchain/raw/bedrock-v1.1-mantle-specification.html
        let mut buffer = MANTLE_TX_HASH_V1_BYTES.to_vec();
        buffer.extend(self.encode());
        buffer
    }
}

/// Mirrors [`Ops`]' `Deserialize`, which is what reads it back.
impl Serialize for OpRefs<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            self.inner().serialize(serializer)
        } else {
            let bytes = self.encode();
            serializer.serialize_bytes(&bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{DeserializeOp as _, SerializeOp as _};

    #[test]
    fn get_returns_the_op_at_the_index_or_none_past_the_end() {
        let ops = Ops::sample();
        let last = ops.len() - 1;
        let references = OpRefs::from(&ops);

        assert_eq!(references.get(0), Some(ops[0].by_ref()));
        assert_eq!(references.get(last), Some(ops[last].by_ref()));
        assert_eq!(references.get(ops.len()), None);
    }

    #[test]
    fn hash_matches_the_ops_it_borrows() {
        let ops = Ops::sample();

        assert_eq!(OpRefs::from(&ops).hash(), ops.hash());
    }

    #[test]
    fn serialize_to_json() {
        let ops = Ops::sample();
        let op_refs = OpRefs::from(&ops);

        assert_eq!(
            serde_json::to_value(&op_refs).expect("the human-readable arm serializes"),
            serde_json::to_value(op_refs.inner()).expect("the inner column serializes")
        );
    }

    #[test]
    fn serialize_to_json_round_trips_into_ops() {
        let ops = Ops::sample();
        let json =
            serde_json::to_string(&OpRefs::from(&ops)).expect("the human-readable arm serializes");

        assert_eq!(
            serde_json::from_str::<Ops>(&json).expect("the human-readable arm deserializes"),
            ops
        );
    }

    #[test]
    fn serialize_to_binary() {
        let ops = Ops::sample();
        let op_refs = OpRefs::from(&ops);

        assert_eq!(
            op_refs.to_bytes().expect("the reference view serializes"),
            bincode::serialize(&op_refs.encode().into_vec()).expect("the envelope serializes")
        );
    }

    #[test]
    fn serialize_to_binary_round_trips_into_ops() {
        let ops = Ops::sample();
        let bytes = OpRefs::from(&ops)
            .to_bytes()
            .expect("the reference view serializes");

        assert_eq!(
            Ops::from_bytes(&bytes).expect("the owned column deserializes"),
            ops
        );
    }
}
