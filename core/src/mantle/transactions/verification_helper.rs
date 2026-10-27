use std::num::NonZeroU64;

use lb_cryptarchia_engine::{Epoch, Slot};
use lb_key_management_system_keys::keys::UnverifiedEd25519PublicKey;
use rpds::HashTrieMapSync;

use crate::{
    block::BlockNumber,
    crypto::{Hash, ZkHash},
    mantle::{
        VerificationError,
        channel::Channels,
        ledger::{DeclarationKeys, Declarations},
        note_set::NoteSets,
        ops::{
            channel::{ChannelId, ChannelKeyIndex},
            pow::{PowNullifier, PowReward, PowTarget},
        },
        programs::Programs,
    },
    sdp::{DeclarationId, MinStake, ServiceType},
};

pub trait OperationVerificationHelper {
    fn get_channels(&self) -> &Channels;

    fn get_note_sets(&self) -> &NoteSets;

    fn get_programs(&self) -> &Programs;

    fn get_block_number(&self) -> BlockNumber;

    fn get_declarations_by_service(
        &self,
        service: ServiceType,
    ) -> Result<&Declarations, VerificationError>;

    /// The keys of the declarations [`Self::get_declarations_by_service`]
    /// returns.
    fn get_declaration_keys_by_service(
        &self,
        service: ServiceType,
    ) -> Result<&DeclarationKeys, VerificationError>;

    fn get_declarations_by_id(
        &self,
        id: &DeclarationId,
    ) -> Result<&Declarations, VerificationError>;

    fn get_min_stake(&self) -> &MinStake;

    fn get_epoch(&self) -> Epoch;

    fn get_block_slot(&self) -> Slot;

    fn get_key_from_channel_at_index(
        &self,
        channel_id: &ChannelId,
        key_index: &ChannelKeyIndex,
    ) -> Result<UnverifiedEd25519PublicKey, VerificationError>;

    // `PoW` claim validation inputs, one per
    // [`ClaimPoWRewardVerificationContext`] field. The current block slot comes
    // from [`Self::get_block_slot`].
    //
    // [`ClaimPoWRewardVerificationContext`]: crate::mantle::ops::pow::ClaimPoWRewardVerificationContext

    /// `d_reward`: the reward difficulty a puzzle ticket must be strictly
    /// below.
    fn get_pow_reward_difficulty(&self) -> PowTarget;

    /// Nullifiers of already-claimed `PoW` solutions.
    fn get_pow_nullifiers(&self) -> &HashTrieMapSync<PowNullifier, Slot>;

    /// `sigma_e`: reward amount per claim for the current epoch.
    fn get_epoch_pow_reward(&self) -> PowReward;

    /// `R_PoW`: current balance of the `PoW` reward pool.
    fn get_pow_reward_pool(&self) -> PowReward;

    /// Randomness nonce of the current epoch, against which a claim's
    /// `epoch_nonce` is matched.
    fn get_current_epoch_nonce(&self) -> ZkHash;

    /// Randomness nonce of the epoch preceding [`Self::get_epoch`], also
    /// accepted for claims mined just before an epoch boundary.
    fn get_previous_epoch_nonce(&self) -> ZkHash;

    /// Slots of the blocks a claim may anchor to, keyed by block hash;
    /// used for the window-of-acceptance check.
    fn get_blocks_slot(&self) -> HashTrieMapSync<Hash, Slot>;

    /// Acceptance window, in slots, for the window-of-acceptance check.
    /// Configured per-deployment.
    fn get_pow_slot_window(&self) -> NonZeroU64;
}

#[cfg(test)]
pub mod test_utils {
    use std::{collections::HashMap, num::NonZeroU64};

    use lb_cryptarchia_engine::{Epoch, Slot};
    use lb_key_management_system_keys::keys::UnverifiedEd25519PublicKey;
    use rpds::HashTrieMapSync;

    use crate::{
        block::BlockNumber,
        crypto::{Hash, ZkHash},
        mantle::{
            VerificationError,
            channel::Channels,
            ledger::{DeclarationKeys, Declarations},
            note_set::NoteSets,
            ops::{
                channel::{ChannelId, ChannelKeyIndex},
                pow::{PowNullifier, PowReward, PowTarget},
            },
            programs::Programs,
            transactions::OperationVerificationHelper,
        },
        sdp::{DeclarationId, MinStake, ServiceType},
    };

    pub struct TestOperationVerificationHelper {
        channels: Channels,
        keys: HashMap<(ChannelId, ChannelKeyIndex), UnverifiedEd25519PublicKey>,
        note_sets: NoteSets,
        programs: Programs,
        block_number: BlockNumber,
        declarations: Declarations,
        declaration_keys: DeclarationKeys,
        min_stake: MinStake,
        epoch: Epoch,
        block_slot: Slot,
        pow_reward_difficulty: PowTarget,
        pow_nullifiers: HashTrieMapSync<PowNullifier, Slot>,
        epoch_pow_reward: PowReward,
        pow_reward_pool: PowReward,
        current_epoch_nonce: ZkHash,
        previous_epoch_nonce: ZkHash,
        blocks_slot: HashTrieMapSync<Hash, Slot>,
        pow_slot_window: NonZeroU64,
    }

    impl TestOperationVerificationHelper {
        #[must_use]
        pub fn new(
            channels: Channels,
            keys: impl IntoIterator<Item = ((ChannelId, ChannelKeyIndex), UnverifiedEd25519PublicKey)>,
        ) -> Self {
            Self {
                channels,
                keys: keys.into_iter().collect(),
                note_sets: NoteSets::new(),
                programs: Programs::new(),
                block_number: 0,
                declarations: Declarations::new_sync(),
                declaration_keys: DeclarationKeys::default(),
                min_stake: MinStake {
                    threshold: 0,
                    timestamp: 0,
                },
                epoch: Epoch::from(0u32),
                block_slot: Slot::from(0u64),
                pow_reward_difficulty: PowTarget::default(),
                pow_nullifiers: HashTrieMapSync::new_sync(),
                epoch_pow_reward: 0,
                pow_reward_pool: 0,
                current_epoch_nonce: ZkHash::default(),
                previous_epoch_nonce: ZkHash::default(),
                blocks_slot: HashTrieMapSync::new_sync(),
                pow_slot_window: NonZeroU64::new(100).expect("100 is not 0"),
            }
        }

        #[must_use]
        pub const fn with_block_slot(mut self, slot: Slot) -> Self {
            self.block_slot = slot;
            self
        }

        #[must_use]
        pub const fn with_pow_reward_difficulty(mut self, difficulty: PowTarget) -> Self {
            self.pow_reward_difficulty = difficulty;
            self
        }

        #[must_use]
        pub const fn with_pow_rewards(
            mut self,
            epoch_pow_reward: PowReward,
            pow_reward_pool: PowReward,
        ) -> Self {
            self.epoch_pow_reward = epoch_pow_reward;
            self.pow_reward_pool = pow_reward_pool;
            self
        }

        #[must_use]
        pub fn with_pow_nullifiers(
            mut self,
            nullifiers: HashTrieMapSync<PowNullifier, Slot>,
        ) -> Self {
            self.pow_nullifiers = nullifiers;
            self
        }

        #[must_use]
        pub const fn with_epoch(mut self, epoch: Epoch) -> Self {
            self.epoch = epoch;
            self
        }

        #[must_use]
        pub const fn with_epoch_nonces(mut self, previous: ZkHash, current: ZkHash) -> Self {
            self.previous_epoch_nonce = previous;
            self.current_epoch_nonce = current;
            self
        }

        #[must_use]
        pub fn with_blocks_slot(
            mut self,
            blocks_slot: impl IntoIterator<Item = (Hash, Slot)>,
        ) -> Self {
            self.blocks_slot = blocks_slot.into_iter().collect();
            self
        }

        #[must_use]
        pub const fn with_pow_slot_window(mut self, slot_window: NonZeroU64) -> Self {
            self.pow_slot_window = slot_window;
            self
        }

        #[must_use]
        pub fn with_note_sets(mut self, note_sets: NoteSets) -> Self {
            self.note_sets = note_sets;
            self
        }

        #[must_use]
        pub const fn with_block_number(mut self, block_number: BlockNumber) -> Self {
            self.block_number = block_number;
            self
        }
    }

    impl OperationVerificationHelper for TestOperationVerificationHelper {
        fn get_channels(&self) -> &Channels {
            &self.channels
        }

        fn get_note_sets(&self) -> &NoteSets {
            &self.note_sets
        }

        fn get_programs(&self) -> &Programs {
            &self.programs
        }

        fn get_block_number(&self) -> BlockNumber {
            self.block_number
        }

        fn get_declarations_by_service(
            &self,
            _service: ServiceType,
        ) -> Result<&Declarations, VerificationError> {
            Ok(&self.declarations)
        }

        fn get_declaration_keys_by_service(
            &self,
            _service: ServiceType,
        ) -> Result<&DeclarationKeys, VerificationError> {
            Ok(&self.declaration_keys)
        }

        fn get_declarations_by_id(
            &self,
            _id: &DeclarationId,
        ) -> Result<&Declarations, VerificationError> {
            Ok(&self.declarations)
        }

        fn get_min_stake(&self) -> &MinStake {
            &self.min_stake
        }

        fn get_epoch(&self) -> Epoch {
            self.epoch
        }

        fn get_block_slot(&self) -> Slot {
            self.block_slot
        }

        fn get_key_from_channel_at_index(
            &self,
            channel_id: &ChannelId,
            key_index: &ChannelKeyIndex,
        ) -> Result<UnverifiedEd25519PublicKey, VerificationError> {
            self.keys.get(&(*channel_id, *key_index)).copied().ok_or(
                VerificationError::KeyNotFound {
                    channel_id: *channel_id,
                    key_index: *key_index,
                },
            )
        }

        fn get_pow_reward_difficulty(&self) -> PowTarget {
            self.pow_reward_difficulty
        }

        fn get_pow_nullifiers(&self) -> &HashTrieMapSync<PowNullifier, Slot> {
            &self.pow_nullifiers
        }

        fn get_epoch_pow_reward(&self) -> PowReward {
            self.epoch_pow_reward
        }

        fn get_pow_reward_pool(&self) -> PowReward {
            self.pow_reward_pool
        }

        fn get_current_epoch_nonce(&self) -> ZkHash {
            self.current_epoch_nonce
        }

        fn get_previous_epoch_nonce(&self) -> ZkHash {
            self.previous_epoch_nonce
        }

        fn get_blocks_slot(&self) -> HashTrieMapSync<Hash, Slot> {
            self.blocks_slot.clone()
        }

        fn get_pow_slot_window(&self) -> NonZeroU64 {
            self.pow_slot_window
        }
    }
}
