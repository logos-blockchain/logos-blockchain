use std::{borrow::Borrow, fmt};

#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WalletId(String);

impl WalletId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl From<String> for WalletId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for WalletId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<&String> for WalletId {
    fn from(value: &String) -> Self {
        Self::new(value.clone())
    }
}

impl AsRef<str> for WalletId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for WalletId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for WalletId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WalletSyncSourceId(String);

impl WalletSyncSourceId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for WalletSyncSourceId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for WalletSyncSourceId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<&String> for WalletSyncSourceId {
    fn from(value: &String) -> Self {
        Self::new(value.clone())
    }
}

impl AsRef<str> for WalletSyncSourceId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for WalletSyncSourceId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for WalletSyncSourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_default() {
            f.write_str("<default>")
        } else {
            f.write_str(self.as_str())
        }
    }
}

#[must_use]
pub fn wallet_id_for_sync_source(
    base_wallet_id: impl AsRef<str>,
    source_id: &WalletSyncSourceId,
) -> WalletId {
    if source_id.is_default() {
        WalletId::new(base_wallet_id.as_ref())
    } else {
        WalletId::new(format!(
            "{}@{}",
            base_wallet_id.as_ref(),
            source_id.as_str()
        ))
    }
}
