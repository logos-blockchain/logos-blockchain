use std::ptr;

use crate::api::{
    cryptarchia::{HeaderId, NoteId},
    types::value::Value,
};

/// One wallet-owned UTXO old enough to take part in the leadership lottery.
#[repr(C)]
pub struct LeaderAgedNote {
    /// The note ID, as 32 little-endian bytes.
    pub id: NoteId,
    /// The value staked by the note.
    pub value: Value,
    /// The wallet address holding the note, as 32 little-endian bytes.
    pub public_key: [u8; 32],
}

/// The wallet's UTXOs that are eligible to lead at `tip`.
///
/// A note is eligible when it is in the epoch's aged UTXO snapshot — the same
/// stake distribution the leadership proof is built against — and its public
/// key is one the wallet holds a key for. `len == 0` means this node cannot
/// win a slot at `tip`: either it owns no notes, or none have aged into the
/// current epoch's snapshot yet.
///
/// The set is reported unfiltered. The leader service additionally skips the
/// faucet UTXO when a `faucet_pk` is configured, which only matters on a
/// faucet node.
#[repr(C)]
pub struct LeaderAgedNotes {
    pub tip: HeaderId,
    pub notes: *mut LeaderAgedNote,
    /// Number of entries in `notes`.
    pub len: usize,
    /// Total value staked across `notes`, saturating.
    pub total_value: Value,
}

impl Default for LeaderAgedNotes {
    fn default() -> Self {
        Self {
            tip: [0; 32],
            notes: ptr::null_mut(),
            len: 0,
            total_value: 0,
        }
    }
}
