use bytes::{Bytes, BytesMut};

#[must_use]
pub fn key_bytes(prefix: &str, id: impl AsRef<[u8]>) -> Bytes {
    let mut buffer = BytesMut::new();

    buffer.extend_from_slice(prefix.as_bytes());
    buffer.extend_from_slice(id.as_ref());

    buffer.freeze()
}

// Combines a 32-byte blob ID (`[u8; 32]`) with a 2-byte column index
// (`u16` represented as `[u8; 2]`).
#[must_use]
pub fn create_share_idx(blob_id: &[u8], column_idx: &[u8]) -> [u8; 34] {
    let mut share_idx = [0u8; 34];
    share_idx[..blob_id.len()].copy_from_slice(blob_id);
    share_idx[blob_id.len()..].copy_from_slice(column_idx);

    share_idx
}
