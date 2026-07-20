use std::sync::Arc;

use lb_cryptarchia_engine::Slot;
use serde::{Deserialize, Serialize};

use crate::{
    events::TxEvent,
    mantle::{
        NoteId,
        channel_notes::{self, ChannelNotes},
        ledger::{self, Operation as _},
        nom::NomCodec,
        ops::channel::{
            ChannelId, ChannelKeyIndex, MsgId,
            config::Keys,
            inscribe::{InscriptionExecutionContext, InscriptionOp},
        },
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash, NomCodec)]
pub struct SlotTimeframe(u32);

impl From<u32> for SlotTimeframe {
    fn from(slot: u32) -> Self {
        Self(slot)
    }
}

impl From<SlotTimeframe> for u32 {
    fn from(slot: SlotTimeframe) -> Self {
        slot.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash, NomCodec)]
pub struct SlotTimeout(u32);

impl From<u32> for SlotTimeout {
    fn from(slot: u32) -> Self {
        Self(slot)
    }
}

impl From<SlotTimeout> for u32 {
    fn from(slot: SlotTimeout) -> Self {
        slot.0
    }
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("Invalid parent {parent:?} for channel {channel_id:?}, expected {actual:?}")]
    InvalidParent {
        channel_id: ChannelId,
        parent: [u8; 32],
        actual: [u8; 32],
    },
    #[error("Unauthorized signer {signer:?} for channel {channel_id:?}")]
    UnauthorizedSigner {
        channel_id: ChannelId,
        signer: String,
    },
    #[error("Invalid signature")]
    InvalidSignature,
    #[error(
        "Invalid signature index {index:?} for channel {channel_id:?} which has {sequencers:?} sequencers"
    )]
    InvalidSignatureIndex {
        channel_id: ChannelId,
        sequencers: usize,
        index: ChannelKeyIndex,
    },
    #[error("Channel {channel_id:?} not found")]
    ChannelNotFound { channel_id: ChannelId },
    #[error("The Channel Config isn't well formed")]
    InvalidChannelConfig,
    #[error("Channel transfer inputs and outputs have different total value")]
    UnbalancedTransfer,
    #[error(transparent)]
    ChannelNotes(#[from] channel_notes::Error),
    #[error("Inputs error: {0}")]
    Inputs(#[from] ledger::InputsError),
    #[error("Outputs error: {0}")]
    Outputs(#[from] ledger::OutputsError),
    #[error(
        "Invalid number of signatures (treshold:?) for channel {channel_id:?}, expected {actual:?}"
    )]
    ThresholdUnmet {
        channel_id: ChannelId,
        threshold: u16,
        actual: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channels {
    pub channels: rpds::HashTrieMapSync<ChannelId, ChannelState>,
    channel_notes: ChannelNotes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelState {
    // Channel Configuration
    pub accredited_keys: Arc<Keys>, // keys.len() <= ChannelKeyIndex::MAX
    pub configuration_threshold: u16, /* indicating how many keys are required to update
                                     * the
                                     * configuration */

    // Message Ordering
    pub tip_message: MsgId,

    // Decentralized Sequencing
    pub tip_slot: Slot,
    pub tip_sequencer: u16, /* indicating the actual sequencer position in the list of
                             * accredited keys */
    pub tip_sequencer_starting_slot: Slot,
    pub posting_timeframe: SlotTimeframe, // number of slots (0 = infinity)
    pub posting_timeout: SlotTimeout,     // number of slots (0 = no timeout)

    // Bridging
    pub transfer_threshold: ChannelKeyIndex, /* indicating how many keys are required to
                                              * transfer or withdraw funds from the
                                              * channel */
}

pub(crate) const DEFAULT_TRANSFER_THRESHOLD: ChannelKeyIndex = 1;

impl Default for Channels {
    fn default() -> Self {
        Self::new()
    }
}

impl Channels {
    pub fn from_genesis(op: &InscriptionOp) -> Result<(Self, Vec<TxEvent>), Error> {
        let (ctx, events) = op.execute(InscriptionExecutionContext {
            channels: Self::default(),
            block_slot: Slot::default(),
        })?;
        Ok((ctx.channels, events))
    }

    #[must_use]
    pub fn new() -> Self {
        Self {
            channels: rpds::HashTrieMapSync::new_sync(),
            channel_notes: ChannelNotes::new(),
        }
    }

    #[must_use]
    pub fn channel_state(&self, channel_id: &ChannelId) -> Option<&ChannelState> {
        self.channels.get(channel_id)
    }

    /// Returns `true` if `note_id` is owned by any channel.
    #[must_use]
    pub fn is_channel_note(&self, note_id: &NoteId) -> bool {
        self.channel_notes.contains(note_id)
    }

    /// Returns the channel owning `note_id`, if it is a channel note.
    #[must_use]
    pub fn get_channel(&self, note_id: &NoteId) -> Option<ChannelId> {
        self.channel_notes.get(note_id).copied()
    }

    /// Returns `true` if `note_id` is a channel note owned by `channel_id`.
    #[must_use]
    pub(crate) fn is_channel_note_of(&self, note_id: &NoteId, channel_id: &ChannelId) -> bool {
        self.channel_notes.is_a_channel(note_id, channel_id)
    }

    /// Registers `note_id` as a channel note owned by `channel_id`.
    pub(crate) fn register_channel_note(
        mut self,
        note_id: &NoteId,
        channel_id: &ChannelId,
    ) -> Result<Self, channel_notes::Error> {
        self.channel_notes = self.channel_notes.into_channel(note_id, channel_id)?;
        Ok(self)
    }

    /// Unregisters `note_id`, releasing it from `channel_id`'s ownership.
    pub(crate) fn unregister_channel_note(
        mut self,
        note_id: &NoteId,
        channel_id: &ChannelId,
    ) -> Result<Self, channel_notes::Error> {
        self.channel_notes = self.channel_notes.into_bedrock(note_id, channel_id)?;
        Ok(self)
    }
}

impl ChannelState {
    // Returns the new sequencer index and its starting slot
    #[must_use]
    pub fn round_robin(&self, block_slot: Slot) -> (u16, Slot) {
        let elapsed_slot_since_last_tip = (block_slot.saturating_sub(self.tip_slot)).into_inner();
        let tip_sequencer_duration =
            (block_slot.saturating_sub(self.tip_sequencer_starting_slot)).into_inner();
        let posting_timeframe = u64::from(self.posting_timeframe.0);
        let posting_timeout = u64::from(self.posting_timeout.0);
        let num_sequencers = self.accredited_keys.len() as u64; // bounded by ChannelKeyIndex::MAX
        let tip_sequencer = u64::from(self.tip_sequencer);
        let is_timed_out = elapsed_slot_since_last_tip >= posting_timeout && posting_timeout != 0;
        let sequencers_timed_out = elapsed_slot_since_last_tip.checked_div(posting_timeout); // None if posting_timeout == 0
        let timeframe_elapsed = tip_sequencer_duration.checked_div(posting_timeframe); // None if timeframe == 0

        // Timeout-based rotation takes priority when timed out.
        // Falls back to timeframe-based rotation, then to the current sequencer.
        let index = sequencers_timed_out
            .filter(|_| is_timed_out)
            .or(timeframe_elapsed)
            .map_or(self.tip_sequencer, |slot| {
                ((tip_sequencer + slot) % num_sequencers) as u16
            });

        // Starting slot mirrors the same priority.
        let starting_slot = sequencers_timed_out
            .filter(|_| is_timed_out)
            .map(|sequencers_timed_out| {
                self.tip_slot
                    .strict_add((sequencers_timed_out * posting_timeout).into())
            })
            .or_else(|| {
                timeframe_elapsed.map(|timeframe_elapsed| {
                    self.tip_sequencer_starting_slot
                        .strict_add((timeframe_elapsed * posting_timeframe).into())
                })
            })
            .unwrap_or(self.tip_sequencer_starting_slot);
        (index, starting_slot)
    }
}

#[cfg(test)]
mod tests {
    use ark_ff::AdditiveGroup as _;
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use lb_utils::blake_rng::RngCore as _;
    use rand::thread_rng;

    use super::*;
    use crate::{
        events::TxEventPayload,
        mantle::{
            Note, Utxo, Value,
            ledger::Utxos,
            ops::{
                OpId as _,
                channel::{
                    Ed25519PublicKey as PublicKey,
                    deposit::{DepositExecutionContext, DepositOp, Metadata},
                    withdraw::{ChannelWithdrawOp, WithdrawExecutionContext},
                },
            },
            transactions::{GasPrices, MantleTxGasContext},
        },
    };

    fn test_public_key(seed: u8) -> PublicKey {
        Ed25519Key::from_bytes(&[seed; 32]).public_key()
    }

    fn make_channel(
        tip_slot: u64,
        tip_sequencer: u16,
        tip_sequencer_starting_slot: u64,
        posting_timeframe: u32,
        posting_timeout: u32,
        num_keys: u8,
    ) -> ChannelState {
        ChannelState {
            tip_slot: Slot::new(tip_slot),
            tip_sequencer,
            tip_sequencer_starting_slot: Slot::new(tip_sequencer_starting_slot),
            posting_timeframe: SlotTimeframe(posting_timeframe),
            posting_timeout: SlotTimeout(posting_timeout),
            accredited_keys: Keys::try_from((0..num_keys).map(test_public_key).collect::<Vec<_>>())
                .unwrap()
                .into(),
            configuration_threshold: 0,
            tip_message: MsgId::root(),
            transfer_threshold: 0,
        }
    }

    fn utxo(value: Value) -> (ZkKey, Utxo) {
        let mut op_id = [0u8; 32];
        thread_rng().fill_bytes(&mut op_id);
        let zk_sk = ZkKey::from(Fr::ZERO);
        let utxo = Utxo {
            op_id,
            output_index: 0,
            note: Note::new(value, zk_sk.to_public_key()),
        };
        (zk_sk, utxo)
    }

    fn utxo_tree(utxos: Vec<Utxo>) -> Utxos {
        let mut utxo_tree = Utxos::new();
        for utxo in utxos {
            (utxo_tree, _) = utxo_tree.insert(utxo.id(), utxo);
        }
        utxo_tree
    }

    impl Channels {
        #[must_use]
        fn with_notes(channel_id: ChannelId, notes: impl IntoIterator<Item = NoteId>) -> Self {
            let mut channels = Self::new();
            for note_id in notes {
                channels = channels
                    .register_channel_note(&note_id, &channel_id)
                    .unwrap();
            }
            channels
        }
    }

    #[test]
    fn channels_to_gas_context_tracks_transfer_thresholds() {
        let first_id = ChannelId::from([1u8; 32]);
        let second_id = ChannelId::from([2u8; 32]);
        let missing_id = ChannelId::from([0u8; 32]);

        let channels = Channels {
            channels: rpds::HashTrieMapSync::new_sync()
                .insert(
                    first_id,
                    ChannelState {
                        accredited_keys: Keys::from(test_public_key(11)).into(),
                        configuration_threshold: 1,
                        tip_message: MsgId::root(),
                        tip_slot: Slot::default(),
                        tip_sequencer: 0,
                        tip_sequencer_starting_slot: Slot::default(),
                        posting_timeframe: 0u32.into(),
                        transfer_threshold: 1,
                        posting_timeout: 0u32.into(),
                    },
                )
                .insert(
                    second_id,
                    ChannelState {
                        accredited_keys: Keys::from([test_public_key(22), test_public_key(23)])
                            .into(),
                        configuration_threshold: 1,
                        tip_message: MsgId::root(),
                        tip_slot: Slot::default(),
                        tip_sequencer: 0,
                        tip_sequencer_starting_slot: Slot::default(),
                        posting_timeframe: 0.into(),
                        transfer_threshold: 2,
                        posting_timeout: 0.into(),
                    },
                ),
            channel_notes: ChannelNotes::new(),
        };

        let gas_context = MantleTxGasContext::from_channels(&channels, GasPrices::new(0, 0));

        assert_eq!(gas_context.transfer_threshold(&first_id), Some(1));
        assert_eq!(gas_context.transfer_threshold(&second_id), Some(2));
        assert_eq!(gas_context.transfer_threshold(&missing_id), None);
    }

    #[test]
    fn deposit_registers_channel_note() {
        let channel_id = ChannelId::from([0u8; 32]);
        let channels = Channels::new();

        let (_, utxo) = utxo(6u64);
        let note_id = utxo.id();

        let deposit_op = DepositOp {
            channel_id,
            inputs: [note_id].into(),
            metadata: Metadata::empty(),
        };

        let utxo_tree = utxo_tree(vec![utxo]);

        let (updated, events) = deposit_op
            .execute(DepositExecutionContext {
                channels,
                utxos: utxo_tree,
                tx_hash: [0; 32].into(),
            })
            .expect("execution should succeed");

        // The note is now owned by the channel but stays in the ledger.
        assert!(updated.channels.is_channel_note_of(&note_id, &channel_id));

        assert_eq!(events.len(), 1);
        let Some(TxEvent {
            tx_hash,
            op_id,
            payload:
                TxEventPayload::Deposit {
                    channel_id: event_channel_id,
                    amount,
                    metadata,
                },
        }) = events.iter().find(|event| {
            matches!(
                event,
                TxEvent {
                    payload: TxEventPayload::Deposit { .. },
                    ..
                }
            )
        })
        else {
            panic!("events should include deposit event")
        };
        assert_eq!(*tx_hash, [0; 32].into());
        assert_eq!(*op_id, deposit_op.op_id());
        assert_eq!(*event_channel_id, deposit_op.channel_id);
        assert_eq!(*amount, utxo.note.value);
        assert_eq!(*metadata, deposit_op.metadata);
    }

    #[test]
    fn withdraw_releases_channel_note() {
        let channel_id = ChannelId::from([0u8; 32]);
        let (_, utxo) = utxo(6u64);
        let note_id = utxo.id();
        let channels = Channels::with_notes(channel_id, [note_id]);

        let withdraw_op = ChannelWithdrawOp {
            channel_id,
            inputs: [note_id].into(),
        };

        let (updated, events) = withdraw_op
            .execute(WithdrawExecutionContext {
                channels,
                tx_hash: [1; 32].into(),
            })
            .expect("execution should succeed");

        // The note is released back to a regular note.
        assert!(!updated.channels.is_channel_note(&note_id));
        assert!(events.is_empty());
    }

    #[test]
    fn withdraw_fails_when_note_not_in_channel() {
        let channel_id = ChannelId::from([0u8; 32]);
        let channels = Channels::new();
        let note_id = utxo(6u64).1.id();

        let withdraw_op = ChannelWithdrawOp {
            channel_id,
            inputs: [note_id].into(),
        };

        let result = withdraw_op.execute(WithdrawExecutionContext {
            channels,
            tx_hash: [0; 32].into(),
        });

        assert!(matches!(
            result,
            Err(Error::ChannelNotes(channel_notes::Error::NotInChannel(_)))
        ));
    }

    // 1. Infinite timeframe (timeframe=0): sequencer holds indefinitely unless
    //    timed out
    #[test]
    fn infinite_timeframe_no_timeout_stays_forever() {
        let channel = make_channel(100, 2, 80, 0, 0, 5);
        assert_eq!(channel.round_robin(100.into()), (2, 80.into()));
        assert_eq!(channel.round_robin(999_999.into()), (2, 80.into()));
    }

    #[test]
    fn infinite_timeframe_not_yet_timed_out() {
        let channel = make_channel(100, 1, 90, 0, 50, 4);
        assert_eq!(channel.round_robin(130.into()), (1, 90.into()));
    }

    #[test]
    fn infinite_timeframe_timed_out() {
        let channel = make_channel(100, 1, 90, 0, 50, 4);
        assert_eq!(channel.round_robin(150.into()), (2, 150.into()));
    }

    #[test]
    fn infinite_timeframe_multiple_timeouts() {
        let channel = make_channel(100, 1, 90, 0, 50, 4);
        assert_eq!(channel.round_robin(220.into()), (3, 200.into()));
    }

    // 2. Normal timeframe rotation (no timeout triggered)
    #[test]
    fn timeframe_rotation_same_slot_no_advance() {
        let channel = make_channel(100, 0, 100, 10, 0, 3);
        assert_eq!(channel.round_robin(100.into()), (0, 100.into()));
    }

    #[test]
    fn timeframe_rotation_within_first_frame() {
        let channel = make_channel(100, 0, 100, 10, 0, 3);
        assert_eq!(channel.round_robin(105.into()), (0, 100.into()));
    }

    #[test]
    fn timeframe_rotation_exact_boundary() {
        let channel = make_channel(100, 0, 100, 10, 0, 3);
        assert_eq!(channel.round_robin(110.into()), (1, 110.into()));
    }

    #[test]
    fn timeframe_rotation_multiple_frames() {
        let channel = make_channel(100, 0, 100, 10, 0, 4);
        assert_eq!(channel.round_robin(125.into()), (2, 120.into()));
    }

    #[test]
    fn timeframe_rotation_wraps_around() {
        let channel = make_channel(100, 2, 100, 10, 0, 3);
        assert_eq!(channel.round_robin(110.into()), (0, 110.into()));
    }

    #[test]
    fn timeframe_rotation_full_cycle() {
        // 3 keys, 3 rotations => back to the same sequencer
        let channel = make_channel(100, 1, 100, 10, 0, 3);
        assert_eq!(channel.round_robin(130.into()), (1, 130.into()));
    }

    #[test]
    fn timeframe_rotation_starting_slot_offset() {
        let channel = make_channel(100, 0, 95, 10, 0, 3);
        assert_eq!(channel.round_robin(105.into()), (1, 105.into()));
    }

    // 3. Timed out sequencers
    #[test]
    fn timeout_exact_boundary() {
        let channel = make_channel(100, 0, 100, 10, 20, 4);
        assert_eq!(channel.round_robin(120.into()), (1, 120.into()));
    }

    #[test]
    fn timeout_skips_multiple_unresponsive_sequencers() {
        let channel = make_channel(100, 0, 100, 5, 10, 4);
        assert_eq!(channel.round_robin(135.into()), (3, 130.into()));
    }

    #[test]
    fn timeout_wraps_past_end_of_key_list() {
        let channel = make_channel(100, 2, 100, 5, 10, 3);
        assert_eq!(channel.round_robin(120.into()), (1, 120.into()));
    }

    #[test]
    fn timeout_wraps_full_cycle() {
        let channel = make_channel(100, 0, 100, 5, 10, 3);
        assert_eq!(channel.round_robin(130.into()), (0, 130.into()));
    }

    // 4. No timeout (timeout=0)
    #[test]
    fn no_timeout_rotates_by_timeframe_even_after_long_absence() {
        let channel = make_channel(100, 0, 100, 10, 0, 3);
        assert_eq!(channel.round_robin(1100.into()), (1, 1100.into()));
    }

    // 5. Just below the timeout threshold
    #[test]
    fn just_below_timeout_uses_timeframe_branch() {
        let channel = make_channel(100, 0, 100, 10, 20, 4);
        assert_eq!(channel.round_robin(119.into()), (1, 110.into()));
    }

    // 6. Single sequencer
    #[test]
    fn single_key_always_index_zero() {
        let channel = make_channel(100, 0, 100, 10, 20, 1);
        assert_eq!(channel.round_robin(100.into()).0, 0);
        assert_eq!(channel.round_robin(115.into()).0, 0);
        assert_eq!(channel.round_robin(130.into()).0, 0);
    }

    // 7. Two sequencers
    #[test]
    fn two_sequencers_alternate() {
        let channel = make_channel(100, 0, 100, 5, 0, 2);
        assert_eq!(channel.round_robin(100.into()).0, 0);
        assert_eq!(channel.round_robin(104.into()).0, 0);
        assert_eq!(channel.round_robin(105.into()).0, 1);
        assert_eq!(channel.round_robin(109.into()).0, 1);
        assert_eq!(channel.round_robin(110.into()).0, 0);
    }

    // 8. 50 sequencers
    #[test]
    fn fifty_sequencers_rotate_and_wrap() {
        let channel = make_channel(0, 0, 0, 5, 0, 50);

        // After 5 slots => sequencer 1
        assert_eq!(channel.round_robin(5.into()).0, 1);
        // After 5*49 = 245 slots => sequencer 49 (last)
        assert_eq!(channel.round_robin(245.into()).0, 49);
        // After 5*50 = 250 slots => wrap back to 0
        assert_eq!(channel.round_robin(250.into()).0, 0);
        // After 5*73 = 365 slots => (0+73)%50 = 23
        assert_eq!(channel.round_robin(365.into()).0, 23);
    }

    #[test]
    fn fifty_sequencers_cascading_timeouts() {
        let channel = make_channel(1000, 10, 1000, 5, 3, 50);
        assert_eq!(channel.round_robin(1090.into()), (40, 1090.into()));
    }

    // 9. State transition: after timeout, new sequencer gets a fresh baseline
    #[test]
    fn after_timeout_new_sequencer_gets_fresh_starting_slot() {
        let channel = make_channel(110, 1, 110, 15, 10, 3);
        assert_eq!(channel.round_robin(125.into()), (2, 120.into()));
        assert_eq!(channel.round_robin(135.into()), (0, 130.into()));
    }

    // 10. Zero elapsed (block_slot == tip_slot)
    #[test]
    fn zero_elapsed_no_change() {
        let channel = make_channel(100, 3, 95, 10, 20, 5);
        assert_eq!(channel.round_robin(100.into()), (3, 95.into()));
    }
}
