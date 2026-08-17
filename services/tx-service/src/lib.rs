pub mod backend;
mod metrics;
pub mod network;
pub mod storage;
pub mod tx;

use std::{
    fmt::{Debug, Error, Formatter},
    pin::Pin,
};

use backend::{MempoolError, Status};
use futures::Stream;
use lb_core::mantle::transactions::hash::PrefixedKey;
use tokio::sync::oneshot::Sender;
pub use tx::{service::TxMempoolService, settings::TxMempoolSettings};

/// A stream of the transactions whose hash shares one reference prefix.
pub type TxsWithCommonPrefix<Tx> = Pin<Box<dyn Stream<Item = Tx> + Send>>;

pub enum MempoolMsg<BlockId, Payload, Item, Key>
where
    Key: PrefixedKey,
{
    Add {
        payload: Payload,
        key: Key,
        reply_channel: Sender<Result<(), MempoolError>>,
    },
    View {
        ancestor_hint: BlockId,
        reply_channel: Sender<Pin<Box<dyn Stream<Item = Item> + Send>>>,
    },
    /// Get a stream of transactions whose hash starts with `prefix`.
    ///
    /// The consumer is responsible for ensuring an upper bound when consuming
    /// the stream as per the block compression spec.
    GetTransactionsByPrefix {
        prefix: Key::Prefix,
        reply_channel: Sender<Result<TxsWithCommonPrefix<Item>, MempoolError>>,
    },
    Remove {
        ids: Vec<Key>,
    },
    Metrics {
        reply_channel: Sender<MempoolMetrics>,
    },
    Status {
        items: Vec<Key>,
        reply_channel: Sender<Vec<Status>>,
    },
}

impl<BlockId, Payload, Item, Key> Debug for MempoolMsg<BlockId, Payload, Item, Key>
where
    BlockId: Debug,
    Payload: Debug,
    Item: Debug,
    Key: Debug + PrefixedKey<Prefix: Debug>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        match self {
            Self::View { ancestor_hint, .. } => {
                write!(f, "MempoolMsg::View {{ ancestor_hint: {ancestor_hint:?} }}")
            }
            Self::GetTransactionsByPrefix { prefix, .. } => {
                write!(
                    f,
                    "MempoolMsg::GetTransactionsByPrefix{{prefix: {prefix:?}}}"
                )
            }
            Self::Add { payload, .. } => write!(f, "MempoolMsg::Add{{payload: {payload:?}}}"),
            Self::Remove { ids } => write!(f, "MempoolMsg::Prune{{ids: {ids:?}}}"),
            Self::Metrics { .. } => write!(f, "MempoolMsg::Metrics"),
            Self::Status { items, .. } => write!(f, "MempoolMsg::Status{{items: {items:?}}}"),
        }
    }
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct MempoolMetrics {
    pub pending_items: usize,
    pub last_item_timestamp: u64,
}
