use std::{collections::HashMap, sync::Arc};

use lb_common_http_client::ApiBlock;
use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction as _, TxHash, Utxo},
};
use lb_testing_framework::{BlockFeed, BlockFeedObservation, BlockRecord};
use thiserror::Error;

use super::state::{
    ObservedWalletChanges, TrackedWalletKeys, TrackedWalletKeysError, WalletChainState,
    WalletObservedOutput, WalletObservedSpend,
};
use crate::common::wallet::{TrackedWallets, WalletUtxos};

#[derive(Debug, Error)]
pub enum WalletBlockFeedTrackerError {
    #[error("block feed has no head for wallet source `{source_node_name}`")]
    MissingSource { source_node_name: String },
    #[error(
        "block feed is missing retained block body `{header_id}` for wallet source \
        `{source_node_name}`"
    )]
    MissingBlockBody {
        source_node_name: String,
        header_id: String,
    },
    #[error(
        "block feed is missing retained height for block `{header_id}` on wallet source \
        `{source_node_name}`"
    )]
    MissingHeaderHeight {
        source_node_name: String,
        header_id: String,
    },
    #[error(
        "block feed is missing canonical header at height `{height}` for wallet source \
        `{source_node_name}`"
    )]
    MissingHeaderAtHeight {
        source_node_name: String,
        height: u64,
    },
    #[error(transparent)]
    TrackedKeys(#[from] TrackedWalletKeysError),
}

impl WalletBlockFeedTrackerError {
    #[must_use]
    pub const fn requires_direct_backfill(&self) -> bool {
        matches!(
            self,
            Self::MissingBlockBody { .. } | Self::MissingHeaderAtHeight { .. }
        )
    }
}

#[derive(Debug, Clone)]
pub struct WalletFeedTrackingBatch {
    source_node_name: String,
    wallet_keys: Vec<TrackedWalletKeys>,
}

impl WalletFeedTrackingBatch {
    #[must_use]
    pub fn new(
        source_node_name: impl Into<String>,
        wallet_keys: impl IntoIterator<Item = TrackedWalletKeys>,
    ) -> Self {
        Self {
            source_node_name: source_node_name.into(),
            wallet_keys: wallet_keys.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn source_node_name(&self) -> &str {
        &self.source_node_name
    }

    #[must_use]
    pub fn wallet_keys(&self) -> &[TrackedWalletKeys] {
        &self.wallet_keys
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.wallet_keys.is_empty()
    }

    pub(crate) fn extend_wallet_keys(
        &mut self,
        wallet_keys: impl IntoIterator<Item = TrackedWalletKeys>,
    ) {
        for wallet_keys in wallet_keys {
            self.add_wallet_keys(wallet_keys);
        }
    }

    fn add_wallet_keys(&mut self, wallet_keys: TrackedWalletKeys) {
        for existing in &mut self.wallet_keys {
            if existing.extend_if_same_wallet(&wallet_keys) {
                return;
            }
        }

        self.wallet_keys.push(wallet_keys);
    }
}

#[derive(Debug, Default)]
pub struct WalletFeedTrackingResult {
    backfill_batches: Vec<WalletFeedTrackingBatch>,
}

impl WalletFeedTrackingResult {
    #[must_use]
    pub fn backfill_batches(&self) -> &[WalletFeedTrackingBatch] {
        &self.backfill_batches
    }

    #[must_use]
    pub const fn needs_backfill(&self) -> bool {
        !self.backfill_batches.is_empty()
    }

    fn add_backfill_batch(&mut self, batch: WalletFeedTrackingBatch) {
        if let Some(existing) = self.backfill_batch_mut(batch.source_node_name()) {
            existing.extend_wallet_keys(batch.wallet_keys);

            return;
        }

        self.backfill_batches.push(batch);
    }

    fn backfill_batch_mut(
        &mut self,
        source_node_name: &str,
    ) -> Option<&mut WalletFeedTrackingBatch> {
        self.backfill_batches
            .iter_mut()
            .find(|batch| batch.source_node_name() == source_node_name)
    }
}

pub struct WalletObservedBlock {
    source_node_name: String,
    height: u64,
    header_id: String,
    wallet_count: usize,
    transaction_count: usize,
    transaction_hashes: Vec<TxHash>,
    observed_outputs: Vec<WalletObservedOutput>,
    observed_spends: Vec<WalletObservedSpend>,
}

impl WalletObservedBlock {
    #[must_use]
    pub fn source_node_name(&self) -> &str {
        &self.source_node_name
    }

    #[must_use]
    pub fn header_id(&self) -> &str {
        &self.header_id
    }

    #[must_use]
    pub const fn wallet_count(&self) -> usize {
        self.wallet_count
    }

    #[must_use]
    pub const fn transaction_count(&self) -> usize {
        self.transaction_count
    }

    #[must_use]
    pub fn transaction_hashes(&self) -> &[TxHash] {
        &self.transaction_hashes
    }

    #[must_use]
    pub fn observed_outputs(&self) -> &[WalletObservedOutput] {
        &self.observed_outputs
    }

    #[must_use]
    pub fn observed_spends(&self) -> &[WalletObservedSpend] {
        &self.observed_spends
    }

    #[must_use]
    pub const fn height(&self) -> u64 {
        self.height
    }
}

/// Observed wallet state for one feed source after applying retained blocks.
pub struct WalletFeedStateResult {
    wallet_utxos: WalletUtxos,
    observed_blocks: Vec<WalletObservedBlock>,
}

impl WalletFeedStateResult {
    #[must_use]
    pub(crate) const fn from_parts(
        wallet_utxos: WalletUtxos,
        observed_blocks: Vec<WalletObservedBlock>,
    ) -> Self {
        Self {
            wallet_utxos,
            observed_blocks,
        }
    }

    #[must_use]
    pub fn observed_blocks(&self) -> &[WalletObservedBlock] {
        &self.observed_blocks
    }

    #[must_use]
    pub fn into_wallet_utxos(self) -> WalletUtxos {
        self.wallet_utxos
    }
}

/// Observed wallet state grouped across multiple feed sources.
pub struct WalletFeedStateResults {
    results: Vec<WalletFeedStateResult>,
}

impl WalletFeedStateResults {
    #[must_use]
    pub const fn new(results: Vec<WalletFeedStateResult>) -> Self {
        Self { results }
    }

    pub fn observed_blocks(&self) -> impl Iterator<Item = &WalletObservedBlock> {
        self.results
            .iter()
            .flat_map(WalletFeedStateResult::observed_blocks)
    }

    #[must_use]
    pub fn into_wallet_utxos(self) -> WalletUtxos {
        self.results
            .into_iter()
            .flat_map(|result| result.into_wallet_utxos().into_iter())
            .collect()
    }
}

/// Tracks wallet state by applying observed block-feed data.
///
/// This type is intentionally limited to feed-backed tracking: register wallets
/// to observe, apply retained feed blocks to those wallets, and expose the
/// latest observed UTXOs per source. Legacy compatibility and direct chain
/// queries live outside this tracker.
#[derive(Default)]
pub struct WalletBlockFeedTracker {
    source_trackers: HashMap<String, WalletSourceTracker>,
    block_bodies_by_header: HashMap<HeaderId, Arc<ApiBlock>>,
}

impl WalletBlockFeedTracker {
    /// Start or expand wallet tracking for the given feed sources.
    pub fn track_wallets(
        &mut self,
        wallets: &mut TrackedWallets,
        tracking_batches: &[WalletFeedTrackingBatch],
        genesis_utxos: &[Utxo],
    ) -> Result<WalletFeedTrackingResult, WalletBlockFeedTrackerError> {
        let mut tracking = WalletFeedTrackingResult::default();

        for batch in tracking_batches {
            if batch.is_empty() {
                continue;
            }

            wallets.ensure_wallets_from_tracked_keys(batch.wallet_keys());
            if let WalletSourceTracking::NeedsBackfill(batch) = self.ensure_source_tracker(
                batch.source_node_name(),
                batch.wallet_keys(),
                genesis_utxos,
            )? {
                tracking.add_backfill_batch(batch);
            }
        }

        Ok(tracking)
    }

    /// Apply all newly observed canonical blocks from the feed.
    pub fn apply_feed(
        &mut self,
        wallets: &mut TrackedWallets,
        feed: &BlockFeed,
        genesis_utxos: &[Utxo],
    ) -> Result<Vec<WalletObservedBlock>, WalletBlockFeedTrackerError> {
        self.remember_block_bodies(&feed.history());

        let Some(observation) = feed.latest_observation() else {
            return Ok(Vec::new());
        };

        let view = WalletFeedChainView::new(observation, &self.block_bodies_by_header);
        Self::advance_source_trackers(wallets, &mut self.source_trackers, &view, genesis_utxos)
    }

    /// Read the current wallet state for one already tracked feed source.
    pub fn observed_wallet_utxos(
        &self,
        source_node_name: &str,
    ) -> Result<WalletUtxos, WalletBlockFeedTrackerError> {
        let source_tracker = self.source_trackers.get(source_node_name).ok_or_else(|| {
            WalletBlockFeedTrackerError::MissingSource {
                source_node_name: source_node_name.to_owned(),
            }
        })?;

        Ok(source_tracker.wallet_utxos())
    }

    /// Replace one source tracker from an externally built chain snapshot.
    pub fn replace_source_state(
        &mut self,
        source_node_name: impl Into<String>,
        wallet_keys: &[TrackedWalletKeys],
        wallet_utxos: WalletUtxos,
        applied_tip: HeaderId,
        applied_height: u64,
    ) -> Result<(), WalletBlockFeedTrackerError> {
        self.source_trackers.insert(
            source_node_name.into(),
            WalletSourceTracker::from_wallet_utxos(
                wallet_keys,
                wallet_utxos,
                applied_tip,
                applied_height,
            )?,
        );

        Ok(())
    }

    fn ensure_source_tracker(
        &mut self,
        source_node_name: &str,
        wallet_keys: &[TrackedWalletKeys],
        genesis_utxos: &[Utxo],
    ) -> Result<WalletSourceTracking, WalletBlockFeedTrackerError> {
        let Some(source_tracker) = self.source_trackers.get(source_node_name) else {
            self.source_trackers.insert(
                source_node_name.to_owned(),
                WalletSourceTracker::new(wallet_keys, genesis_utxos)?,
            );

            return Ok(WalletSourceTracking::Ready);
        };

        if source_tracker.covers_wallet_keys(wallet_keys) {
            return Ok(WalletSourceTracking::Ready);
        }

        let merged_wallet_keys = source_tracker.merged_wallet_keys(wallet_keys);
        if source_tracker.has_applied_blocks() {
            return Ok(WalletSourceTracking::NeedsBackfill(
                WalletFeedTrackingBatch::new(source_node_name, merged_wallet_keys),
            ));
        }

        self.source_trackers.insert(
            source_node_name.to_owned(),
            WalletSourceTracker::new(&merged_wallet_keys, genesis_utxos)?,
        );

        Ok(WalletSourceTracking::Ready)
    }

    fn advance_source_trackers(
        wallets: &mut TrackedWallets,
        source_trackers: &mut HashMap<String, WalletSourceTracker>,
        view: &WalletFeedChainView<'_>,
        genesis_utxos: &[Utxo],
    ) -> Result<Vec<WalletObservedBlock>, WalletBlockFeedTrackerError> {
        let source_names = source_trackers.keys().cloned().collect::<Vec<_>>();
        let mut observed_blocks = Vec::new();

        for source_node_name in source_names {
            let Some(source_tracker) = source_trackers.get_mut(&source_node_name) else {
                continue;
            };

            if !view.has_source(&source_node_name) {
                continue;
            }

            observed_blocks.extend(source_tracker.advance_to_observed_tip(
                wallets,
                view,
                &source_node_name,
                genesis_utxos,
            )?);
        }

        Ok(observed_blocks)
    }

    fn remember_block_bodies(&mut self, history: &[Arc<BlockRecord>]) {
        for record in history {
            for event in &record.events {
                self.block_bodies_by_header
                    .entry(event.header)
                    .or_insert_with(|| Arc::clone(&event.block));
            }
        }
    }
}

enum WalletSourceTracking {
    Ready,
    NeedsBackfill(WalletFeedTrackingBatch),
}

struct WalletSourceTracker {
    chain_state: WalletChainState,
    applied_tip: Option<HeaderId>,
    applied_height: Option<u64>,
}

impl WalletSourceTracker {
    fn new(
        wallet_keys: &[TrackedWalletKeys],
        genesis_utxos: &[Utxo],
    ) -> Result<Self, TrackedWalletKeysError> {
        let mut chain_state = WalletChainState::from_tracked_wallets(wallet_keys)?;
        chain_state.seed_genesis_utxos(genesis_utxos);

        Ok(Self {
            chain_state,
            applied_tip: None,
            applied_height: None,
        })
    }

    fn from_wallet_utxos(
        wallet_keys: &[TrackedWalletKeys],
        wallet_utxos: WalletUtxos,
        applied_tip: HeaderId,
        applied_height: u64,
    ) -> Result<Self, TrackedWalletKeysError> {
        Ok(Self {
            chain_state: WalletChainState::from_wallet_utxos(wallet_keys, wallet_utxos)?,
            applied_tip: Some(applied_tip),
            applied_height: Some(applied_height),
        })
    }

    fn covers_wallet_keys(&self, wallet_keys: &[TrackedWalletKeys]) -> bool {
        self.chain_state.covers_tracked_wallets(wallet_keys)
    }

    const fn has_applied_blocks(&self) -> bool {
        self.applied_tip.is_some() || self.applied_height.is_some()
    }

    fn merged_wallet_keys(&self, wallet_keys: &[TrackedWalletKeys]) -> Vec<TrackedWalletKeys> {
        self.wallet_keys()
            .iter()
            .chain(wallet_keys.iter())
            .cloned()
            .collect()
    }

    fn wallet_keys(&self) -> &[TrackedWalletKeys] {
        self.chain_state.tracked_wallets()
    }

    fn wallet_utxos(&self) -> WalletUtxos {
        self.chain_state
            .wallet_utxos()
            .map(|(wallet_id, utxos)| (wallet_id.clone(), utxos))
            .collect()
    }

    fn advance_to_observed_tip(
        &mut self,
        wallets: &mut TrackedWallets,
        view: &WalletFeedChainView<'_>,
        source_node_name: &str,
        genesis_utxos: &[Utxo],
    ) -> Result<Vec<WalletObservedBlock>, WalletBlockFeedTrackerError> {
        let tip_height = view.source_tip_height(source_node_name)?;
        let start_height = self.next_unapplied_height(view, source_node_name, tip_height)?;
        if start_height > tip_height {
            return Ok(Vec::new());
        }

        if start_height == 1 && self.applied_tip.is_some() {
            self.rebuild(genesis_utxos)?;
        }

        let observed_blocks = view.observed_blocks(source_node_name, start_height, tip_height)?;

        Ok(observed_blocks
            .into_iter()
            .map(|observed_block| {
                self.apply_block(
                    wallets,
                    source_node_name,
                    observed_block.block.as_ref(),
                    observed_block.height,
                )
            })
            .collect())
    }

    fn next_unapplied_height(
        &self,
        view: &WalletFeedChainView<'_>,
        source_node_name: &str,
        tip_height: u64,
    ) -> Result<u64, WalletBlockFeedTrackerError> {
        let (Some(applied_tip), Some(applied_height)) = (self.applied_tip, self.applied_height)
        else {
            return Ok(1);
        };

        if applied_height > tip_height {
            // An external snapshot can seed tracking slightly ahead of the
            // next feed observation. Wait for the feed to catch up instead of
            // rebuilding from an older retained window.
            return Ok(tip_height + 1);
        }

        let canonical_header = view.header_at_height(source_node_name, applied_height)?;
        if canonical_header == applied_tip {
            Ok(applied_height + 1)
        } else {
            Ok(1)
        }
    }

    fn rebuild(&mut self, genesis_utxos: &[Utxo]) -> Result<(), TrackedWalletKeysError> {
        let wallet_keys = self.wallet_keys().to_vec();
        *self = Self::new(&wallet_keys, genesis_utxos)?;
        Ok(())
    }

    fn apply_block(
        &mut self,
        wallets: &mut TrackedWallets,
        source_node_name: &str,
        block: &ApiBlock,
        height: u64,
    ) -> WalletObservedBlock {
        let header_id = block.header.id.to_string();
        wallets.record_header_height(source_node_name, &header_id, height);

        let update = self.apply_block_transactions(wallets, block);
        wallets.record_observed_wallets_utxos(
            header_id.clone(),
            self.chain_state
                .wallet_utxos()
                .map(|(wallet_id, utxos)| (wallet_id.clone(), utxos)),
        );
        self.applied_tip = Some(block.header.id);
        self.applied_height = Some(height);

        WalletObservedBlock {
            source_node_name: source_node_name.to_owned(),
            height,
            header_id,
            wallet_count: self.chain_state.wallet_count(),
            transaction_count: block.transactions.len(),
            transaction_hashes: block
                .transactions
                .iter()
                .map(SignedMantleTx::hash)
                .collect(),
            observed_outputs: update.observed_outputs,
            observed_spends: update.observed_spends,
        }
    }

    fn apply_block_transactions(
        &mut self,
        wallets: &mut TrackedWallets,
        block: &ApiBlock,
    ) -> ObservedWalletChanges {
        let mut observed_outputs = Vec::new();
        let mut observed_spends = Vec::new();

        for tx in &block.transactions {
            let update = self.apply_transaction(wallets, tx);
            observed_outputs.extend(update.observed_outputs);
            observed_spends.extend(update.observed_spends);
        }

        ObservedWalletChanges {
            observed_outputs,
            observed_spends,
        }
    }

    fn apply_transaction(
        &mut self,
        wallets: &mut TrackedWallets,
        tx: &SignedMantleTx,
    ) -> ObservedWalletChanges {
        let update = self.chain_state.apply_transaction(tx);

        for spent in &update.observed_spends {
            wallets.release_spent_note(&spent.wallet_id, spent.note_id);
        }

        update
    }
}

struct WalletFeedChainView<'a> {
    observation: BlockFeedObservation,
    block_bodies_by_header: &'a HashMap<HeaderId, Arc<ApiBlock>>,
}

struct ObservedWalletBlock {
    height: u64,
    block: Arc<ApiBlock>,
}

impl<'a> WalletFeedChainView<'a> {
    const fn new(
        observation: BlockFeedObservation,
        block_bodies_by_header: &'a HashMap<HeaderId, Arc<ApiBlock>>,
    ) -> Self {
        Self {
            observation,
            block_bodies_by_header,
        }
    }

    fn has_source(&self, source_node_name: &str) -> bool {
        self.observation.node_head(source_node_name).is_some()
    }

    fn source_tip_height(
        &self,
        source_node_name: &str,
    ) -> Result<u64, WalletBlockFeedTrackerError> {
        self.observation
            .node(source_node_name)
            .ok_or_else(|| WalletBlockFeedTrackerError::MissingSource {
                source_node_name: source_node_name.to_owned(),
            })?
            .tip_height()
            .ok_or_else(|| WalletBlockFeedTrackerError::MissingHeaderHeight {
                source_node_name: source_node_name.to_owned(),
                header_id: "source tip".to_owned(),
            })
    }

    fn header_at_height(
        &self,
        source_node_name: &str,
        height: u64,
    ) -> Result<HeaderId, WalletBlockFeedTrackerError> {
        self.observation
            .node(source_node_name)
            .ok_or_else(|| WalletBlockFeedTrackerError::MissingSource {
                source_node_name: source_node_name.to_owned(),
            })?
            .header_at_height(height)
            .ok_or_else(|| WalletBlockFeedTrackerError::MissingHeaderAtHeight {
                source_node_name: source_node_name.to_owned(),
                height,
            })
    }

    fn block(
        &self,
        source_node_name: &str,
        header: &HeaderId,
    ) -> Result<Arc<ApiBlock>, WalletBlockFeedTrackerError> {
        self.block_bodies_by_header
            .get(header)
            .cloned()
            .ok_or_else(|| WalletBlockFeedTrackerError::MissingBlockBody {
                source_node_name: source_node_name.to_owned(),
                header_id: header.to_string(),
            })
    }

    fn observed_blocks(
        &self,
        source_node_name: &str,
        start_height: u64,
        tip_height: u64,
    ) -> Result<Vec<ObservedWalletBlock>, WalletBlockFeedTrackerError> {
        let mut blocks = Vec::new();

        for height in start_height..=tip_height {
            let header_id = self.header_at_height(source_node_name, height)?;
            let block = self.block(source_node_name, &header_id)?;
            blocks.push(ObservedWalletBlock { height, block });
        }

        Ok(blocks)
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::Note;
    use lb_key_management_system_service::keys::ZkPublicKey;

    use super::*;
    use crate::common::wallet::WalletId;

    const SOURCE_NODE: &str = "NODE_1";

    fn pk(value: u8) -> ZkPublicKey {
        ZkPublicKey::new(value.into())
    }

    fn utxo(value: u64, output_index: usize, pk: ZkPublicKey) -> Utxo {
        Utxo::new([output_index as u8; 32], output_index, Note::new(value, pk))
    }

    fn wallet_keys(wallet_id: &str, pk: ZkPublicKey) -> TrackedWalletKeys {
        TrackedWalletKeys::new(wallet_id, [pk])
    }

    fn tracking_batch(
        source_node_name: &str,
        wallet_keys: impl IntoIterator<Item = TrackedWalletKeys>,
    ) -> WalletFeedTrackingBatch {
        WalletFeedTrackingBatch::new(source_node_name, wallet_keys)
    }

    #[test]
    fn expanding_applied_source_requires_backfill_without_dropping_existing_state() {
        let alice_utxo = utxo(10, 0, pk(1));
        let alice_keys = wallet_keys("alice", pk(1));
        let bob_keys = wallet_keys("bob", pk(2));
        let initial_batch = tracking_batch(SOURCE_NODE, [alice_keys.clone()]);

        let mut wallets = TrackedWallets::default();
        let mut tracker = WalletBlockFeedTracker::default();
        let tracking = tracker
            .track_wallets(&mut wallets, &[initial_batch], &[alice_utxo])
            .expect("initial wallet tracking should succeed");

        assert!(!tracking.needs_backfill());

        tracker
            .replace_source_state(
                SOURCE_NODE,
                &[alice_keys],
                WalletUtxos::from([(WalletId::from("alice"), vec![alice_utxo])]),
                HeaderId::from([1; 32]),
                1,
            )
            .expect("applied source state should be accepted");

        let expanded_batch = tracking_batch(SOURCE_NODE, [bob_keys]);
        let tracking = tracker
            .track_wallets(&mut wallets, &[expanded_batch], &[alice_utxo])
            .expect("expanded wallet tracking should succeed");

        assert!(tracking.needs_backfill());
        assert_eq!(tracking.backfill_batches().len(), 1);
        assert_eq!(tracking.backfill_batches()[0].wallet_keys().len(), 2);

        let observed_utxos = tracker
            .observed_wallet_utxos(SOURCE_NODE)
            .expect("source state should still exist");

        assert_eq!(
            observed_utxos
                .get("alice")
                .expect("alice should remain observed"),
            &vec![alice_utxo],
        );
    }

    #[test]
    fn expanding_applied_source_merges_backfill_keys_by_source() {
        let alice_utxo = utxo(10, 0, pk(1));
        let alice_keys = wallet_keys("alice", pk(1));
        let bob_keys = wallet_keys("bob", pk(2));
        let carol_keys = wallet_keys("carol", pk(3));
        let initial_batch = tracking_batch(SOURCE_NODE, [alice_keys.clone()]);

        let mut wallets = TrackedWallets::default();
        let mut tracker = WalletBlockFeedTracker::default();
        tracker
            .track_wallets(&mut wallets, &[initial_batch], &[alice_utxo])
            .expect("initial wallet tracking should succeed");

        tracker
            .replace_source_state(
                SOURCE_NODE,
                &[alice_keys],
                WalletUtxos::from([(WalletId::from("alice"), vec![alice_utxo])]),
                HeaderId::from([1; 32]),
                1,
            )
            .expect("applied source state should be accepted");

        let bob_batch = tracking_batch(SOURCE_NODE, [bob_keys]);
        let carol_batch = tracking_batch(SOURCE_NODE, [carol_keys]);
        let tracking = tracker
            .track_wallets(&mut wallets, &[bob_batch, carol_batch], &[alice_utxo])
            .expect("expanded wallet tracking should succeed");

        assert!(tracking.needs_backfill());
        assert_eq!(tracking.backfill_batches().len(), 1);
        assert_eq!(tracking.backfill_batches()[0].wallet_keys().len(), 3);
    }
}
