//! `λSQL`: replicated `SQLite` state over a Logos zone channel.
//!
//! Applications read through a normal `SQLite` connection and submit replicated
//! writes through [`Lsql::query`]. One runtime task owns the zone sequencer
//! and database writer, so the SQL effects and pending publication record
//! commit together before the payload is given to `ZoneSDK`.

mod applier;
mod db;
mod error;
mod local_write;
mod lsql;
mod protocol;
mod runtime;

pub use error::Error;
pub use lsql::{Lsql, LsqlConfig, TransactionBuilder};
pub use protocol::{IdempotencyKey, TxId};
pub use rusqlite::types::ToSql;
