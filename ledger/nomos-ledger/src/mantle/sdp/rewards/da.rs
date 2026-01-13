use std::collections::{BTreeMap, HashMap};

use key_management_system_keys::keys::ZkPublicKey;
use nomos_core::{
    block::BlockNumber,
    mantle::Utxo,
    sdp::{ActivityMetadata, ProviderId, ServiceParameters, ServiceType},
};
use rpds::{HashTrieMapSync, HashTrieSetSync};

use crate::{
    EpochState,
    mantle::sdp::{
        SessionState,
        rewards::{Error, distribute_rewards},
    },
};

const ACTIVITY_THRESHOLD: u64 = 2;

/// Data Availability rewards implementation based on opinion-based peer
/// evaluation.
///
/// Implements the `NomosDA` Rewarding specification where providers submit
/// activity proofs containing opinions about peer service quality, and rewards
/// are distributed based on accumulated positive opinions exceeding a
/// threshold.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewards {
    current_opinions: HashTrieMapSync<ZkPublicKey, usize>,
    past_opinions: HashTrieMapSync<ZkPublicKey, usize>,
    recorded_messages: HashTrieSetSync<ProviderId>, // avoid processing duplicate opinions
    // naming as in the spec, current session is s-1 if s is the session at this which this
    // message was sent
    // current rewarding session s - 1  (s is active session)
    current_session: SessionState,
    // previous rewarding session s - 2 (s is active session)
    prev_session: SessionState,
}

impl Default for Rewards {
    fn default() -> Self {
        Self::new() // Default to 2 (same as previous ACTIVITY_THRESHOLD constant)
    }
}

impl Rewards {
    /// Create a new [`Rewards`] instance with the specified opinion threshold
    #[must_use]
    pub fn new() -> Self {
        Self {
            current_opinions: HashTrieMapSync::new_sync(),
            past_opinions: HashTrieMapSync::new_sync(),
            recorded_messages: HashTrieSetSync::new_sync(),
            current_session: SessionState {
                declarations: rpds::RedBlackTreeMapSync::new_sync(),
                session_n: 0,
            },
            prev_session: SessionState {
                declarations: rpds::RedBlackTreeMapSync::new_sync(),
                session_n: 0,
            },
        }
    }

    fn parse_opinions(opinions: &[u8], n_validators: usize) -> Result<Vec<bool>, Error> {
        let expected_current_len = Self::calculate_opinion_vector_length(n_validators);
        if opinions.len() != expected_current_len {
            return Err(Error::InvalidOpinionLength {
                expected: expected_current_len,
                got: opinions.len(),
            });
        }

        // * self opinion is not checked
        // * we only check opinions up to the number of validators, without checking the
        //   rest of the opinion bits are zero

        Ok((0..n_validators)
            .map(|i| Self::get_opinion_bit(opinions, i))
            .collect())
    }

    /// Calculate expected byte length for opinion vector: ⌈log₂(Ns + 1) / 8⌉
    const fn calculate_opinion_vector_length(node_count: usize) -> usize {
        let bits_needed = (node_count + 1).next_power_of_two().trailing_zeros() as usize;
        bits_needed.div_ceil(8)
    }

    /// Get opinion bit at index i (little-endian encoding)
    fn get_opinion_bit(opinions: &[u8], index: usize) -> bool {
        let byte_index = index / 8;
        let bit_index = index % 8;
        if byte_index >= opinions.len() {
            return false;
        }
        (opinions[byte_index] & (1 << bit_index)) != 0
    }
}

impl super::Rewards for Rewards {
    fn update_active(
        &self,
        provider_id: ProviderId,
        metadata: &ActivityMetadata,
        _block_number: BlockNumber,
    ) -> Result<Self, Error> {
        // Extract DA activity proof from metadata
        let ActivityMetadata::DataAvailability(proof) = metadata else {
            return Err(Error::InvalidProofType);
        };

        if self.recorded_messages.contains(&provider_id) {
            return Err(Error::DuplicateActiveMessage {
                session: proof.current_session,
                provider_id: Box::new(provider_id),
            });
        }

        if proof.current_session != self.current_session.session_n {
            return Err(Error::InvalidSession {
                expected: self.current_session.session_n,
                got: proof.current_session,
            });
        }

        // Process current session opinions
        let mut current_provider_to_zk_id = <BTreeMap<_, Vec<_>>>::new();
        for declaration in self.current_session.declarations.values() {
            current_provider_to_zk_id
                .entry(declaration.provider_id)
                .or_default()
                .push(declaration.zk_id);
        }
        let current_provider_to_zk_id = current_provider_to_zk_id.values().collect::<Vec<_>>();
        let n_validators_current = current_provider_to_zk_id.len();
        let current_opinions =
            Self::parse_opinions(&proof.current_session_opinions, n_validators_current)?;

        let mut new_current_opinions = self.current_opinions.clone();
        for (i, &opinion) in current_opinions.iter().enumerate() {
            if opinion && let Some(zk_ids) = current_provider_to_zk_id.get(i) {
                for zk_id in *zk_ids {
                    let count = new_current_opinions.get(zk_id).copied().unwrap_or(0);
                    new_current_opinions = new_current_opinions.insert(*zk_id, count + 1);
                }
            }
        }

        // Process previous session opinions
        let mut previous_provider_to_zk_id = <BTreeMap<_, Vec<_>>>::new();
        for declaration in self.prev_session.declarations.values() {
            previous_provider_to_zk_id
                .entry(declaration.provider_id)
                .or_default()
                .push(declaration.zk_id);
        }
        let previous_provider_to_zk_id = previous_provider_to_zk_id.values().collect::<Vec<_>>();
        let n_validators_prev = previous_provider_to_zk_id.len();
        let past_opinions =
            Self::parse_opinions(&proof.previous_session_opinions, n_validators_prev)?;

        let mut new_past_opinions = self.past_opinions.clone();
        for (i, &opinion) in past_opinions.iter().enumerate() {
            if opinion && let Some(zk_ids) = previous_provider_to_zk_id.get(i) {
                for zk_id in *zk_ids {
                    let count = new_past_opinions.get(zk_id).copied().unwrap_or(0);
                    new_past_opinions = new_past_opinions.insert(*zk_id, count + 1);
                }
            }
        }

        let new_recorded_messages = self.recorded_messages.insert(provider_id);

        Ok(Self {
            current_opinions: new_current_opinions,
            past_opinions: new_past_opinions,
            recorded_messages: new_recorded_messages,
            current_session: self.current_session.clone(),
            prev_session: self.prev_session.clone(),
        })
    }

    fn update_session(
        &self,
        last_active: &SessionState,
        _next_session_first_epoch_state: &EpochState,
        _config: &ServiceParameters,
    ) -> (Self, Vec<Utxo>) {
        // Calculate activity threshold: θ = Ns / ACTIVITY_THRESHOLD
        let active_threshold = self.current_session.declarations.size() as u64 / ACTIVITY_THRESHOLD;
        let past_threshold = self.prev_session.declarations.size() as u64 / ACTIVITY_THRESHOLD;

        // TODO: Calculate base rewards when session_income is added to config
        // For now using placeholder value of 0
        let session_income = 0;

        // Calculate base rewards
        let active_base_reward = if self.current_session.declarations.is_empty() {
            0
        } else {
            session_income / self.current_session.declarations.size() as u64
        };

        let past_base_reward = if self.prev_session.declarations.is_empty() {
            0
        } else {
            session_income / self.prev_session.declarations.size() as u64
        };

        let mut rewards = HashMap::new();
        // Distribute rewards for current session
        for (zk_id, &opinion_count) in self.current_opinions.iter() {
            if (opinion_count as u64) >= active_threshold {
                let reward = active_base_reward / 2; // half reward
                *rewards.entry(*zk_id).or_insert(0) += reward;
            }
        }

        // Distribute rewards for previous session
        for (zk_id, &opinion_count) in self.past_opinions.iter() {
            if (opinion_count as u64) >= past_threshold {
                let reward = past_base_reward / 2; // half reward
                *rewards.entry(*zk_id).or_insert(0) += reward;
            }
        }

        // Create new rewards state with updated sessions
        let new_state = Self {
            current_opinions: HashTrieMapSync::new_sync(), // Reset for new session
            past_opinions: HashTrieMapSync::new_sync(),    // Move current to past
            recorded_messages: HashTrieSetSync::new_sync(), // Reset recorded messages
            current_session: last_active.clone(),
            prev_session: self.current_session.clone(),
        };

        (
            new_state,
            distribute_rewards(
                rewards,
                last_active.session_n,
                ServiceType::DataAvailability,
            ),
        )
    }

    fn update_epoch(&self, _epoch_state: &EpochState) -> Self {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use nomos_core::sdp::da;

    use super::*;
    use crate::mantle::sdp::rewards::{
        Rewards as _,
        test_utils::{create_provider_id, create_test_session_state, dummy_epoch_state},
    };

    #[test]
    fn test_calculate_opinion_vector_length() {
        // 0 nodes: 0 bytes
        assert_eq!(Rewards::calculate_opinion_vector_length(0), 0);

        // 1-2 nodes: need 2 bits -> 1 byte
        assert_eq!(Rewards::calculate_opinion_vector_length(1), 1);
        assert_eq!(Rewards::calculate_opinion_vector_length(2), 1);

        // 3-4 nodes: need 3 bits -> 1 byte
        assert_eq!(Rewards::calculate_opinion_vector_length(3), 1);
        assert_eq!(Rewards::calculate_opinion_vector_length(4), 1);

        // 5-8 nodes: need 4 bits -> 1 byte
        assert_eq!(Rewards::calculate_opinion_vector_length(8), 1);

        // 9-16 nodes: need 5 bits -> 1 byte
        assert_eq!(Rewards::calculate_opinion_vector_length(16), 1);

        // 17-32 nodes: need 6 bits -> 1 byte
        assert_eq!(Rewards::calculate_opinion_vector_length(32), 1);

        // 33-64 nodes: need 7 bits -> 1 byte
        assert_eq!(Rewards::calculate_opinion_vector_length(64), 1);

        // 65-128 nodes: need 8 bits -> 1 byte
        assert_eq!(Rewards::calculate_opinion_vector_length(128), 1);

        // 129-256 nodes: need 9 bits -> 2 bytes
        assert_eq!(Rewards::calculate_opinion_vector_length(256), 2);
    }

    #[test]
    fn test_get_opinion_bit() {
        let opinions = vec![0b1011_0100, 0b0000_0011];

        // First byte: bits 0-7
        assert!(!Rewards::get_opinion_bit(&opinions, 0)); // 0
        assert!(!Rewards::get_opinion_bit(&opinions, 1)); // 0
        assert!(Rewards::get_opinion_bit(&opinions, 2)); // 1
        assert!(!Rewards::get_opinion_bit(&opinions, 3)); // 0
        assert!(Rewards::get_opinion_bit(&opinions, 4)); // 1
        assert!(Rewards::get_opinion_bit(&opinions, 5)); // 1
        assert!(!Rewards::get_opinion_bit(&opinions, 6)); // 0
        assert!(Rewards::get_opinion_bit(&opinions, 7)); // 1

        // Second byte: bits 8-15
        assert!(Rewards::get_opinion_bit(&opinions, 8)); // 1
        assert!(Rewards::get_opinion_bit(&opinions, 9)); // 1
        assert!(!Rewards::get_opinion_bit(&opinions, 10)); // 0

        // Out of bounds
        assert!(!Rewards::get_opinion_bit(&opinions, 100));
    }

    #[test]
    fn test_rewards_with_no_activity_proofs() {
        let provider1 = create_provider_id(1);
        let provider2 = create_provider_id(2);

        // Create active session with providers
        let active_session =
            create_test_session_state(&[provider1, provider2], ServiceType::DataAvailability, 1);

        // Initialize rewards tracker with the active session
        let rewards_tracker = Rewards {
            current_opinions: HashTrieMapSync::new_sync(),
            past_opinions: HashTrieMapSync::new_sync(),
            recorded_messages: HashTrieSetSync::new_sync(),
            current_session: active_session.clone(),
            prev_session: SessionState {
                declarations: rpds::RedBlackTreeMapSync::new_sync(),
                session_n: 0,
            },
        };

        let config = ServiceParameters {
            lock_period: 10,
            inactivity_period: 20,
            retention_period: 100,
            timestamp: 0,
            session_duration: 10,
        };

        let (_new_state, rewards) =
            rewards_tracker.update_session(&active_session, &dummy_epoch_state(), &config);

        // No activity proofs submitted, so no rewards
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    #[ignore = "TODO: Re-enable when session_income is implemented (currently hardcoded to 0)"]
    fn test_rewards_basic_calculation() {
        // Create 4 providers with different zk_ids
        let provider1 = create_provider_id(1);
        let provider2 = create_provider_id(2);
        let provider3 = create_provider_id(3);
        let provider4 = create_provider_id(4);

        let providers = vec![provider1, provider2, provider3, provider4];
        let active_session =
            create_test_session_state(&providers, ServiceType::DataAvailability, 1);

        // Initialize rewards tracker with the active session
        let mut rewards_tracker = Rewards {
            current_opinions: HashTrieMapSync::new_sync(),
            past_opinions: HashTrieMapSync::new_sync(),
            recorded_messages: HashTrieSetSync::new_sync(),
            current_session: active_session.clone(),
            prev_session: SessionState {
                declarations: rpds::RedBlackTreeMapSync::new_sync(),
                session_n: 0,
            },
        };

        // Helper to create opinion vector with all positive opinions
        let create_all_positive = || vec![0b0000_1111u8]; // All 4 providers positive

        // Provider 1 submits: positive about all
        let proof1 = da::ActivityProof {
            current_session: 1,
            previous_session_opinions: vec![],
            current_session_opinions: create_all_positive(),
        };
        rewards_tracker = rewards_tracker
            .update_active(provider1, &ActivityMetadata::DataAvailability(proof1), 10)
            .unwrap();

        // Provider 2 submits: positive about first 3 (bits 0, 1, 2)
        let proof2 = da::ActivityProof {
            current_session: 1,
            previous_session_opinions: vec![],
            current_session_opinions: vec![0b0000_0111u8],
        };
        rewards_tracker = rewards_tracker
            .update_active(provider2, &ActivityMetadata::DataAvailability(proof2), 10)
            .unwrap();

        // Provider 3 submits: positive about all
        let proof3 = da::ActivityProof {
            current_session: 1,
            previous_session_opinions: vec![],
            current_session_opinions: create_all_positive(),
        };
        rewards_tracker = rewards_tracker
            .update_active(provider3, &ActivityMetadata::DataAvailability(proof3), 10)
            .unwrap();

        let config = ServiceParameters {
            lock_period: 10,
            inactivity_period: 20,
            retention_period: 100,
            timestamp: 0,
            session_duration: 10,
        };

        let (_new_state, reward_utxos) =
            rewards_tracker.update_session(&active_session, &dummy_epoch_state(), &config);

        // Calculate expected rewards dynamically (works for any session_income)
        let session_income = 0; // Currently hardcoded in implementation
        let _active_threshold = providers.len() as u64 / ACTIVITY_THRESHOLD; // 4 / 2 = 2
        let base_reward = if providers.is_empty() {
            0
        } else {
            session_income / providers.len() as u64
        };
        let half_reward = base_reward / 2;

        // Opinion counts: provider0=3, provider1=2, provider2=3, provider3=2
        // All meet threshold of 2, so all 4 should get rewards
        assert_eq!(
            reward_utxos.len(),
            4,
            "All 4 providers should receive rewards"
        );

        // Verify each UTXO has expected structure
        for utxo in &reward_utxos {
            assert_eq!(
                utxo.note.value, half_reward,
                "Reward amount should match calculation"
            );
            // UTXOs should be sorted by zk_id and have sequential output
            // indices
        }

        // Verify total rewards distributed
        let total_rewards: u64 = reward_utxos.iter().map(|u| u.note.value).sum();
        assert_eq!(total_rewards, half_reward * 4);

        // Verify UTXOs are sorted by zk_id (deterministic ordering)
        for i in 1..reward_utxos.len() {
            assert!(
                reward_utxos[i - 1].note.pk <= reward_utxos[i].note.pk,
                "UTXOs should be sorted by zk_id"
            );
        }
    }

    #[test]
    #[ignore = "TODO: Re-enable when session_income is implemented (currently hardcoded to 0)"]
    fn test_rewards_with_previous_session() {
        let provider1 = create_provider_id(1);
        let provider2 = create_provider_id(2);

        // Set up sessions: provider1 in both, provider2 only in current
        let current_session =
            create_test_session_state(&[provider1, provider2], ServiceType::DataAvailability, 1);
        let prev_session =
            create_test_session_state(&[provider1], ServiceType::DataAvailability, 0);

        // Extract zk_ids for verification
        let current_zk_ids: Vec<ZkPublicKey> = current_session
            .declarations
            .values()
            .map(|d| d.zk_id)
            .collect();
        let provider1_zk_id = current_zk_ids[0];
        let provider2_zk_id = current_zk_ids[1];

        // Initialize rewards tracker with both sessions
        let mut rewards_tracker = Rewards {
            current_opinions: HashTrieMapSync::new_sync(),
            past_opinions: HashTrieMapSync::new_sync(),
            recorded_messages: HashTrieSetSync::new_sync(),
            current_session: current_session.clone(),
            prev_session: prev_session.clone(),
        };

        // Provider 1 submits opinions for current session
        let proof1 = da::ActivityProof {
            current_session: 1,
            previous_session_opinions: vec![0b0000_0001u8], // Positive about provider1 in prev
            current_session_opinions: vec![0b0000_0011u8],  // Positive about both in current
        };
        rewards_tracker = rewards_tracker
            .update_active(provider1, &ActivityMetadata::DataAvailability(proof1), 10)
            .unwrap();

        // Provider 2 submits opinions for current session only
        let proof2 = da::ActivityProof {
            current_session: 1,
            previous_session_opinions: vec![0b0000_0001u8], // Positive about provider1 in prev
            current_session_opinions: vec![0b0000_0011u8],  // Positive about both in current
        };
        rewards_tracker = rewards_tracker
            .update_active(provider2, &ActivityMetadata::DataAvailability(proof2), 10)
            .unwrap();

        let config = ServiceParameters {
            lock_period: 10,
            inactivity_period: 20,
            retention_period: 100,
            timestamp: 0,
            session_duration: 10,
        };

        let (_new_state, reward_utxos) =
            rewards_tracker.update_session(&current_session, &dummy_epoch_state(), &config);

        // Calculate expected rewards dynamically
        let session_income = 0; // Currently hardcoded
        let _current_threshold = current_session.declarations.size() as u64 / ACTIVITY_THRESHOLD; // 2/2 = 1
        let _prev_threshold = prev_session.declarations.size() as u64 / ACTIVITY_THRESHOLD; // 1/2 = 0

        let current_base_reward = if current_session.declarations.is_empty() {
            0
        } else {
            session_income / current_session.declarations.size() as u64
        };
        let prev_base_reward = if prev_session.declarations.is_empty() {
            0
        } else {
            session_income / prev_session.declarations.size() as u64
        };

        // Both providers get rewards for current session (both have 2 opinions >=
        // threshold 1) Provider 1 also gets rewards for previous session (2
        // opinions >= threshold 0) So provider1 gets current_half + prev_half,
        // provider2 gets current_half
        let current_half = current_base_reward / 2;
        let prev_half = prev_base_reward / 2;

        // Both providers should be in rewards (both met threshold in current session)
        assert_eq!(
            reward_utxos.len(),
            2,
            "Both providers should receive rewards"
        );

        // Find rewards by zk_id
        let provider1_utxo = reward_utxos
            .iter()
            .find(|u| u.note.pk == provider1_zk_id)
            .expect("Provider1 should have reward UTXO");
        let provider2_utxo = reward_utxos
            .iter()
            .find(|u| u.note.pk == provider2_zk_id)
            .expect("Provider2 should have reward UTXO");

        // Verify amounts (provider1 gets from both sessions, provider2 only from
        // current)
        let expected_provider1_reward = current_half + prev_half;
        let expected_provider2_reward = current_half;

        assert_eq!(
            provider1_utxo.note.value, expected_provider1_reward,
            "Provider1 should get rewards from both sessions"
        );
        assert_eq!(
            provider2_utxo.note.value, expected_provider2_reward,
            "Provider2 should get rewards from current session only"
        );

        // When session_income > 0, provider1 should have more rewards
        // For now with income=0, both are 0
        if session_income > 0 {
            assert!(
                provider1_utxo.note.value > provider2_utxo.note.value,
                "Provider1 should have more total rewards"
            );
        }
    }
}
