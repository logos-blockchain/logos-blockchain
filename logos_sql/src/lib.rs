//! `λSQL`: replicated `SQLite` state over Logos Blockchain.
//!
//! Applications read through a normal `SQLite` connection and submit replicated
//! writes through [`LogosSql::query`]. One runtime task owns the zone sequencer
//! and database writer, so the SQL effects and pending publication record
//! commit together before the payload is given to `ZoneSDK`.

mod applier;
mod db;
mod error;
mod local_write;
mod logos_sql;
mod protocol;
mod runtime;
mod sql;

pub use error::Error;
pub use logos_sql::{LogosSql, LogosSqlConfig};
pub use protocol::{IdempotencyKey, TxId};
pub use rusqlite::types::ToSql;
pub use sql::{QueryBuilder, TransactionBuilder};
