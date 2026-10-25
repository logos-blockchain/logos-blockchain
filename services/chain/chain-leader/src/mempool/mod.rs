use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use lb_core::{header::HeaderId, mantle::transactions::hash::TxHash};
use lb_cryptarchia_engine::Era;

pub mod adapter;

#[async_trait]
pub trait MempoolAdapter<Tx>: Send + Sync {
    async fn rotate_era(&mut self, era: Era);
    async fn complete_era_transition(&mut self, ended_era: Era);

    async fn get_mempool_view(
        &self,
        ancestor_hint: HeaderId,
    ) -> Result<Pin<Box<dyn Stream<Item = Tx> + Send>>, overwatch::DynError>;

    async fn remove_transactions(&self, ids: &[TxHash]) -> Result<(), overwatch::DynError>;

    async fn post_tx(&self, tx: Tx) -> Result<(), overwatch::DynError>;
}
