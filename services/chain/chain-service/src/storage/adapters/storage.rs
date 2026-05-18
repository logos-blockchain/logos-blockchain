use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    marker::PhantomData,
    pin::Pin,
};

use bytes::Bytes;
use futures::{Stream, StreamExt as _, stream};
use lb_core::{
    block::Block,
    codec::{DeserializeOp as _, SerializeOp as _},
    header::HeaderId,
    mantle::{Transaction, TxHash},
    sdp::Declarations,
};
use lb_cryptarchia_engine::Slot;
use lb_storage_service::{
    StorageMsg, StorageService, api::chain::StorageChainApi, backends::StorageBackend,
};
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::oneshot;

use crate::storage::StorageAdapter as StorageAdapterTrait;

pub struct StorageAdapter<Storage, Tx, RuntimeServiceId>
where
    Storage: StorageBackend + Send + Sync + 'static,
{
    pub storage_relay:
        OutboundRelay<<StorageService<Storage, RuntimeServiceId> as ServiceData>::Message>,
    _tx: PhantomData<Tx>,
}

impl<Storage, Tx, RuntimeServiceId> Clone for StorageAdapter<Storage, Tx, RuntimeServiceId>
where
    Storage: StorageBackend + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            storage_relay: self.storage_relay.clone(),
            _tx: PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<Storage, Tx, RuntimeServiceId> StorageAdapterTrait<RuntimeServiceId>
    for StorageAdapter<Storage, Tx, RuntimeServiceId>
where
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>>,
    <Storage as StorageChainApi>::SdpDeclarations: TryFrom<Declarations> + TryInto<Declarations>,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    Tx: Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + 'static
        + Transaction<Hash = TxHash>,
    RuntimeServiceId: 'static,
{
    type Backend = Storage;
    type Block = Block<Tx>;
    type SdpDeclarations = Declarations;
    type Tx = Tx;

    async fn new(
        storage_relay: OutboundRelay<
            <StorageService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self {
        Self {
            storage_relay,
            _tx: PhantomData,
        }
    }

    async fn get_block(&self, header_id: &HeaderId) -> Option<Self::Block> {
        let (sender, receiver) = oneshot::channel();

        self.storage_relay
            .send(StorageMsg::get_block_request(*header_id, sender))
            .await
            .unwrap();

        if let Ok(maybe_block) = receiver.await {
            let block = maybe_block?;
            block.try_into().ok()
        } else {
            tracing::error!("Failed to receive block from storage relay");
            None
        }
    }

    async fn store_block(
        &self,
        header_id: HeaderId,
        parent_id: HeaderId,
        block: Self::Block,
        sdp_declarations: Self::SdpDeclarations,
    ) -> Result<(), overwatch::DynError> {
        let block = block
            .try_into()
            .map_err(|_| "Failed to convert block to storage format")?;
        let sdp_declarations = sdp_declarations
            .try_into()
            .map_err(|_| "Failed to convert sdp_declarations to storage format")?;

        self.storage_relay
            .send(StorageMsg::store_block_request(
                header_id,
                parent_id,
                block,
                sdp_declarations,
            ))
            .await
            .map_err(|_| "Failed to send store block request to storage relay")?;

        Ok(())
    }

    async fn get_block_parent(&self, header_id: &HeaderId) -> Option<HeaderId> {
        let (sender, receiver) = oneshot::channel();

        self.storage_relay
            .send(StorageMsg::get_block_parent_request(*header_id, sender))
            .await
            .unwrap();

        receiver.await.unwrap_or_else(|e| {
            tracing::error!("Failed to receive block parent from storage relay: {e}");
            None
        })
    }

    async fn sdp_declarations_at(
        &self,
        header_id: HeaderId,
    ) -> Result<Option<Self::SdpDeclarations>, overwatch::DynError> {
        let (sender, receiver) = oneshot::channel();
        self.storage_relay
            .send(StorageMsg::get_sdp_declarations_request(header_id, sender))
            .await
            .unwrap();

        let Some(declarations) = receiver
            .await
            .map_err(|_| "Failed to receive SDP declarations from storage")?
        else {
            return Ok(None);
        };

        Ok(declarations
            .try_into()
            .map(Some)
            .map_err(|_| "Failed to deserialize SDP declarations from storage")?)
    }

    /// Returns a stream of [`Self::Block`]s starting from the block with
    /// `from_descendant` (inclusive) until no parent block is found.
    async fn blocks(
        &self,
        from_descendant: HeaderId,
    ) -> Pin<Box<dyn Stream<Item = Self::Block> + Send>> {
        let this = self.clone();
        Box::pin(stream::unfold(
            (this, from_descendant),
            async move |(this, id)| {
                let block = this.get_block(&id).await?;
                let parent_id = block.header().parent();
                Some((block, (this, parent_id)))
            },
        ))
    }

    async fn remove_block(
        &self,
        header_id: HeaderId,
    ) -> Result<Option<Self::Block>, overwatch::DynError> {
        let (sender, receiver) = oneshot::channel();

        self.storage_relay
            .send(StorageMsg::remove_block_request(header_id, sender))
            .await
            .map_err(|_| "Failed to send remove block request to storage relay.")?;

        let Some(removed_block) = receiver
            .await
            .map_err(|_| "No block was deleted from the storage.")?
        else {
            return Ok(None);
        };

        let deserialized_block = removed_block
            .try_into()
            .map_err(|_| "Failed to convert block to storage format.")?;

        Ok(Some(deserialized_block))
    }

    async fn store_immutable_block_ids(
        &self,
        blocks: BTreeMap<Slot, HeaderId>,
    ) -> Result<(), overwatch::DynError> {
        self.storage_relay
            .send(StorageMsg::store_immutable_block_ids_request(blocks))
            .await
            .map_err(|_| "Failed to send store_immutable_block_id request to storage relay")?;
        Ok(())
    }

    async fn store_transactions(
        &self,
        transactions: Vec<Self::Tx>,
    ) -> Result<(), overwatch::DynError> {
        let storage_transactions: HashMap<TxHash, <Storage as StorageChainApi>::Tx> = transactions
            .into_iter()
            .map(|tx| {
                let hash = tx.hash();
                Tx::to_bytes(&tx)
                    .map(|bytes| (hash, bytes.into()))
                    .map_err(|_| "Failed to convert transaction to storage format".into())
            })
            .collect::<Result<HashMap<_, _>, overwatch::DynError>>()?;

        self.storage_relay
            .send(StorageMsg::store_transactions_request(storage_transactions))
            .await
            .map_err(|_| "Failed to send store transactions batch request")?;

        Ok(())
    }

    async fn get_transactions(
        &self,
        tx_hashes: BTreeSet<TxHash>,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Tx> + Send>>, overwatch::DynError> {
        let (sender, receiver) = oneshot::channel();

        self.storage_relay
            .send(StorageMsg::get_transactions_request(tx_hashes, sender))
            .await
            .map_err(|_| "Failed to send get transactions request")?;

        let storage_stream = receiver
            .await
            .map_err(|_| "Failed to receive transactions stream from storage")?;

        let mapped_stream =
            storage_stream.filter_map(async |storage_tx| Tx::from_bytes(storage_tx.as_ref()).ok());

        Ok(Box::pin(mapped_stream))
    }

    async fn remove_transactions(&self, tx_hashes: &[TxHash]) -> Result<(), overwatch::DynError> {
        self.storage_relay
            .send(StorageMsg::remove_transactions_request(tx_hashes.to_vec()))
            .await
            .map_err(|_| "Failed to send remove transactions batch request")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use lb_core::{
        mantle::SignedMantleTx,
        sdp::{Declaration, DeclarationMessage, ServiceType},
    };
    use lb_groth16::{Field as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use lb_ledger::LedgerState;
    use lb_storage_service::backends::rocksdb::RocksBackend;
    use tokio::sync::mpsc;

    use super::*;
    use crate::{
        Cryptarchia,
        tests::{
            TestRuntimeServiceId, ledger_config, spawn_storage_service, try_build_block, utxo,
        },
    };

    type Adapter = StorageAdapter<RocksBackend, SignedMantleTx, TestRuntimeServiceId>;
    type StorageHandle = (tokio::task::JoinHandle<()>, tempfile::TempDir);

    #[tokio::test(flavor = "multi_thread")]
    async fn test_block_stream() {
        let (blocks, storage, _storage_svc) = build_chain(3).await;

        for block in &blocks {
            storage
                .store_block(
                    block.header().id(),
                    block.header().parent(),
                    block.clone(),
                    Declarations::from(HashMap::new()),
                )
                .await
                .unwrap();
        }

        let mut stream = storage.blocks(blocks.last().unwrap().header().id()).await;
        for expected in blocks.iter().rev() {
            assert_eq!(
                stream.next().await.unwrap().header().id(),
                expected.header().id(),
            );
        }
        assert!(stream.next().await.is_none());

        // Unknown starting id terminates the stream immediately.
        let unknown: HeaderId = [99; 32].into();
        assert!(storage.blocks(unknown).await.next().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_sdp_declarations_at_block() {
        let (blocks, storage, _storage_svc) = build_chain(2).await;

        let decl = DeclarationMessage {
            service_type: ServiceType::BlendNetwork,
            locators: vec![],
            provider_id: Ed25519Key::from_bytes(&[0; _]).public_key().into(),
            zk_id: ZkKey::zero().to_public_key(),
            locked_note_id: Fr::ZERO.into(),
        };
        let decls_a = Declarations::from(HashMap::new());
        let decls_b = Declarations::from_iter([(
            decl.service_type,
            HashMap::from_iter([(decl.id(), Declaration::new(0.into(), &decl))]),
        )]);
        storage
            .store_block(
                blocks[0].header().id(),
                blocks[0].header().parent(),
                blocks[0].clone(),
                decls_a.clone(),
            )
            .await
            .unwrap();
        storage
            .store_block(
                blocks[1].header().id(),
                blocks[1].header().parent(),
                blocks[1].clone(),
                decls_b.clone(),
            )
            .await
            .unwrap();

        assert_eq!(
            storage
                .sdp_declarations_at(blocks[0].header().id())
                .await
                .unwrap(),
            Some(decls_a)
        );
        assert_eq!(
            storage
                .sdp_declarations_at(blocks[1].header().id())
                .await
                .unwrap(),
            Some(decls_b)
        );

        // Unknown block id yields None
        let unknown: HeaderId = [99; 32].into();
        assert!(
            storage
                .sdp_declarations_at(unknown)
                .await
                .unwrap()
                .is_none()
        );
    }

    async fn build_chain(
        num_blocks: usize,
    ) -> (Vec<Block<SignedMantleTx>>, Adapter, StorageHandle) {
        let k = NonZero::<u32>::new(1).unwrap();
        let config = ledger_config(k);
        let (zk_key, utxo) = utxo();
        let genesis_id: HeaderId = [0; 32].into();
        let mut cryptarchia = Cryptarchia::from_lib(
            genesis_id,
            LedgerState::from_utxos([utxo], &config),
            genesis_id,
            Declarations::default(),
            config,
            lb_cryptarchia_engine::State::Online,
            Slot::genesis(),
            0,
        );

        let mut blocks = Vec::with_capacity(num_blocks);
        let mut slot = Slot::genesis() + 1;
        for _ in 0..num_blocks {
            let block = try_build_block(&cryptarchia, cryptarchia.tip(), utxo, &zk_key, slot)
                .expect("should find a winning slot");
            cryptarchia
                .try_apply_block(&block, block.header().slot())
                .unwrap();
            slot = block.header().slot() + 1;
            blocks.push(block);
        }

        let (storage_tx, storage_rx) = mpsc::channel(32);
        let storage_svc = spawn_storage_service(storage_rx);
        let storage_adapter = <Adapter as StorageAdapterTrait<TestRuntimeServiceId>>::new(
            OutboundRelay::new(storage_tx),
        )
        .await;
        (blocks, storage_adapter, storage_svc)
    }
}
