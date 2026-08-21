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
