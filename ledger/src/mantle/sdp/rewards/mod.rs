pub mod blend;
#[cfg(test)]
mod test_utils;

use std::collections::HashMap;

use lb_binary_codec::canonical::BinaryEncode as _;
use lb_core::{
    crypto::{Digest, Hash, Hasher},
    mantle::{
        Value,
        notes::{Note, derive_note_nonce},
    },
    sdp::{ActivityMetadata, Declaration, ProviderId, ServiceParameters, ServiceType},
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
    ) -> Result<Self, Error>;

    /// Update rewards state when epoch transition and calculate rewards to
    /// distribute.
    ///
    /// Called during epoch boundaries. Returns a map of `ProviderId` to
    /// reward amounts for providers eligible for rewards in this epoch
    /// transition.
    ///
    /// On a multi-epoch jump — when `last_epoch_state` is more than one
    /// epoch behind `next_epoch_state` — the implementation must transition
    /// to a state that rejects all activity messages, because it is too late
    /// to accept activity messages for the last epoch and we cannot verify
    /// activity proofs for the skipped epochs.
    /// Rewards already earned during the last epoch must still be distributed.
    ///
    /// The internal calculation logic is opaque to the SDP ledger and
    /// determined by the service-specific implementation.
    ///
    /// The epoch that ends is evaluated under `last_params`, the parameters of
    /// its era, and the new epoch is set up under `params`, those of its own.
    fn update_epoch(
        &self,
        // The state of epoch that just ended.
        last_epoch_state: &EpochState,
        // The state of the new epoch
        next_epoch_state: &EpochState,
        config: &ServiceParameters,
        last_params: &Self::Params,
        params: &Self::Params,
    ) -> (Self, Vec<Note>);

    #[must_use]
    fn add_income(&self, income: Value) -> Self;

    // The income earned in the current epoch, which `blend_rewards_pending` also
    // holds.
    fn pending_income(&self) -> Value;
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
/// The hash is computed from the service type and the epoch number, ensuring
/// all nodes produce identical transaction hashes for reward notes. Service
/// Reward Distribution: `hash(ServiceType || epoch_number)`, with `ServiceType`
/// its canonical one-byte discriminant.
fn create_reward_op_id(epoch: Epoch, service_type: ServiceType) -> Hash {
    let mut hasher = Hasher::default();
    let epoch_u8 = epoch.into_inner().to_le_bytes().to_vec();
    <Hasher as Digest>::update(&mut hasher, service_type.encode_to_vec());
    <Hasher as Digest>::update(&mut hasher, &epoch_u8);

    hasher.finalize().into()
}

/// The keys the rewards of a validator are created under: those of the
/// service note of its declaration, `zk_id` being only its Proof of Quota
/// identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RewardKeys {
    pub note_pk: ZkPublicKey,
    pub note_stake_pk: ZkPublicKey,
}

impl From<&Declaration> for RewardKeys {
    fn from(declaration: &Declaration) -> Self {
        Self {
            note_pk: declaration.note_pk,
            note_stake_pk: declaration.note_stake_pk,
        }
    }
}

/// Distributes rewards as notes, sorted by `zk_id` for determinism.
///
/// Creates reward notes that are:
/// - Deterministic: Sorted by `zk_id` in ascending order
/// - One note per `zk_id`, under the keys of its declaration
/// - Filters out 0-value rewards
fn distribute_rewards(
    rewards: HashMap<ZkPublicKey, (RewardAmount, RewardKeys)>,
    epoch: Epoch,
    service_type: ServiceType,
) -> Vec<Note> {
    let mut sorted_rewards: Vec<(ZkPublicKey, (RewardAmount, RewardKeys))> = rewards
        .into_iter()
        .filter(|(_, (amount, _))| *amount > 0)
        .collect();
    sorted_rewards.sort_by_key(|(zk_id, _)| *zk_id);

    let op_id = create_reward_op_id(epoch, service_type);

    sorted_rewards
        .into_iter()
        .enumerate()
        .map(|(output_number, (zk_id, (reward_amount, keys)))| Note {
            stake_public_key: keys.note_stake_pk,
            // The nonce is still derived with the zk_id the reward is counted for.
            ..Note::new(
                reward_amount,
                derive_note_nonce(&op_id, output_number, reward_amount, &zk_id),
                keys.note_pk,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use lb_groth16::Fr;

    use super::*;

    /// Service Reward Distribution: the `op_id` is `hash(ServiceType ||
    /// epoch_number)`, the one-byte discriminant of the service, then the 4
    /// bytes of the epoch.
    #[test]
    fn reward_op_id_hashes_the_one_byte_service_type_and_the_four_byte_epoch() {
        let mut hasher = Hasher::default();
        <Hasher as Digest>::update(&mut hasher, [0x00, 0x07, 0x00, 0x00, 0x00]);
        let expected: Hash = hasher.finalize().into();
        assert_eq!(
            create_reward_op_id(Epoch::new(7), ServiceType::BlendNetwork),
            expected
        );
    }

    #[test]
    fn reward_notes_go_to_the_keys_of_the_service_note_in_zk_id_order() {
        let low = ZkPublicKey::new(Fr::from(1u64));
        let high = ZkPublicKey::new(Fr::from(2u64));
        let unpaid = ZkPublicKey::new(Fr::from(3u64));
        let keys = |n: u64| RewardKeys {
            note_pk: ZkPublicKey::new(Fr::from(n)),
            note_stake_pk: ZkPublicKey::new(Fr::from(n + 1)),
        };
        let epoch = Epoch::new(7);
        let paid = distribute_rewards(
            HashMap::from([
                (high, (20, keys(20))),
                (unpaid, (0, keys(30))),
                (low, (10, keys(10))),
            ]),
            epoch,
            ServiceType::BlendNetwork,
        );
        let op_id = create_reward_op_id(epoch, ServiceType::BlendNetwork);
        // Under note_pk and note_stake_pk, the nonce derived with the zk_id.
        let note = |value, output_number, zk_id: &ZkPublicKey, keys: RewardKeys| Note {
            stake_public_key: keys.note_stake_pk,
            ..Note::new(
                value,
                derive_note_nonce(&op_id, output_number, value, zk_id),
                keys.note_pk,
            )
        };
        assert_eq!(
            paid,
            vec![note(10, 0, &low, keys(10)), note(20, 1, &high, keys(20))]
        );
    }
}
