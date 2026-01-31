use std::fmt::{Debug, Formatter};

use crate::keys::{UnsecuredZkKey, ZkKey, errors::KeyError, secured_key::SecureKeyOperator};

pub struct CheckConditionWithLeaderKey<F> {
    result_channel: tokio::sync::oneshot::Sender<bool>,
    f: F,
}

impl<F> Debug for CheckConditionWithLeaderKey<F> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "CheckConditionWithLeaderKey")
    }
}

impl<F> CheckConditionWithLeaderKey<F> {
    #[must_use]
    pub const fn new(result_channel: tokio::sync::oneshot::Sender<bool>, f: F) -> Self {
        Self { result_channel, f }
    }
}

#[cfg(feature = "unsafe")]
#[async_trait::async_trait]
impl<F> SecureKeyOperator for CheckConditionWithLeaderKey<F>
where
    F: Fn(&UnsecuredZkKey) -> bool + Send + Sync + 'static,
{
    type Key = ZkKey;
    type Error = KeyError;

    async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let Self { result_channel, f } = *self;
        if result_channel.send(f(key.as_unsecured())).is_err() {
            tracing::error!("Failed to send result via channel");
        }
        Ok(())
    }
}

pub struct BuildWithLeaderKey<F, R> {
    result_channel: tokio::sync::oneshot::Sender<R>,
    f: F,
}

impl<F, R> Debug for BuildWithLeaderKey<F, R> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "BuildWithLeaderKey")
    }
}

impl<F, R> BuildWithLeaderKey<F, R> {
    #[must_use]
    pub const fn new(result_channel: tokio::sync::oneshot::Sender<R>, f: F) -> Self {
        Self { result_channel, f }
    }
}

#[cfg(feature = "unsafe")]
#[async_trait::async_trait]
impl<F, R> SecureKeyOperator for BuildWithLeaderKey<F, R>
where
    F: Fn(&UnsecuredZkKey) -> R + Send + Sync + 'static,
    R: Send + Sync + 'static,
{
    type Key = ZkKey;
    type Error = KeyError;

    async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let Self { result_channel, f } = *self;
        if result_channel.send(f(key.as_unsecured())).is_err() {
            tracing::error!("Failed to send result via channel");
        }
        Ok(())
    }
}
