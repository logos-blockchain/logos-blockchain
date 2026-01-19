use bytes::Bytes;
use lb_core::{
    codec::{DeserializeOp as _, SerializeOp as _},
    da::{BlobId, blob::Share},
    mantle::SignedMantleTx,
};
use lb_kzgrs_backend::common::share::{DaLightShare, DaShare, DaSharesCommitments};
use lb_storage_service::{
    api::da::{DaConverter, StorageDaApi},
    backends::rocksdb::RocksBackend,
};

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct DaStorageConverter;

impl DaConverter<RocksBackend> for DaStorageConverter {
    type Share = DaShare;
    type Tx = SignedMantleTx;
    type Error = lb_core::codec::Error;

    fn blob_id_to_storage(blob_id: BlobId) -> Result<BlobId, Self::Error> {
        Ok(blob_id)
    }

    fn blob_id_from_storage(blob_id: BlobId) -> Result<BlobId, Self::Error> {
        Ok(blob_id)
    }

    fn share_index_to_storage(
        share_index: <DaShare as Share>::ShareIndex,
    ) -> Result<<RocksBackend as StorageDaApi>::ShareIndex, Self::Error> {
        Ok(share_index)
    }

    fn share_index_from_storage(
        share_index: <RocksBackend as StorageDaApi>::ShareIndex,
    ) -> Result<<DaShare as Share>::ShareIndex, Self::Error> {
        Ok(share_index)
    }

    fn share_to_storage(service_share: DaLightShare) -> Result<Bytes, Self::Error> {
        service_share.to_bytes()
    }

    fn share_from_storage(backend_share: Bytes) -> Result<DaLightShare, Self::Error> {
        DaLightShare::from_bytes(&backend_share)
    }

    fn commitments_to_storage(
        service_commitments: DaSharesCommitments,
    ) -> Result<Bytes, Self::Error> {
        service_commitments.to_bytes()
    }

    fn commitments_from_storage(
        backend_commitments: Bytes,
    ) -> Result<DaSharesCommitments, Self::Error> {
        DaSharesCommitments::from_bytes(&backend_commitments)
    }

    fn tx_to_storage(
        service_tx: SignedMantleTx,
    ) -> Result<<RocksBackend as StorageDaApi>::Tx, Self::Error> {
        service_tx.to_bytes()
    }

    fn tx_from_storage(
        backend_tx: <RocksBackend as StorageDaApi>::Tx,
    ) -> Result<SignedMantleTx, Self::Error> {
        SignedMantleTx::from_bytes(&backend_tx)
    }
}
