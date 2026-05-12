use core::{error::Error, num::NonZeroUsize, ops::RangeInclusive, pin::Pin};

use futures::{Stream, TryStreamExt as _};
use lb_core::header::HeaderId;
use lb_cryptarchia_engine::Slot;

use crate::{StorageServiceError, backends::StorageBackend};

#[cfg(feature = "rocksdb-backend")]
pub mod rocksdb;

/// A stream of `HeaderId`s, used for scanning immutable header IDs. We return a
/// stream here to allow for efficient pagination of large ranges of immutable
/// blocks.
pub type HeaderIdStream =
    Pin<Box<dyn Stream<Item = Result<HeaderId, Box<dyn Error + Send + Sync>>> + Send>>;

/// Helper to collect a stream of immutable `HeaderId`s into a reversed `Vec`.
pub async fn streamed_immutable_block_ids_reverse_vec<Backend: StorageBackend>(
    backend: &mut Backend,
    slot_range: RangeInclusive<Slot>,
    limit: NonZeroUsize,
) -> Result<Vec<HeaderId>, StorageServiceError> {
    let stream = backend
        .scan_immutable_block_ids_reverse(slot_range, limit)
        .await
        .map_err(|e| StorageServiceError::BackendError(Box::new(e)))?;
    stream
        .try_collect::<Vec<HeaderId>>()
        .await
        .map_err(StorageServiceError::BackendError)
}

/// Helper to collect a stream of immutable `HeaderId`s into a `Vec`.
pub async fn streamed_immutable_block_ids_vec<Backend: StorageBackend>(
    backend: &mut Backend,
    slot_range: RangeInclusive<Slot>,
    limit: NonZeroUsize,
) -> Result<Vec<HeaderId>, StorageServiceError> {
    let stream = backend
        .scan_immutable_block_ids(slot_range, limit)
        .await
        .map_err(|e| StorageServiceError::BackendError(e.into()))?;
    stream
        .try_collect::<Vec<HeaderId>>()
        .await
        .map_err(StorageServiceError::BackendError)
}
