use std::sync::Arc;

use lb_blake2btree::{Blake2bTree, LeafHash};
use lb_codec::BinaryCodec;
use lb_cryptarchia_engine::Slot;
use serde::{Deserialize, Serialize};

use crate::{
    crypto::{Digest as _, Hash, Hasher},
    events::TxEvent,
    mantle::{
        NoteId,
        channel_notes::{self, ChannelNotes},
        ledger::{self, ExecutableOperation as _},
        ops::channel::{
            ChannelId, ChannelKeyIndex, MsgId,
            config::Keys,
            inscribe::{InscriptionExecutionContext, InscriptionOp},
        },
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash, BinaryCodec)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash, BinaryCodec)]
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
    #[error("The Channel Config isn't well-formed")]
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
    channels: Blake2bTree<ChannelId, ChannelState>,
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

// The leaf binds the channel id to its whole state, so that any configuration
// or sequencing update changes the root.
impl LeafHash<ChannelId> for ChannelState {
    fn leaf_hash(&self, channel_id: &ChannelId) -> Hash {
        let mut h = Hasher::new();
        h.update(b"CHANNEL_HASH_V1");
        h.update(channel_id.as_ref());
        for key in self.accredited_keys.iter() {
            h.update(key.as_bytes());
        }
        h.update(self.configuration_threshold.to_le_bytes());
        h.update(self.tip_message.as_ref());
        h.update(self.tip_slot.to_le_bytes());
        h.update(self.tip_sequencer.to_le_bytes());
        h.update(self.tip_sequencer_starting_slot.to_le_bytes());
        h.update(self.posting_timeframe.0.to_le_bytes());
        h.update(self.posting_timeout.0.to_le_bytes());
        h.update(self.transfer_threshold.to_le_bytes());
        h.finalize().into()
    }
}

pub(crate) const DEFAULT_TRANSFER_THRESHOLD: ChannelKeyIndex = 1;

impl Default for Channels {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> IntoIterator for &'a Channels {
    type Item = (&'a ChannelId, &'a ChannelState);
    type IntoIter = <&'a Blake2bTree<ChannelId, ChannelState> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.channels.into_iter()
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
            channels: Blake2bTree::new(),
            channel_notes: ChannelNotes::new(),
        }
    }

    #[must_use]
    pub fn channel_state(&self, channel_id: &ChannelId) -> Option<&ChannelState> {
        self.channels.get_ref(channel_id)
    }

    #[must_use]
    pub fn contains_channel(&self, channel_id: &ChannelId) -> bool {
        self.channels.contains(channel_id)
    }

    #[must_use]
    pub fn iter(&self) -> <&Self as IntoIterator>::IntoIter {
        self.into_iter()
    }

    /// Creates `channel_id` with `channel`, or replaces its state if it already
    /// exists.
    pub(crate) fn set_channel_state(
        mut self,
        channel_id: &ChannelId,
        channel: ChannelState,
    ) -> Self {
        self.channels = if self.channels.contains(channel_id) {
            self.channels
                .update(channel_id, channel)
                .expect("channel is in the tree")
        } else {
            self.channels.insert(*channel_id, channel).0
        };
        self
    }

    #[must_use]
    pub fn channels_root(&self) -> Hash {
        self.channels.root()
    }

    #[must_use]
    pub fn channel_notes_root(&self) -> Hash {
        self.channel_notes.root()
    }

    /// Returns `true` if `note_id` is owned by any channel.
    #[must_use]
    pub fn is_channel_note(&self, note_id: &NoteId) -> bool {
        self.channel_notes.contains(note_id)
    }

    /// Returns the channel owning `note_id`, if it is a channel note.
    #[must_use]
    pub fn get_channel(&self, note_id: &NoteId) -> Option<ChannelId> {
        self.channel_notes.get(note_id)
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
        let elapsed_slot_since_last_tip = block_slot.saturating_sub(self.tip_slot).into_inner();
        let tip_sequencer_duration = block_slot
            .saturating_sub(self.tip_sequencer_starting_slot)
            .into_inner();
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
            transactions::{GasPrices, mantle_tx::MantleTxGasContext},
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

        let channels = Channels::new()
            .set_channel_state(
                &first_id,
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
            .set_channel_state(
                &second_id,
                ChannelState {
                    accredited_keys: Keys::from([test_public_key(22), test_public_key(23)]).into(),
                    configuration_threshold: 1,
                    tip_message: MsgId::root(),
                    tip_slot: Slot::default(),
                    tip_sequencer: 0,
                    tip_sequencer_starting_slot: Slot::default(),
                    posting_timeframe: 0.into(),
                    transfer_threshold: 2,
                    posting_timeout: 0.into(),
                },
            );

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

        // The deposited note is consumed and re-created as a channel note
        // under a new NoteId.
        let deposited = Utxo::new(deposit_op.op_id(), 0, utxo.note).id();
        assert!(!updated.utxos.contains(&note_id));
        assert!(!updated.channels.is_channel_note(&note_id));
        assert!(updated.utxos.contains(&deposited));
        assert!(updated.channels.is_channel_note_of(&deposited, &channel_id));

        assert_eq!(events.len(), 1);
        let Some(TxEvent {
            tx_hash,
            op_id,
            payload:
                TxEventPayload::Deposit {
                    channel_id: event_channel_id,
                    amount,
                    metadata,
                    notes,
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
        assert_eq!(notes.clone().into_inner(), vec![deposited]);
    }

    #[test]
    fn deposit_derives_one_channel_note_per_input() {
        let channel_id = ChannelId::from([0u8; 32]);

        let (_, first) = utxo(6u64);
        let (_, second) = utxo(7u64);

        let deposit_op = DepositOp {
            channel_id,
            inputs: [first.id(), second.id()].into(),
            metadata: Metadata::empty(),
        };

        let (updated, _) = deposit_op
            .execute(DepositExecutionContext {
                channels: Channels::new(),
                utxos: utxo_tree(vec![first, second]),
                tx_hash: [0; 32].into(),
            })
            .expect("execution should succeed");

        // Each input is re-created at its own output index, so the two notes
        // get distinct identifiers even though they share an OpId.
        let first_deposited = Utxo::new(deposit_op.op_id(), 0, first.note).id();
        let second_deposited = Utxo::new(deposit_op.op_id(), 1, second.note).id();
        assert_ne!(first_deposited, second_deposited);

        assert!(!updated.utxos.contains(&first.id()));
        assert!(!updated.utxos.contains(&second.id()));
        assert!(
            updated
                .channels
                .is_channel_note_of(&first_deposited, &channel_id)
        );
        assert!(
            updated
                .channels
                .is_channel_note_of(&second_deposited, &channel_id)
        );
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

    // Leaf commitment
    fn leaf_channel_state() -> ChannelState {
        ChannelState {
            accredited_keys: Keys::from([test_public_key(1), test_public_key(2)]).into(),
            configuration_threshold: 2,
            tip_message: MsgId::from([3u8; 32]),
            tip_slot: Slot::new(42),
            tip_sequencer: 1,
            tip_sequencer_starting_slot: Slot::new(7),
            posting_timeframe: SlotTimeframe(10),
            posting_timeout: SlotTimeout(20),
            transfer_threshold: 2,
        }
    }

    // The encoding is spelled out field by field, so any change to `leaf_hash`
    // has to be mirrored here and in the specification.
    #[test]
    fn channel_leaf_matches_the_specified_encoding() {
        let channel_id = ChannelId::from([0u8; 32]);
        let channel = leaf_channel_state();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"CHANNEL_HASH_V1");
        bytes.extend_from_slice(channel_id.as_ref());
        bytes.extend_from_slice(test_public_key(1).as_bytes());
        bytes.extend_from_slice(test_public_key(2).as_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&[3u8; 32]);
        bytes.extend_from_slice(&42u64.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&7u64.to_le_bytes());
        bytes.extend_from_slice(&10u32.to_le_bytes());
        bytes.extend_from_slice(&20u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());

        let expected: Hash = Hasher::digest(&bytes).into();
        assert_eq!(channel.leaf_hash(&channel_id), expected);
    }

    #[test]
    fn channel_leaf_binds_the_channel_id() {
        let channel = leaf_channel_state();

        assert_ne!(
            channel.leaf_hash(&ChannelId::from([0u8; 32])),
            channel.leaf_hash(&ChannelId::from([1u8; 32]))
        );
    }

    #[test]
    fn channel_leaf_binds_every_field() {
        let channel_id = ChannelId::from([0u8; 32]);
        let channel = leaf_channel_state();
        let leaf = channel.leaf_hash(&channel_id);

        let mutations = [
            ChannelState {
                accredited_keys: Keys::from(test_public_key(9)).into(),
                ..channel.clone()
            },
            ChannelState {
                configuration_threshold: 1,
                ..channel.clone()
            },
            ChannelState {
                tip_message: MsgId::root(),
                ..channel.clone()
            },
            ChannelState {
                tip_slot: Slot::new(43),
                ..channel.clone()
            },
            ChannelState {
                tip_sequencer: 0,
                ..channel.clone()
            },
            ChannelState {
                tip_sequencer_starting_slot: Slot::new(8),
                ..channel.clone()
            },
            ChannelState {
                posting_timeframe: SlotTimeframe(11),
                ..channel.clone()
            },
            ChannelState {
                posting_timeout: SlotTimeout(21),
                ..channel.clone()
            },
            ChannelState {
                transfer_threshold: 1,
                ..channel
            },
        ];

        for mutated in mutations {
            assert_ne!(mutated.leaf_hash(&channel_id), leaf);
        }
    }

    #[test]
    fn channels_root_tracks_insertions_and_updates() {
        let channel_id = ChannelId::from([0u8; 32]);
        let channels = Channels::new();
        let empty_root = channels.channels_root();

        let channels = channels.set_channel_state(&channel_id, leaf_channel_state());
        let root = channels.channels_root();
        assert_ne!(root, empty_root);

        let channels = channels.set_channel_state(
            &channel_id,
            ChannelState {
                tip_slot: Slot::new(43),
                ..leaf_channel_state()
            },
        );
        assert_ne!(channels.channels_root(), root);

        // The state is updated in place, so restoring it restores the root
        // instead of committing to a second leaf.
        let channels = channels.set_channel_state(&channel_id, leaf_channel_state());
        assert_eq!(channels.channels_root(), root);
    }
}
