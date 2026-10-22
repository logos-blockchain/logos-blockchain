//! Invariants checked against the simulated ledger after every settle: what
//! a sequencer reports about deposits, its channel wallet and its turn must
//! agree with the node it talks to.

use std::collections::{BTreeMap, HashMap, HashSet};

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
    /// How many of the node's posts by this sequencer have been judged.
    judged_posts: usize,
    /// Hand-built txs the test submitted whole: the SDK posts those without
    /// a turn, as the caller asked, so they are not judged.
    exempt: HashSet<TxHash>,
    /// The slot the sequencer's view named at the previous check: a post
    /// since then was made at that slot or a later delivered one.
    last_view_slot: Option<Slot>,
    /// Consecutive checks each own pending tx was postable and not posted,
    /// with how often it had been posted when the counting started.
    unposted: HashMap<TxHash, (usize, usize)>,
    violations: Violations,
}

/// Checks in a row a postable tx may go unposted: the resubmit timer at
/// [`crate::harness::RESUBMIT_INTERVAL`] fires every third step.
const UNPOSTED_CHECKS: usize = 4;

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

fn by_id(notes: &[ChannelNote]) -> BTreeMap<NoteId, Value> {
    notes.iter().map(|n| (n.note_id, n.value)).collect()
}

impl Oracles {
    /// Oracles for the sequencer signing as `own` with the given turn
    /// margin.
    #[must_use]
    pub fn for_sequencer(
        own: UnverifiedEd25519PublicKey,
        min_slots_remaining_in_turn: u64,
    ) -> Self {
        Self {
            own: Some(own),
            min_slots_remaining_in_turn,
            ..Self::default()
        }
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

    /// Every post of an own plain inscription was in turn, with the margin,
    /// by what the sequencer could know when it posted: its slot is the
    /// last tip its streams delivered, its channel state is fetched from
    /// the node's actual tip, so either block's rotation may be the one it
    /// computed from. A block mined later at another slot may still reject
    /// the post; that is the chain's doing, not the sequencer's.
    fn check_posts(&mut self, node: &SimNode, channel: ChannelId, name: &str, view_slot: Slot) {
        let (posts, count) = node.posts_since(self.judged_posts);
        self.judged_posts = count;
        let since = self.last_view_slot.replace(view_slot);
        for post in posts {
            let Some(seen) = post.seen else {
                continue;
            };
            if self.exempt.contains(&post.hash)
                || self.own_plain_inscription(&post.tx, channel).is_none()
            {
                continue;
            }
            let references = [seen, post.tip];
            let states: Vec<ChannelState> = references
                .iter()
                .filter_map(|block| node.channel_state_at(*block, channel))
                .collect();
            if states.is_empty() {
                // No channel yet: the SDK publishes freely, as it must.
                continue;
            }
            let mut slots: Vec<Slot> = references
                .iter()
                .filter_map(|block| node.block_slot(*block))
                .collect();
            slots.extend(since);
            let in_turn = slots.iter().any(|slot| {
                states
                    .iter()
                    .any(|state| self.own_turn_with_margin(state, *slot))
            });
            if !in_turn {
                let own_index = self
                    .own
                    .and_then(|own| states[0].accredited_keys.iter().position(|key| *key == own));
                let rotations: Vec<String> = slots
                    .iter()
                    .map(|slot| format!("slot {slot:?}: {:?}", states[0].round_robin(*slot)))
                    .collect();
                self.record(
                    Layer::Configs,
                    format!(
                        "{name}: posted {:?} out of its turn or past the turn margin; own index \
                         {own_index:?}, timeframe {:?}, round robin {}",
                        post.hash,
                        states[0].posting_timeframe,
                        rotations.join(" / ")
                    ),
                );
            }
        }
    }

    /// An own plain inscription that is pending, chains on the channel tip
    /// and whose turn it is must be posted: in the mempool, in a block, or
    /// posted since the check started. One the node ever rejected is left
    /// to the stale refund. Unposted for `UNPOSTED_CHECKS` checks in a row
    /// means it was never re-posted.
    fn check_resubmit(
        &mut self,
        sequencer: &ZoneSequencer<SimNode>,
        node: &SimNode,
        channel: ChannelId,
        name: &str,
    ) {
        let Some(checkpoint) = sequencer.checkpoint() else {
            return;
        };
        let Some(state) = node.channel_state_at_tip(channel) else {
            return;
        };
        let slot = node.tip_slot();
        let mut postable = HashSet::new();
        for (hash, tx) in &checkpoint.pending_txs {
            if self.exempt.contains(hash) {
                continue;
            }
            let Some(info) = self.own_plain_inscription(tx, channel) else {
                continue;
            };
            if info.parent_msg != state.tip_message || !self.own_turn_with_margin(&state, slot) {
                continue;
            }
            if node.in_mempool(hash) || node.is_included(hash) || node.rejected_at(hash).is_some() {
                continue;
            }
            let posted = node.post_count(hash);
            let (checks, since) = self.unposted.entry(*hash).or_insert((0, posted));
            if posted > *since {
                *checks = 0;
                *since = posted;
                continue;
            }
            postable.insert(*hash);
            *checks += 1;
            if *checks == UNPOSTED_CHECKS {
                self.record(
                    Layer::Lineage,
                    format!(
                        "{name}: pending {hash:?} chains on the channel tip in its turn and was \
                         not posted for {UNPOSTED_CHECKS} checks"
                    ),
                );
            }
        }
        self.unposted.retain(|hash, _| postable.contains(hash));
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
    }

    /// Every deposit an event reports must sit on the branch of the tip the
    /// event named (the node may have moved on since it emitted the event);
    /// every finalized tx is reported once.
    pub fn observe(&mut self, event: &Event, node: &SimNode, name: &str) {
        let Event::BlocksProcessed {
            checkpoint,
            deposits,
            finalized,
            ..
        } = event
        else {
            return;
        };
        self.lib = Some(checkpoint.lib);
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
        let view_slot = sequencer.subscribe_channel_view().borrow().current_slot;
        self.check_posts(node, channel, name, view_slot);
        if !sequencer.is_connected() || !node.subscriber_at_tip() || self.lib != Some(node.lib()) {
            return;
        }
        self.check_wallet(sequencer, node, channel, name);
        self.check_turn(sequencer, node, channel, name);
        self.check_finalized(node, channel, name);
        self.check_deposits_reported(node, channel, name);
        self.check_resubmit(sequencer, node, channel, name);
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
        }
    }

    fn record(&mut self, layer: Layer, message: String) {
        self.violations.record(layer, message);
    }
}
