use std::collections::HashMap;

use lb_core::mantle::Utxo;

use crate::common::wallet::{WalletId, WalletUtxos};

#[derive(Debug, Default)]
pub struct WalletChainStateCache {
    utxo_snapshots: WalletUtxoSnapshots,
    header_heights: HashMap<String, HashMap<String, u64>>,
}

impl WalletChainStateCache {
    pub fn record_wallets_utxos(
        &mut self,
        header_id: String,
        wallet_utxos: impl IntoIterator<Item = (WalletId, Vec<Utxo>)>,
    ) {
        self.utxo_snapshots
            .insert_many_wallet_utxos(header_id, wallet_utxos);
    }

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
    pub fn wallet_utxos_for_node_at_header(
        &self,
        node_name: &str,
        header_id: &str,
    ) -> Option<(String, u64, WalletUtxos)> {
        let heights = self.header_heights.get(node_name)?;
        header_id_lookup_keys(header_id).find_map(|header_id| {
            let height = heights.get(&header_id)?;
            let snapshot = self.utxo_snapshots.by_header.get(&header_id)?;
            Some((header_id.clone(), *height, snapshot.to_owned_wallet_utxos()))
        })
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
pub struct WalletUtxoSnapshot {
    header_id: String,
    utxos_by_wallet: WalletUtxos,
}

impl WalletUtxoSnapshot {
    #[must_use]
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
pub struct WalletUtxoSnapshots {
    by_header: HashMap<String, WalletUtxoSnapshot>,
}

impl WalletUtxoSnapshots {
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

    pub fn iter(&self) -> impl Iterator<Item = (&String, &WalletUtxoSnapshot)> {
        self.by_header.iter()
    }
}
