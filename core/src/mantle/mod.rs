pub mod channel;
pub mod gas;
pub mod ledger;
pub mod mock;
pub mod ops;
pub mod traits;
pub mod transactions;

mod channel_notes;
mod fixtures;

pub use gas::{GasCalculator, GasConstants};
pub use ledger::{Note, NoteId, Utxo, Value};
pub use ops::{Op, OpProof};
pub use transactions::{
    CryptarchiaParameter, GenesisTime, SignedMantleTx, hash::TxHash, mantle_tx::MantleTx,
};

pub use crate::mantle::transactions::VerificationError;

pub const MAX_MANTLE_TXS: usize = 1024;
