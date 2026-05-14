pub mod bounded_vec;
pub mod fisheryates;
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
            serializer.serialize_bytes(&src)
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
        use serde::Deserialize as _;
        let bytes = <&[u8]>::deserialize(deserializer)?;
        if bytes.len() == N {
            let mut output = [0u8; N];
            output.copy_from_slice(bytes);
            Ok(output)
        } else {
            Err(<D::Error as serde::de::Error>::invalid_length(
                bytes.len(),
                &format!("{N}").as_str(),
            ))
        }
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

    pub mod serde_bytes_slice {
        use core::fmt::Display;

        use serde::{Deserialize as _, Deserializer, Serializer, de::Error};

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

        pub fn deserialize<'de, T: TryFrom<Vec<u8>, Error: Display>, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<T, D::Error> {
            if deserializer.is_human_readable() {
                let s = String::deserialize(deserializer)?;
                let res = const_hex::decode(s)
                    .map(T::try_from)
                    .map_err(|_| Error::custom("Failed to convert decoded bytes"))?;
                res.map_err(Error::custom)
            } else {
                let res = Vec::<u8>::deserialize(deserializer).map(T::try_from)?;
                res.map_err(Error::custom)
            }
        }
    }
}
