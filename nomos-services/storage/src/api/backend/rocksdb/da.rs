use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use bytes::Bytes;
use libp2p_identity::PeerId;
use multiaddr::Multiaddr;
use nomos_core::{
    codec::{DeserializeOp as _, SerializeOp as _},
    da::BlobId,
    sdp::{ProviderId, SessionNumber},
};
use rocksdb::Error;
use tracing::{debug, error};

use crate::{
    api::{
        backend::rocksdb::utils::{create_share_idx, key_bytes},
        da::StorageDaApi,
    },
    backends::{StorageBackend as _, rocksdb::RocksBackend},
};

pub const DA_VID_KEY_PREFIX: &str = "da/vid/";
pub const DA_BLOB_SHARES_INDEX_PREFIX: &str = concat!("da/verified/", "si");
pub const DA_SHARED_COMMITMENTS_PREFIX: &str = concat!("da/verified/", "sc");
pub const DA_SHARE_PREFIX: &str = concat!("da/verified/", "bl");
pub const DA_ASSIGNATIONS_PREFIX: &str = concat!("da/membership/", "as");
pub const DA_ADDRESSBOOK_PREFIX: &str = concat!("da/membership/", "ab");
pub const DA_TX_PREFIX: &str = concat!("da/verified/", "tx");
pub const DA_PROVIDER_MAPPINGS_PREFIX: &str = concat!("da/membership/", "pm");

#[async_trait]
impl StorageDaApi for RocksBackend {
    type Error = Error;
    type BlobId = BlobId;
    type Share = Bytes;
    type Commitments = Bytes;
    type Tx = Bytes;
    type ShareIndex = [u8; 2];
    type NetworkId = u16;
    type Id = PeerId;

    async fn get_light_share(
        &mut self,
        blob_id: Self::BlobId,
        share_idx: Self::ShareIndex,
    ) -> Result<Option<Self::Share>, Self::Error> {
        let share_idx_bytes = create_share_idx(blob_id.as_ref(), share_idx.as_ref());
        let share_key = key_bytes(DA_SHARE_PREFIX, share_idx_bytes);
        let share_bytes = self.load(&share_key).await?;
        Ok(share_bytes)
    }

    async fn get_blob_light_shares(
        &mut self,
        blob_id: Self::BlobId,
    ) -> Result<Option<Vec<Self::Share>>, Self::Error> {
        let shares_prefix_key = key_bytes(DA_SHARE_PREFIX, blob_id.as_ref());
        let shares_bytes = self
            .load_prefix(&shares_prefix_key, None, None, None)
            .await?;
        if shares_bytes.is_empty() {
            return Ok(None);
        }

        Ok(Some(shares_bytes))
    }

    async fn get_blob_share_indices(
        &mut self,
        blob_id: Self::BlobId,
    ) -> Result<Option<HashSet<Self::ShareIndex>>, Self::Error> {
        let index_key = key_bytes(DA_BLOB_SHARES_INDEX_PREFIX, blob_id.as_ref());
        let indices_bytes = self.load(&index_key).await?;
        let indices = indices_bytes.map(|bytes| {
            HashSet::from_bytes(&bytes).unwrap_or_else(|e| {
                error!("Failed to deserialize indices: {:?}", e);
                HashSet::new()
            })
        });
        Ok(indices)
    }

    async fn store_light_share(
        &mut self,
        blob_id: Self::BlobId,
        share_idx: Self::ShareIndex,
        light_share: Self::Share,
    ) -> Result<(), Self::Error> {
        let share_idx_bytes = create_share_idx(blob_id.as_ref(), share_idx.as_ref());
        let share_key = key_bytes(DA_SHARE_PREFIX, share_idx_bytes);
        let index_key = key_bytes(DA_BLOB_SHARES_INDEX_PREFIX, blob_id.as_ref());

        let txn = self.txn(move |db| {
            if let Err(e) = db.put(&share_key, &light_share) {
                error!("Failed to store share data: {:?}", e);
                return Err(e);
            }

            let mut indices = db.get(&index_key)?.map_or_else(HashSet::new, |bytes| {
                HashSet::from_bytes(&bytes).unwrap_or_else(|e| {
                    error!("Failed to deserialize indices: {:?}", e);
                    HashSet::new()
                })
            });

            indices.insert(share_idx);

            let serialized_indices = indices
                .to_bytes()
                .expect("Serialization of HashSet should not fail");

            if let Err(e) = db.put(&index_key, &serialized_indices) {
                error!("Failed to store indices: {:?}", e);
                return Err(e);
            }

            Ok(None)
        });

        match self.execute(txn).await {
            Ok(_) => {
                debug!("Successfully stored light share and updated indices");
                Ok(())
            }
            Err(e) => {
                error!("Failed to execute transaction: {:?}", e);
                Err(e)
            }
        }
    }

    async fn get_shared_commitments(
        &mut self,
        blob_id: Self::BlobId,
    ) -> Result<Option<Self::Commitments>, Self::Error> {
        let commitments_key = key_bytes(DA_SHARED_COMMITMENTS_PREFIX, blob_id.as_ref());
        let commitments_bytes = self.load(&commitments_key).await?;
        Ok(commitments_bytes)
    }

    async fn store_shared_commitments(
        &mut self,
        blob_id: Self::BlobId,
        shared_commitments: Self::Commitments,
    ) -> Result<(), Self::Error> {
        let commitments_key = key_bytes(DA_SHARED_COMMITMENTS_PREFIX, blob_id.as_ref());
        self.store(commitments_key, shared_commitments).await
    }

    async fn store_assignations(
        &mut self,
        sesion_id: SessionNumber,
        assignations: HashMap<Self::NetworkId, HashSet<Self::Id>>,
    ) -> Result<(), Self::Error> {
        let session_bytes = sesion_id.to_le_bytes();
        let assignations_key = key_bytes(DA_ASSIGNATIONS_PREFIX, session_bytes);
        let serialized_assignations = assignations
            .to_bytes()
            .expect("Serialization of HashMap should not fail");

        match self.store(assignations_key, serialized_assignations).await {
            Ok(()) => {
                debug!("Successfully stored assignations for session {}", sesion_id);
                Ok(())
            }
            Err(e) => {
                error!(
                    "Failed to store assignations for session {}: {:?}",
                    sesion_id, e
                );
                Err(e)
            }
        }
    }

    async fn prune_assignations(
        &mut self,
        cutoff_session: SessionNumber,
    ) -> Result<(), Self::Error> {
        let txn = self.txn(move |db| {
            let mut batch = rocksdb::WriteBatch::default();

            for session in 0..cutoff_session {
                let key = key_bytes(DA_ASSIGNATIONS_PREFIX, session.to_le_bytes());
                batch.delete(&key);
            }

            db.write(batch)?;
            Ok(None)
        });

        self.execute(txn).await??;
        debug!("Pruned assignations older than session {}", cutoff_session);

        Ok(())
    }

    async fn get_assignations(
        &mut self,
        sesion_id: SessionNumber,
    ) -> Result<Option<HashMap<Self::NetworkId, HashSet<Self::Id>>>, Self::Error> {
        let session_bytes = sesion_id.to_le_bytes();
        let assignations_key = key_bytes(DA_ASSIGNATIONS_PREFIX, session_bytes);
        let assignations_bytes = self.load(&assignations_key).await?;

        assignations_bytes.map_or_else(
            || {
                debug!("No membership data found for session {}", sesion_id);
                Ok(None)
            },
            |assignations_data| {
                let assignations = HashMap::from_bytes(&assignations_data).unwrap_or_else(|e| {
                    error!(
                        "Failed to deserialize assignations for session {}: {:?}",
                        sesion_id, e
                    );
                    HashMap::new()
                });

                debug!("Successfully loaded assignations for session {}", sesion_id);
                Ok(Some(assignations))
            },
        )
    }

    async fn store_providerid_mappings(
        &mut self,
        mappings: HashMap<Self::Id, ProviderId>,
    ) -> Result<(), Self::Error> {
        let mut key_provider_map = HashMap::new();

        for (peer_id, provider_id) in mappings {
            let provider_key = key_bytes(DA_PROVIDER_MAPPINGS_PREFIX, peer_id.to_bytes());
            let serialized_provider_id = <ProviderId>::to_bytes(&provider_id)
                .expect("Serialization of ProviderId should not fail");
            key_provider_map.insert(provider_key, serialized_provider_id);
        }

        self.bulk_store(key_provider_map).await.map_err(|e| {
            error!("Failed to store provider mappings: {:?}", e);
            e
        })?;

        debug!("Successfully stored provider mappings");
        Ok(())
    }

    async fn get_provider_id(&mut self, id: Self::Id) -> Result<Option<ProviderId>, Self::Error> {
        let provider_key = key_bytes(DA_PROVIDER_MAPPINGS_PREFIX, id.to_bytes());
        let provider_bytes = self.load(&provider_key).await?;

        provider_bytes.map_or_else(
            || {
                debug!("No ProviderId found for {}", id);
                Ok(None)
            },
            |bytes| match ProviderId::from_bytes(&bytes) {
                Ok(provider_id) => Ok(Some(provider_id)),
                Err(e) => {
                    error!("Failed to deserialize ProviderId for {}: {:?}", id, e);
                    Ok(None)
                }
            },
        )
    }

    async fn store_addresses(
        &mut self,
        ids: HashMap<Self::Id, Multiaddr>,
    ) -> Result<(), Self::Error> {
        let mut key_address_map = HashMap::new();

        for (id, addr) in ids {
            let addressbook_key = key_bytes(DA_ADDRESSBOOK_PREFIX, id.to_bytes());
            let serialized_address = addr
                .to_bytes()
                .expect("Serialization of Multiaddr should not fail");
            key_address_map.insert(addressbook_key, serialized_address);
        }

        self.bulk_store(key_address_map).await.map_err(|e| {
            error!("Failed to store addresses: {:?}", e);
            e
        })?;

        Ok(())
    }

    async fn get_address(&mut self, id: Self::Id) -> Result<Option<Multiaddr>, Self::Error> {
        let addressbook_key = key_bytes(DA_ADDRESSBOOK_PREFIX, id.to_bytes());
        let address_bytes = self.load(&addressbook_key).await?;

        address_bytes.map_or_else(
            || {
                debug!("No address found for {}", id);
                Ok(None)
            },
            |bytes| {
                let address = Multiaddr::from_bytes(&bytes).unwrap_or_else(|e| {
                    error!("Failed to deserialize address for {}: {:?}", id, e);
                    Multiaddr::empty()
                });
                Ok(Some(address))
            },
        )
    }

    async fn store_tx(
        &mut self,
        blob_id: Self::BlobId,
        assignations: u16,
        tx: Self::Tx,
    ) -> Result<(), Self::Error> {
        let tx_key = key_bytes(DA_TX_PREFIX, blob_id.as_ref());
        let serialized_tx_body = tx
            .to_bytes()
            .expect("Serialization of transaction should not fail");

        let mut serialized_tx = Vec::with_capacity(2 + serialized_tx_body.len());
        serialized_tx.extend_from_slice(&assignations.to_le_bytes());
        serialized_tx.extend_from_slice(&serialized_tx_body);

        self.store(tx_key, serialized_tx.into()).await
    }

    async fn get_tx(
        &mut self,
        blob_id: Self::BlobId,
    ) -> Result<Option<(u16, Self::Tx)>, Self::Error> {
        let tx_key = key_bytes(DA_TX_PREFIX, blob_id.as_ref());
        let storage_bytes = self.load(&tx_key).await?;

        let Some(mut assignations) = storage_bytes else {
            return Ok(None);
        };

        let tx_bytes = assignations.split_off(2);
        let assignations_arr: [u8; 2] = match assignations[..2].try_into() {
            Ok(arr) => arr,
            Err(e) => {
                error!("Failed to convert assignations: {:?}", e);
                return Ok(None);
            }
        };

        let assignations = u16::from_le_bytes(assignations_arr);

        let tx = match <Bytes>::from_bytes(&tx_bytes) {
            Ok(tx) => Some((assignations, tx)),
            Err(e) => {
                error!("Failed to deserialize tx: {:?}", e);
                None
            }
        };

        Ok(tx)
    }
}
