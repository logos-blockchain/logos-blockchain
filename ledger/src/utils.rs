macro_rules! serialize_bytes_newtype {
    ($newtype:ty) => {
        #[cfg(feature = "serde")]
        impl serde::Serialize for $newtype {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                lb_utils::serde::serialize_bytes_array(self.0, serializer)
            }
        }

        #[cfg(feature = "serde")]
        impl<'de> serde::de::Deserialize<'de> for $newtype {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                lb_utils::serde::deserialize_bytes_array(deserializer).map(Self)
            }
        }
    };
}

pub(crate) use serialize_bytes_newtype;
