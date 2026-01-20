pub mod mock;

use lb_core::mantle::{SignedMantleTx, ops::channel::ChannelId};

pub struct DispersalStorageError;

pub trait DispersalStorageAdapter {
    fn new() -> Self;
    fn store_tx(
        &mut self,
        channel_id: ChannelId,
        tx: SignedMantleTx,
    ) -> Result<(), DispersalStorageError>;
}
