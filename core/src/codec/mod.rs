//! Serializer for wire formats.
// TODO: we're using bincode for now, but might need strong guarantees about
// the underlying format in the future for standardization.
pub(crate) mod bincode;
pub mod errors;

use bytes::Bytes;
pub use errors::Error;
pub use lb_utils::bounded::UpperBoundedVec;
use serde::{Serialize, de::DeserializeOwned};
pub type Result<T> = std::result::Result<T, Error>;

pub trait SerializeOp {
    fn to_bytes(&self) -> Result<Bytes>;
    fn bytes_size(&self) -> Result<u64>;
}

pub trait DeserializeOp: Sized {
    fn from_bytes(data: &[u8]) -> Result<Self>;
}

impl<T: Serialize> SerializeOp for T {
    fn to_bytes(&self) -> Result<Bytes> {
        bincode::serialize(self)
    }

    fn bytes_size(&self) -> Result<u64> {
        bincode::serialized_size(self)
    }
}

mod sealed {
    pub trait Sealed {}
}

impl<const MAX: usize> sealed::Sealed for UpperBoundedVec<u8, MAX> {}

pub trait BoundedBytes: sealed::Sealed + AsRef<[u8]> + Sized {
    const MAX: usize;

    fn serialize<T: Serialize>(value: &T) -> Result<Self>;
}

impl<const MAX: usize> BoundedBytes for UpperBoundedVec<u8, MAX> {
    const MAX: usize = MAX;

    fn serialize<T: Serialize>(value: &T) -> Result<Self> {
        bincode::serialize_bounded::<_, MAX>(value)
    }
}

pub trait BoundedSerializeOp: SerializeOp + Serialize + Sized {
    type Bytes: BoundedBytes;

    fn to_bounded_bytes(&self) -> Result<Self::Bytes> {
        Self::Bytes::serialize(self)
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
    use serde::Serialize;

    use super::*;

    #[derive(Serialize)]
    struct TestBounded(Vec<u8>);

    impl BoundedSerializeOp for TestBounded {
        type Bytes = UpperBoundedVec<u8, 11>;
    }

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
        let tmp = TestBounded(vec![1u8, 2, 3]);

        let bounded = tmp.to_bounded_bytes().unwrap();

        assert_eq!(bounded.as_slice(), tmp.to_bytes().unwrap().as_ref());
        assert_eq!(bounded.len(), 11);
        assert_eq!(<TestBounded as BoundedSerializeOp>::Bytes::MAX, 11);
    }

    #[test]
    fn bounded_serialization_rejects_values_over_the_limit() {
        let error = TestBounded(vec![1u8, 2, 3, 4])
            .to_bounded_bytes()
            .unwrap_err();

        assert!(matches!(error, Error::Serialize(_)));
    }

    #[test]
    fn serialize_op_remains_object_safe() {
        let value: &dyn SerializeOp = &"test";

        assert_eq!(value.to_bytes().unwrap(), "test".to_bytes().unwrap());
    }
}
