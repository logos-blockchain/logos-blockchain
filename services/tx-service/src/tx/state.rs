use std::{convert::Infallible, marker::PhantomData};

use lb_binary_codec::bincode::DeserializeOp as _;
use lb_services_utils::overwatch::{StateVersion, VersionedState};
use overwatch::{DynError, services::state::ServiceState};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::TxMempoolSettings;

/// State that is maintained across service restarts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxMempoolState<PoolState, PoolSettings, NetworkSettings> {
    /// The (optional) pool snapshot.
    pub(crate) pool: Option<PoolState>,
    #[serde(skip)]
    _phantom: PhantomData<(PoolSettings, NetworkSettings)>,
}

impl<PoolState, PoolSettings, NetworkSettings>
    TxMempoolState<PoolState, PoolSettings, NetworkSettings>
{
    pub const fn pool(&self) -> Option<&PoolState> {
        self.pool.as_ref()
    }
}

impl<PoolState, PoolSettings, NetworkSettings> From<PoolState>
    for TxMempoolState<PoolState, PoolSettings, NetworkSettings>
{
    fn from(value: PoolState) -> Self {
        Self {
            pool: Some(value),
            _phantom: PhantomData,
        }
    }
}

impl<PoolState, PoolSettings, NetworkSettings> ServiceState
    for TxMempoolState<PoolState, PoolSettings, NetworkSettings>
{
    type Error = Infallible;
    type Settings = TxMempoolSettings<PoolSettings, NetworkSettings>;

    fn from_settings(_settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self {
            pool: None,
            _phantom: PhantomData,
        })
    }
}

impl<PoolState, PoolSettings, NetworkSettings> VersionedState
    for TxMempoolState<PoolState, PoolSettings, NetworkSettings>
where
    Self: DeserializeOwned,
{
    const VERSION: StateVersion = StateVersion::new(1);

    /// The only version before 1 is 0, the records written before records
    /// carried a version, in the layout of version 1.
    fn migrate(_from: StateVersion, bytes: &[u8]) -> Result<Self, DynError> {
        Ok(Self::from_bytes(bytes)?)
    }
}
