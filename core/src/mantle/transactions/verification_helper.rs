use lb_cryptarchia_engine::{Epoch, Slot};
use lb_key_management_system_keys::keys::Ed25519PublicKey;

use crate::{
    mantle::{
        VerificationError,
        channel::Channels,
        ledger::{Declarations, Utxos},
        ops::{
            channel::{ChannelId, ChannelKeyIndex},
            leader_claim::{RewardsRoot, VoucherNullifier},
        },
    },
    sdp::{DeclarationId, MinStake, ServiceType, locked_notes::LockedNotes},
};

pub trait OperationVerificationHelper {
    fn get_channels(&self) -> &Channels;

    fn get_locked_notes(&self) -> &LockedNotes;

    fn get_utxos(&self) -> &Utxos;

    fn get_declarations_by_service(
        &self,
        service: ServiceType,
    ) -> Result<&Declarations, VerificationError>;

    fn get_declarations_by_id(
        &self,
        id: &DeclarationId,
    ) -> Result<&Declarations, VerificationError>;

    fn get_min_stake(&self) -> &MinStake;

    fn get_epoch(&self) -> Epoch;

    fn get_block_slot(&self) -> Slot;

    fn get_nullifiers(&self) -> &rpds::HashTrieSetSync<VoucherNullifier>;

    fn get_claimable_vouchers_root(&self) -> &RewardsRoot;

    fn get_channel_transfer_threshold(
        &self,
        channel_id: &ChannelId,
    ) -> Result<ChannelKeyIndex, VerificationError>;

    fn get_key_from_channel_at_index(
        &self,
        channel_id: &ChannelId,
        key_index: &ChannelKeyIndex,
    ) -> Result<Ed25519PublicKey, VerificationError>;
}
