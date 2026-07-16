use std::{collections::HashMap, fmt, sync::Arc};

use bytes::Bytes;

#[derive(Clone, Default)]
pub struct RecoveryData(Arc<HashMap<Vec<u8>, Bytes>>);

impl RecoveryData {
    #[must_use]
    pub fn new(entries: HashMap<Vec<u8>, Bytes>) -> Self {
        Self(Arc::new(entries))
    }

    #[must_use]
    pub fn get(&self, key: &[u8]) -> Option<Bytes> {
        self.0.get(key).cloned()
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
    fn clones_read_multiple_entries() {
        let data = RecoveryData::new(HashMap::from([
            (b"recovery/one".to_vec(), Bytes::from_static(b"one")),
            (b"recovery/two".to_vec(), Bytes::from_static(b"two")),
        ]));
        let cloned_data = data.clone();

        assert_eq!(data.get(b"recovery/one"), Some(Bytes::from_static(b"one")));
        assert_eq!(
            cloned_data.get(b"recovery/two"),
            Some(Bytes::from_static(b"two"))
        );
        assert_eq!(data.get(b"recovery/missing"), None);
    }
}
