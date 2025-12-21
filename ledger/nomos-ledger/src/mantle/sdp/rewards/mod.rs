pub mod blend;
pub mod da;
#[cfg(test)]
mod test_utils;

use std::collections::HashMap;

use groth16::{Fr, fr_from_bytes};
use key_management_system_keys::keys::ZkPublicKey;
use nomos_core::{
    block::BlockNumber,
    crypto::{ZkDigest, ZkHasher},
    mantle::{Note, TxHash, Utxo},
    sdp::{ActivityMetadata, ProviderId, ServiceParameters, ServiceType, SessionNumber},
};
use thiserror::Error;

use super::SessionState;
use crate::EpochState;

pub type RewardAmount = u64;

/// Generic trait for service-specific reward calculation.
///
/// Each service can implement its own rewards logic by implementing this trait.
/// The rewards object is updated with active messages and session transitions,
/// and can calculate expected rewards for each provider based on the service's
/// internal logic.
pub trait Rewards: Clone + PartialEq + Send + Sync + std::fmt::Debug {
    /// Update rewards state when an active message is received.
    ///
    /// Called when a provider submits an active message with metadata
    /// (e.g., activity proofs containing opinions about other providers).
    fn update_active(
        &self,
        declaration_id: ProviderId,
        metadata: &ActivityMetadata,
        block_number: BlockNumber,
    ) -> Result<Self, Error>;

    /// Update rewards state when sessions transition and calculate rewards to
    /// distribute.
    ///
    /// Called during session boundaries when active, `past_session`, and
    /// forming sessions are updated. Returns a map of `ProviderId` to
    /// reward amounts for providers eligible for rewards in this session
    /// transition.
    ///
    /// The internal calculation logic is opaque to the SDP ledger and
    /// determined by the service-specific implementation.
    ///
    /// # Arguments
    /// * `last_active` - The state of the session that just ended.
    /// * `next_session_first_epoch_state` - The epoch state corresponding to
    ///   the 1st block of the session `last_active + 1`.
    fn update_session(
        &self,
        last_active: &SessionState,
        next_session_first_epoch_state: &EpochState,
        config: &ServiceParameters,
    ) -> (Self, Vec<Utxo>);

    /// Update rewards state when a new epoch begins while the session remains
    /// unchanged.
    ///
    /// If the epoch has already been processed previously, this method performs
    /// no update and returns the current state unchanged.
    #[must_use]
    fn update_epoch(&self, epoch_state: &EpochState) -> Self;
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("Target session is not set")]
    TargetSessionNotSet,
    #[error("Invalid session: expected {expected}, got {got}")]
    InvalidSession {
        expected: SessionNumber,
        got: SessionNumber,
    },
    #[error("Invalid opinion length: expected {expected}, got {got}")]
    InvalidOpinionLength { expected: usize, got: usize },
    #[error("Duplicate active message for session {session}, provider {provider_id:?}")]
    DuplicateActiveMessage {
        session: SessionNumber,
        provider_id: Box<ProviderId>,
    },
    #[error("Invalid proof type")]
    InvalidProofType,
    #[error("Invalid proof")]
    InvalidProof,
    #[error("Unknown provider: {0:?}")]
    UnknownProvider(Box<ProviderId>),
}

/// Creates a deterministic transaction hash for reward distribution.
///
/// The hash is computed from a version constant, session number, and service
/// type, ensuring all nodes produce identical transaction hashes for reward
/// notes.
fn create_reward_tx_hash(session_n: SessionNumber, service_type: ServiceType) -> TxHash {
    let mut hasher = ZkHasher::default();
    let session_fr = Fr::from(session_n);
    let service_type_fr = fr_from_bytes(service_type.as_ref().as_bytes())
        .expect("Valid service type fr representation");
    <ZkHasher as ZkDigest>::update(&mut hasher, &service_type_fr);
    <ZkHasher as ZkDigest>::update(&mut hasher, &session_fr);

    TxHash(hasher.finalize())
}

/// Distributes rewards as UTXOs, sorted by `zk_id` for determinism.
///
/// Creates reward notes that are:
/// - Deterministic: Sorted by `zk_id` in ascending order
/// - One note per `zk_id`
/// - Filters out 0-value rewards
fn distribute_rewards(
    rewards: HashMap<ZkPublicKey, RewardAmount>,
    session_n: SessionNumber,
    service_type: ServiceType,
) -> Vec<Utxo> {
    let mut sorted_rewards: Vec<(ZkPublicKey, RewardAmount)> = rewards
        .into_iter()
        .filter(|(_, amount)| *amount > 0)
        .collect();
    sorted_rewards.sort_by_key(|(zk_id, _)| *zk_id);

    let tx_hash = create_reward_tx_hash(session_n, service_type);

    sorted_rewards
        .into_iter()
        .enumerate()
        .map(|(output_index, (zk_id, reward_amount))| {
            Utxo::new(tx_hash, output_index, Note::new(reward_amount, zk_id))
        })
        .collect()
}
