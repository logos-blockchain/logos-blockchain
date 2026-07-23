use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
};

use bytes::Bytes;

use super::{RecoveryError, RecoveryResult};

#[derive(Clone, Default)]
pub struct RecoveryData(Arc<Mutex<HashMap<Vec<u8>, Bytes>>>);

pub trait StorageRecoverySettings {
    const RECOVERY_KEY_SUFFIX: &'static [u8];

    fn recovery_data(&self) -> &RecoveryData;
}

impl RecoveryData {
    #[must_use]
    pub fn new(entries: HashMap<Vec<u8>, Bytes>) -> Self {
        Self(Arc::new(Mutex::new(entries)))
    }

    pub fn take(&self, key: &[u8]) -> RecoveryResult<Option<Bytes>> {
        self.0
            .lock()
            .map_err(|error| RecoveryError::Backend(error.to_string()))
            .map(|mut entries| entries.remove(key))
    }
}

impl fmt::Debug for RecoveryData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("RecoveryData").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_take_their_entries_from_shared_data() {
        let data = RecoveryData::new(HashMap::from([
            (b"recovery/one".to_vec(), Bytes::from_static(b"one")),
            (b"recovery/two".to_vec(), Bytes::from_static(b"two")),
        ]));
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
