//! Tracks the channel's note set (the ledger's `channel_notes` entries for
//! our channel) from block data.
//!
//! Two layers, mirroring how [`super::state::TxState`] tracks the channel
//! lineage: a `base` set derived from finalized blocks only (reorg-immune)
//! and a per-block `overlay` for blocks above LIB on the tracked branch. The
//! view at a tip is the base plus the overlay deltas of the tip's ancestor
//! blocks, so branch changes need no revert logic — a different tip simply
//! walks different blocks.

use std::collections::HashMap;

use lb_core::{
    header::HeaderId,
    mantle::{
        SignedMantleTx,
        ledger::NoteId,
        ops::{Op, OpId as _, channel::ChannelId},
        traits::Hashable as _,
        transactions::{mantle_tx::MantleTx as _, states::Unverified},
    },
};

use super::types::{ChannelNote, ChannelWalletView, DepositGroup};
use crate::adapter::{DepositEvents, DepositOpKey};

/// One channel-note mutation, in on-chain execution order.
#[derive(Debug, Clone)]
pub(super) enum NoteOp {
    Add(ChannelNote),
    Remove(NoteId),
}

/// Channel-note set tracker owned by [`super::state::TxState`].
#[derive(Default)]
pub(super) struct ChannelWallet {
    /// Notes created below LIB and not spent below LIB.
    base: HashMap<NoteId, ChannelNote>,
    /// Note ops per unfinalized block, applied on top of `base` when viewing
    /// a branch. Pruned alongside the block store.
    overlay: HashMap<HeaderId, Vec<NoteOp>>,
}

impl ChannelWallet {
    /// Apply ops from a finalized block directly to the base set.
    pub fn apply_finalized(&mut self, ops: impl IntoIterator<Item = NoteOp>) {
        for op in ops {
            match op {
                NoteOp::Add(note) => {
                    self.base.insert(note.note_id, note);
                }
                NoteOp::Remove(id) => {
                    self.base.remove(&id);
                }
            }
        }
    }

    /// Record the note ops of an unfinalized block.
    pub fn store_overlay(&mut self, block_id: HeaderId, ops: Vec<NoteOp>) {
        if !ops.is_empty() {
            self.overlay.insert(block_id, ops);
        }
    }

    /// Drop a block's overlay entry (block pruned from the store).
    pub fn prune_block(&mut self, block_id: &HeaderId) {
        self.overlay.remove(block_id);
    }

    /// The note set at a branch tip: base plus the overlay ops of
    /// `branch_blocks` (oldest first, LIB excluded — the base already covers
    /// blocks at and below LIB).
    pub fn view<'a>(&self, branch_blocks: impl Iterator<Item = &'a HeaderId>) -> ChannelWalletView {
        let mut finalized = self.base.clone();
        let mut unfinalized: HashMap<NoteId, ChannelNote> = HashMap::new();
        for block_id in branch_blocks {
            let Some(ops) = self.overlay.get(block_id) else {
                continue;
            };
            for op in ops {
                match op {
                    NoteOp::Add(note) => {
                        unfinalized.insert(note.note_id, note.clone());
                    }
                    NoteOp::Remove(id) => {
                        finalized.remove(id);
                        unfinalized.remove(id);
                    }
                }
            }
        }
        ChannelWalletView {
            finalized: finalized.into_values().collect(),
            unfinalized: unfinalized.into_values().collect(),
        }
    }

    /// Export the base set for checkpointing.
    pub fn export_base(&self) -> Vec<ChannelNote> {
        self.base.values().cloned().collect()
    }

    /// Restore the base set from a checkpoint.
    pub fn restore_base(&mut self, notes: Vec<ChannelNote>) {
        self.base = notes.into_iter().map(|n| (n.note_id, n)).collect();
    }
}

/// Extract the channel-note ops of a block's transactions for `channel_id`,
/// in tx-then-op (execution) order.
///
/// `deposit_events` must be the validated per-block lookup from
/// `fetch_block_deposit_events` — it is guaranteed to contain every channel
/// deposit op of these transactions.
pub(super) fn note_ops_from_txs(
    transactions: &[SignedMantleTx<Unverified>],
    channel_id: ChannelId,
    deposit_events: &DepositEvents,
) -> Vec<NoteOp> {
    let mut ops = Vec::new();
    for tx in transactions {
        let tx_hash = tx.mantle_tx().hash();
        for op in tx.mantle_tx().ops() {
            match op {
                Op::ChannelDeposit(deposit) if deposit.channel_id == channel_id => {
                    let op_id = deposit.op_id();
                    let event = deposit_events.get(&DepositOpKey { tx_hash, op_id }).expect(
                        "deposit_events must contain every channel deposit op - \
                         fetch_block_deposit_events invariant",
                    );
                    let group = (event.notes.len() > 1).then_some(DepositGroup {
                        op_id,
                        total: event.amount,
                        size: event.notes.len(),
                    });
                    for note in event.notes.iter() {
                        ops.push(NoteOp::Add(ChannelNote {
                            note_id: note.note_id,
                            value: group.is_none().then_some(event.amount),
                            pk: None,
                            deposit_group: group,
                        }));
                    }
                }
                Op::ChannelTransfer(transfer) if transfer.channel_id == channel_id => {
                    ops.extend(transfer.inputs.iter().map(|id| NoteOp::Remove(*id)));
                    ops.extend(transfer.utxos().map(|utxo| {
                        NoteOp::Add(ChannelNote {
                            note_id: utxo.id(),
                            value: Some(utxo.note.value),
                            pk: Some(utxo.note.pk),
                            deposit_group: None,
                        })
                    }));
                }
                Op::ChannelWithdraw(withdraw) if withdraw.channel_id == channel_id => {
                    ops.extend(withdraw.inputs.iter().map(|id| NoteOp::Remove(*id)));
                }
                _ => {}
            }
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use lb_core::{
        events::DepositNote,
        mantle::{
            Note, Value,
            ledger::{Inputs, Outputs},
            ops::channel::{
                channel_transfer::ChannelTransferOp,
                deposit::{DepositOp, Metadata},
            },
        },
    };
    use lb_groth16::Fr;
    use lb_key_management_system_service::keys::ZkPublicKey;

    use super::*;
    use crate::test_support::header_id;

    fn note_id(seed: u64) -> NoteId {
        NoteId::from(Fr::from(seed))
    }

    fn zk_pk(seed: u64) -> ZkPublicKey {
        Fr::from(seed).into()
    }

    fn deposit_op(channel_id: ChannelId, input_seed: u64) -> DepositOp {
        DepositOp {
            channel_id,
            inputs: Inputs::new([note_id(input_seed)]),
            metadata: Metadata::try_from(b"m".to_vec()).unwrap(),
        }
    }

    fn deposit_events_for(
        tx: &SignedMantleTx<Unverified>,
        op: &DepositOp,
        amount: Value,
        notes: Vec<NoteId>,
    ) -> DepositEvents {
        DepositEvents::from([(
            DepositOpKey {
                tx_hash: tx.mantle_tx().hash(),
                op_id: op.op_id(),
            },
            crate::adapter::DepositEvent {
                amount,
                notes: notes
                    .into_iter()
                    .map(|note_id| DepositNote {
                        note_id,
                        value: 0,
                        pk: Fr::from(0u64).into(),
                    })
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap(),
            },
        )])
    }

    fn added(ops: &[NoteOp]) -> Vec<&ChannelNote> {
        ops.iter()
            .filter_map(|op| match op {
                NoteOp::Add(n) => Some(n),
                NoteOp::Remove(_) => None,
            })
            .collect()
    }

    fn removed(ops: &[NoteOp]) -> Vec<NoteId> {
        ops.iter()
            .filter_map(|op| match op {
                NoteOp::Remove(id) => Some(*id),
                NoteOp::Add(_) => None,
            })
            .collect()
    }

    #[test]
    fn single_input_deposit_yields_exact_value_note() {
        let channel_id = ChannelId::from([1u8; 32]);
        let op = deposit_op(channel_id, 1);
        let tx = crate::test_support::unverified_tx_with_ops(vec![Op::ChannelDeposit(op.clone())]);
        let events = deposit_events_for(&tx, &op, 50, vec![note_id(10)]);

        let ops = note_ops_from_txs(std::slice::from_ref(&tx), channel_id, &events);

        let adds = added(&ops);
        assert_eq!(adds.len(), 1);
        assert_eq!(adds[0].note_id, note_id(10));
        assert_eq!(adds[0].value, Some(50));
        assert_eq!(adds[0].deposit_group, None);
        assert!(removed(&ops).is_empty());
    }

    #[test]
    fn multi_input_deposit_notes_form_a_group() {
        let channel_id = ChannelId::from([1u8; 32]);
        let op = DepositOp {
            channel_id,
            inputs: Inputs::new([note_id(1), note_id(2)]),
            metadata: Metadata::try_from(b"m".to_vec()).unwrap(),
        };
        let tx = crate::test_support::unverified_tx_with_ops(vec![Op::ChannelDeposit(op.clone())]);
        let events = deposit_events_for(&tx, &op, 70, vec![note_id(10), note_id(11)]);

        let ops = note_ops_from_txs(std::slice::from_ref(&tx), channel_id, &events);

        let adds = added(&ops);
        assert_eq!(adds.len(), 2);
        for add in &adds {
            assert_eq!(add.value, None, "per-note values of a group are unknown");
            let group = add.deposit_group.expect("group set");
            assert_eq!(group.op_id, op.op_id());
            assert_eq!(group.total, 70);
            assert_eq!(group.size, 2);
        }
    }

    #[test]
    fn transfer_swaps_inputs_for_exact_outputs() {
        let channel_id = ChannelId::from([1u8; 32]);
        let op = ChannelTransferOp {
            channel_id,
            inputs: Inputs::new([note_id(10)]),
            outputs: Outputs::new([Note::new(30, zk_pk(7)), Note::new(20, zk_pk(8))]),
        };
        let expected_ids: Vec<NoteId> = op.utxos().map(|u| u.id()).collect();
        let tx = crate::test_support::unverified_tx_with_ops(vec![Op::ChannelTransfer(op)]);

        let ops = note_ops_from_txs(std::slice::from_ref(&tx), channel_id, &DepositEvents::new());

        assert_eq!(removed(&ops), vec![note_id(10)]);
        let adds = added(&ops);
        assert_eq!(adds.len(), 2);
        assert_eq!(adds[0].note_id, expected_ids[0]);
        assert_eq!(adds[0].value, Some(30));
        assert_eq!(adds[0].pk, Some(zk_pk(7)));
        assert_eq!(adds[1].value, Some(20));
    }

    #[test]
    fn withdraw_removes_inputs_and_foreign_channels_are_ignored() {
        let channel_id = ChannelId::from([1u8; 32]);
        let other = ChannelId::from([2u8; 32]);
        let withdraw = lb_core::mantle::ops::channel::withdraw::ChannelWithdrawOp {
            channel_id,
            inputs: Inputs::new([note_id(10)]),
        };
        let foreign = lb_core::mantle::ops::channel::withdraw::ChannelWithdrawOp {
            channel_id: other,
            inputs: Inputs::new([note_id(11)]),
        };
        let tx = crate::test_support::unverified_tx_with_ops(vec![
            Op::ChannelWithdraw(withdraw),
            Op::ChannelWithdraw(foreign),
        ]);

        let ops = note_ops_from_txs(std::slice::from_ref(&tx), channel_id, &DepositEvents::new());

        assert_eq!(removed(&ops), vec![note_id(10)]);
        assert!(added(&ops).is_empty());
    }

    fn add(seed: u64, value: Value) -> NoteOp {
        NoteOp::Add(ChannelNote {
            note_id: note_id(seed),
            value: Some(value),
            pk: None,
            deposit_group: None,
        })
    }

    #[test]
    fn view_layers_overlay_over_base_per_branch() {
        let mut wallet = ChannelWallet::default();
        wallet.apply_finalized(vec![add(1, 10)]);
        // Branch A spends the base note and adds a new one; branch B adds a
        // different note and leaves the base untouched.
        wallet.store_overlay(header_id(1), vec![NoteOp::Remove(note_id(1)), add(2, 20)]);
        wallet.store_overlay(header_id(2), vec![add(3, 30)]);

        let at_a = wallet.view(std::iter::once(&header_id(1)));
        assert!(at_a.finalized.is_empty(), "base note spent on branch A");
        assert_eq!(at_a.unfinalized.len(), 1);
        assert_eq!(at_a.unfinalized[0].note_id, note_id(2));

        let at_b = wallet.view(std::iter::once(&header_id(2)));
        assert_eq!(at_b.finalized.len(), 1);
        assert_eq!(at_b.finalized[0].note_id, note_id(1));
        assert_eq!(at_b.unfinalized.len(), 1);
        assert_eq!(at_b.unfinalized[0].note_id, note_id(3));
    }

    #[test]
    fn unfinalized_note_spent_within_the_branch_never_surfaces() {
        let mut wallet = ChannelWallet::default();
        wallet.store_overlay(header_id(1), vec![add(1, 10)]);
        wallet.store_overlay(header_id(2), vec![NoteOp::Remove(note_id(1))]);

        let view = wallet.view([header_id(1), header_id(2)].iter());
        assert!(view.finalized.is_empty());
        assert!(view.unfinalized.is_empty());
    }

    #[test]
    fn export_restore_roundtrip() {
        let mut wallet = ChannelWallet::default();
        wallet.apply_finalized(vec![add(1, 10), add(2, 20)]);
        let mut exported = wallet.export_base();
        exported.sort_by_key(|n| n.note_id);

        let mut restored = ChannelWallet::default();
        restored.restore_base(exported.clone());
        let mut roundtripped = restored.export_base();
        roundtripped.sort_by_key(|n| n.note_id);
        assert_eq!(exported, roundtripped);
    }
}
