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
    /// Return UTXOs for a node at a specific header, accepting header ids with
    /// or without a `0x` prefix.
    ///
    /// The height must have been recorded for `node_name` itself. Heights are
    /// recorded when a node's own source tracker applies the header, and only
    /// then does the header snapshot contain that node's wallets, so
    /// falling back to another node's height here would expose a snapshot
    /// that is still missing this node's wallet UTXOs.
    pub fn wallet_utxos_for_node_at_header(
        &self,
        node_name: &str,
        header_id: &str,
    ) -> Option<(String, u64, WalletUtxos)> {
        header_id_lookup_keys(header_id).find_map(|header_id| {
            let height = self.header_height(node_name, &header_id)?;
            let snapshot = self.utxo_snapshots.by_header.get(&header_id)?;
            Some((header_id.clone(), *height, snapshot.to_owned_wallet_utxos()))
        })
    }

    fn header_height(&self, node_name: &str, header_id: &str) -> Option<&u64> {
        self.header_heights.get(node_name)?.get(header_id)
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

fn header_id_lookup_keys(header_id: &str) -> impl Iterator<Item = String> + '_ {
    let without_prefix = header_id.strip_prefix("0x").unwrap_or(header_id);
    [without_prefix.to_owned(), format!("0x{without_prefix}")].into_iter()
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

    #[must_use]
    pub fn to_owned_wallet_utxos(&self) -> WalletUtxos {
        self.utxos_by_wallet.clone()
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

#[cfg(test)]
mod tests {
    use crate::common::wallet::chain::state_cache::WalletChainStateCache;

    #[test]
    fn exports_wallet_state_once_node_recorded_header_height() {
        let mut cache = WalletChainStateCache::default();
        cache.record_wallets_utxos("header-1".to_owned(), []);
        cache.record_header_height("NODE_1", "header-1", 5);

        let Some((header_id, height, _wallet_utxos)) =
            cache.wallet_utxos_for_node_at_header("NODE_1", "header-1")
        else {
            panic!("expected cached wallet state for header");
        };

        assert_eq!(header_id, "header-1");
        assert_eq!(height, 5);
    }

    #[test]
    fn does_not_export_wallet_state_using_header_height_from_another_node() {
        let mut cache = WalletChainStateCache::default();
        cache.record_wallets_utxos("header-1".to_owned(), []);
        cache.record_header_height("NODE_2", "header-1", 5);

        assert!(
            cache
                .wallet_utxos_for_node_at_header("NODE_1", "header-1")
                .is_none()
        );
    }
}
