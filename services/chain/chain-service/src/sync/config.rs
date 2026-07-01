use std::num::NonZeroUsize;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SyncConfig {
    pub block_provider: BlockProviderConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BlockProviderConfig {
    /// Upper bound on blocks returned per `send_blocks` request handled by
    /// the `BlockProvider`.
    pub batch_size: NonZeroUsize,
}
