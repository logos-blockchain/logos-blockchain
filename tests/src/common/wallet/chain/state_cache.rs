use std::{collections::HashMap, hash::Hash};

use lb_core::mantle::Utxo;

#[derive(Debug)]
pub struct WalletChainStateCache<WalletId> {
    utxo_snapshots: WalletUtxoSnapshots<WalletId>,
    header_heights: HashMap<String, HashMap<String, u64>>,
}

impl<WalletId> Default for WalletChainStateCache<WalletId> {
    fn default() -> Self {
        Self {
            utxo_snapshots: WalletUtxoSnapshots::default(),
            header_heights: HashMap::new(),
        }
    }
}

impl<WalletId> WalletChainStateCache<WalletId>
where
    WalletId: Clone + Eq + Hash,
{
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
    pub const fn utxo_snapshots(&self) -> &WalletUtxoSnapshots<WalletId> {
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
pub struct WalletUtxoSnapshot<WalletId> {
    header_id: String,
    utxos_by_wallet: HashMap<WalletId, Vec<Utxo>>,
}

impl<WalletId> WalletUtxoSnapshot<WalletId>
where
    WalletId: Eq + Hash,
{
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
}

#[derive(Debug)]
pub struct WalletUtxoSnapshots<WalletId> {
    by_header: HashMap<String, WalletUtxoSnapshot<WalletId>>,
}

impl<WalletId> Default for WalletUtxoSnapshots<WalletId> {
    fn default() -> Self {
        Self {
            by_header: HashMap::new(),
        }
    }
}

impl<WalletId> WalletUtxoSnapshots<WalletId>
where
    WalletId: Clone + Eq + Hash,
{
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

    pub fn iter(&self) -> impl Iterator<Item = (&String, &WalletUtxoSnapshot<WalletId>)> {
        self.by_header.iter()
    }
}
