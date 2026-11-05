use std::collections::{HashMap, HashSet};

use lb_core::mantle::notes::NoteNullifier;
use lb_wallet::OwnedNote;
use serde::{Deserialize, Serialize};

use super::WalletId;

/// Numeric balance summary derived from a wallet observation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WalletBalance {
    pub output_count: usize,
    pub value: u64,
}

impl WalletBalance {
    #[must_use]
    pub fn from_utxos(utxos: &[OwnedNote]) -> Self {
        let value = utxos.iter().map(|utxo| utxo.note.value).sum();
        Self {
            output_count: utxos.len(),
            value,
        }
    }
}

/// Specifies which subset of wallet UTXOs to consider when checking or
/// reporting wallet state.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WalletOutputState {
    /// All UTXOs that are on-chain, regardless of whether they are encumbered
    /// or not.
    OnChain,
    /// UTXOs that are currently reserved by pending transactions.
    Reserved,
    /// UTXOs deep enough to spend that are not encumbered, so available for new
    /// transactions.
    Available,
}

/// Wallet state observed from tracked on-chain UTXOs plus local reservations.
#[derive(Debug, Clone)]
pub struct WalletStateView {
    wallet_id: WalletId,
    on_chain_utxos: Vec<OwnedNote>,
    spendable_utxos: Vec<OwnedNote>,
    reserved_utxos: Vec<OwnedNote>,
    available_utxos: Vec<OwnedNote>,
}

impl WalletStateView {
    #[must_use]
    pub fn new(
        wallet_id: impl Into<WalletId>,
        on_chain_utxos: Vec<OwnedNote>,
        spendable_utxos: Vec<OwnedNote>,
        reserved_utxos: Vec<OwnedNote>,
    ) -> Self {
        let available_utxos = available_utxos(&spendable_utxos, &reserved_utxos);

        Self {
            wallet_id: wallet_id.into(),
            on_chain_utxos,
            spendable_utxos,
            reserved_utxos,
            available_utxos,
        }
    }

    #[must_use]
    pub fn wallet_name(&self) -> &str {
        self.wallet_id.as_str()
    }

    #[must_use]
    pub const fn wallet_id(&self) -> &WalletId {
        &self.wallet_id
    }

    #[must_use]
    pub fn balance(&self, state: WalletOutputState) -> WalletBalance {
        match state {
            WalletOutputState::OnChain => WalletBalance::from_utxos(&self.on_chain_utxos),
            WalletOutputState::Reserved => WalletBalance::from_utxos(&self.reserved_utxos),
            WalletOutputState::Available => WalletBalance::from_utxos(&self.available_utxos),
        }
    }

    #[must_use]
    pub fn on_chain_utxos(&self) -> &[OwnedNote] {
        &self.on_chain_utxos
    }

    #[must_use]
    pub fn into_on_chain_and_spendable_utxos(self) -> (Vec<OwnedNote>, Vec<OwnedNote>) {
        (self.on_chain_utxos, self.spendable_utxos)
    }

    #[must_use]
    pub fn into_available_utxos(self) -> Vec<OwnedNote> {
        self.available_utxos
    }
}

fn available_utxos(spendable_utxos: &[OwnedNote], reserved_utxos: &[OwnedNote]) -> Vec<OwnedNote> {
    let reserved_note_ids = reserved_utxos
        .iter()
        .map(|utxo| utxo.nullifier)
        .collect::<HashSet<_>>();

    spendable_utxos
        .iter()
        .filter(|utxo| !reserved_note_ids.contains(&utxo.nullifier))
        .cloned()
        .collect()
}

/// State for one tracked wallet.
///
/// This is the wallet state that is not derived directly from one chain block:
/// the latest synced UTXOs plus local reservations for submitted transactions
/// that may not be reflected by the chain view yet.
#[derive(Debug)]
pub struct TrackedWallet {
    utxos_by_note: HashMap<NoteNullifier, OwnedNote>,
    // the ones of them a spend can use, proven to the root it names
    spendable_utxos: Vec<OwnedNote>,
    pending_state: PendingWalletState,
}

impl TrackedWallet {
    /// Create an empty tracked wallet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            utxos_by_note: HashMap::new(),
            spendable_utxos: Vec::new(),
            pending_state: PendingWalletState::default(),
        }
    }

    /// Replaces the on-chain notes, all of them spendable until
    /// [`Self::replace_spendable_utxos`] says otherwise.
    pub fn replace_on_chain_utxos(&mut self, utxos: impl IntoIterator<Item = OwnedNote>) {
        self.spendable_utxos = utxos.into_iter().collect();
        self.utxos_by_note.clear();
        self.utxos_by_note.extend(
            self.spendable_utxos
                .iter()
                .map(|utxo| (utxo.nullifier, utxo.clone())),
        );
        self.pending_state
            .retain_reserved_note_ids(&self.utxos_by_note.keys().copied().collect());
    }

    pub fn replace_spendable_utxos(&mut self, utxos: Vec<OwnedNote>) {
        self.spendable_utxos = utxos;
    }

    pub fn reserve_utxos(&mut self, reserved_utxos: Vec<OwnedNote>) {
        self.pending_state.reserve_utxos(reserved_utxos);
    }

    pub fn release_reserved_note_ids(
        &mut self,
        note_ids: &HashSet<NoteNullifier>,
    ) -> Vec<OwnedNote> {
        self.pending_state.release_reserved_note_ids(note_ids)
    }

    pub const fn record_fee_spent(&mut self, fee: u64) {
        self.pending_state.record_spent_fee(fee);
    }

    #[must_use]
    pub fn on_chain_utxos(&self) -> Vec<OwnedNote> {
        self.utxos_by_note.values().cloned().collect()
    }

    #[must_use]
    pub fn state_view(&self, wallet_id: impl Into<WalletId>) -> WalletStateView {
        let reserved_utxos = self
            .pending_state
            .reserved_utxos()
            .iter()
            .filter(|utxo| self.utxos_by_note.contains_key(&utxo.nullifier))
            .cloned()
            .collect();

        WalletStateView::new(
            wallet_id,
            self.on_chain_utxos(),
            self.spendable_utxos.clone(),
            reserved_utxos,
        )
    }

    pub fn clear_pending_state(&mut self) {
        self.pending_state = PendingWalletState::default();
    }

    #[must_use]
    pub const fn total_spent_fees(&self) -> u64 {
        self.pending_state.tracked_spent_fees()
    }

    #[must_use]
    pub const fn has_pending_state(&self) -> bool {
        self.pending_state.has_pending_state()
    }

    #[must_use]
    pub fn pending_summary(&self) -> TrackedWalletPendingSummary {
        TrackedWalletPendingSummary {
            reserved_utxos: self.pending_state.reserved_utxos().len(),
            tracked_spent_fees: self.pending_state.tracked_spent_fees(),
        }
    }

    /// Export this tracked wallet's durable read-model state.
    #[must_use]
    pub fn to_state(&self) -> TrackedWalletState {
        TrackedWalletState {
            on_chain_utxos: self.on_chain_utxos(),
            reserved_utxos: self.pending_state.reserved_utxos().to_vec(),
            tracked_spent_fees: self.pending_state.tracked_spent_fees(),
        }
    }

    /// Recreate a tracked wallet from exported read-model state.
    #[must_use]
    pub fn from_state(state: TrackedWalletState) -> Self {
        Self {
            spendable_utxos: state.on_chain_utxos.clone(),
            utxos_by_note: state
                .on_chain_utxos
                .into_iter()
                .map(|utxo| (utxo.nullifier, utxo))
                .collect(),
            pending_state: PendingWalletState {
                reserved_note_ids: state
                    .reserved_utxos
                    .iter()
                    .map(|utxo| utxo.nullifier)
                    .collect(),
                reserved_utxos: state.reserved_utxos,
                tracked_spent_fees: state.tracked_spent_fees,
            },
        }
    }
}

impl Default for TrackedWallet {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TrackedWalletPendingSummary {
    pub reserved_utxos: usize,
    pub tracked_spent_fees: u64,
}

/// Serializable state for one tracked wallet.
///
/// Contains the latest observed on-chain UTXOs plus local pending state that is
/// not directly derivable from a single block observation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackedWalletState {
    on_chain_utxos: Vec<OwnedNote>,
    reserved_utxos: Vec<OwnedNote>,
    tracked_spent_fees: u64,
}

/// Local wallet state for transactions that were submitted but are not yet
/// reflected by the chain view.
#[derive(Debug, Default, Clone)]
struct PendingWalletState {
    reserved_utxos: Vec<OwnedNote>,
    reserved_note_ids: HashSet<NoteNullifier>,
    tracked_spent_fees: u64,
}

impl PendingWalletState {
    fn reserve_utxos(&mut self, utxos: impl IntoIterator<Item = OwnedNote>) {
        for utxo in utxos {
            if self.reserved_note_ids.insert(utxo.nullifier) {
                self.reserved_utxos.push(utxo);
            }
        }
    }

    fn retain_reserved_note_ids(&mut self, on_chain_note_ids: &HashSet<NoteNullifier>) {
        self.reserved_utxos
            .retain(|utxo| on_chain_note_ids.contains(&utxo.nullifier));
        self.reserved_note_ids
            .retain(|note_id| on_chain_note_ids.contains(note_id));
    }

    fn release_reserved_note_ids(&mut self, note_ids: &HashSet<NoteNullifier>) -> Vec<OwnedNote> {
        let mut released = Vec::new();
        self.reserved_utxos.retain(|utxo| {
            if note_ids.contains(&utxo.nullifier) {
                released.push(utxo.clone());
                false
            } else {
                true
            }
        });
        self.reserved_note_ids
            .retain(|note_id| !note_ids.contains(note_id));
        released
    }

    const fn record_spent_fee(&mut self, spent_fee: u64) {
        self.tracked_spent_fees += spent_fee;
    }

    const fn has_pending_state(&self) -> bool {
        !self.reserved_utxos.is_empty() || self.tracked_spent_fees > 0
    }

    const fn tracked_spent_fees(&self) -> u64 {
        self.tracked_spent_fees
    }

    fn reserved_utxos(&self) -> &[OwnedNote] {
        &self.reserved_utxos
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{Note, note_set::LEDGER_SET};
    use lb_groth16::Fr;
    use lb_key_management_system_service::keys::ZkPublicKey;
    use lb_mmr::MerklePath;

    use super::*;

    fn utxo(value: u64, output_index: u64) -> OwnedNote {
        OwnedNote {
            note: Note::new(value, Fr::from(output_index), ZkPublicKey::zero()),
            note_set: LEDGER_SET,
            nullifier: NoteNullifier(Fr::from(output_index)),
            path: MerklePath::try_new(0, Vec::new()).expect("an empty path is bounded"),
        }
    }

    fn nullifier(utxo: &OwnedNote) -> NoteNullifier {
        utxo.nullifier
    }

    #[test]
    fn reservation_index_matches_vector_deduplication_and_chain_retention() {
        let first = utxo(10, 0);
        let second = utxo(20, 1);
        let third = utxo(30, 2);
        let mut pending = PendingWalletState::default();

        pending.reserve_utxos([first.clone(), second.clone()]);
        pending.reserve_utxos([second, third.clone()]);
        assert_eq!(
            pending
                .reserved_utxos()
                .iter()
                .map(nullifier)
                .collect::<Vec<_>>(),
            vec![first.nullifier, utxo(20, 1).nullifier, third.nullifier]
        );
        assert_eq!(pending.reserved_note_ids.len(), 3);

        pending.retain_reserved_note_ids(&HashSet::from([first.nullifier, third.nullifier]));
        assert_eq!(
            pending
                .reserved_utxos()
                .iter()
                .map(nullifier)
                .collect::<Vec<_>>(),
            vec![first.nullifier, third.nullifier]
        );
        assert_eq!(
            pending.reserved_note_ids,
            HashSet::from([first.nullifier, third.nullifier])
        );
    }
}
