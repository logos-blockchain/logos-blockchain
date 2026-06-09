use async_trait::async_trait;
use lb_core::block::BlockNumber;
use overwatch::DynError;

pub mod requests;

#[async_trait]
pub trait StorageMembershipApi {
    async fn save_latest_block(&mut self, block_number: BlockNumber) -> Result<(), DynError>;

    async fn load_latest_block(&mut self) -> Result<Option<BlockNumber>, DynError>;
}
