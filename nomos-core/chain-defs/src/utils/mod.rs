pub mod merkle;
pub mod select;

macro_rules! display_hex_bytes_newtype {
    ($newtype:ty) => {
        impl core::fmt::Display for $newtype {
            fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(f, "0x")?;
                for v in self.0 {
                    write!(f, "{:02x}", v)?;
                }
                Ok(())
            }
        }
    };
}

macro_rules! serde_bytes_newtype {
    ($newtype:ty, $len:expr) => {
        impl serde::Serialize for $newtype {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                if serializer.is_human_readable() {
                    const_hex::const_encode::<$len, false>(&self.0)
                        .as_str()
                        .serialize(serializer)
                } else {
                    self.0.serialize(serializer)
                }
            }
        }

        impl<'de> serde::Deserialize<'de> for $newtype {
            fn deserialize<D>(deserializer: D) -> Result<$newtype, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                if deserializer.is_human_readable() {
                    let s = <String>::deserialize(deserializer)?;
                    const_hex::decode_to_array(s)
                        .map(Self)
                        .map_err(serde::de::Error::custom)
                } else {
                    <[u8; $len] as serde::Deserialize>::deserialize(deserializer).map(Self)
                }
            }
        }
    };
}

pub(crate) use display_hex_bytes_newtype;
pub(crate) use serde_bytes_newtype;
