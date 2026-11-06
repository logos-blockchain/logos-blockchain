use lb_core::{
    block::BlockNumber,
    crypto::Hash,
    mantle::{
        channel::Channels,
        ledger::{Declarations, ProviderIndex},
        note_set::NoteSets,
        ops::{
            channel::{ChannelId, ChannelKeyIndex},
            pow::{PowNullifier, PowReward, PowSlotWindows, PowTarget},
            sdp::SdpError,
        },
        programs::Programs,
        transactions::{OperationVerificationHelper, VerificationError},
    },
    sdp::{Declaration, DeclarationId, MinStake, ServiceType},
};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_key_management_system_keys::keys::UnverifiedEd25519PublicKey;
use rpds::HashTrieMapSync;

use crate::{
    config::{EraScheduledConfig, config_for_slot, pow_slot_windows},
    mantle::LedgerState,
};

pub struct MantleOperationVerificationHelper<'a> {
    ledger_state: &'a LedgerState,
    cryptarchia_ledger: &'a crate::CryptarchiaLedger,
    block_number: BlockNumber,
    eras: &'a EraScheduledConfig,
}

impl<'a> MantleOperationVerificationHelper<'a> {
    #[must_use]
    pub const fn new(
        ledger_state: &'a LedgerState,
        cryptarchia_ledger: &'a crate::CryptarchiaLedger,
        block_number: BlockNumber,
        eras: &'a EraScheduledConfig,
    ) -> Self {
        Self {
            ledger_state,
            cryptarchia_ledger,
            block_number,
            eras,
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

    fn get_declarations_by_id(
        &self,
        id: &DeclarationId,
    ) -> Result<&Declarations, VerificationError> {
        let service = self
            .ledger_state
            .sdp_ledger()
            .get_declaration(id)
            .map(|declaration| declaration.service_type)
            .ok_or(VerificationError::SDPVerificationError(
                SdpError::DeclarationNotFound(*id),
            ))?;
        self.get_declarations_by_service(service)
    }

    fn get_provider_index_by_service(
        &self,
        service: ServiceType,
    ) -> Result<&ProviderIndex, VerificationError> {
        self.ledger_state
            .sdp_ledger()
            .get_provider_index_by_service(service)
            .ok_or(VerificationError::SDPVerificationError(
                SdpError::ServiceNotFound(service),
            ))
    }

    fn get_declaration_by_id(&self, id: &DeclarationId) -> Result<&Declaration, VerificationError> {
        self.ledger_state.sdp_ledger().get_declaration(id).ok_or(
            VerificationError::SDPVerificationError(SdpError::DeclarationNotFound(*id)),
        )
    }

    fn get_min_stake(&self) -> &MinStake {
        &config_for_slot(self.eras, self.cryptarchia_ledger.slot)
            .sdp_config
            .min_stake
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

    fn get_blocks_slot(&self) -> HashTrieMapSync<Hash, Slot> {
        self.ledger_state.pow.block_slots().clone()
    }

    fn get_pow_slot_windows(&self) -> PowSlotWindows {
        pow_slot_windows(self.eras)
    }
}
