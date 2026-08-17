pub mod bounded;
pub mod math;
pub mod net;
pub mod noop_service;
pub mod types;
pub mod yaml;

#[cfg(feature = "rng")]
pub mod blake_rng;

#[cfg(feature = "time")]
pub mod bounded_duration;

#[cfg(feature = "tokio")]
pub mod tokio;

pub mod serde {
    fn serialize_human_readable_bytes_array<const N: usize, S: serde::Serializer>(
        src: [u8; N],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::Serialize as _;
        const_hex::const_encode::<N, false>(&src)
            .as_str()
            .serialize(serializer)
    }

    pub fn serialize_bytes_array<const N: usize, S: serde::Serializer>(
        src: [u8; N],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serialize_human_readable_bytes_array(src, serializer)
        } else {
            // Serialized as a fixed-size tuple so binary formats like bincode
            // do not emit a length prefix for compile-time-sized data. The
            // tuple is built explicitly because serde only implements
            // `Serialize` for arrays up to 32 elements; `src.serialize()`
            // would silently coerce larger arrays to the length-prefixed
            // slice encoding.
            use serde::ser::SerializeTuple as _;
            let mut tuple = serializer.serialize_tuple(N)?;
            for byte in &src {
                tuple.serialize_element(byte)?;
            }
            tuple.end()
        }
    }

    fn deserialize_human_readable_bytes_array<'de, const N: usize, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[u8; N], D::Error> {
        use std::borrow::Cow;

        use serde::Deserialize as _;
        let s: Cow<str> = Cow::deserialize(deserializer)?;
        let mut output = [0u8; N];
        const_hex::decode_to_slice(s.as_ref(), &mut output)
            .map(|()| output)
            .map_err(<D::Error as serde::de::Error>::custom)
    }

    fn deserialize_human_unreadable_bytes_array<
        'de,
        const N: usize,
        D: serde::Deserializer<'de>,
    >(
        deserializer: D,
    ) -> Result<[u8; N], D::Error> {
        struct ArrayVisitor<const N: usize>;

        impl<'de, const N: usize> serde::de::Visitor<'de> for ArrayVisitor<N> {
            type Value = [u8; N];

            fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(formatter, "an array of {N} bytes")
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut output = [0u8; N];
                for (i, byte) in output.iter_mut().enumerate() {
                    *byte = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(i, &self))?;
                }
                Ok(output)
            }
        }

        // Mirrors `serialize_bytes_array`: fixed-size data is encoded as a
        // tuple of bytes, which binary formats read back without a length
        // prefix. serde only provides `Deserialize` for arrays up to 32
        // elements, so larger sizes need this explicit tuple visitor.
        deserializer.deserialize_tuple(N, ArrayVisitor::<N>)
    }

    pub fn deserialize_bytes_array<'de, const N: usize, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[u8; N], D::Error> {
        if deserializer.is_human_readable() {
            deserialize_human_readable_bytes_array(deserializer)
        } else {
            deserialize_human_unreadable_bytes_array(deserializer)
        }
    }

    #[cfg(test)]
    mod tests {
        /// 64 bytes exceeds serde's built-in 32-element array support, so this
        /// exercises the explicit tuple path on both ends.
        struct Bytes64([u8; 64]);

        impl serde::Serialize for Bytes64 {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                super::serialize_bytes_array(self.0, serializer)
            }
        }

        impl<'de> serde::Deserialize<'de> for Bytes64 {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                super::deserialize_bytes_array(deserializer).map(Self)
            }
        }

        #[test]
        fn bincode_encoding_has_no_length_prefix() {
            let bytes = bincode::serialize(&Bytes64([7u8; 64])).unwrap();
            assert_eq!(
                bytes,
                vec![7u8; 64],
                "fixed-size byte arrays must encode as exactly N bytes"
            );
        }

        #[test]
        fn bincode_roundtrips() {
            let original: [u8; 64] = core::array::from_fn(|i| i as u8);
            let bytes = bincode::serialize(&Bytes64(original)).unwrap();
            let decoded: Bytes64 = bincode::deserialize(&bytes).unwrap();
            assert_eq!(decoded.0, original);
        }

        #[test]
        fn bincode_rejects_truncated_input() {
            assert!(bincode::deserialize::<Bytes64>(&[7u8; 63]).is_err());
        }

        #[test]
        fn json_is_hex_string() {
            let json = serde_json::to_string(&Bytes64([0xABu8; 64])).unwrap();
            assert_eq!(json, format!("\"{}\"", "ab".repeat(64)));
            let decoded: Bytes64 = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded.0, [0xABu8; 64]);
        }
    }

    pub mod serde_bytes_slice {
        use core::fmt::Display;
        use std::borrow::Cow;

        use serde::{
            Deserialize as _, Deserializer, Serializer,
            de::{Error, SeqAccess, Visitor},
        };

        use crate::bounded::UpperBoundedVec;

        pub fn serialize<Bytes: AsRef<[u8]>, S: Serializer>(
            bytes: &Bytes,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            let bytes = bytes.as_ref();
            if serializer.is_human_readable() {
                serializer.serialize_str(&const_hex::encode(bytes))
            } else {
                serializer.serialize_bytes(bytes)
            }
        }

        pub fn deserialize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
        where
            T: TryFrom<Vec<u8>>,
            T::Error: Display,
            D: Deserializer<'de>,
        {
            deserialize_bounded::<T, { usize::MAX }, D>(deserializer)
        }

        pub fn deserialize_bounded<'de, T, const MAX: usize, D>(
            deserializer: D,
        ) -> Result<T, D::Error>
        where
            T: TryFrom<Vec<u8>>,
            T::Error: Display,
            D: Deserializer<'de>,
        {
            let bytes: UpperBoundedVec<u8, MAX> = if deserializer.is_human_readable() {
                let encoded = Cow::<str>::deserialize(deserializer)?;
                let max_encoded_len = MAX.saturating_mul(2);
                if encoded.len() > max_encoded_len {
                    return Err(Error::custom(format_args!(
                        "encoded byte string exceeds maximum length of {max_encoded_len} characters"
                    )));
                }
                let decoded = const_hex::decode(encoded.as_ref())
                    .map_err(|error| Error::custom(error.to_string()))?;
                UpperBoundedVec::try_from(decoded).map_err(Error::custom)?
            } else {
                deserializer.deserialize_byte_buf(BoundedBytesVisitor::<MAX>)?
            };

            T::try_from(bytes.into_inner()).map_err(Error::custom)
        }

        struct BoundedBytesVisitor<const MAX: usize>;

        impl<const MAX: usize> BoundedBytesVisitor<MAX> {
            fn validate_len<E: Error>(len: usize) -> Result<(), E> {
                if len > MAX {
                    return Err(E::custom(format_args!(
                        "byte sequence contains {len} items, maximum is {MAX}"
                    )));
                }

                Ok(())
            }
        }

        impl<'de, const MAX: usize> Visitor<'de> for BoundedBytesVisitor<MAX> {
            type Value = UpperBoundedVec<u8, MAX>;

            fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(formatter, "a byte sequence of at most {MAX} bytes")
            }

            fn visit_bytes<E>(self, bytes: &[u8]) -> Result<Self::Value, E>
            where
                E: Error,
            {
                Self::validate_len::<E>(bytes.len())?;

                // The length was checked before allocating the owned copy.
                Ok(UpperBoundedVec::new_unchecked(bytes.to_vec()))
            }

            fn visit_byte_buf<E>(self, bytes: Vec<u8>) -> Result<Self::Value, E>
            where
                E: Error,
            {
                Self::validate_len::<E>(bytes.len())?;

                // Keep the supplied allocation rather than forwarding to
                // visit_bytes, which would copy it with to_vec().
                Ok(UpperBoundedVec::new_unchecked(bytes))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                // An empty vector always satisfies an upper-only bound.
                let capacity = sequence.size_hint().unwrap_or(0).min(MAX);
                let mut bytes = UpperBoundedVec::new_unchecked(Vec::with_capacity(capacity));

                while let Some(byte) = sequence.next_element()? {
                    bytes.try_push(byte).map_err(A::Error::custom)?;
                }

                Ok(bytes)
            }
        }
    }
}
