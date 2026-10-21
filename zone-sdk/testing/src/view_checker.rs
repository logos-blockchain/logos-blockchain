//! The [`ChannelUpdate`] contract, checked on every block event.
//!
//! An extension only adds entries the consumer does not hold. On a conflict,
//! what a consumer holds from the stream (adopted, not orphaned since, not
//! finalized) must equal `common_prefix ++ adopted`, up to the sequencer's own
//! in-flight publishes, which the prefix carries and the stream never echoes.

use std::collections::HashSet;

use lb_core::mantle::{
    OpRef,
    ops::channel::{ChannelId, MsgId},
};
use lb_zone_sdk::{
    node_types::TxHash,
    sequencer::{
        ChannelUpdate, ChannelUpdateTx, Event, FinalizedOp, FinalizedTx, SequencerCheckpoint,
    },
};

use crate::layers::{Layer, Violations};

fn hashes(entries: &[ChannelUpdateTx]) -> Vec<TxHash> {
    entries.iter().map(ChannelUpdateTx::tx_hash).collect()
}

/// What identifies a view entry across updates: its message, since the
/// same message lands again under a new tx hash when re-funded or rebuilt
/// and the contract has consumers dedup by `this_msg`; the tx hash for an
/// entry carrying no message of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Key {
    Msg(MsgId),
    Tx(TxHash),
}

fn key(entry: &ChannelUpdateTx, channel_id: ChannelId) -> Key {
    if let Some(info) = entry.inscription() {
        return Key::Msg(info.this_msg);
    }
    let ChannelUpdateTx::Config(signed) = entry else {
        return Key::Tx(entry.tx_hash());
    };
    signed
        .op_refs_iter()
        .find_map(|op| match op {
            OpRef::ChannelConfig(config) if config.channel == channel_id => Some(config.id()),
            _ => None,
        })
        .map_or_else(|| Key::Tx(entry.tx_hash()), Key::Msg)
}

#[derive(Debug)]
pub struct ViewChecker {
    channel_id: ChannelId,
    held: HashSet<Key>,
    violations: Violations,
}

impl ViewChecker {
    #[must_use]
    pub fn new(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            held: HashSet::new(),
            violations: Violations::default(),
        }
    }

    /// Every violation recorded so far, in order.
    #[must_use]
    pub const fn violations(&self) -> &Violations {
        &self.violations
    }

    fn record(&mut self, message: String) {
        self.violations.record(Layer::Chain, message);
    }

    pub fn observe(&mut self, event: &Event) {
        let Event::BlocksProcessed {
            checkpoint,
            channel_update,
            finalized,
            ..
        } = event
        else {
            return;
        };
        tracing::debug!(
            lib = ?checkpoint.lib,
            prefix = ?match channel_update {
                ChannelUpdate::Conflict { common_prefix, .. } => Some(hashes(common_prefix)),
                ChannelUpdate::Extension { .. } => None,
            },
            adopted = ?hashes(channel_update.adopted()),
            adopted_msgs = ?channel_update
                .adopted()
                .iter()
                .filter_map(|tx| tx.inscription().map(|i| i.this_msg))
                .collect::<Vec<_>>(),
            orphaned = ?hashes(channel_update.orphaned()),
            finalized = ?finalized.iter().map(|tx| tx.tx_hash).collect::<Vec<_>>(),
            pending = ?checkpoint.pending_txs.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
            "block event"
        );
        match channel_update {
            ChannelUpdate::Extension { adopted } => self.add_extension(adopted),
            ChannelUpdate::Conflict {
                adopted, orphaned, ..
            } => {
                self.remove_orphaned(orphaned);
                self.add_adopted(adopted);
            }
        }
        self.remove_finalized(finalized);
        self.check_view(channel_update, checkpoint);
    }

    fn add_extension(&mut self, adopted: &[ChannelUpdateTx]) {
        for tx in adopted {
            if !self.held.insert(key(tx, self.channel_id)) {
                self.record(format!(
                    "{:?} adopted on an extension while already held",
                    key(tx, self.channel_id)
                ));
            }
        }
    }

    fn remove_orphaned(&mut self, orphaned: &[ChannelUpdateTx]) {
        if orphaned.is_empty() {
            self.record("a Conflict with nothing orphaned".to_owned());
        }
        for tx in orphaned {
            self.held.remove(&key(tx, self.channel_id));
        }
    }

    fn add_adopted(&mut self, adopted: &[ChannelUpdateTx]) {
        for tx in adopted {
            self.held.insert(key(tx, self.channel_id));
        }
    }

    fn remove_finalized(&mut self, finalized: &[FinalizedTx]) {
        for tx in finalized {
            self.held.remove(&Key::Tx(tx.tx_hash));
            for op in &tx.ops {
                if let FinalizedOp::Inscription(info) | FinalizedOp::Config(info) = op {
                    self.held.remove(&Key::Msg(info.this_msg));
                }
            }
        }
    }

    /// On a conflict, `canonical_chain()` carries only what is held or in
    /// flight, everything held, and every message after its parent.
    fn check_view(&mut self, update: &ChannelUpdate, checkpoint: &SequencerCheckpoint) {
        let Some(chain) = update.canonical_chain() else {
            return;
        };
        let chain: Vec<&ChannelUpdateTx> = chain.collect();
        let ids: HashSet<MsgId> = chain
            .iter()
            .filter_map(|tx| tx.inscription())
            .map(|info| info.this_msg)
            .collect();
        let mut seen: HashSet<MsgId> = HashSet::new();
        let mut messages = Vec::new();
        for info in chain.iter().filter_map(|tx| tx.inscription()) {
            if ids.contains(&info.parent_msg) && !seen.contains(&info.parent_msg) {
                messages.push(format!(
                    "{:?} listed before its parent {:?}",
                    info.this_msg, info.parent_msg
                ));
            }
            seen.insert(info.this_msg);
        }

        let view: HashSet<Key> = chain.iter().map(|tx| key(tx, self.channel_id)).collect();
        let pending: HashSet<TxHash> = checkpoint.pending_txs.iter().map(|(h, _)| *h).collect();
        for (tx, k) in chain
            .iter()
            .map(|tx| (tx.tx_hash(), key(tx, self.channel_id)))
        {
            if !self.held.contains(&k) && !pending.contains(&tx) {
                messages.push(format!(
                    "common_prefix carries {k:?}, which was never adopted and is not pending"
                ));
            }
        }
        for k in &self.held {
            if !view.contains(k) {
                messages.push(format!(
                    "{k:?} was adopted and never orphaned or finalized, but left common_prefix"
                ));
            }
        }
        for message in messages {
            self.record(message);
        }
    }
}
