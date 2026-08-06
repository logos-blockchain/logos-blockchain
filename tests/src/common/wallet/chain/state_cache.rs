use std::collections::HashMap;

use lb_core::mantle::Utxo;

use crate::common::wallet::{WalletId, WalletUtxos};

#[derive(Debug, Default)]
/// Cache of wallet UTXO snapshots keyed by chain header.
///
/// The cache lets tests ask for wallet state at a specific node/header pair,
/// which is needed when node snapshots and wallet snapshots must line up at the
/// same tip.
pub struct WalletChainStateCache {
    utxo_snapshots: WalletUtxoSnapshots,
    header_heights: HashMap<String, HashMap<String, u64>>,
}

impl WalletChainStateCache {
    /// Record wallet UTXOs observed at `header_id`.
    pub fn record_wallets_utxos(
        &mut self,
        header_id: String,
        wallet_utxos: impl IntoIterator<Item = (WalletId, Vec<Utxo>)>,
    ) {
        self.utxo_snapshots
            .insert_many_wallet_utxos(header_id, wallet_utxos);
    }

    /// Record the height associated with a node/header observation.
    pub fn record_header_height(&mut self, node_name: &str, header_id: &str, height: u64) {
        self.header_heights
            .entry(node_name.to_owned())
            .or_default()
            .insert(header_id.to_owned(), height);
    }

    #[must_use]
    pub const fn utxo_snapshots(&self) -> &WalletUtxoSnapshots {
        &self.utxo_snapshots
    }

    #[must_use]
    pub const fn header_heights(&self) -> &HashMap<String, HashMap<String, u64>> {
        &self.header_heights
    }

    #[must_use]
    pub fn utxo_snapshot_count(&self) -> usize {
        self.utxo_snapshots.len()
    }

    #[must_use]
    pub fn header_height_node_count(&self) -> usize {
        self.header_heights.len()
    }
}

#[derive(Debug)]
/// Wallet UTXOs observed at one chain header.
pub struct WalletUtxoSnapshot {
    header_id: String,
    utxos_by_wallet: WalletUtxos,
}

impl WalletUtxoSnapshot {
    #[must_use]
    /// Create an empty snapshot for `header_id`.
    pub fn new(header_id: String) -> Self {
        Self {
            header_id,
            utxos_by_wallet: HashMap::new(),
        }
    }

    #[must_use]
    pub fn header_id(&self) -> &str {
        &self.header_id
    }

    /// Iterate wallet UTXOs without cloning them.
    pub fn iter(&self) -> impl Iterator<Item = (&WalletId, &[Utxo])> {
        self.utxos_by_wallet
            .iter()
            .map(|(wallet_id, utxos)| (wallet_id, utxos.as_slice()))
    }
}

#[derive(Debug, Default)]
/// Collection of wallet UTXO snapshots keyed by header id.
pub struct WalletUtxoSnapshots {
    by_header: HashMap<String, WalletUtxoSnapshot>,
}

impl WalletUtxoSnapshots {
    /// Add or replace wallet UTXOs for a header snapshot.
    pub fn insert_many_wallet_utxos(
        &mut self,
        header_id: String,
        wallet_utxos: impl IntoIterator<Item = (WalletId, Vec<Utxo>)>,
    ) {
        let snapshot = self
            .by_header
            .entry(header_id.clone())
            .or_insert_with(|| WalletUtxoSnapshot::new(header_id));

        snapshot.utxos_by_wallet.extend(wallet_utxos);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_header.len()
    }

    /// Iterate all stored header snapshots.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &WalletUtxoSnapshot)> {
        self.by_header.iter()
    }
}
