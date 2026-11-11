pub type Hash = [u8; 32];

pub type HeaderId = Hash;

pub type TxHash = Hash;

/// A note (UTXO) identifier, as 32 little-endian bytes. The FFI representation
/// of [`lb_core::mantle::NoteId`].
pub type NoteId = Hash;

/// Converts a raw pointer to a `TxHash` into a `lb_core::mantle::TxHash`.
///
/// # Parameters
///
/// - `tx_hash`: A raw pointer to a `TxHash` (32-byte array).
///
/// # Returns
///
/// - A `lb_core::mantle::TxHash` if successful, or an
///   `OperationStatusCode::ValidationError` if the conversion fails.
///
/// # Safety
///
/// This function is unsafe because it dereferences a raw pointer.
/// The caller must ensure that the pointer is valid and points to a properly
/// initialized `TxHash`.
pub unsafe fn into_tx_hash(tx_hash: *const TxHash) -> lb_core::mantle::TxHash {
    lb_core::mantle::TxHash::from(unsafe { *tx_hash })
}
