//! Invariants checked against the simulated ledger after every settle: what
//! a sequencer reports about deposits, its channel wallet and its turn must
//! agree with the node it talks to.

use std::collections::BTreeMap;

use lb_core::{
    header::HeaderId,
    mantle::{
        NoteId, SignedOps, Value,
        ledger::verification_mode::StandardMode,
        ops::{OpRef, channel::ChannelId},
        traits::Hashable,
        transactions::states::Unverified,
    },
};
use lb_zone_sdk::{
    node_types::TxHash,
    sequencer::{ChannelNote, Event, ZoneSequencer},
};

use crate::node::SimNode;

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
    violations: Vec<String>,
}

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
    #[must_use]
    pub fn violations(&self) -> &[String] {
        &self.violations
    }

    /// Start a new life of the sequencer from `lib`: what it reports as
    /// finalized from now on is the channel history above that block.
    pub fn restart_from(&mut self, lib: Option<HeaderId>) {
        self.finalized.clear();
        self.finalized_from = lib;
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
                self.record(format!(
                    "{name}: tx {:?} reported finalized twice",
                    tx.tx_hash
                ));
            }
            self.finalized.push(tx.tx_hash);
        }
        for deposit in deposits {
            // Before the first pulled event only finalized history has been
            // reported, which is canonical by definition.
            let on_branch = node.pulled_tip().map_or_else(
                || node.is_included(&deposit.tx_hash),
                |tip| node.is_included_at(tip, &deposit.tx_hash),
            );
            if !on_branch {
                self.record(format!(
                    "{name}: deposit {:?} reported from a block that is not canonical",
                    deposit.op_id
                ));
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
        if !sequencer.is_connected() || !node.subscriber_at_tip() || self.lib != Some(node.lib()) {
            return;
        }
        self.check_wallet(sequencer, node, channel, name);
        self.check_turn(sequencer, node, channel, name);
        self.check_finalized(node, channel, name);
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
            self.record(format!(
                "{name}: finalized txs reported {:?} differ from the chain's {:?}",
                self.finalized, expected
            ));
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
            self.record(format!(
                "{name}: finalized channel wallet {finalized:?} differs from the ledger's {expected_finalized:?}"
            ));
        }
        if unfinalized != expected_unfinalized {
            self.record(format!(
                "{name}: unfinalized channel wallet {unfinalized:?} differs from the ledger's {expected_unfinalized:?}"
            ));
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
            self.record(format!(
                "{name}: view names authorized index {:?} at slot {:?}, round robin says {index}",
                view.authorized_key_index, view.current_slot
            ));
        }
        if view.our_turn_to_write && view.own_key_index != Some(index) {
            self.record(format!(
                "{name}: claims the turn at slot {:?} while round robin names index {index}",
                view.current_slot
            ));
        }
    }

    fn record(&mut self, message: String) {
        tracing::error!("oracle violated: {message}");
        self.violations.push(message);
    }
}
