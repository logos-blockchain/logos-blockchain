use std::collections::BTreeMap;

use lb_key_management_system_service::keys::ZkPublicKey;

use super::state::TrackedWalletKeys;
use crate::common::wallet::{WalletChainSourceId, WalletId, wallet_id_for_chain_source};

/// Tracked wallet keys grouped by logical chain source.
#[derive(Debug, Default, Clone)]
pub struct TrackedWalletKeysBySource {
    by_source: BTreeMap<WalletChainSourceId, Vec<TrackedWalletKeys>>,
}

impl TrackedWalletKeysBySource {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            by_source: BTreeMap::new(),
        }
    }

    pub fn add_wallet(
        &mut self,
        source_id: impl Into<WalletChainSourceId>,
        wallet_id: impl Into<WalletId>,
        wallet_pk: ZkPublicKey,
    ) {
        self.by_source
            .entry(source_id.into())
            .or_default()
            .push(TrackedWalletKeys::new(wallet_id, [wallet_pk]));
    }

    pub fn add_wallet_for_each_source(
        &mut self,
        base_wallet_id: impl AsRef<str>,
        wallet_pk: ZkPublicKey,
    ) {
        for (source_id, wallet_keys) in &mut self.by_source {
            wallet_keys.push(TrackedWalletKeys::new(
                wallet_id_for_chain_source(base_wallet_id.as_ref(), source_id),
                [wallet_pk],
            ));
        }
    }

    pub(crate) fn wallet_keys(&self) -> impl Iterator<Item = &TrackedWalletKeys> {
        self.by_source
            .values()
            .flat_map(|wallet_keys| wallet_keys.iter())
    }

    pub fn batches(&self) -> impl Iterator<Item = TrackedWalletKeysForSource> + '_ {
        self.by_source.iter().map(|(source_id, wallet_keys)| {
            TrackedWalletKeysForSource::new(source_id.clone(), wallet_keys.iter().cloned())
        })
    }
}

/// Tracked wallet keys for one logical chain source.
#[derive(Debug, Clone)]
pub struct TrackedWalletKeysForSource {
    source_id: WalletChainSourceId,
    wallet_keys: Vec<TrackedWalletKeys>,
}

impl TrackedWalletKeysForSource {
    #[must_use]
    pub(crate) fn new(
        source_id: WalletChainSourceId,
        wallet_keys: impl IntoIterator<Item = TrackedWalletKeys>,
    ) -> Self {
        Self {
            source_id,
            wallet_keys: wallet_keys.into_iter().collect(),
        }
    }

    #[must_use]
    pub(crate) fn wallet_keys(&self) -> &[TrackedWalletKeys] {
        &self.wallet_keys
    }

    #[must_use]
    pub const fn source_id(&self) -> &WalletChainSourceId {
        &self.source_id
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.wallet_keys.is_empty()
    }
}
