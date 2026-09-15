//! Current local write status and displacements awaiting application handling.

use crate::{TransactionBuilder, TxId, protocol::Transaction};

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
/// Pass this value to `LogosSql::mark_displacement_handled` after deciding how
/// to respond. Its private identity prevents an older response from handling
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

impl Displacement {
    /// Prepares the original SQL and bound parameters as a new write.
    ///
    /// The new write gets a fresh `TxId`; time and random functions run again.
    /// Only resubmit if the SQL is safe against the current database state,
    /// including if the original write returns after another reorganization.
    /// Ordinary execution remains blocked by unhandled displacements. Use
    /// [`crate::LogosSql::retry_displacement`] to retry before marking handled.
    pub fn transaction(&self) -> TransactionBuilder {
        TransactionBuilder::from_transaction(&self.transaction)
    }
}
