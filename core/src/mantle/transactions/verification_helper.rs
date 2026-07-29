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

#[cfg(test)]
pub mod test_utils {
    use std::collections::HashMap;

    use lb_cryptarchia_engine::{Epoch, Slot};
    use rpds::HashTrieSetSync;

    use crate::{
        mantle::{
            Utxo, VerificationError,
            channel::Channels,
            ledger::{Declarations, Utxos},
            ops::{
                channel::{ChannelId, ChannelKeyIndex, Ed25519PublicKey},
                leader_claim::{RewardsRoot, VoucherNullifier},
            },
            transactions::OperationVerificationHelper,
        },
        sdp::{DeclarationId, MinStake, ServiceType, locked_notes::LockedNotes},
    };

    pub struct TestOperationVerificationHelper {
        channels: Channels,
        keys: HashMap<(ChannelId, ChannelKeyIndex), Ed25519PublicKey>,
        locked_notes: LockedNotes,
        utxos: Utxos,
        declarations: Declarations,
        min_stake: MinStake,
        epoch: Epoch,
        block_slot: Slot,
        nullifiers: HashTrieSetSync<VoucherNullifier>,
        claimable_vouchers_root: RewardsRoot,
    }

    impl TestOperationVerificationHelper {
        #[must_use]
        pub fn new(
            channels: Channels,
            keys: impl IntoIterator<Item = ((ChannelId, ChannelKeyIndex), Ed25519PublicKey)>,
        ) -> Self {
            Self {
                channels,
                keys: keys.into_iter().collect(),
                locked_notes: LockedNotes::new(),
                utxos: Utxos::new(),
                declarations: Declarations::new(),
                min_stake: MinStake {
                    threshold: 0,
                    timestamp: 0,
                },
                epoch: Epoch::from(0u32),
                block_slot: Slot::from(0u64),
                nullifiers: HashTrieSetSync::new_sync(),
                claimable_vouchers_root: RewardsRoot::default(),
            }
        }

        #[must_use]
        pub fn with_utxos(mut self, utxos: impl IntoIterator<Item = Utxo>) -> Self {
            for utxo in utxos {
                self.utxos = self.utxos.insert(utxo.id(), utxo).0;
            }
            self
        }
    }

    impl OperationVerificationHelper for TestOperationVerificationHelper {
        fn get_channels(&self) -> &Channels {
            &self.channels
        }

        fn get_locked_notes(&self) -> &LockedNotes {
            &self.locked_notes
        }

        fn get_utxos(&self) -> &Utxos {
            &self.utxos
        }

        fn get_declarations_by_service(
            &self,
            _service: ServiceType,
        ) -> Result<&Declarations, VerificationError> {
            Ok(&self.declarations)
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

        fn get_nullifiers(&self) -> &HashTrieSetSync<VoucherNullifier> {
            &self.nullifiers
        }

        fn get_claimable_vouchers_root(&self) -> &RewardsRoot {
            &self.claimable_vouchers_root
        }

        fn get_channel_transfer_threshold(
            &self,
            channel_id: &ChannelId,
        ) -> Result<ChannelKeyIndex, VerificationError> {
            self.channels
                .channel_state(channel_id)
                .ok_or(VerificationError::ChannelNotFound {
                    channel_id: *channel_id,
                })
                .map(|channel| channel.transfer_threshold)
        }

        fn get_key_from_channel_at_index(
            &self,
            channel_id: &ChannelId,
            key_index: &ChannelKeyIndex,
        ) -> Result<Ed25519PublicKey, VerificationError> {
            self.keys.get(&(*channel_id, *key_index)).copied().ok_or(
                VerificationError::KeyNotFound {
                    channel_id: *channel_id,
                    key_index: *key_index,
                },
            )
        }
    }
}
