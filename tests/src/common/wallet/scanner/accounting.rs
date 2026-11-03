use std::collections::{BTreeSet, HashMap, hash_map::Entry};

use lb_common_http_client::ApiBlock;
use lb_core::{
    events::{BlockEvent, Event, Events},
    mantle::{
        Note, SignedOps, TxHash,
        note_set::{LEDGER_SET, NoteSetIndex, SDP_SET},
        notes::derive_note_nullifier,
        traits::Hashable as _,
    },
};
use lb_key_management_system_service::keys::{UnsecuredZkKey, ZkPublicKey};
use lb_wallet::{KnownNote, KnownNotes, WalletBlock, WalletError, WalletState, revealed_notes};
use thiserror::Error;

use crate::common::wallet::{
    TrackedWalletKeys, TrackedWalletKeysError, WalletId, WalletUtxos,
    chain::state::{GenesisNotes, note_opening},
};

#[derive(Debug, Error)]
pub enum ScannerAccountingError {
    #[error(transparent)]
    TrackedKeys(#[from] TrackedWalletKeysError),
    #[error(transparent)]
    Wallet(#[from] WalletError),
}

#[derive(Clone)]
/// Wallet note accounting derived by replaying scanned chain blocks.
pub struct ScannerAccounting {
    tracked_wallets: Vec<TrackedWalletKeys>,
    public_key_to_wallet: HashMap<ZkPublicKey, WalletId>,
    secret_keys: HashMap<ZkPublicKey, UnsecuredZkKey>,
    // the openings of the tracked keys, adopted once their commitment is
    // appended
    known_notes: KnownNotes,
    // the note sets as the tracked keys see them; service notes are in the
    // SDP set, so they are not spendable
    state: WalletState,
    observed_transaction_hashes: BTreeSet<TxHash>,
}

impl ScannerAccounting {
    /// Build accounting for tracked wallets seeded from the genesis notes.
    pub fn new(
        tracked_wallets: Vec<TrackedWalletKeys>,
        genesis: &GenesisNotes,
    ) -> Result<Self, ScannerAccountingError> {
        let mut accounting = Self::empty(tracked_wallets)?;
        for (note_set, notes) in [
            (LEDGER_SET, &genesis.notes),
            (SDP_SET, &genesis.service_notes),
        ] {
            for note in notes {
                accounting.add_known_note(note_set, *note);
            }
        }
        // The genesis declarations spend their ledger notes.
        accounting
            .known_notes
            .retain(|_, known| !genesis.spent.contains(&known.nullifier));
        accounting.state = WalletState::from_genesis(
            &accounting.known_notes,
            &genesis.notes,
            &genesis.service_notes,
            0.into(),
        )?;

        Ok(accounting)
    }

    fn empty(tracked_wallets: Vec<TrackedWalletKeys>) -> Result<Self, ScannerAccountingError> {
        let mut public_key_to_wallet: HashMap<ZkPublicKey, WalletId> = HashMap::new();
        let mut secret_keys = HashMap::new();

        for tracked_wallet in &tracked_wallets {
            let wallet_id = tracked_wallet.wallet_id().clone();
            for pk in tracked_wallet.wallet_pks() {
                match public_key_to_wallet.entry(pk) {
                    Entry::Occupied(entry) if entry.get() != &wallet_id => {
                        return Err(TrackedWalletKeysError::DuplicatePublicKey {
                            public_key: pk,
                            first_wallet: entry.get().clone(),
                            second_wallet: wallet_id,
                        }
                        .into());
                    }
                    Entry::Occupied(_) => {}
                    Entry::Vacant(entry) => {
                        entry.insert(wallet_id.clone());
                    }
                }
            }
            secret_keys.extend(
                tracked_wallet
                    .wallet_keys()
                    .map(|(pk, key)| (*pk, key.clone())),
            );
        }

        Ok(Self {
            tracked_wallets,
            public_key_to_wallet,
            secret_keys,
            known_notes: KnownNotes::new(),
            state: WalletState::from_genesis(&KnownNotes::new(), &[], &[], 0.into())?,
            observed_transaction_hashes: BTreeSet::new(),
        })
    }

    /// Apply one block to tracked wallet state.
    pub fn apply_block(&mut self, block: &ApiBlock, events: &Events) -> Result<(), WalletError> {
        self.observe_block_transactions(block);

        // The reward notes and the service notes the block reveals, then the
        // notes the test wallets created, are adopted once appended.
        let revealed = revealed_notes(
            events,
            block.transactions.iter().flat_map(SignedOps::op_refs_iter),
        );
        for (note_set, note) in &revealed {
            self.add_known_note(*note_set, *note);
        }
        let updates = events
            .iter()
            .filter_map(|event| match event {
                Event::Block(BlockEvent::NoteSetUpdated(update)) => Some(update.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        for update in &updates {
            for commitment in &update.commitments {
                if let Some(note) = note_opening(commitment) {
                    self.add_known_note(update.note_set, note);
                }
            }
        }

        self.state = self.state.apply_block(
            &self.known_notes,
            &WalletBlock {
                id: block.header.id,
                parent: block.header.parent_block,
                // The test wallets do not lead, so their aged paths are not
                // rotated.
                epoch: self.state.epoch,
                updates,
                revealed,
            },
        )?;
        Ok(())
    }

    /// Record transaction hashes from a block without changing wallet UTXOs.
    pub fn observe_block_transactions(&mut self, block: &ApiBlock) {
        self.observed_transaction_hashes
            .extend(block.transactions.iter().map(SignedOps::hash));
    }

    #[must_use]
    /// Return currently unspent ledger notes grouped by wallet id.
    pub fn wallet_utxos(&self) -> WalletUtxos {
        let mut wallet_utxos = self
            .tracked_wallets
            .iter()
            .map(|tracked_wallet| (tracked_wallet.wallet_id().clone(), Vec::new()))
            .collect::<WalletUtxos>();
        for owned in self
            .state
            .notes_owned_by_pks(self.public_key_to_wallet.keys())
        {
            if let Some(wallet_id) = self.public_key_to_wallet.get(&owned.note.public_key) {
                wallet_utxos
                    .entry(wallet_id.clone())
                    .or_default()
                    .push(owned);
            }
        }
        wallet_utxos
    }

    #[must_use]
    pub const fn wallet_state(&self) -> &WalletState {
        &self.state
    }

    #[must_use]
    pub const fn known_notes(&self) -> &KnownNotes {
        &self.known_notes
    }

    #[must_use]
    pub const fn secret_keys(&self) -> &HashMap<ZkPublicKey, UnsecuredZkKey> {
        &self.secret_keys
    }

    #[must_use]
    /// Return all transaction hashes observed by this accounting instance.
    pub const fn observed_transaction_hashes(&self) -> &BTreeSet<TxHash> {
        &self.observed_transaction_hashes
    }

    #[must_use]
    /// Return the number of wallets tracked by this accounting instance.
    pub const fn tracked_wallet_count(&self) -> usize {
        self.tracked_wallets.len()
    }

    // An opening of a tracked key, with the nullifier its secret key derives.
    fn add_known_note(&mut self, note_set: NoteSetIndex, note: Note) {
        let Some(secret_key) = self.secret_keys.get(&note.public_key) else {
            return;
        };
        let commitment = note.commitment();
        self.known_notes.insert(
            commitment,
            KnownNote {
                note,
                note_set,
                nullifier: derive_note_nullifier(&commitment, secret_key.as_fr()),
            },
        );
    }
}
