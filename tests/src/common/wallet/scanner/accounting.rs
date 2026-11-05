use std::collections::{BTreeSet, HashMap, VecDeque, hash_map::Entry};

use lb_common_http_client::ApiBlock;
use lb_core::{
    events::{BlockEvent, Event, Events},
    header::HeaderId,
    mantle::{
        Note, SignedOps, TxHash,
        note_set::{LEDGER_SET, NoteSetIndex, NoteSetUpdate, SDP_SET},
        notes::derive_note_nullifier,
        traits::Hashable as _,
    },
};
use lb_key_management_system_service::keys::{UnsecuredZkKey, ZkPublicKey};
use lb_wallet::{
    KnownNote, KnownNotes, SPEND_ROOT_DEPTH, Wallet, WalletBlock, WalletError, WalletState,
    revealed_notes,
};
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
    // the note sets as the tracked keys see them, at the applied blocks a spend
    // can name and with the openings adopted once their commitment is appended;
    // service notes are in the SDP set, so they are not spendable
    wallet: Wallet<()>,
    // the last block applied to `wallet`
    tip: HeaderId,
    // the blocks `wallet` holds, oldest first: the tip and those down to the
    // spend root
    held_blocks: VecDeque<HeaderId>,
    observed_transaction_hashes: BTreeSet<TxHash>,
}

// The genesis block as `wallet` knows it. The scanner only applies a block on
// top of the last one, so the accounting needs no id of it.
const GENESIS_ID: [u8; 32] = [0; 32];

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
        accounting.wallet = Wallet::from_genesis(
            [],
            accounting.wallet.known_notes().clone(),
            HeaderId::from(GENESIS_ID),
            &genesis.notes,
            &genesis.service_notes,
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
            wallet: Wallet::from_genesis(
                [],
                KnownNotes::new(),
                HeaderId::from(GENESIS_ID),
                &[],
                &[],
            )?,
            tip: HeaderId::from(GENESIS_ID),
            held_blocks: VecDeque::from([HeaderId::from(GENESIS_ID)]),
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

        self.apply_wallet_block(block.header.id, updates, revealed)
    }

    fn apply_wallet_block(
        &mut self,
        id: HeaderId,
        updates: Vec<NoteSetUpdate>,
        revealed: Vec<(NoteSetIndex, Note)>,
    ) -> Result<(), WalletError> {
        self.wallet.apply_block(&WalletBlock {
            id,
            parent: self.tip,
            // The test wallets do not lead, so their aged paths are not
            // rotated.
            epoch: 0.into(),
            updates,
            revealed,
        })?;
        self.tip = id;
        self.held_blocks.push_back(id);
        while self.held_blocks.len() > SPEND_ROOT_DEPTH + 1 {
            self.wallet.prune_states(self.held_blocks.pop_front());
        }
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
        let tip_state = self
            .wallet
            .wallet_state_at(self.tip)
            .expect("the accounting holds its tip");
        self.utxos_of(&tip_state)
    }

    #[must_use]
    /// Return the unspent ledger notes a spend can use, grouped by wallet id:
    /// those under the root the node wallet would name, their paths leading to
    /// it.
    pub fn spendable_utxos(&self) -> WalletUtxos {
        self.utxos_of(&self.spend_state())
    }

    /// The state a spend of the tracked wallets is built and proven against.
    #[must_use]
    pub fn spend_state(&self) -> WalletState {
        self.wallet
            .spend_state(self.tip)
            .expect("the accounting holds its tip")
    }

    fn utxos_of(&self, state: &WalletState) -> WalletUtxos {
        let mut wallet_utxos = self
            .tracked_wallets
            .iter()
            .map(|tracked_wallet| (tracked_wallet.wallet_id().clone(), Vec::new()))
            .collect::<WalletUtxos>();
        for owned in state.notes_owned_by_pks(self.public_key_to_wallet.keys()) {
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
    pub const fn known_notes(&self) -> &KnownNotes {
        self.wallet.known_notes()
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
        let nullifier = derive_note_nullifier(&note.commitment(), secret_key.as_fr());
        self.wallet.add_known_notes([KnownNote {
            note,
            note_set,
            nullifier,
        }]);
    }
}

#[cfg(test)]
mod tests {
    use lb_core::crypto::ZkHasher;
    use lb_groth16::Fr;

    use super::*;

    #[test]
    fn a_note_is_spendable_once_it_is_under_the_spend_root() {
        let key = UnsecuredZkKey::new(Fr::from(7u64));
        let wallet_id = WalletId::from("wallet".to_owned());
        let old = Note::new(100, Fr::from(1u64), key.to_public_key());
        let young = Note::new(50, Fr::from(2u64), key.to_public_key());
        let mut accounting = ScannerAccounting::new(
            vec![TrackedWalletKeys::new(wallet_id.clone(), [key])],
            &GenesisNotes {
                notes: vec![old],
                service_notes: Vec::new(),
            },
        )
        .expect("accounting should build");
        accounting.add_known_note(LEDGER_SET, young);

        let block_id = |id: u8| HeaderId::from([id; 32]);
        let values = |utxos: &WalletUtxos| {
            let mut values = utxos[&wallet_id]
                .iter()
                .map(|utxo| utxo.note.value)
                .collect::<Vec<_>>();
            values.sort_unstable();
            values
        };

        accounting
            .apply_wallet_block(
                block_id(1),
                vec![NoteSetUpdate {
                    note_set: LEDGER_SET,
                    nullifiers: Vec::new(),
                    commitments: vec![young.commitment()],
                }],
                Vec::new(),
            )
            .expect("block should apply");
        assert_eq!(values(&accounting.wallet_utxos()), [50, 100]);
        assert_eq!(values(&accounting.spendable_utxos()), [100]);

        for id in 2..=5 {
            assert_eq!(values(&accounting.spendable_utxos()), [100]);
            accounting
                .apply_wallet_block(block_id(id), Vec::new(), Vec::new())
                .expect("block should apply");
        }
        assert_eq!(values(&accounting.spendable_utxos()), [50, 100]);

        // Block 6 changes the root at the tip, the spend still names the old one.
        let root_of = |utxos: &WalletUtxos| {
            let utxo = &utxos[&wallet_id][0];
            utxo.path.root::<ZkHasher>(utxo.note.commitment().0)
        };
        let root_before = root_of(&accounting.wallet_utxos());
        let other = Note::new(1, Fr::from(3u64), ZkPublicKey::zero());
        accounting
            .apply_wallet_block(
                block_id(6),
                vec![NoteSetUpdate {
                    note_set: LEDGER_SET,
                    nullifiers: Vec::new(),
                    commitments: vec![other.commitment()],
                }],
                Vec::new(),
            )
            .expect("block should apply");
        assert_ne!(root_of(&accounting.wallet_utxos()), root_before);
        assert_eq!(root_of(&accounting.spendable_utxos()), root_before);
    }
}
