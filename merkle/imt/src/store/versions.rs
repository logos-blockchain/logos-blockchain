//! The versions of the committed blocks, shared by every tree of a ledger.

use lb_kv_store::{KvStore, WriteBatch};

use super::{
    BlockId, BlockVersionKey, ImtError, LiveVersion, LiveVersionKey, NextVersionKey, StoreKey as _,
    StoreValue as _, Version,
};

/// The version of a block, and the writes recording it.
#[derive(Debug)]
pub struct VersionAllocation {
    pub version: Version,
    /// Whether the block already had this version, as when blocks are replayed
    /// at restart.
    pub reused: bool,
    pub writes: WriteBatch,
}

/// A committed block that is not final yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveBlock {
    pub version: Version,
    pub block_id: BlockId,
}

/// Allocates the version of each committed block and remembers it until the
/// block is final or discarded.
#[derive(Clone, Debug)]
pub struct VersionRegistry<S> {
    store: S,
}

impl<S: KvStore> VersionRegistry<S> {
    pub const fn new(store: S) -> Self {
        Self { store }
    }

    pub fn lookup(&self, block_id: BlockId) -> Result<Option<Version>, ImtError> {
        self.store
            .get(&BlockVersionKey { block_id }.encode())
            .map_err(ImtError::storage)?
            .map(|bytes| {
                Version::decode(&bytes)
                    .map_err(|error| ImtError::corrupted("block version", &error))
            })
            .transpose()
    }

    /// The version of `block_id`: the one it already has, or the next one. The
    /// writes must be applied before the next allocation.
    pub fn allocate(&self, block_id: BlockId) -> Result<VersionAllocation, ImtError> {
        if let Some(version) = self.lookup(block_id)? {
            return Ok(VersionAllocation {
                version,
                reused: true,
                writes: WriteBatch::new(),
            });
        }
        let version = self
            .store
            .get(&NextVersionKey.encode())
            .map_err(ImtError::storage)?
            .map(|bytes| {
                Version::decode(&bytes).map_err(|error| ImtError::corrupted("next version", &error))
            })
            .transpose()?
            .unwrap_or(Version::GENESIS.next());
        let mut writes = WriteBatch::new();
        writes
            .put(BlockVersionKey { block_id }.encode(), version.encode())
            .put(
                LiveVersionKey { version }.encode(),
                LiveVersion { block_id }.encode(),
            )
            .put(NextVersionKey.encode(), version.next().encode());
        Ok(VersionAllocation {
            version,
            reused: false,
            writes,
        })
    }

    /// The writes forgetting the version of a block, once it is final or
    /// discarded.
    #[must_use]
    pub fn release_writes(block: LiveBlock) -> WriteBatch {
        let mut writes = WriteBatch::new();
        writes
            .delete(
                BlockVersionKey {
                    block_id: block.block_id,
                }
                .encode(),
            )
            .delete(
                LiveVersionKey {
                    version: block.version,
                }
                .encode(),
            );
        writes
    }

    /// Every committed block that is not final or discarded yet.
    pub fn live_blocks(&self) -> Result<Vec<LiveBlock>, ImtError> {
        self.store
            .scan_prefix(&LiveVersionKey::prefix())
            .map_err(ImtError::storage)?
            .into_iter()
            .map(|entry| {
                let key = LiveVersionKey::decode(&entry.key)
                    .map_err(|error| ImtError::corrupted("live version key", &error))?;
                let live = LiveVersion::decode(&entry.value)
                    .map_err(|error| ImtError::corrupted("live version", &error))?;
                Ok(LiveBlock {
                    version: key.version,
                    block_id: live.block_id,
                })
            })
            .collect()
    }
}
