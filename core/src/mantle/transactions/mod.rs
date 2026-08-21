pub mod builder;
pub mod codec;
pub mod errors;
pub mod gas;
pub mod genesis_tx;
pub mod hash;
pub mod states;
pub mod tx_list;
pub mod verification_helper;
pub mod verified_ops;

use std::sync::LazyLock;

pub use builder::{MantleTxBuilder, TxBuilderError};
pub use errors::VerificationError;
pub use gas::{GENESIS_EXECUTION_GAS_PRICE, GENESIS_STORAGE_GAS_PRICE, GasPrices};
pub use genesis_tx::{CryptarchiaParameter, GenesisTime, GenesisTx};
pub use hash::TxHash;
pub use tx_list::{OpProofRefs, OpProofs, OpRefs, Ops, SignedOps, TxBoundedVec, TxList};
pub use verification_helper::OperationVerificationHelper;
pub use verified_ops::VerifiedOperations;

pub(crate) static MANTLE_TX_HASH_V1_BYTES: LazyLock<Vec<u8>> =
    LazyLock::new(|| b"MANTLE_TXHASH_V1".to_vec());

// ==============================================================================
// Memory Safety Limits
// ==============================================================================
// These limits are not designed to mimic system limits, but rather to prevent
// unbounded memory usage from malicious inputs. They prevent memory
// over-allocation attacks where untrusted input specifies allocation sizes.
// Values are chosen to not limit normal operations while preventing excessive
// memory usage (e.g., 68GB allocation). As an example, if the network currently
// limits maximum transaction size to 1MiB, for memory safety limits we can
// allow 4MiB.
pub const MAX_OPS_PER_TX: usize = u8::MAX as usize;
