use async_trait::async_trait;
use lb_core::block::BlockNumber;
use lb_log_targets::storage;
use overwatch::DynError;
use tracing::{debug, error};

use crate::{
    api::membership::StorageMembershipApi,
    backends::{StorageBackend as _, rocksdb::RocksBackend},
};

pub const MEMBERSHIP_LATEST_BLOCK_KEY: &str = "membership/latest_block";

const LOG_TARGET: &str = storage::rocksdb::MEMBERSHIP;

#[async_trait]
impl StorageMembershipApi for RocksBackend {
    async fn save_latest_block(&mut self, block_number: BlockNumber) -> Result<(), DynError> {
        let block_bytes = block_number.to_le_bytes();

        match self
            .store(
                MEMBERSHIP_LATEST_BLOCK_KEY.into(),
                block_bytes.to_vec().into(),
            )
            .await
        {
            Ok(()) => {
                debug!(target: LOG_TARGET, "Successfully stored latest block {}", block_number);
                Ok(())
            }
            Err(e) => {
                error!(target: LOG_TARGET, "Failed to store latest block: {:?}", e);
                Err(e.into())
            }
        }
    }

    async fn load_latest_block(&mut self) -> Result<Option<BlockNumber>, DynError> {
        let data = self.load(MEMBERSHIP_LATEST_BLOCK_KEY.as_bytes()).await?;

        match data {
            None => {
                debug!(target: LOG_TARGET, "No latest block found");
                Ok(None)
            }
            Some(bytes) => {
                if bytes.len() != 8 {
                    error!(
                        target: LOG_TARGET,
                        "Invalid block number bytes length: {}",
                        bytes.len()
                    );
                    return Ok(None);
                }

                let block_bytes: [u8; 8] = bytes[..8].try_into().unwrap();
                let block_number = BlockNumber::from_le_bytes(block_bytes);
                debug!(target: LOG_TARGET, "Successfully loaded latest block {}", block_number);
                Ok(Some(block_number))
            }
        }
    }
}
