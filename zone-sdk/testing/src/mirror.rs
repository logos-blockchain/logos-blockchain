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

use crate::{
    layers::{Layer, Violations},
    node::SimNode,
};

#[derive(Debug, Default)]
pub struct Mirror {
    chain: Vec<ChannelUpdateTx>,
    /// LIB of the last processed event; the mirror is only compared with the
    /// node once both agree on it.
    lib: Option<HeaderId>,
    pending: HashSet<TxHash>,
    violations: Violations,
}

/// Which lineage of `channel` a tx moves: `Some(true)` for the message
/// lineage, `Some(false)` for a config-only tx, `None` for neither.
fn lineage(tx: &SignedOps<Unverified, StandardMode>, channel: ChannelId) -> Option<bool> {
    let mut config = false;
    for op in tx.op_refs_iter() {
        match op {
            OpRef::ChannelInscribe(op) if op.channel_id == channel => return Some(true),
            OpRef::ChannelConfig(op) if op.channel() == channel => config = true,
            _ => {}
        }
    }
    config.then_some(false)
}

/// The lineage a view entry belongs to, as [`lineage`] classifies it.
/// The entry kind a mined tx must be reported as, from the shapes the SDK's
/// own API produces: `publish` is one inscription, paid for by at most one
/// plain transfer; `publish_atomic_withdraw` adds a channel transfer and
/// withdraws; `publish_pin_deposit` adds one channel transfer and no
/// withdraw; `channel_config` is one config; anything else, and anything
/// carrying another channel's or a foreign op, is custom.
fn shape(tx: &SignedOps<Unverified, StandardMode>, channel: ChannelId) -> &'static str {
    let (mut inscribes, mut configs, mut withdraws, mut channel_transfers, mut transfers) =
        (0, 0, 0, 0, 0);
    let mut foreign = false;
    for op in tx.op_refs_iter() {
        match op {
            OpRef::ChannelInscribe(i) if i.channel_id == channel => inscribes += 1,
            OpRef::ChannelConfig(c) if c.channel() == channel => configs += 1,
            OpRef::ChannelWithdraw(w) if w.channel_id == channel => withdraws += 1,
            OpRef::ChannelTransfer(t) if t.channel_id == channel => channel_transfers += 1,
            OpRef::Transfer(_) => transfers += 1,
            _ => foreign = true,
        }
    }
    let clean = !foreign && transfers <= 1;
    match (clean, inscribes, configs, withdraws, channel_transfers) {
        (true, 1, 0, 1.., 0 | 1) => "atomic withdraw",
        (true, 1, 0, 0, 1) => "pin deposit",
        (true, 1, 0, 0, 0) => "inscription",
        (true, 0, 1, 0, 0) => "config",
        _ => "custom",
    }
}

const fn entry_shape(entry: &ChannelUpdateTx) -> &'static str {
    match entry {
        ChannelUpdateTx::Inscription(_) => "inscription",
        ChannelUpdateTx::AtomicWithdraw(_) => "atomic withdraw",
        ChannelUpdateTx::PinDeposit(_) => "pin deposit",
        ChannelUpdateTx::Config(_) => "config",
        ChannelUpdateTx::Custom(_) => "custom",
    }
}

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
    pub const fn violations(&self) -> &Violations {
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

        // The kind each mined entry is reported as.
        for tx in &unfinalized {
            let hash = tx.hash();
            let Some(entry) = self.chain.iter().find(|entry| entry.tx_hash() == hash) else {
                continue;
            };
            let (reported, mined) = (entry_shape(entry), shape(tx, channel));
            if reported != mined {
                self.record(format!(
                    "{name}: mirrored view reports {hash:?} as {reported}, but the mined tx is \
                     {mined} by shape"
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
        self.violations.record(Layer::Chain, message);
    }
}
