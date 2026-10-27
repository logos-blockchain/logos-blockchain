use std::num::NonZeroU64;

use lb_core::{
    block::BlockNumber,
    crypto::{Hash, ZkHash},
    mantle::{
        channel::Channels,
        ledger::{DeclarationKeys, Declarations},
        note_set::NoteSets,
        ops::{
            channel::{ChannelId, ChannelKeyIndex},
            pow::{PowNullifier, PowReward, PowTarget},
            sdp::SdpError,
        },
        programs::Programs,
        transactions::{OperationVerificationHelper, VerificationError},
    },
    sdp::{DeclarationId, MinStake, ServiceType},
};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_key_management_system_keys::keys::UnverifiedEd25519PublicKey;
use rpds::HashTrieMapSync;

use crate::mantle::LedgerState;

pub struct MantleOperationVerificationHelper<'a> {
    ledger_state: &'a LedgerState,
    cryptarchia_ledger: &'a crate::CryptarchiaLedger,
    block_number: BlockNumber,
    config: &'a crate::Config,
}

impl<'a> MantleOperationVerificationHelper<'a> {
    #[must_use]
    pub const fn new(
        ledger_state: &'a LedgerState,
        cryptarchia_ledger: &'a crate::CryptarchiaLedger,
        block_number: BlockNumber,
        config: &'a crate::Config,
    ) -> Self {
        Self {
            ledger_state,
            cryptarchia_ledger,
            block_number,
            config,
        }
    }
}

impl OperationVerificationHelper for MantleOperationVerificationHelper<'_> {
    fn get_channels(&self) -> &Channels {
        self.ledger_state.channels()
    }

    fn get_note_sets(&self) -> &NoteSets {
        &self.cryptarchia_ledger.note_sets
    }

    fn get_programs(&self) -> &Programs {
        self.ledger_state.programs()
    }

    fn get_block_number(&self) -> BlockNumber {
        self.block_number
    }

    fn get_declarations_by_service(
        &self,
        service: ServiceType,
    ) -> Result<&Declarations, VerificationError> {
        self.ledger_state
            .sdp_ledger()
            .get_declarations_by_service(service)
            .ok_or(VerificationError::SDPVerificationError(
                SdpError::ServiceNotFound(service),
            ))
    }

    fn get_declaration_keys_by_service(
        &self,
        service: ServiceType,
    ) -> Result<&DeclarationKeys, VerificationError> {
        self.ledger_state
            .sdp_ledger()
            .get_declaration_keys_by_service(service)
            .ok_or(VerificationError::SDPVerificationError(
                SdpError::ServiceNotFound(service),
            ))
    }

    fn get_declarations_by_id(
        &self,
        id: &DeclarationId,
    ) -> Result<&Declarations, VerificationError> {
        self.ledger_state
            .sdp_ledger()
            .get_declarations_by_id(id)
            .ok_or(VerificationError::SDPVerificationError(
                SdpError::DeclarationNotFound(*id),
            ))
    }

    fn get_min_stake(&self) -> &MinStake {
        &self.config.sdp_config.min_stake
    }

    fn get_epoch(&self) -> Epoch {
        self.cryptarchia_ledger.epoch_state().epoch()
    }

    fn get_block_slot(&self) -> Slot {
        self.cryptarchia_ledger.slot
    }

    fn get_key_from_channel_at_index(
        &self,
        channel_id: &ChannelId,
        key_index: &ChannelKeyIndex,
    ) -> Result<UnverifiedEd25519PublicKey, VerificationError> {
        self.ledger_state
            .channels()
            .channel_state(channel_id)
            .ok_or(VerificationError::ChannelNotFound {
                channel_id: *channel_id,
            })?
            .accredited_keys
            .get(*key_index as usize)
            .ok_or(VerificationError::KeyNotFound {
                channel_id: *channel_id,
                key_index: *key_index,
            })
            .cloned()
    }

    fn get_pow_reward_difficulty(&self) -> PowTarget {
        self.ledger_state.pow.reward_difficulty()
    }

    fn get_pow_nullifiers(&self) -> &HashTrieMapSync<PowNullifier, Slot> {
        self.ledger_state.pow.nullifiers()
    }

    fn get_epoch_pow_reward(&self) -> PowReward {
        self.ledger_state.pow.epoch_reward()
    }

    fn get_pow_reward_pool(&self) -> PowReward {
        self.ledger_state.pow.reward_pool()
    }

    fn get_current_epoch_nonce(&self) -> ZkHash {
        self.cryptarchia_ledger.epoch_state.nonce
    }

    fn get_previous_epoch_nonce(&self) -> ZkHash {
        self.cryptarchia_ledger.previous_epoch_nonce
    }

    fn get_blocks_slot(&self) -> HashTrieMapSync<Hash, Slot> {
        self.ledger_state.pow.block_slots().clone()
    }

    fn get_pow_slot_window(&self) -> NonZeroU64 {
        self.config.pow_config.reward.slot_window
    }
}
