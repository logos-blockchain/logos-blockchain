pub mod blend;
#[cfg(test)]
mod test_utils;

use std::collections::HashMap;

use lb_core::{
    codec::SerializeOp as _,
    crypto::{Digest, Hash, Hasher},
    mantle::{Note, Utxo, Value},
    sdp::{ActivityMetadata, ProviderId, ServiceParameters, ServiceType},
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::ZkPublicKey;
use thiserror::Error;

use crate::EpochState;

pub type RewardAmount = u64;

/// Generic trait for service-specific reward calculation.
///
/// Each service can implement its own rewards logic by implementing this trait.
/// The rewards object is updated with active messages and epoch transitions,
/// and can calculate expected rewards for each provider based on the service's
/// internal logic.
pub trait Rewards: Clone + PartialEq + Send + Sync + std::fmt::Debug {
    /// Service-specific reward parameters.
    type Params;

    /// Update rewards state when an active message is received.
    ///
    /// Called when a provider submits an active message with metadata
    /// (e.g., activity proofs containing opinions about other providers).
    fn update_active(
        &self,
        declaration_id: ProviderId,
        metadata: &ActivityMetadata,
        params: &Self::Params,
    ) -> Result<Self, Error>;

    /// Update rewards state when epoch transition and calculate rewards to
    /// distribute.
    ///
    /// Called during epoch boundaries. Returns a map of `ProviderId` to
    /// reward amounts for providers eligible for rewards in this epoch
    /// transition.
    ///
    /// The internal calculation logic is opaque to the SDP ledger and
    /// determined by the service-specific implementation.
    fn update_epoch(
        &self,
        // The state of epoch that just ended.
        last_epoch_state: &EpochState,
        // The state of the new epoch
        next_epoch_state: &EpochState,
        config: &ServiceParameters,
        params: &Self::Params,
    ) -> (Self, Vec<Utxo>);

    #[must_use]
    fn add_income(&self, income: Value) -> Self;
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("Target epoch is not set")]
    TargetEpochNotSet,
    #[error("Invalid epoch: expected {expected}, got {got}")]
    InvalidEpoch { expected: Epoch, got: Epoch },
    #[error("Invalid opinion length: expected {expected}, got {got}")]
    InvalidOpinionLength { expected: usize, got: usize },
    #[error("Duplicate active message for epoch {epoch}, provider {provider_id:?}")]
    DuplicateActiveMessage {
        epoch: Epoch,
        provider_id: Box<ProviderId>,
    },
    #[error("Invalid proof type")]
    InvalidProofType,
    #[error("Invalid proof")]
    InvalidProof,
    #[error("Hamming distance too large")]
    HammingDistanceTooLarge,
    #[error("Unknown provider: {0:?}")]
    UnknownProvider(Box<ProviderId>),
}

/// Creates a deterministic transaction hash for reward distribution.
///
/// The hash is computed from a version constant, epoch number, and service
/// type, ensuring all nodes produce identical transaction hashes for reward
/// notes.
fn create_reward_op_id(epoch: Epoch, service_type: ServiceType) -> Hash {
    let mut hasher = Hasher::default();
    let epoch_u8 = epoch.into_inner().to_le_bytes().to_vec();
    let service_type_u8 = service_type
        .to_bytes()
        .expect("conversion to bytes should succeed")
        .to_vec();
    <Hasher as Digest>::update(&mut hasher, &service_type_u8);
    <Hasher as Digest>::update(&mut hasher, &epoch_u8);

    hasher.finalize().into()
}

/// Distributes rewards as UTXOs, sorted by `zk_id` for determinism.
///
/// Creates reward notes that are:
/// - Deterministic: Sorted by `zk_id` in ascending order
/// - One note per `zk_id`
/// - Filters out 0-value rewards
fn distribute_rewards(
    rewards: HashMap<ZkPublicKey, RewardAmount>,
    epoch: Epoch,
    service_type: ServiceType,
) -> Vec<Utxo> {
    let mut sorted_rewards: Vec<(ZkPublicKey, RewardAmount)> = rewards
        .into_iter()
        .filter(|(_, amount)| *amount > 0)
        .collect();
    sorted_rewards.sort_by_key(|(zk_id, _)| *zk_id);

    let op_id = create_reward_op_id(epoch, service_type);

    sorted_rewards
        .into_iter()
        .enumerate()
        .map(|(output_index, (zk_id, reward_amount))| {
            Utxo::new(op_id, output_index, Note::new(reward_amount, zk_id))
        })
        .collect()
}
