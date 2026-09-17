use std::{
    collections::{BTreeMap, HashMap},
    fmt::{Debug, Formatter},
    num::NonZeroUsize,
    ops::RangeInclusive,
    pin::Pin,
};

use bytes::Bytes;
use futures::Stream;
use lb_core::{header::HeaderId, mantle::TxHash};
use lb_cryptarchia_engine::Slot;
use tokio::sync::oneshot::Sender;

use crate::{StorageReplyReceiver, backend::StorageTransaction, rocksdb::Transaction};

/// Messages accepted by the storage service.
pub enum StorageMsg {
    Load {
        key: Bytes,
        reply_channel: Sender<Option<Bytes>>,
    },
    LoadPrefix {
        prefix: Bytes,
        start_key: Option<Bytes>,
        end_key: Option<Bytes>,
        limit: Option<NonZeroUsize>,
        reply_channel: Sender<Vec<Bytes>>,
    },
    Store {
        key: Bytes,
        value: Bytes,
    },
    Remove {
        key: Bytes,
        reply_channel: Sender<Option<Bytes>>,
    },
    Execute {
        transaction: Transaction,
        reply_channel: Sender<<Transaction as StorageTransaction>::Result>,
    },

    GetBlock {
        header_id: HeaderId,
        response_tx: Sender<Option<Bytes>>,
    },
    StoreBlockData {
        header_id: HeaderId,
        parent_id: HeaderId,
        block: Bytes,
        events: Bytes,
        immutable_ids: BTreeMap<Slot, HeaderId>,
        response_tx: Sender<Result<(), String>>,
    },
    RemoveBlock {
        header_id: HeaderId,
        response_tx: Sender<Option<Bytes>>,
    },
    GetBlockParent {
        header_id: HeaderId,
        response_tx: Sender<Option<HeaderId>>,
    },
    GetBlockEvents {
        header_id: HeaderId,
        response_tx: Sender<Option<Bytes>>,
    },
    StoreImmutableBlockIds {
        ids: BTreeMap<Slot, HeaderId>,
        response_tx: Sender<Result<(), String>>,
    },
    GetImmutableBlockId {
        slot: Slot,
        response_tx: Sender<Option<HeaderId>>,
    },
    ScanImmutableBlockIds {
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
        response_tx: Sender<Vec<HeaderId>>,
    },
    ScanImmutableBlockIdsReverse {
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
        response_tx: Sender<Vec<HeaderId>>,
    },
    StoreTransactions {
        transactions: HashMap<TxHash, Bytes>,
    },
    GetTransactions {
        tx_hashes: Vec<TxHash>,
        response_tx: Sender<Pin<Box<dyn Stream<Item = Bytes> + Send>>>,
    },
    RemoveTransactions {
        tx_hashes: Vec<TxHash>,
    },
}

impl Debug for StorageMsg {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load { key, .. } => {
                write!(f, "Load {{ {key:?} }}")
            }
            Self::LoadPrefix {
                prefix,
                start_key,
                end_key,
                limit,
                ..
            } => {
                write!(
                    f,
                    "LoadPrefix {{ {prefix:?} {start_key:?}, {end_key:?}, limit: {limit:?} }}"
                )
            }
            Self::Store { key, value } => {
                write!(f, "Store {{ {key:?}, {value:?}}}")
            }
            Self::Remove { key, .. } => {
                write!(f, "Remove {{ {key:?} }}")
            }
            Self::Execute { .. } => write!(f, "Execute transaction"),
            Self::GetBlock { .. } => write!(f, "GetBlock {{ .. }}"),
            Self::StoreBlockData { .. } => write!(f, "StoreBlockData {{ .. }}"),
            Self::RemoveBlock { .. } => write!(f, "RemoveBlock {{ .. }}"),
            Self::GetBlockParent { .. } => write!(f, "GetBlockParent {{ .. }}"),
            Self::GetBlockEvents { .. } => write!(f, "GetBlockEvents {{ .. }}"),
            Self::StoreImmutableBlockIds { .. } => write!(f, "StoreImmutableBlockIds {{ .. }}"),
            Self::GetImmutableBlockId { .. } => write!(f, "GetImmutableBlockId {{ .. }}"),
            Self::ScanImmutableBlockIds { .. } => write!(f, "ScanImmutableBlockIds {{ .. }}"),
            Self::ScanImmutableBlockIdsReverse { .. } => {
                write!(f, "ScanImmutableBlockIdsReverse {{ .. }}")
            }
            Self::StoreTransactions { .. } => write!(f, "StoreTransactions {{ .. }}"),
            Self::GetTransactions { .. } => write!(f, "GetTransactions {{ .. }}"),
            Self::RemoveTransactions { .. } => write!(f, "RemoveTransactions {{ .. }}"),
        }
    }
}

impl StorageMsg {
    pub fn new_load_message(key: Bytes) -> (Self, StorageReplyReceiver<Option<Bytes>>) {
        let (reply_channel, receiver) = tokio::sync::oneshot::channel();
        (
            Self::Load { key, reply_channel },
            StorageReplyReceiver::new(receiver),
        )
    }

    #[must_use]
    pub const fn get_block_request(
        header_id: HeaderId,
        response_tx: Sender<Option<Bytes>>,
    ) -> Self {
        Self::GetBlock {
            header_id,
            response_tx,
        }
    }

    pub const fn store_block_data_request(
        header_id: HeaderId,
        parent_id: HeaderId,
        block: Bytes,
        events: Bytes,
        immutable_ids: BTreeMap<Slot, HeaderId>,
        response_tx: Sender<Result<(), String>>,
    ) -> Self {
        Self::StoreBlockData {
            header_id,
            parent_id,
            block,
            events,
            immutable_ids,
            response_tx,
        }
    }

    #[must_use]
    pub const fn get_block_parent_request(
        header_id: HeaderId,
        response_tx: Sender<Option<HeaderId>>,
    ) -> Self {
        Self::GetBlockParent {
            header_id,
            response_tx,
        }
    }

    #[must_use]
    pub const fn get_block_events_request(
        header_id: HeaderId,
        response_tx: Sender<Option<Bytes>>,
    ) -> Self {
        Self::GetBlockEvents {
            header_id,
            response_tx,
        }
    }

    #[must_use]
    pub const fn remove_block_request(
        header_id: HeaderId,
        response_tx: Sender<Option<Bytes>>,
    ) -> Self {
        Self::RemoveBlock {
            header_id,
            response_tx,
        }
    }

    #[must_use]
    pub const fn store_immutable_block_ids_request(
        ids: BTreeMap<Slot, HeaderId>,
        response_tx: Sender<Result<(), String>>,
    ) -> Self {
        Self::StoreImmutableBlockIds { ids, response_tx }
    }

    #[must_use]
    pub const fn get_immutable_block_id_request(
        slot: Slot,
        response_tx: Sender<Option<HeaderId>>,
    ) -> Self {
        Self::GetImmutableBlockId { slot, response_tx }
    }

    #[must_use]
    pub const fn scan_immutable_block_ids_request(
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
        response_tx: Sender<Vec<HeaderId>>,
    ) -> Self {
        Self::ScanImmutableBlockIds {
            slot_range,
            limit,
            response_tx,
        }
    }

    #[must_use]
    pub const fn store_transactions_request(transactions: HashMap<TxHash, Bytes>) -> Self {
        Self::StoreTransactions { transactions }
    }

    #[must_use]
    pub const fn get_transactions_request(
        tx_hashes: Vec<TxHash>,
        response_tx: Sender<Pin<Box<dyn Stream<Item = Bytes> + Send>>>,
    ) -> Self {
        Self::GetTransactions {
            tx_hashes,
            response_tx,
        }
    }

    #[must_use]
    pub const fn remove_transactions_request(tx_hashes: Vec<TxHash>) -> Self {
        Self::RemoveTransactions { tx_hashes }
    }
}
