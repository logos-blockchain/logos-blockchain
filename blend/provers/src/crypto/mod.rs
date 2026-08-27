use std::{num::NonZeroU64, sync::Arc};

use derivative::Derivative;
pub use lb_blend_message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader;
use lb_blend_proofs::quota::Quota;
use lb_key_management_system_keys::keys::X25519PrivateKey;
use rayon::ThreadPool;

pub mod core_and_leader;
pub use self::core_and_leader::{
    receive::EpochCryptographicProcessor as CoreAndLeaderReceiverOnlyEpochCryptographicProcessor,
    send::EpochCryptographicProcessor as CoreAndLeaderSenderOnlyEpochCryptographicProcessor,
    send_and_receive::EpochCryptographicProcessor as CoreAndLeaderSendAndReceiveEpochCryptographicProcessor,
};
pub mod leader;
pub use self::leader::send::EpochCryptographicProcessor as LeaderSenderOnlyEpochCryptographicProcessor;

#[cfg(test)]
mod test_utils;

#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct EpochCryptographicProcessorSettings {
    /// The non-ephemeral encryption key (NEK) derived from the secret key
    /// corresponding to the public key registered in the membership (SDP).
    #[derivative(Debug = "ignore")]
    pub non_ephemeral_encryption_key: X25519PrivateKey,
    /// `ß_c`: number of blending operations for each locally generated message.
    pub num_blend_layers: NonZeroU64,
    /// The dedicated thread pool the `PoW` puzzle search runs on, built once
    /// for the service and shared by every epoch's processor.
    pub pow_mining_pool: Arc<ThreadPool>,
    /// How much of this epoch's core quota has already been spent, counted in
    /// key indices. Zero for an epoch entered fresh; recovered from the
    /// persisted state when a restart lands part-way through one, so the
    /// generator resumes rather than replaying key nullifiers.
    pub spent_core_quota: Quota,
}
