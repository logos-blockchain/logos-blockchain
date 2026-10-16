use std::sync::Arc;

use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use rayon::ThreadPool;

/// Era-agnostic components derived from the provided settings at service
/// startup.
#[derive(Clone)]
pub struct RuntimeComponents {
    /// The non-ephemeral signing key, exfiltrated from the KMS.
    // TODO: This will go once we do not need to pass the secret key anymore,
    // i.e., when we have libp2p integration with KMS.
    pub non_ephemeral_signing_key: UnsecuredEd25519Key,
    /// The pool the `PoW` puzzles of every epoch are searched on.
    pub pow_mining_pool: Arc<ThreadPool>,
}
