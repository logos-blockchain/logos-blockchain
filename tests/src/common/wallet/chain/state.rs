use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex, PoisonError},
};

use lb_core::mantle::{
    Note,
    notes::{NoteCommitment, NoteNullifier},
    traits::GenesisTx as _,
    transactions::GenesisTx,
};
use lb_key_management_system_service::keys::{UnsecuredZkKey, ZkPublicKey};
use lb_wallet::OwnedNote;
use thiserror::Error;

use crate::common::wallet::WalletId;

pub type WalletUtxos = HashMap<WalletId, Vec<OwnedNote>>;

/// Wallet keys whose chain notes should be tracked as one test wallet.
#[derive(Debug, Clone)]
pub struct TrackedWalletKeys {
    wallet_id: WalletId,
    // the secret keys derive the nullifiers of the notes and prove their spending
    wallet_keys: HashMap<ZkPublicKey, UnsecuredZkKey>,
}

impl TrackedWalletKeys {
    #[must_use]
    pub fn new(
        wallet_id: impl Into<WalletId>,
        wallet_keys: impl IntoIterator<Item = UnsecuredZkKey>,
    ) -> Self {
        Self {
            wallet_id: wallet_id.into(),
            wallet_keys: wallet_keys
                .into_iter()
                .map(|key| (key.to_public_key(), key))
                .collect(),
        }
    }

    #[must_use]
    pub const fn wallet_id(&self) -> &WalletId {
        &self.wallet_id
    }

    pub(crate) fn wallet_pks(&self) -> impl Iterator<Item = ZkPublicKey> + '_ {
        self.wallet_keys.keys().copied()
    }

    pub(crate) fn wallet_keys(&self) -> impl Iterator<Item = (&ZkPublicKey, &UnsecuredZkKey)> {
        self.wallet_keys.iter()
    }
}

#[derive(Debug, Clone, Eq, Error, PartialEq)]
pub enum TrackedWalletKeysError {
    #[error(
        "Multiple wallets have the same public key {public_key:?}: '{first_wallet}' and \
        '{second_wallet}'"
    )]
    DuplicatePublicKey {
        public_key: ZkPublicKey,
        first_wallet: WalletId,
        second_wallet: WalletId,
    },
}

// The notes a genesis block creates: its distribution in the ledger set, the
// service notes of its declarations in the SDP set, and the nullifiers the
// declarations spend.
#[derive(Debug, Clone, Default)]
pub struct GenesisNotes {
    pub notes: Vec<Note>,
    pub service_notes: Vec<Note>,
    pub spent: Vec<NoteNullifier>,
}

impl GenesisNotes {
    #[must_use]
    pub fn from_genesis_tx(genesis_tx: &GenesisTx) -> Self {
        Self {
            notes: genesis_tx
                .distribution()
                .iter()
                .enumerate()
                .map(|(index, allocation)| allocation.note(index))
                .collect(),
            service_notes: genesis_tx
                .sdp_declarations()
                .map(|declaration| declaration.operation().service_note())
                .collect(),
            spent: genesis_tx
                .sdp_declarations()
                .flat_map(|declaration| declaration.operation().inputs.iter().copied())
                .collect(),
        }
    }
}

// Openings of the notes the test wallets create. A block reveals only their
// commitments, so a scanner adopts an appended commitment of its keys from
// here.
static NOTE_OPENINGS: LazyLock<Mutex<HashMap<NoteCommitment, Note>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn record_note_openings(notes: impl IntoIterator<Item = Note>) {
    NOTE_OPENINGS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .extend(notes.into_iter().map(|note| (note.commitment(), note)));
}

#[must_use]
pub fn note_opening(commitment: &NoteCommitment) -> Option<Note> {
    NOTE_OPENINGS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(commitment)
        .copied()
}
