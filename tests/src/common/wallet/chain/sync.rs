pub use super::{
    feed::WalletObservedBlock as WalletSyncedBlock,
    state::{
        TrackedWalletKeysError as WalletSyncTrackedKeysError,
        WalletObservedOutput as WalletSyncedOutput, WalletObservedSpend as WalletSyncedSpend,
        WalletUtxos,
    },
    tracked_keys::{
        TrackedWalletKeysBySource as WalletSyncTrackedKeysBySource,
        TrackedWalletKeysForSource as WalletSyncTrackedKeysForSource,
    },
};

pub struct WalletSyncResult {
    wallet_utxos: WalletUtxos,
    synced_blocks: Vec<WalletSyncedBlock>,
}

impl WalletSyncResult {
    #[must_use]
    pub const fn from_parts(
        wallet_utxos: WalletUtxos,
        synced_blocks: Vec<WalletSyncedBlock>,
    ) -> Self {
        Self {
            wallet_utxos,
            synced_blocks,
        }
    }

    #[must_use]
    pub fn synced_blocks(&self) -> &[WalletSyncedBlock] {
        &self.synced_blocks
    }

    #[must_use]
    pub fn into_wallet_utxos(self) -> WalletUtxos {
        self.wallet_utxos
    }
}

pub struct WalletSyncResults {
    results: Vec<WalletSyncResult>,
}

impl WalletSyncResults {
    #[must_use]
    pub const fn new(results: Vec<WalletSyncResult>) -> Self {
        Self { results }
    }

    pub fn synced_blocks(&self) -> impl Iterator<Item = &WalletSyncedBlock> {
        self.results
            .iter()
            .flat_map(WalletSyncResult::synced_blocks)
    }

    #[must_use]
    pub fn into_wallet_utxos(self) -> WalletUtxos {
        self.results
            .into_iter()
            .flat_map(|result| result.into_wallet_utxos().into_iter())
            .collect()
    }
}
