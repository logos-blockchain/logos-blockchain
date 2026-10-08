use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use lb_core::era::ForkDigests;

use super::{RecoveryError, RecoveryResult};

/// The recovery records a node found in its storage when it started, which
/// its services take their states from, and the chain they belong to.
#[derive(Clone)]
pub struct RecoveryData {
    entries: Arc<Mutex<HashMap<Vec<u8>, Bytes>>>,
    /// The fork digest of every era of the chain: a record stamped with
    /// another one was written on another chain.
    forks: Arc<ForkDigests>,
}

pub trait StorageRecoverySettings {
    const RECOVERY_KEY_SUFFIX: &'static [u8];

    fn recovery_data(&self) -> &RecoveryData;
}

impl RecoveryData {
    #[must_use]
    pub fn new(entries: HashMap<Vec<u8>, Bytes>, forks: Arc<ForkDigests>) -> Self {
        Self {
            entries: Arc::new(Mutex::new(entries)),
            forks,
        }
    }

    pub fn take(&self, key: &[u8]) -> RecoveryResult<Option<Bytes>> {
        self.entries
            .lock()
            .map_err(|error| RecoveryError::Backend(error.to_string()))
            .map(|mut entries| entries.remove(key))
    }

    /// The fork digest of every era of the chain the records belong to.
    #[must_use]
    pub const fn forks(&self) -> &Arc<ForkDigests> {
        &self.forks
    }
}

impl fmt::Debug for RecoveryData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("RecoveryData").finish()
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZero;
    use std::time::Duration;

    use lb_core::era::ForkDigest;
    use lb_cryptarchia_engine::era::{BlockVersion, EraEntriesAfterGenesis, EraEntry};
    use time::OffsetDateTime;

    use super::*;

    #[test]
    fn clones_take_their_entries_from_shared_data() {
        let forks = ForkDigests::new(
            OffsetDateTime::UNIX_EPOCH,
            EraEntry {
                block_version: BlockVersion::V1,
                slot_duration: Duration::from_secs(1),
                epoch_length_in_slots: NonZero::new(10).unwrap(),
                transition_slots: 0,
                parameters: ForkDigest::from([0; 32]),
            },
            EraEntriesAfterGenesis::empty(),
        )
        .unwrap();
        let data = RecoveryData::new(
            HashMap::from([
                (b"recovery/one".to_vec(), Bytes::from_static(b"one")),
                (b"recovery/two".to_vec(), Bytes::from_static(b"two")),
            ]),
            Arc::new(forks),
        );
        let cloned_data = data.clone();

        assert_eq!(
            data.take(b"recovery/one").unwrap(),
            Some(Bytes::from_static(b"one"))
        );
        assert_eq!(cloned_data.take(b"recovery/one").unwrap(), None);
        assert_eq!(
            cloned_data.take(b"recovery/two").unwrap(),
            Some(Bytes::from_static(b"two"))
        );
        assert_eq!(data.take(b"recovery/missing").unwrap(), None);
    }
}
