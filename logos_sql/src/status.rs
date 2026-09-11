//! Local write statuses and notifications of status changes.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures_util::Stream;
use tokio::sync::broadcast;
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};

use crate::TxId;

/// The current status of a local write.
///
/// A displaced write can become live again if a channel reorganization restores
/// it. Once a write is [`Self::Finalized`], its status cannot change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteStatus {
    /// The write's SQL changes are present in `LIVE.db`.
    Live,
    /// The write's SQL changes have been removed from `LIVE.db`.
    Displaced,
    /// The write is part of finalized channel history.
    Finalized,
}

/// A notification that a local write's status changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteStatusChange {
    /// The transaction ID returned when the write was submitted.
    pub tx_id: TxId,
    /// Current status after the change.
    pub status: WriteStatus,
}

impl WriteStatusChange {
    pub(crate) const fn new(tx_id: TxId, status: WriteStatus) -> Self {
        Self { tx_id, status }
    }
}

/// Error reported when the application reads status notifications too slowly.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WriteStatusChangesError {
    /// The number of notifications dropped before the application read them.
    #[error("missed {0} write status changes; query the current status")]
    Lagged(u64),
}

/// A stream of status changes for local writes.
///
/// Notifications are kept in memory and are lost on restart. Reading too
/// slowly drops older notifications and produces
/// [`Lagged`](WriteStatusChangesError::Lagged).
/// Call [`LogosSql::write_status`](crate::LogosSql::write_status) for each
/// write you are tracking to get its latest saved status.
pub struct WriteStatusChanges {
    inner: BroadcastStream<WriteStatusChange>,
}

impl WriteStatusChanges {
    pub(crate) fn new(receiver: broadcast::Receiver<WriteStatusChange>) -> Self {
        Self {
            inner: BroadcastStream::new(receiver),
        }
    }
}

impl Stream for WriteStatusChanges {
    type Item = Result<WriteStatusChange, WriteStatusChangesError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(context) {
            Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(missed)))) => {
                Poll::Ready(Some(Err(WriteStatusChangesError::Lagged(missed))))
            }
            Poll::Ready(Some(Ok(change))) => Poll::Ready(Some(Ok(change))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
