//! Current local write status and displacements awaiting application handling.

use crate::{TxId, protocol::Transaction};

/// The current status of a local write.
///
/// A displaced write can return after a reorganization. Finalized is terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteStatus {
    /// The write's SQL changes are present in `LIVE.db`.
    Live,
    /// The write's SQL changes have been removed from `LIVE.db`.
    Displaced,
    /// The write is part of finalized channel history.
    Finalized,
}

/// Why Logos SQL removed a local write. Neither reason implies SQL failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplacementReason {
    /// `ZoneSDK` reported the write orphaned from its channel history.
    Orphaned,
    /// Channel history changed underneath a pending local write.
    /// Logos SQL removed its optimistic effects without re-executing its SQL.
    PendingWriteInvalidated,
}

/// A displacement the application has not yet handled.
///
/// Retry through `LogosSql::retry_displacement`, or call
/// `LogosSql::mark_displacement_handled` to continue without retrying.
/// Its private identity prevents an older response from handling
/// a later displacement of the same write.
#[derive(Clone, Debug, PartialEq)]
pub struct Displacement {
    /// The local write whose effects were removed.
    pub tx_id: TxId,
    /// What caused its removal.
    pub reason: DisplacementReason,
    pub(crate) id: TxId,
    pub(crate) transaction: Transaction,
}
