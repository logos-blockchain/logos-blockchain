pub mod builder;
pub mod codec;
pub mod errors;
pub mod gas;
pub mod genesis_tx;
pub mod hash;
pub mod mantle_tx;
pub mod states;
pub mod tx;
pub mod verification_helper;

pub use builder::{MantleTxBuilder, TxBuilderError};
pub use errors::VerificationError;
pub use gas::{GENESIS_EXECUTION_GAS_PRICE, GENESIS_STORAGE_GAS_PRICE, GasPrices};
pub use genesis_tx::{CryptarchiaParameter, GenesisTime, GenesisTx};
pub use hash::TxHash;
use lb_utils::bounded::UpperBoundedVec;
pub use mantle_tx::{MantleTx, MantleTxContext, MantleTxGasContext};
pub use tx::{SignedMantleTx, VerifiedOps};
pub use verification_helper::OperationVerificationHelper;

use crate::mantle::{Op, OpProof};

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
pub type Ops = UpperBoundedVec<Op, MAX_OPS_PER_TX>;
pub type OpsProofs = UpperBoundedVec<OpProof, MAX_OPS_PER_TX>;
