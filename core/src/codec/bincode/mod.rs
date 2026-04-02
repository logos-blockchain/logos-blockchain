use std::sync::LazyLock;

use bincode::{
    Options as _,
    config::{
        FixintEncoding, LittleEndian, RejectTrailing, WithOtherEndian, WithOtherIntEncoding,
        WithOtherLimit, WithOtherTrailing,
    },
};

// Type composition is cool but also makes naming types a bit awkward
pub type BincodeOptions = WithOtherTrailing<
    WithOtherIntEncoding<
        WithOtherLimit<
            WithOtherEndian<bincode::DefaultOptions, LittleEndian>,
            bincode::config::Infinite,
        >,
        FixintEncoding,
    >,
    RejectTrailing,
>;

pub static OPTIONS: LazyLock<BincodeOptions> = LazyLock::new(|| {
    bincode::DefaultOptions::new()
        .with_little_endian()
        .with_no_limit()
        .with_fixint_encoding()
        .reject_trailing_bytes()
});

// Serialization functions
use bytes::Bytes;
use serde::{Serialize, de::DeserializeOwned};

use crate::codec::{Error as WireError, Result};

/// Serialize an object directly into bytes
pub fn serialize<T: Serialize>(item: &T) -> Result<Bytes> {
    Ok(OPTIONS
        .serialize(&item)
        .map_err(|e| WireError::Serialize(Box::new(e)))?
        .into())
}

/// Get the serialized size of an object without actually serializing it
pub fn serialized_size<T: Serialize>(item: &T) -> Result<u64> {
    OPTIONS
        .serialized_size(item)
        .map_err(|e| WireError::Serialize(Box::new(e)))
}

/// Deserialize an object directly from bytes
pub fn deserialize<T: DeserializeOwned>(data: &[u8]) -> Result<T> {
    OPTIONS
        .deserialize(data)
        .map_err(|e| WireError::Deserialize(Box::new(e)))
}
