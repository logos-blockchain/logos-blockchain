//! Invariants checked against the simulated ledger after every settle: what
//! a sequencer reports about deposits, its channel wallet and its turn must
//! agree with the node it talks to.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::Duration,
};

use lb_common_http_client::Slot;
use lb_core::{
    crypto::Hash,
    header::HeaderId,
    mantle::{
        NoteId, SignedOps, Value,
        channel::ChannelState,
        ledger::verification_mode::StandardMode,
        ops::{
            OpId as _, OpRef,
            channel::{ChannelId, MsgId},
        },
        traits::Hashable,
        transactions::states::Unverified,
    },
};
use lb_key_management_system_service::keys::UnverifiedEd25519PublicKey;
use lb_zone_sdk::{
    node_types::TxHash,
    sequencer::{
        ChannelNote, Event, FinalizedOp, InscriptionInfo, SequencerCheckpoint, ZoneSequencer,
        channel_inscriptions,
    },
};
use tokio::time::Instant;

use crate::{
    layers::{Layer, Violations},
    node::SimNode,
};

#[derive(Debug, Default)]
pub struct Oracles {
    /// LIB of the last processed event; compared only once the sequencer
    /// agrees with the node on it.
    lib: Option<HeaderId>,
    /// Every finalized tx reported since `finalized_from`, in report order.
    finalized: Vec<TxHash>,
    /// The LIB the sequencer's current life started from: finalized txs
    /// above it are expected, those below were reported in an earlier life
    /// or never (fresh start without history). `None` means genesis.
    finalized_from: Option<HeaderId>,
    /// Whether this life resumed an earlier one, so what it reports is
    /// held to what an uninterrupted sequencer would have reported.
    resumed: bool,
    /// Every deposit op reported through `deposits` or `finalized`, across
    /// lives: what the canonical chain holds must appear here eventually.
    reported_deposits: HashSet<Hash>,
    /// The sequencer's key, to tell its own inscriptions from the ones it
    /// tracks for others.
    own: Option<UnverifiedEd25519PublicKey>,
    /// The sequencer's `min_slots_remaining_in_turn`.
    min_slots_remaining_in_turn: u64,
    /// The sequencer's `stale_refund_slots`.
    stale_refund_slots: u64,
    /// The sequencer's resubmit timer: it ticks on the paused clock every
    /// `resubmit_interval` from `started_at`, when the sequencer was
    /// initialised, the first tick at once.
    resubmit_interval: Duration,
    started_at: Option<Instant>,
    /// When the previous check ran.
    last_check: Option<Instant>,
    /// Each own pending message's current tx hash and funding stamp, from
    /// the checkpoint: a change is a re-fund, legal only after the window.
    fundings: HashMap<MsgId, (TxHash, Slot)>,
    /// Messages reported orphaned since last seen pending: a consumer
    /// republishing one on the same parent gives it the same id under a
    /// fresh tx, funded now, which is not a re-fund.
    orphaned: HashSet<MsgId>,
    /// For each stale posted own tx, the in-turn ticks it sat un-refunded
    /// through, and its stamp when the counting started.
    unrefunded: HashMap<TxHash, (usize, Slot)>,
    /// How many of the node's posts by this sequencer have been judged.
    judged_posts: usize,
    /// The stream position at the previous check: the posts judged now were
    /// decided since then, inside one settle, where no time passes.
    settle_start: usize,
    /// Hand-built txs the test submitted whole: the SDK posts those without
    /// a turn, as the caller asked, so they are not judged.
    exempt: HashSet<TxHash>,
    /// The turns the sequencer announced, in order, each with how many
    /// events its stream had delivered by then: what it believed when it
    /// decided a post. Announcements are in band with the block events, so
    /// the order is the sequencer's own.
    claims: Vec<(usize, bool)>,
    /// Whether the previous check found the sequencer in turn.
    was_in_turn: bool,
    /// For each own pending tx chaining on the channel tip, the in-turn
    /// ticks it waited unposted through, with how often it had been posted
    /// when the counting started.
    unposted: HashMap<TxHash, (usize, usize)>,
    violations: Violations,
}

/// Checks at which the sequencer was in turn and its resubmit timer had
/// ticked since the previous check, with a postable or stale tx left as it
/// was, before that is a violation: the second absorbs a tick that fired
/// before the block that opened the turn in the same settle.
const IDLE_TICKS: usize = 2;

/// Whether `tx` carries an op of `channel`.
fn touches_channel(tx: &SignedOps<Unverified, StandardMode>, channel: ChannelId) -> bool {
    tx.op_refs_iter().any(|op| match op {
        OpRef::ChannelInscribe(i) => i.channel_id == channel,
        OpRef::ChannelConfig(c) => c.channel == channel,
        OpRef::ChannelDeposit(d) => d.channel_id == channel,
        OpRef::ChannelWithdraw(w) => w.channel_id == channel,
        OpRef::ChannelTransfer(t) => t.channel_id == channel,
        _ => false,
    })
}

/// An own pending inscription: its message, tx hash and funding stamp.
type Funding = (MsgId, TxHash, Slot);

fn by_id(notes: &[ChannelNote]) -> BTreeMap<NoteId, Value> {
    notes.iter().map(|n| (n.note_id, n.value)).collect()
}

impl Oracles {
    /// Oracles for the sequencer signing as `own` with the given turn
    /// margin and stale window.
    #[must_use]
    pub fn for_sequencer(
        own: UnverifiedEd25519PublicKey,
        min_slots_remaining_in_turn: u64,
        stale_refund_slots: u64,
        resubmit_interval: Duration,
    ) -> Self {
        Self {
            own: Some(own),
            min_slots_remaining_in_turn,
            stale_refund_slots,
            resubmit_interval,
            started_at: Some(Instant::now()),
            ..Self::default()
        }
    }

    /// Ticks of the resubmit timer up to `at`.
    fn ticks_by(&self, at: Instant) -> u64 {
        let Some(started_at) = self.started_at else {
            return 0;
        };
        let period = self.resubmit_interval.as_nanos().max(1);
        u64::try_from(at.saturating_duration_since(started_at).as_nanos() / period + 1)
            .expect("fits")
    }

    #[must_use]
    pub const fn violations(&self) -> &Violations {
        &self.violations
    }

    /// Leave `hash`, a tx the test built and submitted whole, out of the
    /// turn and resubmit checks.
    pub fn exempt(&mut self, hash: TxHash) {
        self.exempt.insert(hash);
    }

    /// Whether the last turn the sequencer announced by the time its
    /// stream had delivered `delivered` events claimed the turn.
    fn claimed_turn_at(&self, delivered: usize) -> Option<bool> {
        self.claims
            .iter()
            .rev()
            .find(|(by, _)| *by <= delivered)
            .map(|(_, claim)| *claim)
    }

    /// The inscription of `tx` when it is a plain own publish: one channel
    /// inscription signed by this sequencer and no other channel op, the
    /// shape the SDK gates on the turn.
    fn own_plain_inscription(
        &self,
        tx: &SignedOps<Unverified, StandardMode>,
        channel: ChannelId,
    ) -> Option<InscriptionInfo> {
        let own = self.own?;
        let channel_ops = tx
            .op_refs_iter()
            .filter(|op| !matches!(op, OpRef::Transfer(_)))
            .count();
        let inscriptions = channel_inscriptions(tx, channel);
        let [info] = inscriptions.as_slice() else {
            return None;
        };
        (channel_ops == 1 && info.signer == Some(own)).then(|| info.clone())
    }

    /// Whether the round robin hands this sequencer the turn at `slot` with
    /// at least the configured margin left, the SDK's own rule.
    fn own_turn_with_margin(&self, state: &ChannelState, slot: Slot) -> bool {
        let Some(own) = self.own else {
            return false;
        };
        let Some(index) = state.accredited_keys.iter().position(|key| *key == own) else {
            return false;
        };
        let (authorized, turn_start) = state.round_robin(slot);
        if usize::from(authorized) != index {
            return false;
        }
        let timeframe = u64::from(u32::from(state.posting_timeframe.clone()));
        if self.min_slots_remaining_in_turn == 0 || timeframe == 0 {
            return true;
        }
        let closes_at = (turn_start.into_inner() + timeframe)
            .checked_sub(self.min_slots_remaining_in_turn)
            .map_or(0, |slot| slot + 1);
        slot.into_inner() < closes_at
    }

    /// Every post of an own plain inscription was made while the sequencer's
    /// own view claimed the turn: the view published at the stream position
    /// of the call, at the one before, since a post decided before a
    /// delivery reaches the node after it, or at any position since the
    /// previous check, since inside one settle a backlog is delivered with
    /// no time for the post to get ahead of it. Whether the view was right
    /// is [`Self::check_turn`]'s question; whether the node then took the
    /// post is the chain's.
    fn check_posts(&mut self, node: &SimNode, channel: ChannelId, name: &str) {
        let (posts, count) = node.posts_since(self.judged_posts);
        self.judged_posts = count;
        let settle_start = std::mem::replace(&mut self.settle_start, node.delivered_count());
        for post in posts {
            if post.delivered == 0
                || self.exempt.contains(&post.hash)
                || self.own_plain_inscription(&post.tx, channel).is_none()
            {
                continue;
            }
            let claimed = [post.delivered - 1, post.delivered]
                .iter()
                .any(|delivered| self.claimed_turn_at(*delivered).is_none_or(|claim| claim))
                || self
                    .claims
                    .iter()
                    .any(|(by, claim)| *claim && (settle_start..=post.delivered).contains(by));
            if claimed {
                continue;
            }
            let slots: Vec<Slot> = node
                .delivered_tips(post.delivered.saturating_sub(2), post.delivered)
                .into_iter()
                .filter_map(|tip| node.block_slot(tip))
                .collect();
            self.record(
                Layer::Configs,
                format!(
                    "{name}: posted {:?} at stream slots {slots:?} after announcing no turn",
                    post.hash
                ),
            );
        }
    }

    /// An own plain inscription that is pending and chains on the channel
    /// tip must be posted once its owner's turn comes: in the mempool, in a
    /// block, or posted since it began waiting. One the node ever rejected
    /// is left to the stale refund. One waiting as the turn begins is
    /// re-posted as the sequencer sees the turn, by the next check; one
    /// that became postable in the turn, on the resubmit timer, within
    /// [`IDLE_TICKS`] in-turn ticks.
    #[expect(
        clippy::too_many_arguments,
        reason = "the turn and tick facts of this check"
    )]
    fn check_resubmit(
        &mut self,
        sequencer: &ZoneSequencer<SimNode>,
        node: &SimNode,
        channel: ChannelId,
        name: &str,
        in_turn: bool,
        turn_began: bool,
        ticked: bool,
    ) {
        let Some(checkpoint) = sequencer.checkpoint() else {
            return;
        };
        let Some(state) = node.channel_state_at_tip(channel) else {
            return;
        };
        let mut waiting = HashSet::new();
        for (hash, tx) in &checkpoint.pending_txs {
            if self.exempt.contains(hash) {
                continue;
            }
            let Some(info) = self.own_plain_inscription(tx, channel) else {
                continue;
            };
            if info.parent_msg != state.tip_message {
                continue;
            }
            if node.in_mempool(hash) || node.is_included(hash) || node.rejected_at(hash).is_some() {
                continue;
            }
            let posted = node.post_count(hash);
            waiting.insert(*hash);
            let was_waiting = self.unposted.contains_key(hash);
            let (ticks, count) = self.unposted.entry(*hash).or_insert((0, posted));
            if posted > *count {
                *ticks = 0;
                *count = posted;
                continue;
            }
            if !in_turn {
                continue;
            }
            let idle = if turn_began && was_waiting {
                // Waiting as the turn began: the turn's own re-post is due.
                IDLE_TICKS
            } else if ticked {
                *ticks += 1;
                *ticks
            } else {
                *ticks
            };
            if idle >= IDLE_TICKS {
                *ticks = 0;
                self.record(
                    Layer::Lineage,
                    format!(
                        "{name}: pending {hash:?} chains on the channel tip and was not posted in \
                         its owner's turn"
                    ),
                );
            }
        }
        self.unposted.retain(|hash, _| waiting.contains(hash));
    }

    /// A restart from `before` restored every own pending inscription the
    /// chain had not finalized, as the same message under whatever hash a
    /// re-fund on restore gave it: compare with `after`, the first
    /// checkpoint of the new life.
    pub fn check_restored(
        &mut self,
        before: &SequencerCheckpoint,
        after: Option<&SequencerCheckpoint>,
        orphaned_since: &HashSet<MsgId>,
        node: &SimNode,
        channel: ChannelId,
        name: &str,
    ) {
        // Restored, or restored and already shed as orphaned for the zone
        // to republish: both are the message accounted for.
        let mut restored: HashSet<MsgId> = after
            .map(|cp| {
                cp.pending_txs
                    .iter()
                    .flat_map(|(_, tx)| channel_inscriptions(tx, channel))
                    .map(|info| info.this_msg)
                    .collect()
            })
            .unwrap_or_default();
        restored.extend(orphaned_since.iter().copied());
        for (hash, tx) in &before.pending_txs {
            let Some(info) = self.own_plain_inscription(tx, channel) else {
                continue;
            };
            if node.is_tx_finalized(hash) || restored.contains(&info.this_msg) {
                continue;
            }
            self.record(
                Layer::Restarts,
                format!(
                    "{name}: pending {hash:?} (message {:?}) was not restored from the checkpoint",
                    info.this_msg
                ),
            );
        }
    }

    /// Start a new life of the sequencer from `lib`: what it reports as
    /// finalized from now on is the channel history above that block.
    pub fn restart_from(&mut self, lib: Option<HeaderId>) {
        self.finalized.clear();
        self.finalized_from = lib;
        self.resumed = true;
        self.started_at = Some(Instant::now());
        self.unposted.clear();
        self.unrefunded.clear();
    }

    /// Every deposit an event reports must sit on the branch of the tip the
    /// event named (the node may have moved on since it emitted the event);
    /// every finalized tx is reported once.
    pub fn observe(&mut self, event: &Event, node: &SimNode, name: &str) {
        if let Event::TurnNotification { notification } = event {
            self.claims
                .push((node.delivered_count(), notification.our_turn_to_write));
            return;
        }
        let Event::BlocksProcessed {
            checkpoint,
            channel_update,
            deposits,
            finalized,
        } = event
        else {
            return;
        };
        self.lib = Some(checkpoint.lib);
        self.orphaned.extend(
            channel_update
                .orphaned()
                .iter()
                .filter_map(|entry| entry.inscription().map(|info| info.this_msg)),
        );
        for tx in finalized {
            if self.finalized.contains(&tx.tx_hash) {
                self.record(
                    Layer::Lineage,
                    format!("{name}: tx {:?} reported finalized twice", tx.tx_hash),
                );
            }
            self.finalized.push(tx.tx_hash);
        }
        for tx in finalized {
            for op in &tx.ops {
                if let FinalizedOp::Deposit(deposit) = op {
                    self.reported_deposits.insert(deposit.op_id);
                }
            }
        }
        for deposit in deposits {
            self.reported_deposits.insert(deposit.op_id);
            // Before the first pulled event only finalized history has been
            // reported, which is canonical by definition.
            let on_branch = node.pulled_tip().map_or_else(
                || node.is_included(&deposit.tx_hash),
                |tip| node.is_included_at(tip, &deposit.tx_hash),
            );
            if !on_branch {
                self.record(
                    Layer::Notes,
                    format!(
                        "{name}: deposit {:?} reported from a block that is not canonical",
                        deposit.op_id
                    ),
                );
            }
        }
    }

    /// Compare the wallet view, the turn view and the finalized history of
    /// `sequencer` with `node`, once the sequencer has caught up with it:
    /// connected, told about the tip, agreeing on LIB.
    pub fn check(
        &mut self,
        sequencer: &mut ZoneSequencer<SimNode>,
        node: &SimNode,
        channel: ChannelId,
        name: &str,
    ) {
        let now = Instant::now();
        let ticked = self
            .last_check
            .replace(now)
            .is_none_or(|previous| self.ticks_by(now) > self.ticks_by(previous));
        self.check_posts(node, channel, name);
        self.check_refunds_not_early(sequencer, channel, name);
        if !sequencer.is_connected() || !node.subscriber_at_tip() || self.lib != Some(node.lib()) {
            return;
        }
        self.check_wallet(sequencer, node, channel, name);
        self.check_turn(sequencer, node, channel, name);
        self.check_finalized(node, channel, name);
        self.check_deposits_reported(node, channel, name);
        // Past the gate the stream position is the node's tip: the slot and
        // channel state there are the sequencer's own.
        let in_turn = node
            .channel_state_at_tip(channel)
            .is_some_and(|state| self.own_turn_with_margin(&state, node.tip_slot()));
        let turn_began = in_turn && !self.was_in_turn;
        self.was_in_turn = in_turn;
        self.check_resubmit(sequencer, node, channel, name, in_turn, turn_began, ticked);
        self.check_refunds_not_late(sequencer, node, channel, name, in_turn && ticked);
    }

    /// Own pending plain inscriptions with their funding stamp, from the
    /// checkpoint; the stamp is the sequencer's LIB slot when the tx was
    /// funded or last re-funded.
    fn own_fundings(
        &self,
        sequencer: &ZoneSequencer<SimNode>,
        channel: ChannelId,
    ) -> Option<(Slot, Vec<Funding>)> {
        let checkpoint = sequencer.checkpoint()?;
        let stamps: HashMap<TxHash, Slot> = checkpoint
            .funding
            .iter()
            .map(|record| (record.tx_hash, record.funded_at))
            .collect();
        let fundings = checkpoint
            .pending_txs
            .iter()
            .filter(|(hash, _)| !self.exempt.contains(hash))
            .filter_map(|(hash, tx)| {
                let info = self.own_plain_inscription(tx, channel)?;
                Some((info.this_msg, *hash, *stamps.get(hash)?))
            })
            .collect();
        Some((checkpoint.lib_slot, fundings))
    }

    /// A pending own inscription is re-funded, under a new hash or stamped
    /// anew after a failed re-fund, only once its stamp is more than
    /// `stale_refund_slots` LIB slots old: the new stamp, the LIB slot of
    /// the re-fund, must be that far past the old one.
    fn check_refunds_not_early(
        &mut self,
        sequencer: &ZoneSequencer<SimNode>,
        channel: ChannelId,
        name: &str,
    ) {
        let Some((_, fundings)) = self.own_fundings(sequencer, channel) else {
            return;
        };
        let window = self.stale_refund_slots;
        for (msg, hash, funded_at) in fundings {
            let previous = self.fundings.insert(msg, (hash, funded_at));
            if self.orphaned.remove(&msg) {
                continue;
            }
            let Some((old_hash, old_stamp)) = previous else {
                continue;
            };
            if funded_at == old_stamp {
                continue;
            }
            let aged = u64::from(funded_at).saturating_sub(u64::from(old_stamp));
            if window == 0 || aged <= window {
                self.record(
                    Layer::Lineage,
                    format!(
                        "{name}: message {msg:?} ({old_hash:?} -> {hash:?}) was re-funded \
                         {aged} LIB slots after its funding, within the stale window of {window}"
                    ),
                );
            }
        }
    }

    /// A posted own inscription not mined on the tip's branch whose stamp is
    /// more than `stale_refund_slots` LIB slots behind the sequencer's LIB
    /// is re-funded, a new stamp, by the resubmit timer's next tick in its
    /// owner's turn: `opportunity` is one such tick since the last check.
    fn check_refunds_not_late(
        &mut self,
        sequencer: &ZoneSequencer<SimNode>,
        node: &SimNode,
        channel: ChannelId,
        name: &str,
        opportunity: bool,
    ) {
        let window = self.stale_refund_slots;
        if window == 0 {
            return;
        }
        let Some((lib_slot, fundings)) = self.own_fundings(sequencer, channel) else {
            return;
        };
        let mut stale = HashSet::new();
        for (msg, hash, funded_at) in fundings {
            let age = u64::from(lib_slot).saturating_sub(u64::from(funded_at));
            if age <= window || node.post_count(&hash) == 0 || node.is_included(&hash) {
                continue;
            }
            stale.insert(hash);
            let (ticks, stamp) = self.unrefunded.entry(hash).or_insert((0, funded_at));
            if funded_at != *stamp {
                *ticks = 0;
                *stamp = funded_at;
                continue;
            }
            if !opportunity {
                continue;
            }
            *ticks += 1;
            if *ticks == IDLE_TICKS {
                *ticks = 0;
                self.record(
                    Layer::Lineage,
                    format!(
                        "{name}: posted pending {hash:?} (message {msg:?}) funded {age} LIB slots \
                         ago, past the stale window of {window}, was not re-funded on \
                         {IDLE_TICKS} resubmit ticks in its owner's turn"
                    ),
                );
            }
        }
        self.unrefunded.retain(|hash, _| stale.contains(hash));
    }

    /// Every deposit of `channel` on the canonical chain has been reported,
    /// through `deposits` when its block turned canonical or through
    /// `finalized` once LIB passed it.
    fn check_deposits_reported(&mut self, node: &SimNode, channel: ChannelId, name: &str) {
        for tx in &node.canonical_txs() {
            for op in tx.op_refs_iter() {
                let OpRef::ChannelDeposit(deposit) = op else {
                    continue;
                };
                if deposit.channel_id != channel {
                    continue;
                }
                let op_id = deposit.op_id();
                if !self.reported_deposits.contains(&op_id) {
                    self.record(
                        Layer::Notes,
                        format!("{name}: canonical deposit {op_id:?} was never reported"),
                    );
                }
            }
        }
    }

    /// The finalized channel txs reported in this life are exactly the
    /// node's finalized channel txs above the life's starting LIB, in chain
    /// order.
    fn check_finalized(&mut self, node: &SimNode, channel: ChannelId, name: &str) {
        let expected: Vec<TxHash> = self
            .finalized_from
            .map_or_else(
                || node.finalized_txs(),
                |from| node.finalized_txs_after(from),
            )
            .iter()
            .filter(|tx| touches_channel(tx, channel))
            .map(Hashable::hash)
            .collect();
        if self.finalized != expected {
            let layer = if self.resumed {
                Layer::Restarts
            } else {
                Layer::Lineage
            };
            self.record(
                layer,
                format!(
                    "{name}: finalized txs reported {:?} differ from the chain's {:?}",
                    self.finalized, expected
                ),
            );
        }
    }

    /// `finalized` = channel notes present both at LIB and at the tip;
    /// `unfinalized` = channel notes at the tip that LIB does not have.
    fn check_wallet(
        &mut self,
        sequencer: &mut ZoneSequencer<SimNode>,
        node: &SimNode,
        channel: ChannelId,
        name: &str,
    ) {
        let view = sequencer.handle().channel_wallet();
        let at_tip: BTreeMap<NoteId, Value> = node
            .ledger_at_tip()
            .channel_notes(&channel)
            .into_iter()
            .map(|(id, note)| (id, note.value))
            .collect();
        let at_lib: BTreeMap<NoteId, Value> = node
            .block(node.lib())
            .expect("lib block")
            .ledger
            .channel_notes(&channel)
            .into_iter()
            .map(|(id, note)| (id, note.value))
            .collect();
        let expected_finalized: BTreeMap<NoteId, Value> = at_tip
            .iter()
            .filter(|(id, _)| at_lib.contains_key(id))
            .map(|(id, value)| (*id, *value))
            .collect();
        let expected_unfinalized: BTreeMap<NoteId, Value> = at_tip
            .iter()
            .filter(|(id, _)| !at_lib.contains_key(id))
            .map(|(id, value)| (*id, *value))
            .collect();
        let finalized = by_id(&view.finalized);
        let unfinalized = by_id(&view.unfinalized);
        if finalized != expected_finalized {
            self.record(
                Layer::Notes,
                format!(
                    "{name}: finalized channel wallet {finalized:?} differs from the ledger's {expected_finalized:?}"
                ),
            );
        }
        if unfinalized != expected_unfinalized {
            self.record(
                Layer::Notes,
                format!(
                    "{name}: unfinalized channel wallet {unfinalized:?} differs from the ledger's {expected_unfinalized:?}"
                ),
            );
        }
    }

    /// The authorized index the view names is the round robin's at the
    /// view's slot, and a claimed turn is backed by it.
    fn check_turn(
        &mut self,
        sequencer: &ZoneSequencer<SimNode>,
        node: &SimNode,
        channel: ChannelId,
        name: &str,
    ) {
        let view = sequencer.subscribe_channel_view().borrow().clone();
        let Some(state) = node.channel_state_at_tip(channel) else {
            return;
        };
        let (index, _) = state.round_robin(view.current_slot);
        if view.authorized_key_index != Some(index) {
            self.record(
                Layer::Configs,
                format!(
                    "{name}: view names authorized index {:?} at slot {:?}, round robin says {index}",
                    view.authorized_key_index, view.current_slot
                ),
            );
        }
        if view.our_turn_to_write && view.own_key_index != Some(index) {
            self.record(
                Layer::Configs,
                format!(
                    "{name}: claims the turn at slot {:?} while round robin names index {index}",
                    view.current_slot
                ),
            );
        } else if view.our_turn_to_write && !self.own_turn_with_margin(&state, view.current_slot) {
            self.record(
                Layer::Configs,
                format!(
                    "{name}: claims the turn at slot {:?} with fewer than {} slots left in it",
                    view.current_slot, self.min_slots_remaining_in_turn
                ),
            );
        }
    }

    fn record(&mut self, layer: Layer, message: String) {
        self.violations.record(layer, message);
    }
}
