use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt as _};
use lb_core::{
    block::Block,
    header::HeaderId,
    mantle::{Transaction, TxHash},
    sdp::Declarations,
};
use lb_cryptarchia_engine::Slot;
use lb_storage_service::{api::chain::StorageChainApi, backends::StorageBackend};
use overwatch::DynError;
use serde::{Serialize, de::DeserializeOwned};

use crate::storage::{StorageAdapter as ChainStorageAdapter, adapters::StorageAdapter};

/// Storage operations the SDP snapshot machinery needs.
///
/// Scoped down to just what `take_sdp_snapshot` / `find_sdp_snapshot_block`
/// actually use, so tests can stand up a minimal in-memory implementation.
#[async_trait]
pub trait Storage {
    /// Walk back from `from_descendant` via parent links and yield
    /// `(HeaderId, Slot)` for each block visited. Terminates when no further
    /// parent block is known.
    async fn block_ids(
        &self,
        from_descendant: HeaderId,
    ) -> Pin<Box<dyn Stream<Item = (HeaderId, Slot)> + Send>>;

    /// Returns the SDP declarations stored alongside `block`, or `None` if
    /// the block isn't known.
    async fn sdp_declarations_at(&self, block: HeaderId) -> Result<Option<Declarations>, DynError>;
}

#[async_trait]
impl<S, Tx, RuntimeServiceId> Storage for StorageAdapter<S, Tx, RuntimeServiceId>
where
    S: StorageBackend + Send + Sync + 'static,
    <S as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>>,
    <S as StorageChainApi>::SdpDeclarations: TryFrom<Declarations> + TryInto<Declarations>,
    <S as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
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
    async fn block_ids(
        &self,
        from_descendant: HeaderId,
    ) -> Pin<Box<dyn Stream<Item = (HeaderId, Slot)> + Send>> {
        let blocks =
            <Self as ChainStorageAdapter<RuntimeServiceId>>::blocks(self, from_descendant).await;
        Box::pin(blocks.map(|block| (block.header().id(), block.header().slot())))
    }

    async fn sdp_declarations_at(&self, block: HeaderId) -> Result<Option<Declarations>, DynError> {
        <Self as ChainStorageAdapter<RuntimeServiceId>>::sdp_declarations_at(self, block).await
    }
}
