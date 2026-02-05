use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use lb_core::{header::HeaderId, mantle::TxHash};

pub mod adapter;

#[async_trait]
pub trait MempoolAdapter<Tx>: Send + Sync {
    async fn get_mempool_view(
        &self,
        ancestor_hint: HeaderId,
    ) -> Result<Pin<Box<dyn Stream<Item = Tx> + Send>>, overwatch::DynError>;

    async fn remove_transactions(&self, ids: &[TxHash]) -> Result<(), overwatch::DynError>;

    async fn post_tx(&self, tx: Tx) -> Result<(), overwatch::DynError>;
}
