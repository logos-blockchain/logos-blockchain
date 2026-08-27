//! Serializer for wire formats.
// TODO: we're using bincode for now, but might need strong guarantees about
// the underlying format in the future for standardization.
pub(crate) mod bincode;
pub mod errors;

use bytes::Bytes;
pub use errors::Error;
use lb_utils::bounded::UpperBoundedVec;
use serde::{Serialize, de::DeserializeOwned};
pub type Result<T> = std::result::Result<T, Error>;

pub trait SerializeOp {
    fn to_bytes(&self) -> Result<Bytes>;

    /// Serialize the value into a byte vector whose length is bounded by
    /// `MAX`.
    fn to_bounded_bytes<const MAX: usize>(&self) -> Result<UpperBoundedVec<u8, MAX>>
    where
        Self: Sized;
    fn bytes_size(&self) -> Result<u64>;
}

pub trait DeserializeOp: Sized {
    fn from_bytes(data: &[u8]) -> Result<Self>;
}

impl<T: Serialize> SerializeOp for T {
    fn to_bytes(&self) -> Result<Bytes> {
        bincode::serialize(self)
    }

    fn to_bounded_bytes<const MAX: usize>(&self) -> Result<UpperBoundedVec<u8, MAX>> {
        bincode::serialize_bounded(self)
    }

    fn bytes_size(&self) -> Result<u64> {
        bincode::serialized_size(self)
    }
}

impl<T: DeserializeOwned> DeserializeOp for T {
    fn from_bytes(data: &[u8]) -> Result<Self> {
        bincode::deserialize(data)
    }
}

pub trait SerdeOp: SerializeOp + DeserializeOp {}

impl<T> SerdeOp for T where T: SerializeOp + DeserializeOp {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_deserialize() {
        let tmp = String::from("much wow, very cool");
        let serialized = tmp.to_bytes().unwrap();
        let deserialized: String = String::from_bytes(&serialized).unwrap();
        assert_eq!(tmp, deserialized);
    }

    #[test]
    fn serialize_deserialize_owned() {
        let tmp = String::from("much wow, very cool");
        let serialized = tmp.to_bytes().unwrap();
        let deserialized: String = String::from_bytes(&serialized).unwrap();
        assert_eq!(tmp, deserialized);
    }

    #[test]
    fn test_serialized_size() {
        let tmp = String::from("test");
        let size = tmp.bytes_size().unwrap();
        let serialized = tmp.to_bytes().unwrap();
        assert_eq!(size as usize, serialized.len());
    }

    #[test]
    fn bounded_serialization_preserves_the_wire_format() {
        let tmp = vec![1u8, 2, 3];

        let bounded = tmp.to_bounded_bytes::<11>().unwrap();

        assert_eq!(bounded.as_slice(), tmp.to_bytes().unwrap().as_ref());
        assert_eq!(bounded.len(), 11);
    }

    #[test]
    fn bounded_serialization_rejects_values_over_the_limit() {
        let error = vec![1u8, 2, 3].to_bounded_bytes::<10>().unwrap_err();

        assert!(matches!(error, Error::Serialize(_)));
    }

    #[test]
    fn serialize_op_remains_object_safe() {
        let value: &dyn SerializeOp = &"test";

        assert_eq!(value.to_bytes().unwrap(), "test".to_bytes().unwrap());
    }
}
