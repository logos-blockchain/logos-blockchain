use lb_core::{
    crypto::{Hash, ZkHash},
    mantle::{
        channel::Channels,
        ledger::{Declarations, Utxos},
        ops::{
            channel::{ChannelId, ChannelKeyIndex},
            leader_claim::{RewardsRoot, VoucherNullifier},
            pow::{PowNullifier, PowReward, PowTarget},
            sdp::SdpError,
        },
        transactions::{OperationVerificationHelper, VerificationError},
    },
    sdp::{DeclarationId, MinStake, ServiceType, locked_notes::LockedNotes},
};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_key_management_system_keys::keys::Ed25519PublicKey;
use rpds::{HashTrieMapSync, HashTrieSetSync};

use crate::mantle::LedgerState;

pub struct MantleOperationVerificationHelper<'a> {
    ledger_state: &'a LedgerState,
    cryptarchia_ledger: &'a crate::CryptarchiaLedger,
    config: &'a crate::Config,
}

impl<'a> MantleOperationVerificationHelper<'a> {
    #[must_use]
    pub const fn new(
        ledger_state: &'a LedgerState,
        cryptarchia_ledger: &'a crate::CryptarchiaLedger,
        config: &'a crate::Config,
    ) -> Self {
        Self {
            ledger_state,
            cryptarchia_ledger,
            config,
        }
    }
}

impl OperationVerificationHelper for MantleOperationVerificationHelper<'_> {
    fn get_channels(&self) -> &Channels {
        self.ledger_state.channels()
    }

    fn get_locked_notes(&self) -> &LockedNotes {
        self.ledger_state.locked_notes()
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
    ) -> Result<Ed25519PublicKey, VerificationError> {
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

    fn get_pow_slot_window(&self) -> u64 {
        self.config.pow_config.reward.slot_window
    }
}
