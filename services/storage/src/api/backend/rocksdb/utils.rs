use bytes::{Bytes, BytesMut};

#[must_use]
pub fn key_bytes(prefix: &str, id: impl AsRef<[u8]>) -> Bytes {
    let mut buffer = BytesMut::new();

    buffer.extend_from_slice(prefix.as_bytes());
    buffer.extend_from_slice(id.as_ref());

    buffer.freeze()
}

#[must_use]
pub fn key_bytes_raw(prefix: &[u8], suffix: impl AsRef<[u8]>) -> Vec<u8> {
    prefix
        .iter()
        .chain(suffix.as_ref().iter())
        .copied()
        .collect()
}
