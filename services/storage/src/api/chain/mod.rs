pub mod requests;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    error::Error,
    fmt::Debug,
    num::NonZeroUsize,
    ops::RangeInclusive,
    pin::Pin,
};

use async_trait::async_trait;
use futures::Stream;
use lb_core::{header::HeaderId, mantle::TxHash};
use lb_cryptarchia_engine::Slot;

use crate::api::backend::rocksdb::chain::HeaderIdStream;

#[async_trait]
pub trait StorageChainApi {
    type Error: Error + Send + Sync + Debug + 'static;
    type Block: Send + Sync;
    type Tx: Send + Sync;

    async fn get_block(&mut self, header_id: HeaderId) -> Result<Option<Self::Block>, Self::Error>;

    async fn store_block(
        &mut self,
        header_id: HeaderId,
        parent_id: HeaderId,
        block: Self::Block,
    ) -> Result<(), Self::Error>;

    async fn remove_block(
        &mut self,
        header_id: HeaderId,
    ) -> Result<Option<Self::Block>, Self::Error>;

    async fn get_block_parent(
        &mut self,
        header_id: HeaderId,
    ) -> Result<Option<HeaderId>, Self::Error>;

    async fn store_immutable_block_ids(
        &mut self,
        ids: BTreeMap<Slot, HeaderId>,
    ) -> Result<(), Self::Error>;

    async fn get_immutable_block_id(&mut self, slot: Slot)
    -> Result<Option<HeaderId>, Self::Error>;

    async fn scan_immutable_block_ids(
        &mut self,
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
    ) -> Result<HeaderIdStream, Self::Error>;

    async fn scan_immutable_block_ids_reverse(
        &mut self,
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
    ) -> Result<HeaderIdStream, Self::Error>;

    async fn store_transactions(
        &mut self,
        transactions: HashMap<TxHash, Self::Tx>,
    ) -> Result<(), Self::Error>;

    async fn get_transactions(
        &mut self,
        tx_hashes: BTreeSet<TxHash>,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Tx> + Send>>, Self::Error>;

    async fn remove_transactions(&mut self, tx_hashes: &[TxHash]) -> Result<(), Self::Error>;
}
