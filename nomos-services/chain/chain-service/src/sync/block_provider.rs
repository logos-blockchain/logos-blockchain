use std::{
    collections::{HashSet, VecDeque},
    fmt::Debug,
    marker::PhantomData,
    num::NonZeroUsize,
    ops::RangeInclusive,
};

use bytes::Bytes;
use cryptarchia_engine::{Branch, Slot};
use cryptarchia_sync::{BlocksResponse, ProviderResponse};
use futures::{StreamExt as _, TryStreamExt as _, future, stream, stream::BoxStream};
use nomos_core::{block::Block, header::HeaderId};
use nomos_storage::{StorageMsg, api::chain::StorageChainApi, backends::StorageBackend};
use overwatch::DynError;
use serde::Serialize;
use thiserror::Error;
use tokio::sync::{mpsc::Sender, oneshot};
use tracing::{error, info};

use crate::relays::StorageRelay;

const MAX_NUMBER_OF_BLOCKS: usize = 1000;

#[derive(Debug, Error)]
pub enum GetBlocksError {
    #[error("Storage channel dropped")]
    ChannelDropped,
    #[error("Block not found in storage for header {0:?}")]
    BlockNotFound(HeaderId),
    #[error("Failed to find start block")]
    StartBlockNotFound,
    #[error("Invalid state: {0}")]
    InvalidState(String),
    #[error("Failed to send to channel: {0}")]
    SendError(String),
    #[error("Failed to convert block")]
    ConversionError,
}

#[derive(Debug, Clone)]
struct BlockInfo {
    id: HeaderId,
    slot: Slot,
    location: BlockLocation,
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum BlockLocation {
    Engine,
    Storage,
}

pub struct BlockProvider<Storage, Tx>
where
    Storage: StorageBackend,
    Tx: Clone + Eq,
{
    storage_relay: StorageRelay<Storage>,
    _phantom: PhantomData<Tx>,
}

impl<Storage, Tx> BlockProvider<Storage, Tx>
where
    Storage: StorageBackend + 'static,
    <Storage as StorageChainApi>::Block: TryInto<Block<Tx>> + Into<Bytes>,
    Tx: Serialize + Clone + Eq + Send + Sync + 'static,
{
    pub const fn new(storage_relay: StorageRelay<Storage>) -> Self {
        Self {
            storage_relay,
            _phantom: PhantomData,
        }
    }

    /// Creates a block stream that leads from one of the [`known_blocks`]
    /// to the [`target_block`], and sends it to the [`reply_sender`].
    pub async fn send_blocks(
        &self,
        cryptarchia: &cryptarchia_engine::Cryptarchia<HeaderId>,
        target_block: HeaderId,
        known_blocks: &HashSet<HeaderId>,
        reply_sender: Sender<BlocksResponse>,
    ) {
        info!(
            "Providing blocks:
            target_block={target_block:?},
            known_blocks={known_blocks:?},"
        );

        match self
            .block_stream(target_block, known_blocks, cryptarchia)
            .await
        {
            Ok(stream) => {
                let response = ProviderResponse::Available(stream);
                if let Err(e) = reply_sender.send(response).await {
                    error!("Failed to send blocks stream: {e}");
                }
            }
            Err(e) => {
                Self::send_error(
                    format!("Failed to create a block stream: {e:?}"),
                    reply_sender,
                )
                .await;
            }
        }
    }

    /// Creates a block stream that leads from one of the [`known_blocks`]
    /// to the [`target_block`], in parent-to-child order.
    ///
    /// The stream includes at most [`MAX_NUMBER_OF_BLOCKS`] blocks.
    /// The stream may or may not reach the [`target_block`].
    ///
    /// In any error case, [`GetBlocksError`] is returned.
    /// If the [`target_block`] is not found in the engine and the storage,
    /// [`GetBlocksError::BlockNotFound`] is returned.
    /// If none of the [`known_blocks`] is found in the engine and the storage,
    /// [`GetBlocksError::StartBlockNotFound`] is returned.
    async fn block_stream(
        &self,
        target_block: HeaderId,
        known_blocks: &HashSet<HeaderId>,
        cryptarchia: &cryptarchia_engine::Cryptarchia<HeaderId>,
    ) -> Result<BoxStream<'static, Result<Bytes, DynError>>, DynError> {
        let path = self
            .find_path(cryptarchia, target_block, known_blocks)
            .await?;

        let stream = self.stream_blocks_from_path(path);
        Ok(stream)
    }

    /// Creates a stream from a computed path of block IDs
    fn stream_blocks_from_path(
        &self,
        path: Vec<HeaderId>,
    ) -> BoxStream<'static, Result<Bytes, DynError>> {
        let storage = self.storage_relay.clone();

        // Skip a block already known to the requester.
        let path = path.into_iter().skip(1);

        let stream = stream::iter(path)
            .then(move |id| {
                let storage = storage.clone();

                async move {
                    Self::load_block_bytes(id, &storage)
                        .await
                        .map_err(DynError::from)?
                        .ok_or_else(|| DynError::from(GetBlocksError::BlockNotFound(id)))
                }
            })
            .map_err(DynError::from)
            .take_while(|result| future::ready(result.is_ok()));

        Box::pin(stream)
    }

    /// Finds the path from one of the known blocks to the target block
    async fn find_path(
        &self,
        cryptarchia: &cryptarchia_engine::Cryptarchia<HeaderId>,
        target_block: HeaderId,
        known_blocks: &HashSet<HeaderId>,
    ) -> Result<Vec<HeaderId>, GetBlocksError> {
        let target_info = self.find_target_block(cryptarchia, target_block).await?;

        let start_info = self
            .find_optimal_start_block(cryptarchia, known_blocks, &target_info)
            .await?;

        self.compute_path_between_endpoints(cryptarchia, start_info, target_info)
            .await
    }

    /// Locates the target block and determines its slot and location
    async fn find_target_block(
        &self,
        cryptarchia: &cryptarchia_engine::Cryptarchia<HeaderId>,
        target_block: HeaderId,
    ) -> Result<BlockInfo, GetBlocksError> {
        if let Some(target_branch) = cryptarchia.branches().get(&target_block) {
            return Ok(BlockInfo {
                id: target_block,
                slot: target_branch.slot(),
                location: BlockLocation::Engine,
            });
        }

        if let Some(target_storage_block) = self.load_immutable_block(target_block).await? {
            return Ok(BlockInfo {
                id: target_block,
                slot: target_storage_block.header().slot(),
                location: BlockLocation::Storage,
            });
        }

        Err(GetBlocksError::BlockNotFound(target_block))
    }

    /// Finds the optimal starting block from the given [`known_blocks`]
    /// for building a block stream that leads to the target block
    /// indicated by [`target_info`].
    ///
    /// This function first tries to find the optimal starting block
    /// from the engine, if the target block and some of the
    /// [`known_blocks`] are present in the engine.
    /// If not, it returns the most recent block among the [`known_blocks`]
    /// stored as immutable blocks in the storage.
    ///
    /// In any error case, [`GetBlocksError`] is returned.
    /// If the [`target_block`] is not found in the engine and the storage,
    /// [`GetBlocksError::BlockNotFound`] is returned.
    /// If none of the [`known_blocks`] is found in the engine and the storage,
    /// [`GetBlocksError::StartBlockNotFound`] is returned.
    async fn find_optimal_start_block(
        &self,
        cryptarchia: &cryptarchia_engine::Cryptarchia<HeaderId>,
        known_blocks: &HashSet<HeaderId>,
        target_info: &BlockInfo,
    ) -> Result<BlockInfo, GetBlocksError> {
        if target_info.location == BlockLocation::Engine
            && let Some(start_info) =
                Self::find_optimal_start_block_from_engine(cryptarchia, known_blocks, target_info)?
        {
            return Ok(start_info);
        }

        self.find_optimal_start_block_from_storage(known_blocks)
            .await?
            .ok_or(GetBlocksError::StartBlockNotFound)
    }

    fn find_optimal_start_block_from_engine(
        cryptarchia: &cryptarchia_engine::Cryptarchia<HeaderId>,
        known_blocks: &HashSet<HeaderId>,
        target_info: &BlockInfo,
    ) -> Result<Option<BlockInfo>, GetBlocksError> {
        let target_block = cryptarchia
            .branches()
            .get(&target_info.id)
            .ok_or(GetBlocksError::BlockNotFound(target_info.id))?;

        if let Some(lca) = Self::max_lca(cryptarchia, target_block, known_blocks) {
            return Ok(Some(BlockInfo {
                id: lca.id(),
                slot: lca.slot(),
                location: BlockLocation::Engine,
            }));
        }

        Ok(None)
    }

    async fn find_optimal_start_block_from_storage(
        &self,
        known_blocks: &HashSet<HeaderId>,
    ) -> Result<Option<BlockInfo>, GetBlocksError> {
        if let Some(immutable_block) = self
            .find_max_slot_immutable_block(known_blocks.iter().copied())
            .await?
        {
            return Ok(Some(BlockInfo {
                id: immutable_block.header().id(),
                slot: immutable_block.header().slot(),
                location: BlockLocation::Storage,
            }));
        }

        // Check if the genesis block is stored as immutable
        let maybe_genesis = self.find_immutable_genesis_block().await;

        if let Ok(Some(genesis_block_id)) = &maybe_genesis
            && !known_blocks.contains(&genesis_block_id.id)
        {
            return Ok(None);
        }

        maybe_genesis
    }

    async fn find_immutable_genesis_block(&self) -> Result<Option<BlockInfo>, GetBlocksError> {
        let genesis_block_id = self
            .scan_immutable_block_ids(
                Slot::genesis()..=Slot::genesis(),
                NonZeroUsize::new(1).expect("NonZeroUsize should be > 0"),
            )
            .await?
            .into_iter()
            .next()
            .ok_or(GetBlocksError::StartBlockNotFound)?;

        Ok(Some(BlockInfo {
            id: genesis_block_id,
            slot: Slot::genesis(),
            location: BlockLocation::Storage,
        }))
    }

    async fn compute_path_between_endpoints(
        &self,
        cryptarchia: &cryptarchia_engine::Cryptarchia<HeaderId>,
        start_info: BlockInfo,
        target_info: BlockInfo,
    ) -> Result<Vec<HeaderId>, GetBlocksError> {
        // Add 1 because we remove first(known block) from the path
        let limit = (MAX_NUMBER_OF_BLOCKS + 1)
            .try_into()
            .expect("MAX_NUMBER_OF_BLOCKS should be > 0");

        match start_info.location {
            BlockLocation::Engine => {
                Self::compute_path_from_engine(cryptarchia, start_info.id, target_info.id, limit)
                    .map(Into::into)
            }

            BlockLocation::Storage => {
                self.compute_path_from_storage_and_engine(
                    cryptarchia,
                    start_info.slot,
                    target_info.id,
                    target_info.slot,
                    limit,
                )
                .await
            }
        }
    }

    fn compute_path_from_engine(
        cryptarchia: &cryptarchia_engine::Cryptarchia<HeaderId>,
        start_block: HeaderId,
        target_block: HeaderId,
        limit: NonZeroUsize,
    ) -> Result<VecDeque<HeaderId>, GetBlocksError> {
        let mut path = VecDeque::new();
        let branches = cryptarchia.branches();

        let mut current = target_block;
        loop {
            path.push_front(current);

            if path.len() > limit.get() {
                path.pop_back();
            }

            if current == start_block {
                return Ok(path);
            }

            match branches.get(&current).map(Branch::parent) {
                Some(parent) => {
                    if parent == current {
                        return Err(GetBlocksError::InvalidState(format!(
                            "Genesis block reached before reaching start_block: {start_block:?}"
                        )));
                    }
                    current = parent;
                }
                None => {
                    return Err(GetBlocksError::InvalidState(format!(
                        "Couldn't reach start_block: {start_block:?}"
                    )));
                }
            }
        }
    }

    /// Builds a list of block IDs using storage scan + engine path when needed
    async fn compute_path_from_storage_and_engine(
        &self,
        cryptarchia: &cryptarchia_engine::Cryptarchia<HeaderId>,
        start_block_slot: Slot,
        target_block: HeaderId,
        target_block_slot: Slot,
        limit: NonZeroUsize,
    ) -> Result<Vec<HeaderId>, GetBlocksError> {
        let mut storage_path = self
            .scan_immutable_block_ids(start_block_slot..=target_block_slot, limit)
            .await?;

        if let Some(last_id) = storage_path.last()
            && *last_id == target_block
        {
            return Ok(storage_path);
        }

        let Some(remaining_limit) = NonZeroUsize::new(limit.get() - storage_path.len()) else {
            // If the limit is reached, return the storage path as is
            return Ok(storage_path);
        };

        let engine_path = Self::compute_path_from_engine(
            cryptarchia,
            cryptarchia.lib(),
            target_block,
            remaining_limit,
        )?;

        storage_path.extend(engine_path.iter());

        Ok(storage_path)
    }

    fn max_lca(
        cryptarchia: &cryptarchia_engine::Cryptarchia<HeaderId>,
        target_branch: &Branch<HeaderId>,
        known_blocks: &HashSet<HeaderId>,
    ) -> Option<Branch<HeaderId>> {
        let branches = cryptarchia.branches();
        known_blocks
            .iter()
            .filter_map(|known| {
                branches
                    .get(known)
                    .map(|known_branch| branches.lca(known_branch, target_branch))
            })
            .max_by_key(Branch::length)
    }

    async fn find_max_slot_immutable_block(
        &self,
        ids: impl Iterator<Item = HeaderId>,
    ) -> Result<Option<Block<Tx>>, GetBlocksError> {
        Ok(self
            .load_immutable_blocks(ids)
            .await?
            .iter()
            .max_by_key(|block| block.header().slot())
            .cloned())
    }

    async fn load_immutable_blocks(
        &self,
        ids: impl Iterator<Item = HeaderId>,
    ) -> Result<Vec<Block<Tx>>, GetBlocksError> {
        let mut blocks = Vec::new();
        for id in ids {
            if let Some(block) = self.load_immutable_block(id).await? {
                blocks.push(block);
            }
        }

        Ok(blocks)
    }

    /// Loads an immutable block by its ID from the storage.
    /// If the block is not found, or if it is not stored as immutable,
    /// returns [`None`].
    async fn load_immutable_block(
        &self,
        id: HeaderId,
    ) -> Result<Option<Block<Tx>>, GetBlocksError> {
        let Some(block) = Self::load_block(id, &self.storage_relay).await? else {
            return Ok(None);
        };

        // Check if the block is stored as immutable in the storage
        let (tx, rx) = oneshot::channel();
        self.storage_relay
            .send(StorageMsg::get_immutable_block_id_request(
                block.header().slot(),
                tx,
            ))
            .await
            .map_err(|(e, _)| GetBlocksError::SendError(e.to_string()))?;

        match rx.await.map_err(|_| GetBlocksError::ChannelDropped)? {
            Some(_) => Ok(Some(block)),
            None => Ok(None),
        }
    }

    /// Loads a block from storage, regardless of whether it is stored as
    /// immutable or not.
    async fn load_block(
        id: HeaderId,
        storage: &StorageRelay<Storage>,
    ) -> Result<Option<Block<Tx>>, GetBlocksError> {
        let (tx, rx) = oneshot::channel();
        storage
            .send(StorageMsg::get_block_request(id, tx))
            .await
            .map_err(|(e, _)| GetBlocksError::SendError(e.to_string()))?;

        let response = rx.await.map_err(|_| GetBlocksError::ChannelDropped)?;

        match response {
            None => Ok(None),
            Some(block) => Ok(Some(
                block
                    .try_into()
                    .map_err(|_| GetBlocksError::ConversionError)?,
            )),
        }
    }

    /// Loads a block from storage as raw bytes, avoiding
    /// deserialization/serialization
    async fn load_block_bytes(
        id: HeaderId,
        storage: &StorageRelay<Storage>,
    ) -> Result<Option<Bytes>, GetBlocksError> {
        let (tx, rx) = oneshot::channel();
        storage
            .send(StorageMsg::get_block_request(id, tx))
            .await
            .map_err(|(e, _)| GetBlocksError::SendError(e.to_string()))?;

        let response = rx.await.map_err(|_| GetBlocksError::ChannelDropped)?;

        // Convert storage block type to Bytes to satisfy trait bounds.
        // For RocksBackend this is Bytes -> Bytes (no-op).
        Ok(response.map(Into::into))
    }

    /// Scans immutable block IDs from the storage,
    /// starting from the `start_slot`, limited to `limit`.
    async fn scan_immutable_block_ids(
        &self,
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
    ) -> Result<Vec<HeaderId>, GetBlocksError> {
        let (tx, rx) = oneshot::channel();

        self.storage_relay
            .send(StorageMsg::scan_immutable_block_ids_request(
                slot_range, limit, tx,
            ))
            .await
            .map_err(|(e, _)| GetBlocksError::SendError(e.to_string()))?;

        rx.await.map_err(|_| GetBlocksError::ChannelDropped)
    }

    async fn send_error(reason: String, reply_sender: Sender<BlocksResponse>) {
        error!(reason);

        if let Err(e) = reply_sender
            .send(ProviderResponse::Unavailable { reason })
            .await
            .map_err(|_| GetBlocksError::SendError("Failed to send error response".to_owned()))
        {
            error!("Failed to send error response: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, num::NonZero};

    use cryptarchia_engine::Config;
    use futures::StreamExt as _;
    use groth16::Fr;
    use key_management_system_keys::keys::Ed25519Key;
    use nomos_core::{
        codec::DeserializeOp as _,
        mantle::{Note, SignedMantleTx, ledger::Utxo},
        proofs::leader_proof::{LeaderPrivate, LeaderPublic},
        utils::merkle::MerkleNode,
    };
    use nomos_storage::{
        StorageService,
        backends::rocksdb::{RocksBackend, RocksBackendSettings},
    };
    use num_bigint::BigUint;
    use overwatch::{derive_services, overwatch::OverwatchRunner};
    use tempfile::TempDir;
    use tokio::{runtime::Handle, sync::mpsc};

    use super::*;

    #[tokio::test]
    async fn test_only_engine_path() {
        let mut env = TestEnv::new().await;

        let ids = env.setup_chain_with_blocks(10).await;
        let known_blocks = HashSet::from([ids[1], ids[3], ids[6]]);
        let target_block = *ids.last().unwrap();

        // Should start from the most recent known block (ids[6]) but skip it in result
        let expected = ids[7..=9].to_vec();
        env.retrieve_and_validate_blocks(target_block, &known_blocks, &expected)
            .await;
    }

    #[tokio::test]
    async fn test_only_storage_path() {
        let env = TestEnv::new().await;

        let ids = env.create_storage_only_blocks(8).await;

        let known_blocks = HashSet::from([ids[2]]);
        let target_block = ids[6];
        let expected = ids[3..=6].to_vec();

        env.retrieve_and_validate_blocks(target_block, &known_blocks, &expected)
            .await;
    }

    #[tokio::test]
    async fn test_storage_to_engine_hybrid_path() {
        let mut env = TestEnv::new().await;

        let storage_blocks = env.create_storage_only_blocks(5).await;
        let engine_blocks = env.create_engine_only_blocks(3).await;

        let known_blocks = HashSet::from([storage_blocks[2]]);
        let target_block = engine_blocks[1];

        let expected = vec![
            storage_blocks[3],
            storage_blocks[4],
            engine_blocks[0],
            engine_blocks[1],
        ];

        env.retrieve_and_validate_blocks(target_block, &known_blocks, &expected)
            .await;
    }

    #[tokio::test]
    async fn test_error_target_not_found() {
        let mut env = TestEnv::new().await;
        let ids = env.setup_chain_with_blocks(5).await;

        let known_blocks = HashSet::from([ids[0]]);
        let fake_target = HeaderId::from([99u8; 32]);

        env.test_error_scenario(fake_target, &known_blocks, "BlockNotFound")
            .await;
    }

    #[tokio::test]
    async fn test_error_known_blocks_not_found() {
        let mut env = TestEnv::new().await;
        let ids = env.setup_chain_with_blocks(5).await;

        let fake_known = HashSet::from([HeaderId::from([98u8; 32]), HeaderId::from([97u8; 32])]);
        let target_block = ids[4];

        env.test_error_scenario(target_block, &fake_known, "StartBlockNotFound")
            .await;
    }

    #[tokio::test]
    async fn test_error_all_blocks_nonexistent() {
        let env = TestEnv::new().await;

        let fake_target_block = HeaderId::from([88u8; 32]);
        let known_blocks = HashSet::from([HeaderId::from([99u8; 32])]);

        env.test_error_scenario(fake_target_block, &known_blocks, "BlockNotFound")
            .await;
    }

    #[tokio::test]
    async fn test_path_length_limit() {
        let mut env = TestEnv::new().await;

        let long_chain = env.setup_chain_with_blocks(MAX_NUMBER_OF_BLOCKS + 1).await;

        let known_blocks = HashSet::from([long_chain[0]]);
        let target_block = *long_chain.last().unwrap();

        let retrieved_blocks = env
            .retrieve_blocks_from_provider(target_block, &known_blocks)
            .await;

        assert_eq!(
            retrieved_blocks.len(),
            MAX_NUMBER_OF_BLOCKS,
            "Retrieved blocks should not exceed MAX_NUMBER_OF_BLOCKS minus the skipped known block"
        );
    }

    #[derive_services]
    pub struct TestServices {
        pub storage: StorageService<RocksBackend, RuntimeServiceId>,
    }

    #[expect(dead_code, reason = "Fix in a separate PR")]
    struct TestEnv {
        service: overwatch::overwatch::Overwatch<RuntimeServiceId>,
        storage_relay: StorageRelay<RocksBackend>,
        cryptarchia: cryptarchia_engine::Cryptarchia<HeaderId>,
        proof: nomos_core::proofs::leader_proof::Groth16LeaderProof,
        provider: BlockProvider<RocksBackend, SignedMantleTx>,
    }

    impl TestEnv {
        async fn new() -> Self {
            let (service, storage_relay) = Self::setup_storage().await;
            let cryptarchia = Self::new_cryptarchia(HeaderId::from([0; 32]));
            let proof = Self::make_test_proof();
            let provider = BlockProvider::new(storage_relay.clone());

            Self {
                service,
                storage_relay,
                cryptarchia,
                proof,
                provider,
            }
        }

        async fn setup_storage() -> (
            overwatch::overwatch::Overwatch<RuntimeServiceId>,
            StorageRelay<RocksBackend>,
        ) {
            let temp_path = TempDir::new().unwrap();
            let service = OverwatchRunner::<TestServices>::run(
                TestServicesServiceSettings {
                    storage: RocksBackendSettings {
                        db_path: temp_path.path().to_path_buf(),
                        read_only: false,
                        column_family: None,
                    },
                },
                Some(Handle::current()),
            )
            .unwrap();

            let service_ids = service.handle().retrieve_service_ids().await.unwrap();
            service
                .handle()
                .start_service_sequence(service_ids)
                .await
                .unwrap();

            let storage_relay = service
                .handle()
                .relay::<StorageService<RocksBackend, RuntimeServiceId>>()
                .await
                .expect("Relay connection with StorageService should succeed");

            (service, storage_relay)
        }

        fn create_block_sequence(
            &self,
            count: usize,
            slot_offset: u64,
        ) -> Vec<(Block<SignedMantleTx>, HeaderId, HeaderId, Slot)> {
            let mut blocks = Vec::new();
            let mut prev_header = HeaderId::from([0u8; 32]);

            for i in 0..count {
                let slot = Slot::from(slot_offset + i as u64);
                let block = self
                    .build_block_with_parent(prev_header, slot)
                    .expect("Failed to build block with parent");
                let header_id = block.header().id();

                blocks.push((block, header_id, prev_header, slot));
                prev_header = header_id;
            }

            blocks
        }

        async fn setup_chain_with_blocks(&mut self, count: usize) -> Vec<HeaderId> {
            let blocks = self.create_block_sequence(count, 0);
            let mut ids = Vec::new();

            for (block, header_id, prev_header, slot) in blocks {
                self.add_block(&block, header_id, prev_header, slot).await;
                ids.push(header_id);
            }

            ids
        }

        async fn create_storage_only_blocks(&self, count: usize) -> Vec<HeaderId> {
            let blocks = self.create_block_sequence(count, 0);
            let mut ids = Vec::new();

            for (block, header_id, _prev_header, slot) in blocks {
                self.store_block_in_storage(&block, header_id, slot).await;
                ids.push(header_id);
            }

            ids
        }

        async fn create_engine_only_blocks(&mut self, count: usize) -> Vec<HeaderId> {
            let blocks = self.create_block_sequence(count, 100);
            let mut ids = Vec::new();

            for (i, (block, header_id, prev_header, slot)) in blocks.iter().enumerate() {
                if i == 0 {
                    self.cryptarchia = Self::new_cryptarchia(*header_id);
                }

                if i > 0 {
                    self.cryptarchia = self
                        .cryptarchia
                        .receive_block(*header_id, *prev_header, *slot)
                        .expect("Failed to add block to cryptarchia")
                        .0;
                }

                // Store in storage but NOT as immutable storage
                self.store_block_only(block, *header_id).await;
                ids.push(*header_id);
            }

            ids
        }

        fn build_block_with_parent(
            &self,
            prev_header: HeaderId,
            slot: Slot,
        ) -> Option<Block<SignedMantleTx>> {
            let dummy_signing_key = Ed25519Key::from_bytes(&[1u8; 32]);
            Block::create(
                prev_header,
                slot,
                self.proof.clone(),
                vec![],
                &dummy_signing_key,
            )
            .ok()
        }

        async fn add_block(
            &mut self,
            block: &Block<SignedMantleTx>,
            header_id: HeaderId,
            prev_header: HeaderId,
            slot: Slot,
        ) {
            self.cryptarchia = self
                .cryptarchia
                .receive_block(header_id, prev_header, slot)
                .expect("Failed to add block to cryptarchia")
                .0;

            self.store_block_in_storage(block, header_id, slot).await;
        }

        async fn store_block_only(&self, block: &Block<SignedMantleTx>, header_id: HeaderId) {
            let store_result: Result<_, _> = block.clone().try_into();
            self.storage_relay
                .send(StorageMsg::store_block_request(
                    header_id,
                    store_result.unwrap(),
                ))
                .await
                .expect("Failed to store block");
        }

        async fn store_block_in_storage(
            &self,
            block: &Block<SignedMantleTx>,
            header_id: HeaderId,
            slot: Slot,
        ) {
            // Store block
            self.store_block_only(block, header_id).await;

            // Mark as immutable
            self.storage_relay
                .send(StorageMsg::store_immutable_block_ids_request(
                    BTreeMap::from([(slot, header_id)]),
                ))
                .await
                .expect("Failed to store immutable block id");
        }

        async fn retrieve_blocks_from_provider(
            &self,
            target_block: HeaderId,
            known_blocks: &HashSet<HeaderId>,
        ) -> Vec<HeaderId> {
            let (tx, mut rx) = mpsc::channel(1);

            self.provider
                .send_blocks(&self.cryptarchia, target_block, known_blocks, tx)
                .await;

            let mut blocks = Vec::new();
            if let Some(ProviderResponse::Available(mut stream)) = rx.recv().await {
                while let Some(res) = &stream.next().await {
                    if let Ok(bytes) = &res {
                        let block: Block<()> = Block::from_bytes(bytes).unwrap();
                        blocks.push(block.header().id());
                    } else {
                        break;
                    }
                }
            }
            blocks
        }

        async fn retrieve_and_validate_blocks(
            &self,
            target_block: HeaderId,
            known_blocks: &HashSet<HeaderId>,
            expected_blocks: &[HeaderId],
        ) {
            let retrieved_blocks = self
                .retrieve_blocks_from_provider(target_block, known_blocks)
                .await;

            assert_eq!(
                retrieved_blocks, expected_blocks,
                "Retrieved blocks should match expected blocks in order"
            );
        }

        async fn test_error_scenario(
            &self,
            target_block: HeaderId,
            known_blocks: &HashSet<HeaderId>,
            expected_error_type: &str,
        ) {
            let (tx, mut rx) = mpsc::channel(1);
            self.provider
                .send_blocks(&self.cryptarchia, target_block, known_blocks, tx)
                .await;

            let error_occurred = if let Some(response) = rx.recv().await {
                match response {
                    ProviderResponse::Unavailable { reason } => {
                        reason.contains(expected_error_type)
                    }
                    ProviderResponse::Available(mut stream) => {
                        stream.next().await.is_some_and(|result| result.is_err())
                    }
                }
            } else {
                false
            };

            assert!(
                error_occurred,
                "Expected {expected_error_type} error but operation succeeded",
            );
        }

        fn make_test_proof() -> nomos_core::proofs::leader_proof::Groth16LeaderProof {
            let public_inputs = LeaderPublic::new(
                Fr::from(1), // aged root
                Fr::from(2), // latest root
                Fr::from(3), // epoch nonce
                0,           // slot
                1000,        // total stake
            );

            let utxo = Utxo {
                tx_hash: Fr::from(BigUint::from(1u8)).into(),
                output_index: 0,
                note: Note::new(100, Fr::from(5).into()),
            };

            let aged_path = vec![MerkleNode::Right(Fr::from(0u8))];
            let latest_path = vec![MerkleNode::Left(Fr::from(0u8))];

            let private_inputs = LeaderPrivate::new(
                public_inputs,
                utxo,
                &aged_path,
                &latest_path,
                Fr::from(6), // secret key
                &Ed25519Key::from_bytes(&[1u8; 32]).public_key(),
            );

            nomos_core::proofs::leader_proof::Groth16LeaderProof::prove(
                private_inputs,
                nomos_core::mantle::ops::leader_claim::VoucherCm::default(),
            )
            .expect("Proof generation should succeed")
        }

        fn new_cryptarchia(lib: HeaderId) -> cryptarchia_engine::Cryptarchia<HeaderId> {
            <cryptarchia_engine::Cryptarchia<_>>::from_lib(
                lib,
                Config {
                    security_param: NonZero::new(1).unwrap(),
                    active_slot_coeff: 1.0,
                },
                cryptarchia_engine::State::Bootstrapping,
            )
        }
    }
}
