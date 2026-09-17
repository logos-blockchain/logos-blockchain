pub mod backend;
pub mod chain;
mod storage_api;
pub use storage_api::StorageApi;

#[cfg(all(test, feature = "rocksdb-backend"))]
mod tests {
    use std::collections::BTreeMap;

    use bytes::Bytes;
    use lb_core::header::HeaderId;
    use tempfile::TempDir;
    use tokio::sync::oneshot;

    use crate::{
        StorageServiceError,
        api::chain::requests::ChainApiRequest,
        backends::{
            StorageBackend as _,
            rocksdb::{RocksBackend, RocksBackendSettings},
        },
    };

    #[tokio::test]
    async fn chain_requests_store_load_and_remove_block() {
        let directory = TempDir::new().unwrap();
        let mut backend = RocksBackend::new(RocksBackendSettings {
            db_path: directory.path().to_path_buf(),
            read_only: false,
            column_family: None,
        })
        .unwrap();
        let header_id = HeaderId::from([1; 32]);
        let block = Bytes::from_static(b"block");
        let (response_tx, response_rx) = oneshot::channel();
        ChainApiRequest::StoreBlockData {
            header_id,
            parent_id: HeaderId::from([0; 32]),
            block: block.clone(),
            events: Bytes::from_static(b"events"),
            immutable_ids: BTreeMap::new(),
            response_tx,
        }
        .execute(&mut backend)
        .await
        .unwrap();
        response_rx.await.unwrap().unwrap();

        let (response_tx, response_rx) = oneshot::channel();
        ChainApiRequest::GetBlock {
            header_id,
            response_tx,
        }
        .execute(&mut backend)
        .await
        .unwrap();
        assert_eq!(response_rx.await.unwrap(), Some(block.clone()));

        let (response_tx, response_rx) = oneshot::channel();
        ChainApiRequest::RemoveBlock {
            header_id,
            response_tx,
        }
        .execute(&mut backend)
        .await
        .unwrap();
        assert_eq!(response_rx.await.unwrap(), Some(block));

        let (response_tx, response_rx) = oneshot::channel();
        ChainApiRequest::GetBlock {
            header_id,
            response_tx,
        }
        .execute(&mut backend)
        .await
        .unwrap();
        assert_eq!(response_rx.await.unwrap(), None);
    }

    #[tokio::test]
    async fn chain_request_reports_dropped_reply_channel() {
        let directory = TempDir::new().unwrap();
        let mut backend = RocksBackend::new(RocksBackendSettings {
            db_path: directory.path().to_path_buf(),
            read_only: false,
            column_family: None,
        })
        .unwrap();
        let (response_tx, response_rx) = oneshot::channel();
        drop(response_rx);

        let result = ChainApiRequest::GetBlock {
            header_id: HeaderId::from([1; 32]),
            response_tx,
        }
        .execute(&mut backend)
        .await;

        assert!(matches!(
            result,
            Err(StorageServiceError::ReplyError { .. })
        ));
    }
}
