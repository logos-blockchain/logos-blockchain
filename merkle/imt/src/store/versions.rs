//! The version each committed block writes its nodes under, shared by every
//! tree of a ledger.

use lb_kv_store::{KvStore, WriteBatch};

use super::{
    BlockId, BlockVersionKey, ImtError, NextVersionKey, StoreKey as _, StoreValue as _, Version,
};

/// The version given to a block, and the writes recording it (none if the
/// block already had one).
#[derive(Debug)]
pub struct VersionAllocation {
    pub version: Version,
    pub writes: WriteBatch,
}

/// A committed block that is not finalized or discarded yet, with its version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveBlock {
    pub version: Version,
    pub block_id: BlockId,
}

/// Gives each committed block a version and keeps it under `BlockVersionKey`
/// until the block is finalized or discarded.
#[derive(Clone, Debug)]
pub struct VersionRegistry<S> {
    store: S,
}

impl<S: KvStore> VersionRegistry<S> {
    pub const fn new(store: S) -> Self {
        Self { store }
    }

    /// The version of `block_id`, if it has one.
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

    /// The version of `block_id`. A block replayed at restart gets back the one
    /// it has, so it rewrites the same nodes. A new block gets the next one,
    /// starting after `Version::GENESIS`, which the genesis tree uses. Apply
    /// the writes before the next call.
    pub fn allocate(&self, block_id: BlockId) -> Result<VersionAllocation, ImtError> {
        if let Some(version) = self.lookup(block_id)? {
            return Ok(VersionAllocation {
                version,
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
            .put(NextVersionKey.encode(), version.next().encode());
        Ok(VersionAllocation { version, writes })
    }

    /// The writes forgetting the version of `block_id`, once it is finalized or
    /// discarded.
    #[must_use]
    pub fn release_writes(block_id: BlockId) -> WriteBatch {
        let mut writes = WriteBatch::new();
        writes.delete(BlockVersionKey { block_id }.encode());
        writes
    }

    /// Every block that still has a version.
    pub fn live_blocks(&self) -> Result<Vec<LiveBlock>, ImtError> {
        self.store
            .scan_prefix(&BlockVersionKey::prefix())
            .map_err(ImtError::storage)?
            .into_iter()
            .map(|entry| {
                let key = BlockVersionKey::decode(&entry.key)
                    .map_err(|error| ImtError::corrupted("block version key", &error))?;
                let version = Version::decode(&entry.value)
                    .map_err(|error| ImtError::corrupted("block version", &error))?;
                Ok(LiveBlock {
                    version,
                    block_id: key.block_id,
                })
            })
            .collect()
    }
}
