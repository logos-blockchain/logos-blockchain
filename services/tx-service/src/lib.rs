pub mod backend;
pub mod network;
pub mod storage;
pub mod tx;
pub mod verify;

use std::{
    collections::BTreeSet,
    fmt::{Debug, Error, Formatter},
    pin::Pin,
};

use backend::{MempoolError, Status};
use futures::Stream;
use tokio::sync::{broadcast, oneshot::Sender};
pub use tx::{service::TxMempoolService, settings::TxMempoolSettings};

/// Response for `GetTransactionsByHashes` request
#[derive(Debug, Clone)]
pub struct TransactionsByHashesResponse<Item, Key> {
    /// Transactions that were found in the mempool
    found: Vec<Item>,
    /// Hashes of transactions that were not found in the mempool
    not_found: BTreeSet<Key>,
}

impl<Item, Key> TransactionsByHashesResponse<Item, Key>
where
    Key: Ord,
{
    #[must_use]
    pub const fn new(found: Vec<Item>, not_found: BTreeSet<Key>) -> Self {
        Self { found, not_found }
    }

    #[must_use]
    pub fn all_found(&self) -> bool {
        self.not_found.is_empty()
    }

    #[must_use]
    pub const fn not_found(&self) -> &BTreeSet<Key> {
        &self.not_found
    }

    #[must_use]
    pub fn into_found(self) -> Vec<Item> {
        self.found
    }
}

pub enum MempoolMsg<BlockId, Payload, Item, Key> {
    Add {
        payload: Payload,
        key: Key,
        reply_channel: Sender<Result<(), MempoolError>>,
    },
    View {
        ancestor_hint: BlockId,
        reply_channel: Sender<Pin<Box<dyn Stream<Item = Item> + Send>>>,
    },
    /// Get specific transactions from mempool by their hashes
    ///
    /// Returns both found transactions and not found hashes.
    GetTransactionsByHashes {
        hashes: Vec<Key>,
        reply_channel: Sender<Result<TransactionsByHashesResponse<Item, Key>, MempoolError>>,
    },
    Prune {
        ids: Vec<Key>,
    },
    MarkInBlock {
        ids: Vec<Key>,
        block: BlockId,
    },
    Metrics {
        reply_channel: Sender<MempoolMetrics>,
    },
    Status {
        items: Vec<Key>,
        reply_channel: Sender<Vec<Status<BlockId>>>,
    },
    Subscribe {
        reply_channel: Sender<broadcast::Receiver<Item>>,
    },
}

impl<BlockId, Payload, Item, Key> Debug for MempoolMsg<BlockId, Payload, Item, Key>
where
    BlockId: Debug,
    Payload: Debug,
    Item: Debug,
    Key: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        match self {
            Self::View { ancestor_hint, .. } => {
                write!(f, "MempoolMsg::View {{ ancestor_hint: {ancestor_hint:?} }}")
            }
            Self::GetTransactionsByHashes { hashes, .. } => {
                write!(
                    f,
                    "MempoolMsg::GetTransactionsByHashes{{hashes: {hashes:?}}}"
                )
            }
            Self::Add { payload, .. } => write!(f, "MempoolMsg::Add{{payload: {payload:?}}}"),
            Self::Prune { ids } => write!(f, "MempoolMsg::Prune{{ids: {ids:?}}}"),
            Self::MarkInBlock { ids, block } => {
                write!(
                    f,
                    "MempoolMsg::MarkInBlock{{ids: {ids:?}, block: {block:?}}}"
                )
            }
            Self::Metrics { .. } => write!(f, "MempoolMsg::Metrics"),
            Self::Status { items, .. } => write!(f, "MempoolMsg::Status{{items: {items:?}}}"),
            Self::Subscribe { .. } => write!(f, "MempoolMsg::Subscribe"),
        }
    }
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct MempoolMetrics {
    pub pending_items: usize,
    pub last_item_timestamp: u64,
}
