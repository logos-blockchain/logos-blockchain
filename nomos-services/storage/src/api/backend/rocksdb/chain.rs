use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    num::NonZeroUsize,
    ops::RangeInclusive,
    pin::Pin,
};

use async_trait::async_trait;
use bytes::Bytes;
use cryptarchia_engine::Slot;
use futures::{Stream, StreamExt as _, stream};
use nomos_core::{header::HeaderId, mantle::TxHash};
use rocksdb::WriteBatch;

use crate::{
    api::{
        backend::rocksdb::{Error, utils::key_bytes},
        chain::StorageChainApi,
    },
    backends::{StorageBackend as _, rocksdb::RocksBackend},
};

const IMMUTABLE_BLOCK_PREFIX: &str = "immutable_block/slot/";

#[async_trait]
impl StorageChainApi for RocksBackend {
    type Error = Error;
    type Block = Bytes;
    type Tx = Bytes;
    async fn get_block(&mut self, header_id: HeaderId) -> Result<Option<Self::Block>, Self::Error> {
        let header_id: [u8; 32] = header_id.into();
        let key = Bytes::copy_from_slice(&header_id);
        self.load(&key).await.map_err(Into::into)
    }

    async fn store_block(
        &mut self,
        header_id: HeaderId,
        block: Self::Block,
    ) -> Result<(), Self::Error> {
        let header_id: [u8; 32] = header_id.into();
        let key = Bytes::copy_from_slice(&header_id);
        self.store(key, block).await.map_err(Into::into)
    }

    async fn remove_block(
        &mut self,
        header_id: HeaderId,
    ) -> Result<Option<Self::Block>, Self::Error> {
        let encoded_header_id: [u8; 32] = header_id.into();
        let key = Bytes::copy_from_slice(&encoded_header_id);
        self.remove(&key).await.map_err(Into::into)
    }

    async fn store_immutable_block_ids(
        &mut self,
        ids: BTreeMap<Slot, HeaderId>,
    ) -> Result<(), Self::Error> {
        let db_transaction = self.txn(move |db| {
            let mut batch = WriteBatch::default();
            for (slot, header_id) in ids {
                // use be_bytes to keep prefix ordering
                let key = key_bytes(IMMUTABLE_BLOCK_PREFIX, slot.to_be_bytes());
                let header_id: [u8; 32] = header_id.into();
                batch.put(key, Bytes::copy_from_slice(&header_id));
            }
            db.write(batch)?;
            Ok(None)
        });
        drop(self.execute(db_transaction).await?);

        Ok(())
    }

    async fn get_immutable_block_id(
        &mut self,
        slot: Slot,
    ) -> Result<Option<HeaderId>, Self::Error> {
        // use be_bytes to keep prefix ordering
        let key = key_bytes(IMMUTABLE_BLOCK_PREFIX, slot.to_be_bytes());
        self.load(&key)
            .await?
            .map(|bytes| bytes.as_ref().try_into().map_err(Into::into))
            .transpose()
    }

    async fn scan_immutable_block_ids(
        &mut self,
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
    ) -> Result<Vec<HeaderId>, Self::Error> {
        // use be_bytes to keep prefix ordering
        let start_key = slot_range.start().to_be_bytes();
        let end_key = slot_range.end().to_be_bytes();
        let result = self
            .load_prefix(
                IMMUTABLE_BLOCK_PREFIX.as_ref(),
                Some(&start_key),
                Some(&end_key),
                Some(limit),
            )
            .await?;

        result
            .into_iter()
            .map(|bytes| bytes.as_ref().try_into().map_err(Into::into))
            .collect::<Result<Vec<HeaderId>, Error>>()
    }

    async fn store_transactions(
        &mut self,
        transactions: HashMap<TxHash, Self::Tx>,
    ) -> Result<(), Self::Error> {
        let batch_items: HashMap<Bytes, Bytes> = transactions
            .into_iter()
            .map(|(tx_hash, tx_bytes)| (tx_hash.into(), tx_bytes))
            .collect();

        self.bulk_store(batch_items).await.map_err(Into::into)
    }

    async fn get_transactions(
        &mut self,
        tx_hashes: BTreeSet<TxHash>,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Tx> + Send>>, Self::Error> {
        if tx_hashes.is_empty() {
            return Ok(Box::pin(stream::empty()));
        }

        let stream = stream::iter(tx_hashes).filter_map({
            let backend = self.clone();
            move |tx_hash| {
                let mut backend = backend.clone();
                async move {
                    let key: Bytes = tx_hash.into();
                    match backend.load(&key).await {
                        Ok(Some(tx)) => Some(tx),
                        Ok(None) => {
                            tracing::debug!("Transaction not found: {tx_hash:?}");
                            None
                        }
                        Err(e) => {
                            tracing::error!(
                                "Database error loading transaction {tx_hash:?}: {e:?}",
                            );
                            None
                        }
                    }
                }
            }
        });

        Ok(Box::pin(stream))
    }

    async fn remove_transactions(&mut self, tx_hashes: &[TxHash]) -> Result<(), Self::Error> {
        let keys: Vec<Bytes> = tx_hashes.iter().map(|&tx_hash| tx_hash.into()).collect();

        let db_transaction = self.txn(move |db| {
            let mut batch = WriteBatch::default();
            for key in keys {
                batch.delete(key);
            }
            db.write(batch)?;
            Ok(None)
        });

        drop(self.execute(db_transaction).await?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::iter;

    use futures::StreamExt as _;
    use tempfile::TempDir;

    use super::*;
    use crate::backends::rocksdb::RocksBackendSettings;

    #[tokio::test]
    async fn immutable_block_ids() {
        let temp_dir = TempDir::new().unwrap();
        let mut backend = RocksBackend::new(RocksBackendSettings {
            db_path: temp_dir.path().to_path_buf(),
            read_only: false,
            column_family: None,
        })
        .unwrap();

        // Store
        backend
            .store_immutable_block_ids(
                [(0.into(), [0u8; 32].into()), (1.into(), [1u8; 32].into())].into(),
            )
            .await
            .unwrap();

        // Get
        assert_eq!(
            backend.get_immutable_block_id(0.into()).await.unwrap(),
            Some([0u8; 32].into())
        );
        assert_eq!(
            backend.get_immutable_block_id(1.into()).await.unwrap(),
            Some([1u8; 32].into())
        );

        // Scan
        assert_eq!(
            backend
                .scan_immutable_block_ids(
                    RangeInclusive::new(0.into(), 1.into()),
                    NonZeroUsize::new(2).unwrap()
                )
                .await
                .unwrap(),
            vec![[0u8; 32].into(), [1u8; 32].into()]
        );
        assert_eq!(
            backend
                .scan_immutable_block_ids(
                    RangeInclusive::new(0.into(), 1.into()),
                    NonZeroUsize::new(1).unwrap()
                )
                .await
                .unwrap(),
            vec![[0u8; 32].into()]
        );
        assert_eq!(
            backend
                .scan_immutable_block_ids(
                    RangeInclusive::new(0.into(), 0.into()),
                    NonZeroUsize::new(2).unwrap()
                )
                .await
                .unwrap(),
            vec![[0u8; 32].into()]
        );
        assert_eq!(
            backend
                .scan_immutable_block_ids(
                    RangeInclusive::new(1.into(), 2.into()),
                    NonZeroUsize::new(2).unwrap()
                )
                .await
                .unwrap(),
            vec![[1u8; 32].into()]
        );
    }

    #[tokio::test]
    async fn test_transaction_basic_flow() {
        let temp_dir = TempDir::new().unwrap();
        let mut backend = RocksBackend::new(RocksBackendSettings {
            db_path: temp_dir.path().to_path_buf(),
            read_only: false,
            column_family: None,
        })
        .unwrap();

        let tx_hash = TxHash::default();
        let tx_bytes = Bytes::from(vec![0x01, 0x02, 0x03]);

        let mut transactions = HashMap::new();
        transactions.insert(tx_hash, tx_bytes.clone());
        backend.store_transactions(transactions).await.unwrap();

        let retrieved_stream = backend
            .get_transactions(iter::once(tx_hash).collect())
            .await
            .unwrap();
        let retrieved: Vec<_> = retrieved_stream.collect().await;
        assert_eq!(retrieved, vec![tx_bytes]);

        backend.remove_transactions(&[tx_hash]).await.unwrap();

        let empty_stream = backend
            .get_transactions(iter::once(tx_hash).collect())
            .await
            .unwrap();
        let empty: Vec<_> = empty_stream.collect().await;
        assert!(empty.is_empty());
    }
}
