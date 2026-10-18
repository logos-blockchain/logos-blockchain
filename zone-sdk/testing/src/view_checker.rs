//! The [`ChannelUpdate`] contract, checked on every block event.
//!
//! An extension only adds entries the consumer does not hold. On a conflict,
//! what a consumer holds from the stream (adopted, not orphaned since, not
//! finalized) must equal `common_prefix ++ adopted`, up to the sequencer's own
//! in-flight publishes, which the prefix carries and the stream never echoes.

use std::collections::HashSet;

use lb_core::mantle::ops::channel::MsgId;
use lb_zone_sdk::{
    node_types::TxHash,
    sequencer::{ChannelUpdate, ChannelUpdateTx, Event, FinalizedTx, SequencerCheckpoint},
};

#[derive(Debug, Default)]
pub struct ViewChecker {
    held: HashSet<TxHash>,
    violations: Vec<String>,
}

impl ViewChecker {
    /// Every violation recorded so far, in order.
    #[must_use]
    pub fn violations(&self) -> &[String] {
        &self.violations
    }

    fn record(&mut self, message: String) {
        tracing::error!("channel view contract violated: {message}");
        self.violations.push(message);
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
            if !self.held.insert(tx.tx_hash()) {
                self.record(format!(
                    "{:?} adopted on an extension while already held",
                    tx.tx_hash()
                ));
            }
        }
    }

    fn remove_orphaned(&mut self, orphaned: &[ChannelUpdateTx]) {
        if orphaned.is_empty() {
            self.record("a Conflict with nothing orphaned".to_owned());
        }
        for tx in orphaned {
            self.held.remove(&tx.tx_hash());
        }
    }

    fn add_adopted(&mut self, adopted: &[ChannelUpdateTx]) {
        for tx in adopted {
            self.held.insert(tx.tx_hash());
        }
    }

    fn remove_finalized(&mut self, finalized: &[FinalizedTx]) {
        for tx in finalized {
            self.held.remove(&tx.tx_hash);
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

        let view: HashSet<TxHash> = chain.iter().map(|tx| tx.tx_hash()).collect();
        let pending: HashSet<TxHash> = checkpoint.pending_txs.iter().map(|(h, _)| *h).collect();
        for tx in &view {
            if !self.held.contains(tx) && !pending.contains(tx) {
                messages.push(format!(
                    "common_prefix carries {tx:?}, which was never adopted and is not pending"
                ));
            }
        }
        for tx in &self.held {
            if !view.contains(tx) {
                messages.push(format!(
                    "{tx:?} was adopted and never orphaned or finalized, but left common_prefix"
                ));
            }
        }
        for message in messages {
            self.record(message);
        }
    }
}
