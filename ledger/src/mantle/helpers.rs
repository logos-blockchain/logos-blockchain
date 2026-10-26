use std::num::NonZeroU64;

use lb_core::{
    crypto::{Hash, ZkHash},
    mantle::{
        channel::Channels,
        ledger::{Declarations, ProviderIndex, Utxos},
        ops::{
            channel::{ChannelId, ChannelKeyIndex},
            leader_claim::{RewardsRoot, VoucherNullifier},
            pow::{PowNullifier, PowReward, PowTarget},
            sdp::SdpError,
        },
        transactions::{OperationVerificationHelper, VerificationError},
    },
    sdp::{Declaration, DeclarationId, MinStake, ServiceType, service_notes::ServiceNotes},
};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_key_management_system_keys::keys::UnverifiedEd25519PublicKey;
use rpds::{HashTrieMapSync, HashTrieSetSync};

use crate::{
    config::{EraScheduledConfig, config_for_epoch, config_for_slot},
    mantle::LedgerState,
};

pub struct MantleOperationVerificationHelper<'a> {
    ledger_state: &'a LedgerState,
    cryptarchia_ledger: &'a crate::CryptarchiaLedger,
    eras: &'a EraScheduledConfig,
}

impl<'a> MantleOperationVerificationHelper<'a> {
    #[must_use]
    pub const fn new(
        ledger_state: &'a LedgerState,
        cryptarchia_ledger: &'a crate::CryptarchiaLedger,
        eras: &'a EraScheduledConfig,
    ) -> Self {
        Self {
            ledger_state,
            cryptarchia_ledger,
            eras,
        }
    }
}

impl OperationVerificationHelper for MantleOperationVerificationHelper<'_> {
    fn get_channels(&self) -> &Channels {
        self.ledger_state.channels()
    }

    fn get_service_notes(&self) -> &ServiceNotes {
        self.ledger_state.service_notes()
    }

    fn get_utxos(&self) -> &Utxos {
        &self.cryptarchia_ledger.utxos
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
        self.ledger_state.leaders.epoch()
    }

    fn get_block_slot(&self) -> Slot {
        self.cryptarchia_ledger.slot
    }

    fn get_nullifiers(&self) -> &HashTrieSetSync<VoucherNullifier> {
        self.ledger_state.leaders.nullifiers()
    }

    fn get_claimable_vouchers_root(&self) -> &RewardsRoot {
        self.ledger_state.vouchers_snapshot_root()
    }

    fn get_channel_transfer_threshold(
        &self,
        channel_id: &ChannelId,
    ) -> Result<ChannelKeyIndex, VerificationError> {
        self.ledger_state
            .channels()
            .channel_state(channel_id)
            .ok_or(VerificationError::ChannelNotFound {
                channel_id: *channel_id,
            })
            .map(|channel_state| channel_state.transfer_threshold)
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

    fn get_current_epoch_pow_slot_window(&self) -> NonZeroU64 {
        config_for_epoch(self.eras, self.cryptarchia_ledger.epoch_state.epoch)
            .pow_config
            .reward
            .slot_window
    }

    fn get_previous_epoch_pow_slot_window(&self) -> NonZeroU64 {
        // The genesis epoch has none before it: until the first epoch
        // boundary, the previous epoch's nonce is the genesis epoch's.
        let previous_epoch = self
            .cryptarchia_ledger
            .epoch_state
            .epoch
            .into_inner()
            .saturating_sub(1);
        config_for_epoch(self.eras, Epoch::new(previous_epoch))
            .pow_config
            .reward
            .slot_window
    }
}
