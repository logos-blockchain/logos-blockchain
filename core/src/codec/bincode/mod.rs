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
use bytes::{BufMut as _, Bytes, BytesMut};
use serde::{Serialize, de::DeserializeOwned};

use crate::codec::{Error as WireError, Result};

const ONE_GB_MEMORY_WARNING_THRESHOLD: usize = 1024 * 1024 * 1024; // 1 GB
const DEFAULT_SERIALIZATION_CAPACITY: usize = 16 * 1024; // 16 KB

/// Serialize an object directly into bytes
pub fn serialize<T: Serialize>(item: &T) -> Result<Bytes> {
    // Start with a reasonable default capacity to avoid multiple reallocations.
    // This will be automatically resized if the serialized data exceeds this
    // capacity, but it helps optimize for small to medium-sized objects. The
    // alternative would be to compute the serialized size first and allocate
    // exactly that much memory, but that would require serializing the object
    // twice (once to compute size and once to serialize), which is inefficient
    // for larger objects.
    let buf = BytesMut::with_capacity(DEFAULT_SERIALIZATION_CAPACITY);

    let mut writer = buf.writer();
    bincode::serialize_into(&mut writer, item).map_err(|e| WireError::Serialize(Box::new(e)))?;

    let buf = writer.into_inner();
    let size = buf.len();

    if size > ONE_GB_MEMORY_WARNING_THRESHOLD {
        tracing::warn!("Large serialization detected: {size} bytes. This may impact memory usage.");
    }

    Ok(buf.freeze())
}

/// Get the serialized size of an object without actually serializing it
pub fn serialized_size<T: Serialize>(item: &T) -> Result<u64> {
    OPTIONS
        .serialized_size(item)
        .map_err(|e| WireError::Serialize(Box::new(e)))
}

/// Deserialize an object directly from bytes
pub fn deserialize<T: DeserializeOwned>(data: &[u8]) -> Result<T> {
    if data.len() > ONE_GB_MEMORY_WARNING_THRESHOLD {
        tracing::warn!(
            "Large deserialization detected: {} bytes. This may impact memory usage.",
            data.len()
        );
    }
    OPTIONS
        .deserialize(data)
        .map_err(|e| WireError::Deserialize(Box::new(e)))
}
