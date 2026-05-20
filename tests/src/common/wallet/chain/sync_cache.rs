use std::{borrow::Borrow, collections::HashMap, hash::Hash};

use lb_core::mantle::Utxo;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct WalletChainSyncHeight {
    value: u64,
    accuracy: WalletChainSyncHeightAccuracy,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum WalletChainSyncHeightAccuracy {
    Exact,
    Estimated,
}

impl WalletChainSyncHeight {
    #[must_use]
    pub const fn exact(value: u64) -> Self {
        Self {
            value,
            accuracy: WalletChainSyncHeightAccuracy::Exact,
        }
    }

    #[must_use]
    pub const fn estimated(value: u64) -> Self {
        Self {
            value,
            accuracy: WalletChainSyncHeightAccuracy::Estimated,
        }
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.value
    }

    #[must_use]
    pub const fn log_prefix(self) -> &'static str {
        match self.accuracy {
            WalletChainSyncHeightAccuracy::Exact => "",
            WalletChainSyncHeightAccuracy::Estimated => "~",
        }
    }

    #[must_use]
    pub const fn advance_by(self, offset: u64) -> Self {
        Self {
            value: self.value + offset,
            accuracy: self.accuracy,
        }
    }
}

#[derive(Debug)]
pub struct WalletChainSyncCache<WalletId> {
    utxo_snapshots: WalletUtxoSnapshots<WalletId>,
    header_heights: HashMap<String, HashMap<String, u64>>,
}

impl<WalletId> Default for WalletChainSyncCache<WalletId> {
    fn default() -> Self {
        Self {
            utxo_snapshots: WalletUtxoSnapshots::default(),
            header_heights: HashMap::new(),
        }
    }
}

impl<WalletId> WalletChainSyncCache<WalletId>
where
    WalletId: Clone + Eq + Hash,
{
    #[must_use]
    pub fn cached_utxos<Q>(&self, header_id: &str, wallet_id: &Q) -> Option<&[Utxo]>
    where
        WalletId: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.utxo_snapshots.get(header_id, wallet_id)
    }

    #[must_use]
    pub fn has_cached_wallets<'a, Q>(
        &self,
        header_id: &str,
        wallet_ids: impl IntoIterator<Item = &'a Q>,
    ) -> bool
    where
        WalletId: Borrow<Q> + 'a,
        Q: Eq + Hash + ?Sized + 'a,
    {
        self.utxo_snapshots
            .contains_all_wallets(header_id, wallet_ids)
    }

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
    pub fn next_chain_sync_height(
        &self,
        cached_ancestor_header_id: Option<&String>,
        node_name: &str,
        reached_chain_start: bool,
    ) -> WalletChainSyncHeight {
        let Some(cached_header_id) = cached_ancestor_header_id else {
            return if reached_chain_start {
                WalletChainSyncHeight::exact(1)
            } else {
                WalletChainSyncHeight::estimated(1)
            };
        };

        self.header_heights
            .get(node_name)
            .and_then(|heights| heights.get(cached_header_id))
            .copied()
            .map_or(WalletChainSyncHeight::estimated(1), |height| {
                WalletChainSyncHeight::exact(height + 1)
            })
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
    #[must_use]
    pub fn get<Q>(&self, header_id: &str, wallet_id: &Q) -> Option<&[Utxo]>
    where
        WalletId: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.by_header
            .get(header_id)?
            .utxos_by_wallet
            .get(wallet_id)
            .map(Vec::as_slice)
    }

    #[must_use]
    pub fn contains_all_wallets<'a, Q>(
        &self,
        header_id: &str,
        wallet_ids: impl IntoIterator<Item = &'a Q>,
    ) -> bool
    where
        WalletId: Borrow<Q> + 'a,
        Q: Eq + Hash + ?Sized + 'a,
    {
        self.by_header.get(header_id).is_some_and(|snapshot| {
            wallet_ids
                .into_iter()
                .all(|wallet_id| snapshot.utxos_by_wallet.contains_key(wallet_id))
        })
    }

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
