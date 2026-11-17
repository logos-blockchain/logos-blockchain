use std::ptr;

use lb_core::mantle::note_set::NoteSetIndex;

use crate::api::{
    cryptarchia::{HeaderId, NoteCommitment},
    types::value::Value,
};

/// The kind of a note set.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteSetKind {
    Ledger,
    Sdp,
    Channel,
}

/// A note set of the ledger: the ledger set, the SDP set or the set of a
/// channel.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteSet {
    pub kind: NoteSetKind,
    /// The channel of a channel set; zero for the other sets.
    pub channel_id: [u8; 32],
}

impl From<NoteSetIndex> for NoteSet {
    fn from(index: NoteSetIndex) -> Self {
        match index {
            NoteSetIndex::Ledger => Self {
                kind: NoteSetKind::Ledger,
                channel_id: [0; 32],
            },
            NoteSetIndex::Sdp => Self {
                kind: NoteSetKind::Sdp,
                channel_id: [0; 32],
            },
            NoteSetIndex::Channel(channel_id) => Self {
                kind: NoteSetKind::Channel,
                channel_id: *channel_id.as_ref(),
            },
        }
    }
}

/// One wallet-owned note old enough to take part in the leadership lottery.
#[repr(C)]
pub struct LeaderAgedNote {
    /// The note commitment, as 32 little-endian bytes.
    pub commitment: NoteCommitment,
    /// The note set holding the note.
    pub note_set: NoteSet,
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
#[repr(C)]
pub struct LeaderAgedNotes {
    pub tip: HeaderId,
    /// When the list is empty this pointer is not null, but it points to
    /// nothing: check the length before reading through it.
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
