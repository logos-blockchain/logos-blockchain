use std::{convert::Infallible, marker::PhantomData};

use overwatch::services::state::ServiceState;
use serde::{Deserialize, Serialize};

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
