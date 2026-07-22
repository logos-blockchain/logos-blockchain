use std::ptr;

use crate::api::{
    cryptarchia::{HeaderId, NoteId},
    types::value::Value,
};

/// A single spendable wallet note (UTXO): its note ID and value.
#[repr(C)]
pub struct WalletNote {
    /// The note ID.
    pub id: NoteId,
    /// The value held by the note.
    pub value: Value,
}

/// The set of spendable notes for a wallet address at a given tip.
#[repr(C)]
pub struct WalletNotes {
    pub tip: HeaderId,
    pub notes: *mut WalletNote,
    pub len: usize,
}

impl Default for WalletNotes {
    fn default() -> Self {
        Self {
            tip: [0; 32],
            notes: ptr::null_mut(),
            len: 0,
        }
    }
}
