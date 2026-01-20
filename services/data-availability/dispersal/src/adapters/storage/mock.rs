use std::collections::HashMap;

use lb_core::mantle::{SignedMantleTx, ops::channel::ChannelId};

use super::DispersalStorageAdapter;

pub struct MockDispersalStorageAdapter {
    transactions: HashMap<ChannelId, SignedMantleTx>,
}

impl DispersalStorageAdapter for MockDispersalStorageAdapter {
    fn new() -> Self {
        Self {
            transactions: HashMap::new(),
        }
    }

    fn store_tx(
        &mut self,
        channel_id: ChannelId,
        tx: SignedMantleTx,
    ) -> Result<(), super::DispersalStorageError> {
        self.transactions.insert(channel_id, tx);
        Ok(())
    }
}
