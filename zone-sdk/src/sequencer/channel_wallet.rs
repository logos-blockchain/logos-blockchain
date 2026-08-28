//! Tracks the channel's note set (the ledger's `channel_notes` entries for
//! our channel) from block data.
//!
//! Two layers, mirroring how [`super::state::TxState`] tracks the channel
//! lineage: a `base` set derived from finalized blocks only (reorg-immune)
//! and a per-block `overlay` for blocks above LIB on the tracked branch. The
//! view at a tip is the base plus the overlay deltas of the tip's ancestor
//! blocks, so branch changes need no revert logic — a different tip simply
//! walks different blocks.

use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
};

use lb_common_http_client::Slot;
use lb_core::{
    header::HeaderId,
    mantle::{
        SignedOps, Value,
        ledger::{MAX_TRANSACTION_INPUTS, NoteId, verification_mode::StandardMode},
        ops::{OpId as _, OpRef, channel::ChannelId},
        traits::Hashable as _,
        transactions::states::Unverified,
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

    /// Find a tracked note by id, anywhere in the wallet (the finalized base
    /// or any unfinalized overlay). A `NoteId` is a content commitment, so
    /// `id → (value, pk)` is a function and any tracked copy is authoritative
    /// regardless of branch — enough to recover a consumed note's value/key.
    pub(super) fn find_note(&self, id: &NoteId) -> Option<&ChannelNote> {
        self.base.get(id).or_else(|| {
            self.overlay.values().flatten().find_map(|op| match op {
                NoteOp::Add(note) if note.note_id == *id => Some(note),
                NoteOp::Add(_) | NoteOp::Remove(_) => None,
            })
        })
    }
}

/// Extract the channel-note ops of a block's transactions for `channel_id`,
/// in tx-then-op (execution) order.
///
/// `deposit_events` must be the validated per-block lookup from
/// `fetch_block_deposit_events` — it is guaranteed to contain every channel
/// deposit op of these transactions.
pub(super) fn note_ops_from_txs(
    transactions: &[SignedOps<Unverified, StandardMode>],
    channel_id: ChannelId,
    deposit_events: &DepositEvents,
    slot: Slot,
) -> Vec<NoteOp> {
    let mut ops = Vec::new();
    for tx in transactions {
        let tx_hash = tx.hash();
        for op in tx.op_refs() {
            match op {
                OpRef::ChannelDeposit(deposit) if deposit.channel_id == channel_id => {
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
                OpRef::ChannelTransfer(transfer) if transfer.channel_id == channel_id => {
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
                OpRef::ChannelWithdraw(withdraw) if withdraw.channel_id == channel_id => {
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
/// - [`WithdrawInputs::Auto`] covers `amount` with the newest notes first
///   within an own-key-first tier — spending recent notes and preserving the
///   older matured positions that carry live stake — falling back to
///   largest-first when the newest notes cannot cover within the input bound.
///   It then sweeps dust (notes too small to ever cover on their own, `value *
///   MAX_TRANSACTION_INPUTS < amount`) into the remaining input slots, smallest
///   and oldest first, so the channel wallet stays compact rather than letting
///   an unspent dust set grow without bound.
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
            let (own, other): (Vec<&ChannelNote>, Vec<&ChannelNote>) =
                pool.into_iter().partition(|n| n.pk == own_key);

            let total = own
                .iter()
                .chain(&other)
                .try_fold(0u64, |acc, n| acc.checked_add(n.value))
                .ok_or_else(|| Error::Network("channel note value overflow".into()))?;
            if total < amount {
                return Err(Error::Network(format!(
                    "insufficient channel funds: have {total}, need {amount}"
                )));
            }

            // Cover the amount preferring the newest notes within an
            // own-key-first tier: spending recent notes leaves the older,
            // matured positions — the ones carrying live PoS/leadership stake —
            // intact. Fall back to largest-first when the newest notes cannot
            // cover within the input limit (e.g. the recent notes are all dust
            // and the covering value sits in older large notes).
            let cover = cover_from(&ordered(&own, &other, |n| Reverse(n.slot)), amount)
                .or_else(|| cover_from(&ordered(&own, &other, |n| Reverse(n.value)), amount))
                .ok_or_else(|| {
                    Error::Network("cannot cover withdrawal under the 255-input limit".into())
                })?;

            // Sweep dust into the remaining input slots — smallest, then oldest,
            // first — so the channel wallet stays compact instead of letting an
            // unspent dust set grow without bound. The change note absorbs the
            // swept value. Only genuine dust is swept: a note so small that even
            // a full 255-input transaction of it could not cover `amount`
            // (`value * MAX_TRANSACTION_INPUTS < amount`). That leaves medium and
            // large positions — the matured stake preserved by the cover step —
            // untouched, and such dust carries negligible stake anyway.
            let taken: HashSet<NoteId> = cover.iter().copied().collect();
            let mut sweep: Vec<&ChannelNote> = own
                .iter()
                .chain(&other)
                .copied()
                .filter(|n| {
                    !taken.contains(&n.note_id)
                        && n.value.saturating_mul(MAX_TRANSACTION_INPUTS as u64) < amount
                })
                .collect();
            sweep.sort_by(|a, b| a.value.cmp(&b.value).then(a.slot.cmp(&b.slot)));

            let mut selected = cover;
            for note in sweep {
                if selected.len() >= MAX_TRANSACTION_INPUTS {
                    break;
                }
                selected.push(note.note_id);
            }
            Ok(selected)
        }
    }
}

/// Order `own` notes ahead of `other`, each sorted by `key` (natural order —
/// wrap in [`Reverse`] for descending). Used to build a covering-preference
/// order over the two tiers.
fn ordered<'a, K: Ord>(
    own: &[&'a ChannelNote],
    other: &[&'a ChannelNote],
    key: impl Fn(&ChannelNote) -> K,
) -> Vec<&'a ChannelNote> {
    let mut result = own.to_vec();
    result.sort_by_key(|n| key(n));
    let mut rest = other.to_vec();
    rest.sort_by_key(|n| key(n));
    result.extend(rest);
    result
}

/// Accumulate `ordered` notes until their value covers `amount`, returning the
/// selected ids. `None` if the notes cannot cover `amount` within
/// [`MAX_TRANSACTION_INPUTS`].
fn cover_from(ordered: &[&ChannelNote], amount: Value) -> Option<Vec<NoteId>> {
    let mut selected = Vec::new();
    let mut sum: Value = 0;
    for note in ordered {
        if sum >= amount {
            break;
        }
        if selected.len() == MAX_TRANSACTION_INPUTS {
            return None;
        }
        selected.push(note.note_id);
        sum = sum.checked_add(note.value)?;
    }
    (sum >= amount).then_some(selected)
}

#[cfg(test)]
mod tests {
    use lb_core::{
        events::DepositNote,
        mantle::{
            Note, Op,
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
        tx: &SignedOps<Unverified, StandardMode>,
        op: &DepositOp,
        amount: Value,
        notes: Vec<DepositNote>,
    ) -> DepositEvents {
        DepositEvents::from([(
            DepositOpKey {
                tx_hash: tx.hash(),
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
        note_at(seed, value, pk, 1)
    }

    fn note_at(seed: u64, value: Value, pk: ZkPublicKey, slot: u64) -> ChannelNote {
        ChannelNote {
            note_id: note_id(seed),
            value,
            pk,
            slot: Slot::from(slot),
        }
    }

    #[test]
    fn auto_covers_with_the_newest_note_and_preserves_the_aged_one() {
        let own = zk_pk(1);
        // Two notes that each cover the amount: one aged (slot 1), one recent
        // (slot 9). Auto spends the recent note and leaves the aged, matured
        // position — the one carrying live stake — intact.
        let view = ChannelWalletView {
            finalized: vec![note_at(1, 10_000, own, 1), note_at(2, 10_000, own, 9)],
            unfinalized: Vec::new(),
        };

        let selected = select_channel_notes(&view, own, 10_000, &WithdrawInputs::Auto).unwrap();

        assert_eq!(selected, vec![note_id(2)]);
    }

    #[test]
    fn auto_covers_with_multiple_newest_notes_when_no_single_note_covers() {
        let own = zk_pk(1);
        // No single note covers 700; newest-first takes the two most recent and
        // leaves the oldest note untouched.
        let view = ChannelWalletView {
            finalized: vec![
                note_at(1, 400, own, 3), // newest
                note_at(2, 400, own, 2),
                note_at(3, 400, own, 1), // oldest — preserved
            ],
            unfinalized: Vec::new(),
        };

        let selected = select_channel_notes(&view, own, 700, &WithdrawInputs::Auto).unwrap();

        assert_eq!(selected, vec![note_id(1), note_id(2)]);
    }

    #[test]
    fn auto_sweeps_dust_around_a_covering_note_up_to_the_input_bound() {
        let own = zk_pk(1);
        // A covering note plus a flood of newer dust. Auto covers with the big
        // note and sweeps dust into the remaining slots, filling to the
        // 255-input bound (1 cover + 254 dust) rather than leaving dust unspent.
        let mut finalized = vec![note_at(1, 10_000, own, 1)];
        finalized.extend((0..300).map(|i| note_at(1000 + i, 1, own, 2)));
        let view = ChannelWalletView {
            finalized,
            unfinalized: Vec::new(),
        };

        let selected = select_channel_notes(&view, own, 10_000, &WithdrawInputs::Auto).unwrap();

        assert_eq!(selected.len(), MAX_TRANSACTION_INPUTS);
        assert!(selected.contains(&note_id(1)));
        // Every other slot is a swept dust note.
        assert_eq!(
            selected.iter().filter(|id| **id != note_id(1)).count(),
            MAX_TRANSACTION_INPUTS - 1
        );
    }

    #[test]
    fn auto_falls_back_to_largest_first_when_newest_notes_cannot_cover() {
        let own = zk_pk(1);
        // The newest notes are a dust flood that cannot cover the amount within
        // 255 inputs; the covering value sits in an older large note. Newest-
        // first would exhaust its budget on dust, so Auto falls back to
        // largest-first, spends the large note, then sweeps dust.
        let mut finalized = vec![note_at(1, 1_000, own, 1)]; // old, covers
        finalized.extend((0..300).map(|i| note_at(1000 + i, 1, own, 5))); // newer dust
        let view = ChannelWalletView {
            finalized,
            unfinalized: Vec::new(),
        };

        let selected = select_channel_notes(&view, own, 500, &WithdrawInputs::Auto).unwrap();

        assert!(selected.contains(&note_id(1)));
        assert_eq!(selected.len(), MAX_TRANSACTION_INPUTS);
    }

    #[test]
    fn auto_errors_when_coverage_needs_more_than_the_input_bound() {
        let own = zk_pk(1);
        // The funds exist (300 total) but covering 256 needs 256 value-1 inputs,
        // over the 255-input limit. Auto reports the limit error rather than
        // returning an over-limit selection.
        let finalized: Vec<ChannelNote> = (0..300).map(|i| note_at(1000 + i, 1, own, 1)).collect();
        let view = ChannelWalletView {
            finalized,
            unfinalized: Vec::new(),
        };

        let err = select_channel_notes(&view, own, 256, &WithdrawInputs::Auto).unwrap_err();

        assert!(matches!(err, Error::Network(_)));
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

    #[test]
    fn explicit_selection_rejects_duplicate_ids() {
        let own = zk_pk(1);
        let view = ChannelWalletView {
            finalized: vec![note(1, 100, own), note(2, 100, own)],
            unfinalized: Vec::new(),
        };

        let ids = vec![note_id(1), note_id(1)];
        let err =
            select_channel_notes(&view, own, 100, &WithdrawInputs::Explicit(ids)).unwrap_err();

        assert!(matches!(err, Error::Network(_)));
    }

    #[test]
    fn explicit_selection_rejects_untracked_ids() {
        let own = zk_pk(1);
        let view = ChannelWalletView {
            finalized: vec![note(1, 100, own)],
            unfinalized: Vec::new(),
        };

        // `note_id(99)` is not in the tracked note set.
        let ids = vec![note_id(99)];
        let err =
            select_channel_notes(&view, own, 100, &WithdrawInputs::Explicit(ids)).unwrap_err();

        assert!(matches!(err, Error::Network(_)));
    }

    #[test]
    fn explicit_selection_rejects_insufficient_coverage() {
        let own = zk_pk(1);
        let view = ChannelWalletView {
            finalized: vec![note(1, 40, own), note(2, 30, own)],
            unfinalized: Vec::new(),
        };

        // 40 + 30 = 70 < 100.
        let ids = vec![note_id(1), note_id(2)];
        let err =
            select_channel_notes(&view, own, 100, &WithdrawInputs::Explicit(ids)).unwrap_err();

        assert!(matches!(err, Error::Network(_)));
    }

    #[test]
    fn explicit_reproduces_a_prior_selection_exactly() {
        // A republish that wants the identical transfer inputs passes the
        // previously resolved ids as `Explicit`; selection returns exactly them,
        // in order. Reproduction is a caller choice — the inscription lineage,
        // not the input set, is what prevents a double-withdraw (see
        // [`WithdrawInputs`]).
        let own = zk_pk(1);
        let view = ChannelWalletView {
            finalized: vec![note(1, 60, own), note(2, 40, own), note(3, 5, own)],
            unfinalized: Vec::new(),
        };

        let prior = vec![note_id(2), note_id(1)];
        let selected =
            select_channel_notes(&view, own, 100, &WithdrawInputs::Explicit(prior.clone()))
                .unwrap();

        assert_eq!(selected, prior);
    }
}
