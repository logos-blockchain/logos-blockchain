use std::collections::{BTreeMap, HashMap};

use lb_core::mantle::{NoteId, TxHash, Utxo};

use super::{
    TrackedWallet, WalletId, WalletReservedInputs, WalletStateView, WalletSyncRequests,
    chain::{
        scan::WalletScanRequest,
        sync_cache::{WalletChainSyncCache, WalletChainSyncHeight},
    },
};

/// Collection of wallets tracked from a chain view.
///
/// This is the wallet-facing entry point for code that needs synced wallet
/// state, submitted transactions, and chain sync cache state without
/// coordinating those maps directly.
#[derive(Debug, Default)]
pub struct TrackedWallets {
    wallets: HashMap<WalletId, TrackedWallet>,
    chain_sync_cache: WalletChainSyncCache<WalletId>,
    submitted_tx_hashes: HashMap<WalletId, Vec<TxHash>>,
}

impl TrackedWallets {
    pub fn record_wallet_reservation(
        &mut self,
        wallet_id: impl Into<WalletId>,
        tx_hash: TxHash,
        reserved_inputs: WalletReservedInputs,
        spent_fee: u64,
    ) -> RecordedWalletSubmission {
        let wallet_id = wallet_id.into();
        let (sender_inputs, scenario_fee_inputs) =
            reserved_inputs.into_sender_and_fee_sponsor_inputs();
        let sender_reserved_inputs = sender_inputs.clone();

        self.reserve_wallet_inputs(wallet_id.clone(), sender_inputs);
        self.record_submitted_tx_hash(wallet_id.clone(), tx_hash);
        self.record_spent_fee(wallet_id, spent_fee);

        RecordedWalletSubmission {
            sender_reserved_inputs,
            fee_sponsor_reserved_inputs: scenario_fee_inputs,
        }
    }

    pub fn clear_encumbrances(&mut self, wallet_name: &str) {
        self.clear_pending_state(wallet_name);
    }

    #[must_use]
    pub fn submitted_tx_hashes_for(&self, wallet_name: &str) -> &[TxHash] {
        self.submitted_tx_hashes
            .get(wallet_name)
            .map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn total_tracked_spent_fees(&self) -> u64 {
        self.wallets
            .values()
            .map(TrackedWallet::total_spent_fees)
            .sum()
    }

    #[must_use]
    pub fn diagnostics(&self) -> WalletDiagnostics {
        let pending_states = self
            .wallets
            .iter()
            .filter(|(_, wallet)| wallet.has_pending_state())
            .map(|(wallet_id, wallet)| {
                let state = wallet.pending_summary();
                WalletPendingStateDiagnostics {
                    wallet_id: wallet_id.clone(),
                    reserved_utxos: state.reserved_utxos,
                    tracked_spent_fees: state.tracked_spent_fees,
                }
            })
            .collect();

        let utxo_snapshots = self
            .chain_sync_cache
            .utxo_snapshots()
            .iter()
            .map(|(block_hash, snapshot)| WalletUtxoSnapshotDiagnostics {
                block_hash: block_hash.clone(),
                header_id: snapshot.header_id().to_owned(),
                non_empty_wallets: snapshot
                    .iter()
                    .filter(|(_, utxos)| !utxos.is_empty())
                    .map(|(wallet_id, utxos)| (wallet_id.clone(), utxos.len()))
                    .collect(),
            })
            .collect();

        let header_heights = self
            .chain_sync_cache
            .header_heights()
            .iter()
            .map(|(node_name, heights)| {
                let mut heights = heights.values().copied().collect::<Vec<_>>();
                heights.sort_unstable();
                (node_name.clone(), heights)
            })
            .collect();

        WalletDiagnostics {
            utxo_snapshot_count: self.utxo_snapshot_count(),
            pending_wallet_count: self.pending_wallet_count(),
            header_height_node_count: self.header_height_node_count(),
            pending_states,
            utxo_snapshots,
            header_heights,
        }
    }

    pub(crate) fn ensure_wallets_from_scan_requests(&mut self, requests: &[WalletScanRequest]) {
        for request in requests {
            self.ensure_wallet(request.wallet_id().clone());
        }
    }

    #[must_use]
    pub(crate) fn cached_utxos(&self, header_id: &str, wallet_id: &str) -> Option<&[Utxo]> {
        self.chain_sync_cache.cached_utxos(header_id, wallet_id)
    }

    #[must_use]
    pub(crate) fn has_cached_wallets(
        &self,
        header_id: &str,
        wallet_ids: impl IntoIterator<Item = WalletId>,
    ) -> bool {
        let wallet_ids = wallet_ids.into_iter().collect::<Vec<_>>();
        self.chain_sync_cache
            .has_cached_wallets(header_id, wallet_ids.iter().map(WalletId::as_str))
    }

    pub(crate) fn record_synced_wallets_utxos(
        &mut self,
        header_id: String,
        wallet_utxos: impl IntoIterator<Item = (WalletId, Vec<Utxo>)>,
    ) {
        let wallet_utxos = wallet_utxos.into_iter().collect::<Vec<_>>();

        self.chain_sync_cache
            .record_wallets_utxos(header_id, wallet_utxos.iter().cloned());

        for (wallet_id, utxos) in wallet_utxos {
            if !utxos.is_empty() {
                self.ensure_wallet(wallet_id.clone());
            }

            if let Some(wallet) = self.wallet_mut(wallet_id.as_str()) {
                wallet.replace_on_chain_utxos(utxos);
            }
        }
    }

    pub(crate) fn record_header_height(&mut self, node_name: &str, header_id: &str, height: u64) {
        self.chain_sync_cache
            .record_header_height(node_name, header_id, height);
    }

    #[must_use]
    pub(crate) fn next_chain_sync_height(
        &self,
        cached_ancestor_header_id: Option<&String>,
        node_name: &str,
        reached_chain_start: bool,
    ) -> WalletChainSyncHeight {
        self.chain_sync_cache.next_chain_sync_height(
            cached_ancestor_header_id,
            node_name,
            reached_chain_start,
        )
    }

    pub(crate) fn release_spent_note(&mut self, wallet_id: &WalletId, spent: NoteId) {
        if let Some(wallet) = self.wallet_mut(wallet_id.as_str()) {
            wallet.release_spent_note(spent);
        }
    }

    #[must_use]
    pub(crate) fn observe_synced_wallets(
        &self,
        requests: &WalletSyncRequests,
    ) -> BTreeMap<WalletId, WalletStateView> {
        let mut observations = BTreeMap::new();

        for request in requests.wallet_requests() {
            let wallet_id = request.wallet_id();
            let observation = self.observe_wallet(wallet_id);

            observations.insert(wallet_id.clone(), observation);
        }

        observations
    }

    fn ensure_wallet(&mut self, wallet_id: impl Into<WalletId>) {
        self.wallets.entry(wallet_id.into()).or_default();
    }

    fn wallet(&self, wallet_id: &str) -> Option<&TrackedWallet> {
        self.wallets.get(wallet_id)
    }

    fn wallet_mut(&mut self, wallet_id: &str) -> Option<&mut TrackedWallet> {
        self.wallets.get_mut(wallet_id)
    }

    fn observe_wallet(&self, wallet_id: &WalletId) -> WalletStateView {
        self.wallet(wallet_id.as_str()).map_or_else(
            || WalletStateView::new(wallet_id.clone(), Vec::new(), Vec::new()),
            |wallet| wallet.state_view(wallet_id.clone()),
        )
    }

    fn reserve_wallet_inputs(
        &mut self,
        wallet_id: impl Into<WalletId>,
        reserved_utxos: impl IntoIterator<Item = Utxo>,
    ) {
        let wallet_id = wallet_id.into();
        self.ensure_wallet(wallet_id.clone());
        if let Some(wallet) = self.wallet_mut(wallet_id.as_str()) {
            wallet.reserve_utxos(reserved_utxos.into_iter().collect());
        }
    }

    fn record_spent_fee(&mut self, wallet_id: impl Into<WalletId>, spent_fee: u64) {
        let wallet_id = wallet_id.into();
        if let Some(wallet) = self.wallet_mut(wallet_id.as_str()) {
            wallet.record_fee_spent(spent_fee);
        }
    }

    fn record_submitted_tx_hash(&mut self, wallet_id: WalletId, tx_hash: TxHash) {
        self.submitted_tx_hashes
            .entry(wallet_id)
            .or_default()
            .push(tx_hash);
    }

    fn clear_pending_state(&mut self, wallet_name: &str) {
        if let Some(wallet) = self.wallet_mut(wallet_name) {
            wallet.clear_pending_state();
        }
    }

    #[must_use]
    fn utxo_snapshot_count(&self) -> usize {
        self.chain_sync_cache.utxo_snapshot_count()
    }

    #[must_use]
    fn pending_wallet_count(&self) -> usize {
        self.wallets
            .values()
            .filter(|wallet| wallet.has_pending_state())
            .count()
    }

    #[must_use]
    fn header_height_node_count(&self) -> usize {
        self.chain_sync_cache.header_height_node_count()
    }
}

pub struct RecordedWalletSubmission {
    sender_reserved_inputs: Vec<Utxo>,
    fee_sponsor_reserved_inputs: Vec<Utxo>,
}

impl RecordedWalletSubmission {
    #[must_use]
    pub fn sender_reserved_inputs(&self) -> &[Utxo] {
        &self.sender_reserved_inputs
    }

    #[must_use]
    pub fn fee_sponsor_reserved_inputs(&self) -> &[Utxo] {
        &self.fee_sponsor_reserved_inputs
    }

    #[must_use]
    pub fn into_fee_sponsor_reserved_inputs(self) -> Vec<Utxo> {
        self.fee_sponsor_reserved_inputs
    }
}

pub struct WalletDiagnostics {
    pub utxo_snapshot_count: usize,
    pub pending_wallet_count: usize,
    pub header_height_node_count: usize,
    pub pending_states: Vec<WalletPendingStateDiagnostics>,
    pub utxo_snapshots: Vec<WalletUtxoSnapshotDiagnostics>,
    pub header_heights: Vec<(String, Vec<u64>)>,
}

pub struct WalletPendingStateDiagnostics {
    pub wallet_id: WalletId,
    pub reserved_utxos: usize,
    pub tracked_spent_fees: u64,
}

pub struct WalletUtxoSnapshotDiagnostics {
    pub block_hash: String,
    pub header_id: String,
    pub non_empty_wallets: Vec<(WalletId, usize)>,
}
