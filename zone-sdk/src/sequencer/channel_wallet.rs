//! Tracks the channel's note set (the ledger's `channel_notes` entries for
//! our channel) from block data.
//!
//! Two layers, mirroring how [`super::state::TxState`] tracks the channel
//! lineage: a `base` set derived from finalized blocks only (reorg-immune)
//! and a per-block `overlay` for blocks above LIB on the tracked branch. The
//! view at a tip is the base plus the overlay deltas of the tip's ancestor
//! blocks, so branch changes need no revert logic — a different tip simply
//! walks different blocks.

use std::collections::{HashMap, HashSet};

use lb_common_http_client::Slot;
use lb_core::{
    header::HeaderId,
    mantle::{
        SignedMantleTx, Value,
        ledger::{MAX_TRANSACTION_INPUTS, NoteId},
        ops::{Op, OpId as _, channel::ChannelId},
        traits::Hashable as _,
        transactions::{mantle_tx::MantleTx as _, states::Unverified},
    },
};
use lb_key_management_system_service::keys::ZkPublicKey;

use super::types::{ChannelNote, ChannelWalletView, Error, WithdrawInputs};
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
    slot: Slot,
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
                    for note in event.notes.iter() {
                        ops.push(NoteOp::Add(ChannelNote {
                            note_id: note.note_id,
                            value: note.value,
                            pk: note.pk,
                            slot,
                        }));
                    }
                }
                Op::ChannelTransfer(transfer) if transfer.channel_id == channel_id => {
                    ops.extend(transfer.inputs.iter().map(|id| NoteOp::Remove(*id)));
                    ops.extend(transfer.utxos().map(|utxo| {
                        NoteOp::Add(ChannelNote {
                            note_id: utxo.id(),
                            value: utxo.note.value,
                            pk: utxo.note.pk,
                            slot,
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

/// Select the channel notes that fund a withdrawal of `amount`.
///
/// The spendable pool is the tip view — `finalized ++ unfinalized` — since a
/// note on the tracked branch is spendable regardless of finality. `own_key`
/// is the sequencer's own key, used to prefer notes it controls.
///
/// - [`WithdrawInputs::Explicit`] validates the caller's chosen ids against the
///   pool: every id present, no duplicates, count within the input bound, and
///   their combined value covers `amount`. The ids are returned in the given
///   order.
/// - [`WithdrawInputs::Auto`] takes largest-first within an own-key-first tier
///   (own-key notes, then the rest, each sorted by value descending),
///   accumulating until the running value covers `amount`. Because dust sorts
///   last within each tier, a flood of dust notes never blocks a coverable
///   withdrawal.
///
/// Errors ([`Error::Network`]) when the choice is invalid, cannot cover
/// `amount`, or would exceed the `MAX_TRANSACTION_INPUTS` limit.
pub(super) fn select_channel_notes(
    view: &ChannelWalletView,
    own_key: ZkPublicKey,
    amount: Value,
    choice: &WithdrawInputs,
) -> Result<Vec<NoteId>, Error> {
    let pool: Vec<&ChannelNote> = view
        .finalized
        .iter()
        .chain(view.unfinalized.iter())
        .collect();

    match choice {
        WithdrawInputs::Explicit(ids) => {
            if ids.len() > MAX_TRANSACTION_INPUTS {
                return Err(Error::Network(format!(
                    "explicit withdraw inputs ({}) exceed the {MAX_TRANSACTION_INPUTS}-input limit",
                    ids.len()
                )));
            }
            let by_id: HashMap<NoteId, Value> = pool.iter().map(|n| (n.note_id, n.value)).collect();
            let mut seen = HashSet::with_capacity(ids.len());
            let mut sum: Value = 0;
            for id in ids {
                if !seen.insert(*id) {
                    return Err(Error::Network(format!(
                        "duplicate explicit withdraw input: {id:?}"
                    )));
                }
                let value = by_id.get(id).ok_or_else(|| {
                    Error::Network(format!(
                        "explicit withdraw input is not a spendable channel note: {id:?}"
                    ))
                })?;
                sum = sum.checked_add(*value).ok_or_else(|| {
                    Error::Network("explicit withdraw input value overflow".into())
                })?;
            }
            if sum < amount {
                return Err(Error::Network(format!(
                    "explicit withdraw inputs cover {sum}, need {amount}"
                )));
            }
            Ok(ids.clone())
        }
        WithdrawInputs::Auto => {
            let (mut own, mut other): (Vec<&ChannelNote>, Vec<&ChannelNote>) =
                pool.into_iter().partition(|n| n.pk == own_key);
            own.sort_by_key(|n| std::cmp::Reverse(n.value));
            other.sort_by_key(|n| std::cmp::Reverse(n.value));

            // Best-fit: the smallest single note that alone covers the amount,
            // taking the own-key tier before depositors' notes. Such a note is
            // never dust (its value is at least the withdrawal amount), leaves
            // the larger notes — the larger staking positions — intact, and
            // mints minimal change. `own`/`other` are sorted descending, so
            // `rfind` returns the smallest covering note.
            if let Some(note_id) = own
                .iter()
                .rfind(|n| n.value >= amount)
                .or_else(|| other.iter().rfind(|n| n.value >= amount))
                .map(|n| n.note_id)
            {
                return Ok(vec![note_id]);
            }

            // No single note covers the amount, so several are required: take
            // largest-first, own tier before depositors'. Largest-first keeps
            // the input count minimal and is dust-safe — a flood of small notes
            // sits at the tail, reached only when the large notes genuinely
            // cannot cover the amount on their own.
            let mut selected = Vec::new();
            let mut sum: Value = 0;
            for note in own.into_iter().chain(other) {
                if sum >= amount {
                    break;
                }
                if selected.len() == MAX_TRANSACTION_INPUTS {
                    return Err(Error::Network(
                        "cannot cover withdrawal under the 255-input limit".into(),
                    ));
                }
                selected.push(note.note_id);
                sum = sum
                    .checked_add(note.value)
                    .ok_or_else(|| Error::Network("channel note value overflow".into()))?;
            }
            if sum < amount {
                return Err(Error::Network(format!(
                    "insufficient channel funds: have {sum}, need {amount}"
                )));
            }
            Ok(selected)
        }
    }
}

#[cfg(test)]
mod tests {
    use lb_core::{
        events::DepositNote,
        mantle::{
            Note,
            ledger::{Inputs, Outputs},
            ops::channel::{
                channel_transfer::ChannelTransferOp,
                deposit::{DepositOp, Metadata},
            },
        },
    };
    use lb_groth16::Fr;

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

    fn dep_note(seed: u64, value: Value) -> DepositNote {
        DepositNote {
            note_id: note_id(seed),
            value,
            pk: zk_pk(seed),
        }
    }

    fn deposit_events_for(
        tx: &SignedMantleTx<Unverified>,
        op: &DepositOp,
        amount: Value,
        notes: Vec<DepositNote>,
    ) -> DepositEvents {
        DepositEvents::from([(
            DepositOpKey {
                tx_hash: tx.mantle_tx().hash(),
                op_id: op.op_id(),
            },
            crate::adapter::DepositEvent {
                amount,
                notes: notes.try_into().unwrap(),
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
        let events = deposit_events_for(&tx, &op, 50, vec![dep_note(10, 50)]);

        let ops = note_ops_from_txs(
            std::slice::from_ref(&tx),
            channel_id,
            &events,
            Slot::from(9),
        );

        let adds = added(&ops);
        assert_eq!(adds.len(), 1);
        assert_eq!(adds[0].note_id, note_id(10));
        assert_eq!(adds[0].value, 50);
        assert_eq!(adds[0].pk, zk_pk(10));
        assert_eq!(adds[0].slot, Slot::from(9));
        assert!(removed(&ops).is_empty());
    }

    #[test]
    fn multi_input_deposit_yields_exact_per_note_values() {
        let channel_id = ChannelId::from([1u8; 32]);
        let op = DepositOp {
            channel_id,
            inputs: Inputs::new([note_id(1), note_id(2)]),
            metadata: Metadata::try_from(b"m".to_vec()).unwrap(),
        };
        let tx = crate::test_support::unverified_tx_with_ops(vec![Op::ChannelDeposit(op.clone())]);
        let events = deposit_events_for(&tx, &op, 70, vec![dep_note(10, 30), dep_note(11, 40)]);

        let ops = note_ops_from_txs(
            std::slice::from_ref(&tx),
            channel_id,
            &events,
            Slot::from(9),
        );

        let adds = added(&ops);
        assert_eq!(adds.len(), 2);
        assert_eq!(adds[0].note_id, note_id(10));
        assert_eq!(adds[0].value, 30);
        assert_eq!(adds[0].pk, zk_pk(10));
        assert_eq!(adds[0].slot, Slot::from(9));
        assert_eq!(adds[1].note_id, note_id(11));
        assert_eq!(adds[1].value, 40);
        assert_eq!(adds[1].pk, zk_pk(11));
        assert_eq!(adds[1].slot, Slot::from(9));
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

        let ops = note_ops_from_txs(
            std::slice::from_ref(&tx),
            channel_id,
            &DepositEvents::new(),
            Slot::from(9),
        );

        assert_eq!(removed(&ops), vec![note_id(10)]);
        let adds = added(&ops);
        assert_eq!(adds.len(), 2);
        assert_eq!(adds[0].note_id, expected_ids[0]);
        assert_eq!(adds[0].value, 30);
        assert_eq!(adds[0].pk, zk_pk(7));
        assert_eq!(adds[0].slot, Slot::from(9));
        assert_eq!(adds[1].value, 20);
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

        let ops = note_ops_from_txs(
            std::slice::from_ref(&tx),
            channel_id,
            &DepositEvents::new(),
            Slot::from(9),
        );

        assert_eq!(removed(&ops), vec![note_id(10)]);
        assert!(added(&ops).is_empty());
    }

    fn add(seed: u64, value: Value) -> NoteOp {
        NoteOp::Add(ChannelNote {
            note_id: note_id(seed),
            value,
            pk: zk_pk(seed),
            slot: Slot::from(1),
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

    fn note(seed: u64, value: Value, pk: ZkPublicKey) -> ChannelNote {
        ChannelNote {
            note_id: note_id(seed),
            value,
            pk,
            slot: Slot::from(1),
        }
    }

    #[test]
    fn auto_selection_ignores_dust_and_takes_a_single_covering_note() {
        let own = zk_pk(1);
        // ~300 dust notes of value 1 plus a couple of large notes, all under
        // the sequencer's own key. Best-fit must cover the amount with a single
        // large note and never wade into the dust.
        let mut finalized: Vec<ChannelNote> = (0..300).map(|i| note(1000 + i, 1, own)).collect();
        finalized.push(note(1, 10_000, own));
        finalized.push(note(2, 10_000, own));
        let view = ChannelWalletView {
            finalized,
            unfinalized: Vec::new(),
        };

        let selected = select_channel_notes(&view, own, 10_000, &WithdrawInputs::Auto).unwrap();

        assert_eq!(selected.len(), 1);
        // The picked note is one of the 10_000-value notes, not dust.
        assert!(selected[0] == note_id(1) || selected[0] == note_id(2));
    }

    #[test]
    fn auto_selection_best_fit_leaves_the_largest_note_intact() {
        let own = zk_pk(1);
        // A large note, a mid note that alone covers the amount, and some dust.
        // Best-fit must pick the smallest single covering note (the mid one),
        // leaving the largest note untouched — a plain largest-first policy
        // would instead crack the 100_000 note.
        let mut finalized = vec![note(1, 100_000, own), note(2, 500, own)];
        finalized.extend((0..5).map(|i| note(100 + i, 1, own)));
        let view = ChannelWalletView {
            finalized,
            unfinalized: Vec::new(),
        };

        let selected = select_channel_notes(&view, own, 300, &WithdrawInputs::Auto).unwrap();

        assert_eq!(selected, vec![note_id(2)]);
    }

    #[test]
    fn auto_selection_falls_back_to_largest_first_when_no_single_note_covers() {
        let own = zk_pk(1);
        // No single note covers 700, so several are required. Largest-first
        // keeps the count minimal (600 + 300) rather than sweeping small notes.
        let view = ChannelWalletView {
            finalized: vec![
                note(1, 600, own),
                note(2, 300, own),
                note(3, 200, own),
                note(4, 50, own),
            ],
            unfinalized: Vec::new(),
        };

        let selected = select_channel_notes(&view, own, 700, &WithdrawInputs::Auto).unwrap();

        assert_eq!(selected, vec![note_id(1), note_id(2)]);
    }

    #[test]
    fn explicit_selection_returns_the_chosen_ids_in_order() {
        let own = zk_pk(1);
        let view = ChannelWalletView {
            finalized: vec![note(1, 40, own), note(2, 60, own), note(3, 5, own)],
            unfinalized: Vec::new(),
        };
        let ids = vec![note_id(2), note_id(1)];

        let selected =
            select_channel_notes(&view, own, 100, &WithdrawInputs::Explicit(ids.clone())).unwrap();

        assert_eq!(selected, ids);
    }

    #[test]
    fn explicit_selection_rejects_over_the_input_limit() {
        let own = zk_pk(1);
        let view = ChannelWalletView {
            finalized: Vec::new(),
            unfinalized: Vec::new(),
        };
        let ids: Vec<NoteId> = (0..=(MAX_TRANSACTION_INPUTS as u64)).map(note_id).collect();
        assert!(ids.len() > MAX_TRANSACTION_INPUTS);

        let err = select_channel_notes(&view, own, 1, &WithdrawInputs::Explicit(ids)).unwrap_err();

        assert!(matches!(err, Error::Network(_)));
    }
}
