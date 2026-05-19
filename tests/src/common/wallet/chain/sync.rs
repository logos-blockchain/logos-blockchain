use std::collections::BTreeMap;

use lb_common_http_client::ApiBlock;
use lb_core::mantle::Utxo;
use lb_key_management_system_service::keys::ZkPublicKey;
use thiserror::Error;

pub use super::scan::{WalletSyncRequestError, WalletSyncedOutput, WalletSyncedSpend, WalletUtxos};
use super::{
    scan::{WalletChainScan, WalletChainScanner, WalletScanRequest},
    source::WalletChainSource,
    sync_cache::WalletChainSyncHeight,
};
use crate::common::wallet::{
    TrackedWallets, WalletId, WalletSyncSourceId, wallet_id_for_sync_source,
};

#[derive(Debug, Error)]
pub enum WalletSyncError<FetchError> {
    #[error(transparent)]
    Request(WalletSyncRequestError),
    #[error(transparent)]
    FetchBlock(FetchError),
}

pub struct WalletSyncResult {
    wallet_utxos: WalletUtxos,
    synced_blocks: Vec<WalletSyncedBlock>,
}

impl WalletSyncResult {
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

#[derive(Debug, Default, Clone)]
pub struct WalletSyncRequests {
    by_source: BTreeMap<WalletSyncSourceId, Vec<WalletScanRequest>>,
}

impl WalletSyncRequests {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            by_source: BTreeMap::new(),
        }
    }

    pub fn add_wallet(
        &mut self,
        source_id: impl Into<WalletSyncSourceId>,
        wallet_id: impl Into<WalletId>,
        wallet_pk: ZkPublicKey,
    ) {
        self.by_source
            .entry(source_id.into())
            .or_default()
            .push(WalletScanRequest::new(wallet_id, [wallet_pk]));
    }

    pub fn add_wallet_for_each_source(
        &mut self,
        base_wallet_id: impl AsRef<str>,
        wallet_pk: ZkPublicKey,
    ) {
        for (source_id, requests) in &mut self.by_source {
            requests.push(WalletScanRequest::new(
                wallet_id_for_sync_source(base_wallet_id.as_ref(), source_id),
                [wallet_pk],
            ));
        }
    }

    pub(crate) fn wallet_requests(&self) -> impl Iterator<Item = &WalletScanRequest> {
        self.by_source.values().flat_map(|requests| requests.iter())
    }

    pub fn sync_batches(&self) -> impl Iterator<Item = WalletSyncBatch> + '_ {
        self.by_source.iter().map(|(source_id, requests)| {
            WalletSyncBatch::new(source_id.clone(), requests.iter().cloned())
        })
    }
}

#[derive(Debug, Clone)]
pub struct WalletSyncBatch {
    source_id: WalletSyncSourceId,
    scan_requests: Vec<WalletScanRequest>,
}

impl WalletSyncBatch {
    #[must_use]
    pub(crate) fn new(
        source_id: WalletSyncSourceId,
        scan_requests: impl IntoIterator<Item = WalletScanRequest>,
    ) -> Self {
        Self {
            source_id,
            scan_requests: scan_requests.into_iter().collect(),
        }
    }

    #[must_use]
    pub(crate) fn scan_requests(&self) -> &[WalletScanRequest] {
        &self.scan_requests
    }

    #[must_use]
    pub const fn source_id(&self) -> &WalletSyncSourceId {
        &self.source_id
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.scan_requests.is_empty()
    }
}

pub struct WalletSyncedBlock {
    source_node_name: String,
    height: WalletChainSyncHeight,
    header_id: String,
    wallet_count: usize,
    transaction_count: usize,
    synced_outputs: Vec<WalletSyncedOutput>,
    synced_spends: Vec<WalletSyncedSpend>,
}

impl WalletSyncedBlock {
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
    pub fn synced_outputs(&self) -> &[WalletSyncedOutput] {
        &self.synced_outputs
    }

    #[must_use]
    pub fn synced_spends(&self) -> &[WalletSyncedSpend] {
        &self.synced_spends
    }

    #[must_use]
    pub const fn height_value(&self) -> u64 {
        self.height.value()
    }

    #[must_use]
    pub const fn height_log_prefix(&self) -> &str {
        self.height.log_prefix()
    }
}

async fn sync_wallets_from_chain<BlockSource>(
    wallets: &mut TrackedWallets,
    source: &mut BlockSource,
    requests: &[WalletScanRequest],
    genesis_utxos: &[Utxo],
) -> Result<WalletSyncResult, WalletSyncError<BlockSource::Error>>
where
    BlockSource: WalletChainSource,
{
    wallets.ensure_wallets_from_scan_requests(requests);

    let mut sync = WalletChainSync::new(source.source_node_name(), requests)
        .map_err(WalletSyncError::Request)?;
    let mut current = source.tip();

    loop {
        let Some(block) = source
            .fetch_block(current)
            .await
            .map_err(WalletSyncError::FetchBlock)?
        else {
            sync.reached_chain_start = true;
            break;
        };

        let header_id = block.header.id.to_string();
        if sync.seed_cached_ancestor_if_complete(wallets, &header_id) {
            break;
        }

        let parent = block.header.parent_block;
        sync.tail_blocks.push(block);
        current = parent;
    }

    Ok(sync.sync_tail(wallets, genesis_utxos))
}

pub async fn sync_batch_from_chain<BlockSource>(
    wallets: &mut TrackedWallets,
    source: &mut BlockSource,
    sync_batch: &WalletSyncBatch,
    genesis_utxos: &[Utxo],
) -> Result<WalletSyncResult, WalletSyncError<BlockSource::Error>>
where
    BlockSource: WalletChainSource,
{
    sync_wallets_from_chain(wallets, source, sync_batch.scan_requests(), genesis_utxos).await
}

pub async fn sync_batches_from_chain<BlockSource>(
    wallets: &mut TrackedWallets,
    sync_batches: impl IntoIterator<Item = (WalletSyncBatch, BlockSource)>,
    genesis_utxos: &[Utxo],
) -> Result<WalletSyncResults, WalletSyncError<BlockSource::Error>>
where
    BlockSource: WalletChainSource,
{
    let mut results = Vec::new();

    for (sync_batch, mut source) in sync_batches {
        if sync_batch.is_empty() {
            continue;
        }

        results
            .push(sync_batch_from_chain(wallets, &mut source, &sync_batch, genesis_utxos).await?);
    }

    Ok(WalletSyncResults::new(results))
}

struct WalletChainSync {
    source_node_name: String,
    scanner: WalletChainScanner,
    tail_blocks: Vec<ApiBlock>,
    cached_ancestor_header_id: Option<String>,
    reached_chain_start: bool,
}

impl WalletChainSync {
    fn new(
        source_node_name: impl Into<String>,
        requests: &[WalletScanRequest],
    ) -> Result<Self, WalletSyncRequestError> {
        Ok(Self {
            source_node_name: source_node_name.into(),
            scanner: WalletChainScanner::from_requests(requests)?,
            tail_blocks: Vec::new(),
            cached_ancestor_header_id: None,
            reached_chain_start: false,
        })
    }

    fn seed_wallets_from_cached_snapshot(&mut self, wallets: &TrackedWallets, header_id: &str) {
        let cached_wallets = self
            .scanner
            .requests()
            .iter()
            .filter_map(|request| {
                wallets
                    .cached_utxos(header_id, request.wallet_name())
                    .map(|utxos| (request.wallet_id().clone(), utxos.to_vec()))
            })
            .collect::<Vec<_>>();

        for (wallet_id, cached_utxos) in cached_wallets {
            self.scanner
                .seed_wallet_from_cached_snapshot(wallet_id.as_str(), &cached_utxos);
        }
    }

    fn seed_cached_ancestor_if_complete(
        &mut self,
        wallets: &TrackedWallets,
        header_id: &str,
    ) -> bool {
        let wallet_ids = self
            .scanner
            .requests()
            .iter()
            .map(WalletScanRequest::wallet_id)
            .cloned();

        if !wallets.has_cached_wallets(header_id, wallet_ids) {
            return false;
        }

        self.seed_wallets_from_cached_snapshot(wallets, header_id);
        self.cached_ancestor_header_id = Some(header_id.to_owned());
        true
    }

    fn sync_tail(
        mut self,
        wallets: &mut TrackedWallets,
        genesis_utxos: &[Utxo],
    ) -> WalletSyncResult {
        if self.reached_chain_start {
            self.scanner.seed_genesis_utxos(genesis_utxos);
        }

        self.tail_blocks.reverse();
        let tail_blocks = std::mem::take(&mut self.tail_blocks);

        let first_uncached_height = wallets.next_chain_sync_height(
            self.cached_ancestor_header_id.as_ref(),
            &self.source_node_name,
            self.reached_chain_start,
        );
        let mut synced_blocks = Vec::with_capacity(tail_blocks.len());

        for (i, block) in tail_blocks.iter().enumerate() {
            let synced_block =
                self.sync_block(wallets, block, first_uncached_height.advance_by(i as u64));
            synced_blocks.push(synced_block);
        }

        WalletSyncResult {
            wallet_utxos: self.scanner.into_wallet_utxos(),
            synced_blocks,
        }
    }

    fn sync_block(
        &mut self,
        wallets: &mut TrackedWallets,
        block: &ApiBlock,
        height: WalletChainSyncHeight,
    ) -> WalletSyncedBlock {
        let header_id = block.header.id.to_string();
        wallets.record_header_height(&self.source_node_name, &header_id, height.value());

        let scan = self.scan_block_transactions(wallets, block);
        wallets.record_synced_wallets_utxos(
            header_id.clone(),
            self.scanner
                .wallet_utxos()
                .map(|(wallet_id, utxos)| (wallet_id.clone(), utxos)),
        );

        WalletSyncedBlock {
            source_node_name: self.source_node_name.clone(),
            height,
            header_id,
            wallet_count: self.scanner.wallet_count(),
            transaction_count: block.transactions.len(),
            synced_outputs: scan.synced_outputs,
            synced_spends: scan.synced_spends,
        }
    }

    fn scan_block_transactions(
        &mut self,
        wallets: &mut TrackedWallets,
        block: &ApiBlock,
    ) -> WalletChainScan {
        let mut synced_outputs = Vec::new();
        let mut synced_spends = Vec::new();

        for tx in &block.transactions {
            let scan = self.scanner.scan_transaction(tx);
            for spent in &scan.synced_spends {
                wallets.release_spent_note(&spent.wallet_id, spent.note_id);
            }

            synced_outputs.extend(scan.synced_outputs);
            synced_spends.extend(scan.synced_spends);
        }

        WalletChainScan {
            synced_outputs,
            synced_spends,
        }
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::Note;

    use super::*;

    fn pk(value: u8) -> ZkPublicKey {
        ZkPublicKey::new(value.into())
    }

    fn utxo(value: u64, output_index: usize, pk: ZkPublicKey) -> Utxo {
        Utxo::new([output_index as u8; 32], output_index, Note::new(value, pk))
    }

    #[test]
    fn cached_ancestor_sync_does_not_reseed_genesis_outputs() {
        let wallet_id = WalletId::from("alice");
        let request = WalletScanRequest::new(wallet_id.clone(), [pk(1)]);
        let mut wallets = TrackedWallets::default();
        wallets
            .record_synced_wallets_utxos("cached-header".to_owned(), [(wallet_id.clone(), vec![])]);

        let mut sync =
            WalletChainSync::new("node-a", &[request]).expect("sync request should be valid");
        assert!(sync.seed_cached_ancestor_if_complete(&wallets, "cached-header"));

        let result = sync.sync_tail(&mut wallets, &[utxo(10, 0, pk(1))]);
        let wallet_utxos = result.into_wallet_utxos();

        assert!(
            wallet_utxos
                .get(wallet_id.as_str())
                .expect("wallet should be present")
                .is_empty()
        );
    }

    #[test]
    fn incomplete_cached_header_does_not_seed_partial_wallet_snapshot() {
        let alice = WalletId::from("alice");
        let bob = WalletId::from("bob");
        let requests = [
            WalletScanRequest::new(alice.clone(), [pk(1)]),
            WalletScanRequest::new(bob.clone(), [pk(2)]),
        ];
        let mut wallets = TrackedWallets::default();
        wallets.record_synced_wallets_utxos(
            "partial-header".to_owned(),
            [(alice.clone(), vec![utxo(99, 9, pk(1))])],
        );

        let mut sync =
            WalletChainSync::new("node-a", &requests).expect("sync request should be valid");
        assert!(!sync.seed_cached_ancestor_if_complete(&wallets, "partial-header"));
        sync.reached_chain_start = true;

        let result = sync.sync_tail(&mut wallets, &[utxo(10, 0, pk(1)), utxo(20, 1, pk(2))]);
        let wallet_utxos = result.into_wallet_utxos();

        assert_eq!(
            wallet_utxos
                .get(alice.as_str())
                .expect("alice should be present")
                .iter()
                .map(|utxo| utxo.note.value)
                .collect::<Vec<_>>(),
            vec![10]
        );
        assert_eq!(
            wallet_utxos
                .get(bob.as_str())
                .expect("bob should be present")
                .iter()
                .map(|utxo| utxo.note.value)
                .collect::<Vec<_>>(),
            vec![20]
        );
    }
}
