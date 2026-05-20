use std::collections::{HashMap, HashSet};

use lb_core::mantle::{NoteId, Utxo};

use super::WalletId;

/// Numeric balance summary derived from a wallet observation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WalletBalance {
    pub output_count: usize,
    pub value: u64,
}

impl WalletBalance {
    #[must_use]
    pub fn from_utxos(utxos: &[Utxo]) -> Self {
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
    /// UTXOs that are not encumbered and are available for new transactions.
    Available,
}

/// Wallet state observed from tracked on-chain UTXOs plus local reservations.
#[derive(Debug, Clone)]
pub struct WalletStateView {
    wallet_id: WalletId,
    on_chain_utxos: Vec<Utxo>,
    reserved_utxos: Vec<Utxo>,
    available_utxos: Vec<Utxo>,
}

impl WalletStateView {
    #[must_use]
    pub fn new(
        wallet_id: impl Into<WalletId>,
        on_chain_utxos: Vec<Utxo>,
        reserved_utxos: Vec<Utxo>,
    ) -> Self {
        let available_utxos = available_utxos(&on_chain_utxos, &reserved_utxos);

        Self {
            wallet_id: wallet_id.into(),
            on_chain_utxos,
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
    pub fn into_available_utxos(self) -> Vec<Utxo> {
        self.available_utxos
    }
}

fn available_utxos(on_chain_utxos: &[Utxo], reserved_utxos: &[Utxo]) -> Vec<Utxo> {
    let reserved_note_ids = reserved_utxos.iter().map(Utxo::id).collect::<HashSet<_>>();

    on_chain_utxos
        .iter()
        .copied()
        .filter(|utxo| !reserved_note_ids.contains(&utxo.id()))
        .collect()
}

/// State for one tracked wallet.
///
/// This is the wallet state that is not derived directly from one chain block:
/// the latest synced UTXOs plus local reservations for submitted transactions
/// that may not be reflected by the chain view yet.
#[derive(Debug)]
pub struct TrackedWallet {
    utxos_by_note: HashMap<NoteId, Utxo>,
    pending_state: PendingWalletState,
}

impl TrackedWallet {
    /// Create an empty tracked wallet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            utxos_by_note: HashMap::new(),
            pending_state: PendingWalletState::default(),
        }
    }

    pub fn replace_on_chain_utxos(&mut self, utxos: impl IntoIterator<Item = Utxo>) {
        self.utxos_by_note.clear();
        self.utxos_by_note
            .extend(utxos.into_iter().map(|utxo| (utxo.id(), utxo)));
    }

    pub fn reserve_utxos(&mut self, reserved_utxos: Vec<Utxo>) {
        self.pending_state.reserve_utxos(reserved_utxos);
    }

    pub const fn record_fee_spent(&mut self, fee: u64) {
        self.pending_state.record_spent_fee(fee);
    }

    #[must_use]
    pub fn on_chain_utxos(&self) -> Vec<Utxo> {
        self.utxos_by_note.values().copied().collect()
    }

    #[must_use]
    pub fn state_view(&self, wallet_id: impl Into<WalletId>) -> WalletStateView {
        WalletStateView::new(
            wallet_id,
            self.on_chain_utxos(),
            self.pending_state.reserved_utxos().to_vec(),
        )
    }

    pub fn release_spent_note(&mut self, note_id: NoteId) {
        self.pending_state.release_spent_note(note_id);
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

/// Local wallet state for transactions that were submitted but are not yet
/// reflected by the chain view.
#[derive(Debug, Default, Clone)]
struct PendingWalletState {
    reserved_utxos: Vec<Utxo>,
    tracked_spent_fees: u64,
}

impl PendingWalletState {
    fn reserve_utxos(&mut self, utxos: impl IntoIterator<Item = Utxo>) {
        let mut reserved_note_ids = self
            .reserved_utxos
            .iter()
            .map(Utxo::id)
            .collect::<HashSet<_>>();
        self.reserved_utxos.extend(
            utxos
                .into_iter()
                .filter(|utxo| reserved_note_ids.insert(utxo.id())),
        );
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

    fn reserved_utxos(&self) -> &[Utxo] {
        &self.reserved_utxos
    }

    fn release_spent_note(&mut self, spent: NoteId) {
        self.reserved_utxos.retain(|utxo| utxo.id() != spent);
    }
}
