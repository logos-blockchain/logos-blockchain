use std::{borrow::Cow, collections::BTreeMap};

use super::{
    BinaryEncode, CodecExamples, CodecFixture, CodecFixtures,
    length_prefix::{MAX_ENCODABLE_LENGTH, encode_length_prefix_into, length_prefix_len},
    sealed,
};

// A map: its length as four bytes, then each entry's key and value, in
// ascending key order. That is the map's own order, so equal maps encode alike
// however they were built. A map is unbounded, so it takes the widest prefix
// there is; one of more than `u32::MAX` entries cannot be encoded.
impl<K, V> BinaryEncode for BTreeMap<K, V>
where
    K: BinaryEncode + Ord,
    V: BinaryEncode,
{
    fn encoded_length(&self) -> usize {
        length_prefix_len::<MAX_ENCODABLE_LENGTH>()
            .checked_add(
                self.iter()
                    .map(|(key, value)| key.encoded_length() + value.encoded_length())
                    .sum::<usize>(),
            )
            .expect("Encoded length overflow")
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        encode_length_prefix_into::<MAX_ENCODABLE_LENGTH>(self.len(), out);
        for (key, value) in self {
            key.encode_into(out);
            value.encode_into(out);
        }
    }
}

impl<K, V> sealed::Sealed for BTreeMap<K, V>
where
    K: CodecExamples,
    V: CodecExamples,
{
}

// The fixtures are the empty map, and the map holding the first fixture of the
// key under the first fixture of the value, so every `BTreeMap`
// monomorphization has fixtures without writing any.
impl<K, V> CodecExamples for BTreeMap<K, V>
where
    K: CodecExamples + Ord,
    V: CodecExamples,
{
    fn fixtures() -> CodecFixtures<Self> {
        let key = K::fixtures()
            .into_iter()
            .next()
            .expect("`CodecExamples::fixtures` is non-empty");
        let value = V::fixtures()
            .into_iter()
            .next()
            .expect("`CodecExamples::fixtures` is non-empty");

        let mut empty = Vec::new();
        encode_length_prefix_into::<MAX_ENCODABLE_LENGTH>(0, &mut empty);
        let mut single = Vec::new();
        encode_length_prefix_into::<MAX_ENCODABLE_LENGTH>(1, &mut single);
        single.extend_from_slice(key.bytes.as_ref());
        single.extend_from_slice(value.bytes.as_ref());

        [
            CodecFixture {
                value: Self::new(),
                bytes: Cow::Owned(empty),
            },
            CodecFixture {
                value: [(key.value, value.value)].into(),
                bytes: Cow::Owned(single),
            },
        ]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::canonical::{BinaryEncode as _, assert_codec_fixtures_encode_only};

    #[test]
    fn fixtures_hold() {
        assert_codec_fixtures_encode_only::<BTreeMap<u8, u8>>();
    }

    #[test]
    fn entries_follow_a_four_byte_length_in_ascending_key_order() {
        // Inserted out of order: the encoding follows the keys.
        let map = BTreeMap::from([(2u8, 20u8), (1u8, 10u8)]);
        assert_eq!(map.encode_to_vec(), [2, 0, 0, 0, 1, 10, 2, 20]);
    }
}
