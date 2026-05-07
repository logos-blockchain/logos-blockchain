use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    num::NonZeroUsize,
    ops::RangeInclusive,
};

use async_trait::async_trait;
use bytes::Bytes;
use lb_core::{
    block::{BlockNumber, SessionNumber},
    header::HeaderId,
    sdp::{Locator, ProviderId, ServiceType},
};
use lb_cryptarchia_engine::Slot;
use overwatch::DynError;
use thiserror::Error;

use super::{StorageBackend, StorageTransaction};
use crate::api::{StorageBackendApi, chain::StorageChainApi, membership::StorageMembershipApi};

#[derive(Debug, Error)]
#[error("Errors in MockStorage should not happen")]
pub enum MockStorageError {}

pub type MockStorageTransaction = Box<dyn Fn(&mut HashMap<Bytes, Bytes>) + Send + Sync>;

impl StorageTransaction for MockStorageTransaction {
    type Result = ();
    type Transaction = Self;
}

//
pub struct MockStorage {
    inner: HashMap<Bytes, Bytes>,
}

impl core::fmt::Debug for MockStorage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        format!("MockStorage {{ inner: {:?} }}", self.inner).fmt(f)
    }
}

#[async_trait]
impl StorageBackend for MockStorage {
    type Settings = ();
    type Error = MockStorageError;
    type Transaction = MockStorageTransaction;

    fn new(_config: Self::Settings) -> Result<Self, <Self as StorageBackend>::Error> {
        Ok(Self {
            inner: HashMap::new(),
        })
    }

    async fn store(
        &mut self,
        key: Bytes,
        value: Bytes,
    ) -> Result<(), <Self as StorageBackend>::Error> {
        let _ = self.inner.insert(key, value);
        Ok(())
    }

    async fn bulk_store<I>(&mut self, items: I) -> Result<(), <Self as StorageBackend>::Error>
    where
        I: IntoIterator<Item = (Bytes, Bytes)> + Send + 'static,
    {
        for (key, value) in items {
            let _ = self.inner.insert(key, value);
        }
        Ok(())
    }

    async fn load(&mut self, key: &[u8]) -> Result<Option<Bytes>, <Self as StorageBackend>::Error> {
        Ok(self.inner.get(key).cloned())
    }

    async fn load_prefix(
        &mut self,
        _key: &[u8],
        _start_key: Option<&[u8]>,
        _end_key: Option<&[u8]>,
        _limit: Option<NonZeroUsize>,
    ) -> Result<Vec<Bytes>, <Self as StorageBackend>::Error> {
        unimplemented!()
    }

    async fn remove(
        &mut self,
        key: &[u8],
    ) -> Result<Option<Bytes>, <Self as StorageBackend>::Error> {
        Ok(self.inner.remove(key))
    }

    async fn execute(
        &mut self,
        transaction: Self::Transaction,
    ) -> Result<(), <Self as StorageBackend>::Error> {
        transaction(&mut self.inner);
        Ok(())
    }
}

#[async_trait]
impl StorageChainApi for MockStorage {
    type Error = MockStorageError;
    type Block = Bytes;

    async fn get_block(
        &mut self,
        _header_id: HeaderId,
    ) -> Result<Option<Self::Block>, Self::Error> {
        unimplemented!()
    }

    async fn store_block(
        &mut self,
        _header_id: HeaderId,
        _parent_id: HeaderId,
        _block: Self::Block,
    ) -> Result<(), Self::Error> {
        unimplemented!()
    }

    async fn remove_block(
        &mut self,
        _header_id: HeaderId,
    ) -> Result<Option<Self::Block>, Self::Error> {
        unimplemented!()
    }

    async fn get_block_parent(
        &mut self,
        _header_id: HeaderId,
    ) -> Result<Option<HeaderId>, Self::Error> {
        unimplemented!()
    }

    async fn store_immutable_block_ids(
        &mut self,
        _ids: BTreeMap<Slot, HeaderId>,
    ) -> Result<(), Self::Error> {
        unimplemented!()
    }

    async fn get_immutable_block_id(
        &mut self,
        _slot: Slot,
    ) -> Result<Option<HeaderId>, Self::Error> {
        unimplemented!()
    }

    async fn scan_immutable_block_ids(
        &mut self,
        _slot_range: RangeInclusive<Slot>,
        _limit: NonZeroUsize,
    ) -> Result<Vec<HeaderId>, Self::Error> {
        unimplemented!()
    }
}

#[async_trait]
impl StorageBackendApi for MockStorage {}

#[async_trait]
impl StorageMembershipApi for MockStorage {
    async fn save_active_session(
        &mut self,
        _service_type: ServiceType,
        _session_id: SessionNumber,
        _providers: &HashMap<ProviderId, BTreeSet<Locator>>,
    ) -> Result<(), DynError> {
        unimplemented!()
    }

    async fn load_active_session(
        &mut self,
        _service_type: ServiceType,
    ) -> Result<Option<(SessionNumber, HashMap<ProviderId, BTreeSet<Locator>>)>, DynError> {
        unimplemented!()
    }

    async fn save_latest_block(&mut self, _block_number: BlockNumber) -> Result<(), DynError> {
        unimplemented!()
    }

    async fn load_latest_block(&mut self) -> Result<Option<BlockNumber>, DynError> {
        unimplemented!()
    }

    async fn save_next_session(
        &mut self,
        _service_type: ServiceType,
        _session_id: SessionNumber,
        _providers: &HashMap<ProviderId, BTreeSet<Locator>>,
    ) -> Result<(), DynError> {
        unimplemented!()
    }

    async fn load_next_session(
        &mut self,
        _service_type: ServiceType,
    ) -> Result<Option<(SessionNumber, HashMap<ProviderId, BTreeSet<Locator>>)>, DynError> {
        unimplemented!()
    }
}
