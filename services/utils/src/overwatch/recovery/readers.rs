use std::{
    fmt,
    sync::{Arc, Mutex},
};

use bytes::Bytes;

use crate::overwatch::recovery::{RecoveryError, RecoveryResult};

type ReadFn = dyn FnOnce(&[u8]) -> RecoveryResult<Option<Bytes>> + Send;

#[derive(Clone)]
pub struct RecoveryReader(Arc<Mutex<Option<Box<ReadFn>>>>);

impl RecoveryReader {
    pub fn new(read: impl FnOnce(&[u8]) -> RecoveryResult<Option<Bytes>> + Send + 'static) -> Self {
        Self(Arc::new(Mutex::new(Some(Box::new(read)))))
    }

    pub fn read(&self, key: &[u8]) -> RecoveryResult<Option<Bytes>> {
        let read = self
            .0
            .lock()
            .map_err(|error| RecoveryError::Backend(error.to_string()))?
            .take()
            .ok_or_else(|| {
                RecoveryError::Backend(
                    "Recovery reader was already consumed; in-process service restart is \
                     unsupported"
                        .into(),
                )
            })?;

        read(key)
    }
}

impl fmt::Debug for RecoveryReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("RecoveryReader").finish()
    }
}
