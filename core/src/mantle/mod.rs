pub mod channel;
mod channel_notes;
pub mod gas;
pub mod ledger;
pub mod mock;
pub mod nom;
pub mod ops;
pub mod traits;
pub mod transactions;

pub use gas::{GasCalculator, GasConstants};
pub use ledger::{Note, NoteId, Utxo, Value};
pub use ops::{Op, OpProof};
pub use transactions::{CryptarchiaParameter, GenesisTime, hash::TxHash, mantle_tx::MantleTx};

pub use crate::mantle::transactions::{SignedMantleTx, VerificationError};

pub const MAX_MANTLE_TXS: usize = 1024;
