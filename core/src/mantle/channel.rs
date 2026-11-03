use std::sync::Arc;

use lb_binary_codec::canonical::BinaryCodec;
use lb_cryptarchia_engine::Slot;
use lb_program_spend::ZkProgramSpendInputsError;
use lb_zktransfer::ZkTransferInputsError;
use serde::{Deserialize, Serialize};

use crate::{
    events::TxEvent,
    mantle::{
        gas::ThresholdSource,
        ledger::verification_mode::GenesisMode,
        note_set::{NoteSetError, NoteSetIndex, NoteSets},
        notes::{NoteNullifier, ProgramId},
        ops::{
            SignedOperation,
            channel::{
                ChannelId, ChannelKeyIndex, MsgId, NULL_CHANNEL_ID, UnverifiedChannelKeys,
                inscribe::{InscriptionExecutionContext, InscriptionOp},
            },
        },
        transactions::states::Verified,
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
    #[error("Invalid signer")]
    InvalidSigner,
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
    #[error(
        "Invalid number of signatures (treshold:?) for channel {channel_id:?}, expected {actual:?}"
    )]
    ThresholdUnmet {
        channel_id: ChannelId,
        threshold: u16,
        actual: usize,
    },
    #[error("Channel {channel_id:?} has no note set")]
    NoNoteSet { channel_id: ChannelId },
    #[error(transparent)]
    NoteSet(#[from] NoteSetError),
    #[error(transparent)]
    ZkTransferInputs(#[from] ZkTransferInputsError),
    #[error("The inscription carries {proofs} step proofs for {steps} steps")]
    StepProofCount { steps: usize, proofs: usize },
    #[error("Note {0:?} is consumed by two steps of the inscription")]
    StepDoubleSpend(NoteNullifier),
    #[error("Program {0:?} is not registered")]
    UnknownProgram(ProgramId),
    #[error("The proof of step {step} is not of the kind of the step")]
    StepProofKind { step: usize },
    #[error("Program outputs of step {step} name an output it does not have")]
    ProgramOutputsOutOfRange { step: usize },
    #[error("Step {step} carries {proofs} payment proofs for {payments} payments")]
    PaymentProofCount {
        step: usize,
        payments: usize,
        proofs: usize,
    },
    #[error("Step {step} carries {openings} openings for {program_outputs} outputs of the program")]
    ProgramNoteCount {
        step: usize,
        openings: usize,
        program_outputs: usize,
    },
    #[error("The opening of output {output} of step {step} does not match it")]
    ProgramNoteOpening { step: usize, output: usize },
    #[error("The payments of step {step} overflow")]
    PaidValueOverflow { step: usize },
    #[error(transparent)]
    ZkProgramSpendInputs(#[from] ZkProgramSpendInputsError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channels {
    pub channels: rpds::HashTrieMapSync<ChannelId, ChannelState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelState {
    // We cannot use verified Ed25519 public keys here because genesis creates a channel with a key
    // of all `0`s, which would otherwise fail to deserialize here.
    // Channel Configuration
    pub accredited_keys: Arc<UnverifiedChannelKeys>, // keys.len() <= ChannelKeyIndex::MAX
    pub configuration_threshold: u16,                /* indicating how many keys are required to
                                                      * update
                                                      * the
                                                      * configuration */

    // Message Ordering
    pub tip_message: MsgId,     // last message of the channel
    pub config_tip_hash: MsgId, // last configuration of the channel

    // Decentralized Sequencing
    pub tip_slot: Slot,
    pub tip_sequencer: u16, /* indicating the actual sequencer position in the list of
                             * accredited keys */
    pub tip_sequencer_starting_slot: Slot,
    pub posting_timeframe: SlotTimeframe, // number of slots (0 = infinity)
    pub posting_timeout: SlotTimeout,     // number of slots (0 = no timeout)

    // Bridging
    pub note_set: Option<NoteSetIndex>, // index of the note set, None for the null channel
}

impl Default for Channels {
    fn default() -> Self {
        Self::new()
    }
}

impl ThresholdSource for Channels {
    fn configuration_threshold(&self, channel: &ChannelId) -> ChannelKeyIndex {
        self.channels
            .get(channel)
            .map_or(0, |channel| channel.configuration_threshold)
    }
}

impl Channels {
    pub fn from_genesis(
        signed_operations: [SignedOperation<InscriptionOp, Verified, GenesisMode>; 2],
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        // The Cryptarchia inscription creates the null channel and the distribution
        // inscription follows it; `GenesisTx` checked the null channel, the null key,
        // the parent chain and the absence of steps. The null channel gets no note set.
        let mut context = InscriptionExecutionContext {
            channels: Self::default(),
            note_sets: NoteSets::new(),
            block_slot: Slot::default(),
        };
        let mut all_events = Vec::new();
        for signed_operation in signed_operations {
            let (result, events) = signed_operation
                .execute(context)
                .map_err(|(_signed_operation, error)| error)?;
            context = result;
            all_events.extend(events);
        }
        Ok((context.channels, all_events))
    }

    #[must_use]
    pub fn new() -> Self {
        Self {
            channels: rpds::HashTrieMapSync::new_sync(),
        }
    }

    #[must_use]
    pub fn channel_state(&self, channel_id: &ChannelId) -> Option<&ChannelState> {
        self.channels.get(channel_id)
    }

    #[must_use]
    pub fn create_channel(
        mut self,
        channel_id: ChannelId,
        block_slot: Slot,
        keys: UnverifiedChannelKeys,
        note_sets: NoteSets,
    ) -> (Self, NoteSets) {
        // The null channel only receives the genesis inscriptions and holds no note.
        let (note_sets, note_set) = if channel_id == NULL_CHANNEL_ID {
            (note_sets, None)
        } else {
            let (note_sets, index) = note_sets.push_empty();
            (note_sets, Some(index))
        };
        self.channels = self.channels.insert(
            channel_id,
            ChannelState {
                accredited_keys: Arc::new(keys),
                configuration_threshold: 1,
                tip_message: MsgId::root(),
                config_tip_hash: MsgId::root(),
                tip_slot: block_slot,
                tip_sequencer: 0,
                tip_sequencer_starting_slot: block_slot,
                posting_timeframe: 0.into(),
                posting_timeout: 0.into(),
                note_set,
            },
        );
        (self, note_sets)
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

    /// The first slot after `slot` at which [`Self::round_robin`] can change
    /// hands. Before the timeout rotation is active: the next timeframe
    /// multiple from the tip sequencer's starting slot, or the slot the
    /// timeout kicks in, whichever comes first. Once it is active only the
    /// timeout grid from the tip slot moves the turn. `None` when neither
    /// period is set.
    #[must_use]
    pub fn next_round_robin_boundary(&self, slot: Slot) -> Option<Slot> {
        let current = slot.into_inner();
        let next_on_grid = |anchor: Slot, period: u32| -> Option<u64> {
            if period == 0 {
                return None;
            }
            let (anchor, period) = (anchor.into_inner(), u64::from(period));
            let periods_elapsed = current.saturating_sub(anchor) / period;
            anchor.checked_add(periods_elapsed.checked_add(1)?.checked_mul(period)?)
        };
        let by_timeout = next_on_grid(self.tip_slot, self.posting_timeout.0);
        let timed_out = self.posting_timeout.0 != 0
            && current.saturating_sub(self.tip_slot.into_inner())
                >= u64::from(self.posting_timeout.0);
        if timed_out {
            return by_timeout.map(Slot::from);
        }
        let by_timeframe = next_on_grid(self.tip_sequencer_starting_slot, self.posting_timeframe.0);
        by_timeframe
            .into_iter()
            .chain(by_timeout)
            .min()
            .map(Slot::from)
    }
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_keys::keys::{Ed25519Key, UnverifiedEd25519PublicKey};

    use super::*;
    use crate::mantle::note_set::NoteSet;

    fn test_public_key(seed: u8) -> UnverifiedEd25519PublicKey {
        Ed25519Key::from_bytes(&[seed; 32])
            .public_key()
            .into_unverified()
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
            accredited_keys: UnverifiedChannelKeys::try_from(
                (0..num_keys).map(test_public_key).collect::<Vec<_>>(),
            )
            .unwrap()
            .into(),
            configuration_threshold: 0,
            tip_message: MsgId::root(),
            config_tip_hash: MsgId::root(),
            note_set: None,
        }
    }

    #[test]
    fn create_channel_gives_the_null_channel_no_note_set_and_others_the_next_set() {
        let keys = UnverifiedChannelKeys::from(test_public_key(1));
        let (channels, note_sets) = Channels::new().create_channel(
            NULL_CHANNEL_ID,
            Slot::new(3),
            keys.clone(),
            NoteSets::new(),
        );
        assert_eq!(note_sets, NoteSets::new());
        let (channels, note_sets) = channels.create_channel(
            ChannelId::from([1u8; 32]),
            Slot::new(3),
            keys.clone(),
            note_sets,
        );
        let (channels, note_sets) = channels.create_channel(
            ChannelId::from([2u8; 32]),
            Slot::new(3),
            keys.clone(),
            note_sets,
        );

        assert_eq!(
            channels.channel_state(&NULL_CHANNEL_ID).unwrap().note_set,
            None
        );
        assert_eq!(
            channels
                .channel_state(&ChannelId::from([2u8; 32]))
                .unwrap()
                .note_set,
            Some(3)
        );
        assert_eq!(note_sets.get(3), Some(&NoteSet::empty()));
        assert!(note_sets.get(4).is_none());
        assert_eq!(
            channels.channel_state(&ChannelId::from([1u8; 32])),
            Some(&ChannelState {
                accredited_keys: Arc::new(keys),
                configuration_threshold: 1,
                tip_message: MsgId::root(),
                config_tip_hash: MsgId::root(),
                tip_slot: Slot::new(3),
                tip_sequencer: 0,
                tip_sequencer_starting_slot: Slot::new(3),
                posting_timeframe: 0.into(),
                posting_timeout: 0.into(),
                note_set: Some(2),
            })
        );
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

    #[test]
    fn next_boundary_none_without_periods() {
        let channel = make_channel(100, 2, 80, 0, 0, 5);
        assert_eq!(channel.next_round_robin_boundary(100.into()), None);
    }

    #[test]
    fn next_boundary_follows_timeout_grid_from_tip_slot() {
        let channel = make_channel(100, 1, 90, 0, 50, 4);
        assert_eq!(
            channel.next_round_robin_boundary(100.into()),
            Some(150.into())
        );
        assert_eq!(
            channel.next_round_robin_boundary(149.into()),
            Some(150.into())
        );
        assert_eq!(
            channel.next_round_robin_boundary(150.into()),
            Some(200.into())
        );
    }

    #[test]
    fn next_boundary_follows_timeframe_grid_from_starting_slot() {
        let channel = make_channel(100, 0, 95, 10, 0, 3);
        assert_eq!(
            channel.next_round_robin_boundary(100.into()),
            Some(105.into())
        );
        assert_eq!(
            channel.next_round_robin_boundary(105.into()),
            Some(115.into())
        );
    }

    #[test]
    fn next_boundary_ignores_the_timeframe_grid_once_timed_out() {
        // timeframe grid 3, 8, 13, 18, 23, 28, 33; timeout kicks in at 22 and
        // then rotates at 34, 46, ...
        let channel = make_channel(10, 0, 3, 5, 12, 4);
        assert_eq!(
            channel.next_round_robin_boundary(18.into()),
            Some(22.into())
        );
        assert_eq!(
            channel.next_round_robin_boundary(21.into()),
            Some(22.into())
        );
        assert_eq!(
            channel.next_round_robin_boundary(22.into()),
            Some(34.into())
        );
        assert_eq!(
            channel.next_round_robin_boundary(33.into()),
            Some(34.into())
        );
    }

    /// Every reported boundary is the first slot where `round_robin` moves,
    /// and every move is reported by the slot before it.
    #[test]
    fn next_boundary_matches_round_robin_transitions() {
        let channels = [
            make_channel(100, 1, 90, 0, 50, 4),
            make_channel(100, 0, 95, 10, 0, 3),
            make_channel(100, 0, 95, 10, 7, 3),
            make_channel(100, 2, 100, 3, 8, 2),
            make_channel(10, 0, 3, 5, 12, 4),
        ];
        for channel in &channels {
            for slot in 10u64..200 {
                let boundary = channel
                    .next_round_robin_boundary(slot.into())
                    .unwrap()
                    .into_inner();
                let here = channel.round_robin(slot.into());
                for between in slot..boundary {
                    assert_eq!(channel.round_robin(between.into()), here, "slot {between}");
                }
                if channel.round_robin((slot + 1).into()) != here {
                    assert_eq!(boundary, slot + 1, "transition after slot {slot}");
                }
            }
        }
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
