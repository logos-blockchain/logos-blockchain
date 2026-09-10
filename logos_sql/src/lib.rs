//! `λSQL`: replicated `SQLite` state over Logos Blockchain.
//!
//! Applications read through a normal `SQLite` connection and submit replicated
//! writes through [`LogosSql::execute`]. One runtime task owns the zone
//! sequencer and database writer, so the SQL effects and pending publication
//! commit together before the payload is given to `ZoneSDK`.

mod applier;
mod db;
mod error;
mod functions;
mod logos_sql;
mod protocol;
mod runtime;
mod sql;

pub use error::Error;
pub use logos_sql::{LogosSql, LogosSqlConfig};
pub use protocol::TxId;
pub use rusqlite::types::ToSql;
pub use sql::TransactionBuilder;
