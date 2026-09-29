//! `λSQL`: replicated `SQLite` state over Logos Blockchain.
//!
//! Applications read through a normal `SQLite` connection and submit replicated
//! writes through [`LogosSql::execute`]. One runtime task owns the zone
//! sequencer and database writer, so the SQL effects and pending publication
//! commit together before the payload is given to `ZoneSDK`.
//!
//! Application transactions share a fixed SQLite execution budget across their
//! statements. Exceeding it rolls back the transaction; channel replay records
//! the rejection and continues. This is a work limit, not a wall-clock timeout
//! or a total memory/disk quota.

mod applier;
mod db;
mod error;
mod functions;
mod logos_sql;
mod protocol;
mod runtime;
mod sql;
mod status;

pub use error::Error;
pub use logos_sql::{LogosSql, LogosSqlConfig, WriterConfig};
pub use protocol::TxId;
pub use rusqlite::types::ToSql;
pub use sql::TransactionBuilder;
pub use status::{Displacement, DisplacementReason, WriteStatus};
