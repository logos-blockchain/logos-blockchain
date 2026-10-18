//! An LEZ-style consumer of `ChannelUpdate`s, checked against the simulated
//! ledger's truth.
//!
//! The mirror keeps the non-finalized channel view the way an execution zone
//! does: append `adopted` on an extension, rebuild from `canonical_chain()` on
//! a conflict, drop finalized entries. After every settle it is compared with
//! what the node's canonical chain really holds for the channel.

use std::collections::HashSet;

use lb_core::{
    header::HeaderId,
    mantle::{
        OpRef, SignedOps, ledger::verification_mode::StandardMode, ops::channel::ChannelId,
        traits::Hashable, transactions::states::Unverified,
    },
};
use lb_zone_sdk::{
    node_types::TxHash,
    sequencer::{ChannelUpdate, ChannelUpdateTx, Event},
};

use crate::node::SimNode;

#[derive(Debug, Default)]
pub struct Mirror {
    chain: Vec<ChannelUpdateTx>,
    /// LIB of the last processed event; the mirror is only compared with the
    /// node once both agree on it.
    lib: Option<HeaderId>,
    pending: HashSet<TxHash>,
    violations: Vec<String>,
}

/// Which lineage of `channel` a tx moves: `Some(true)` for the message
/// lineage, `Some(false)` for a config-only tx, `None` for neither.
fn lineage(tx: &SignedOps<Unverified, StandardMode>, channel: ChannelId) -> Option<bool> {
    let mut config = false;
    for op in tx.op_refs_iter() {
        match op {
            OpRef::ChannelInscribe(op) if op.channel_id == channel => return Some(true),
            OpRef::ChannelConfig(op) if op.channel == channel => config = true,
            _ => {}
        }
    }
    config.then_some(false)
}

/// The lineage a view entry belongs to, as [`lineage`] classifies it.
fn entry_lineage(entry: &ChannelUpdateTx, channel: ChannelId) -> Option<bool> {
    match entry {
        ChannelUpdateTx::Config(tx) | ChannelUpdateTx::Custom(tx) => lineage(tx, channel),
        ChannelUpdateTx::Inscription(_)
        | ChannelUpdateTx::AtomicWithdraw(_)
        | ChannelUpdateTx::PinDeposit(_) => Some(true),
    }
}

fn hashes<'a>(
    txs: impl IntoIterator<Item = &'a SignedOps<Unverified, StandardMode>>,
) -> Vec<TxHash> {
    txs.into_iter().map(Hashable::hash).collect()
}

impl Mirror {
    #[must_use]
    pub fn violations(&self) -> &[String] {
        &self.violations
    }

    /// The mirrored non-finalized view, in lineage order.
    #[must_use]
    pub fn chain(&self) -> &[ChannelUpdateTx] {
        &self.chain
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
            ChannelUpdate::Extension { adopted } => {
                for tx in adopted {
                    // Entries can be re-reported after a restart; dedup like a
                    // consumer would.
                    if !self.chain.iter().any(|held| held.tx_hash() == tx.tx_hash()) {
                        self.chain.push(tx.clone());
                    }
                }
            }
            ChannelUpdate::Conflict { .. } => {
                self.chain = channel_update
                    .canonical_chain()
                    .expect("conflict carries the view")
                    .cloned()
                    .collect();
            }
        }
        let finalized: HashSet<TxHash> = finalized.iter().map(|tx| tx.tx_hash).collect();
        self.chain.retain(|tx| !finalized.contains(&tx.tx_hash()));
        self.lib = Some(checkpoint.lib);
        self.pending = checkpoint.pending_txs.iter().map(|(h, _)| *h).collect();
    }

    /// Compare the mirrored view with `node`'s canonical chain for `channel`.
    /// Skipped while the sequencer has not caught up: it disagrees with the
    /// node on LIB, its stream has not told it about the tip yet, or it is
    /// reconnecting after a failed block.
    pub fn check(&mut self, node: &SimNode, connected: bool, channel: ChannelId, name: &str) {
        if !connected || !node.subscriber_at_tip() || self.lib != Some(node.lib()) {
            return;
        }
        let unfinalized = node.unfinalized_txs();
        let truth: Vec<(TxHash, bool)> = unfinalized
            .iter()
            .filter_map(|tx| lineage(tx, channel).map(|messages| (tx.hash(), messages)))
            .collect();
        let held: Vec<(TxHash, bool)> = self
            .chain
            .iter()
            .map(|entry| {
                (
                    entry.tx_hash(),
                    entry_lineage(entry, channel).unwrap_or(true),
                )
            })
            .collect();
        let held_set: HashSet<TxHash> = held.iter().map(|(h, _)| *h).collect();

        for (hash, _) in &truth {
            if !held_set.contains(hash) && !self.pending.contains(hash) {
                self.record(format!(
                    "{name}: canonical tx {hash:?} is neither in the mirrored view nor pending"
                ));
            }
        }

        // Each lineage is ordered on its own; the SDK does not interleave
        // message and config entries.
        let truth_set: HashSet<TxHash> = truth.iter().map(|(h, _)| *h).collect();
        for messages in [true, false] {
            let mined_in_view: Vec<TxHash> = held
                .iter()
                .filter(|(h, m)| *m == messages && truth_set.contains(h))
                .map(|(h, _)| *h)
                .collect();
            let truth_in_view: Vec<TxHash> = truth
                .iter()
                .filter(|(h, m)| *m == messages && held_set.contains(h))
                .map(|(h, _)| *h)
                .collect();
            if mined_in_view != truth_in_view {
                self.record(format!(
                    "{name}: mirrored order {mined_in_view:?} differs from the chain order {truth_in_view:?}"
                ));
            }
        }

        let known: HashSet<TxHash> = hashes(node.canonical_txs().iter())
            .into_iter()
            .chain(hashes(node.mempool().iter()))
            .chain(node.rejected().into_iter().map(|r| r.tx_hash))
            .chain(self.pending.iter().copied())
            .collect();
        for (hash, _) in &held {
            if !known.contains(hash) {
                self.record(format!(
                    "{name}: mirrored view holds {hash:?}, which the node never saw"
                ));
            }
        }
    }

    fn record(&mut self, message: String) {
        tracing::error!("mirror diverged: {message}");
        self.violations.push(message);
    }
}
