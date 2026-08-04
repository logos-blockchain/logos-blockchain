pub mod error;
mod voucher;

use std::{
    borrow::Borrow,
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Debug,
};

pub use error::WalletError;
use lb_core::{
    block::{Block, BlockTransactions},
    crypto::{Hash, ZkHasher},
    events::{Event, Events, HeaderEvent, TxEvent, TxEventPayload},
    header::HeaderId,
    mantle::{
        GasConstants, NoteId, TxHash, Utxo, Value,
        ops::{
            Op, OpId as _,
            channel::{
                channel_transfer::ChannelTransferOp, deposit::DepositOp,
                withdraw::ChannelWithdrawOp,
            },
            leader_claim::{VoucherCm, VoucherNullifier},
            transfer::TransferOp,
        },
        traits::MantleTxWithProofs,
        transactions::{
            MAX_OPS_PER_TX, MantleTxContext, builder::MantleTxBuilder, mantle_tx::MantleTx as _,
        },
    },
    proofs::leader_proof::LeaderProof as _,
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_ledger::LedgerState;
use lb_log_targets::wallet;
use lb_mmr::{MerkleMountainRange, MerklePath};
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Serialize};
use tracing::info;

pub use crate::voucher::{Voucher, Vouchers};

const LOG_TARGET: &str = wallet::CORE;

/// A lightweight projection of a `Block<Tx>` carrying only what the wallet
/// needs to mirror the ledger's effect on UTXOs and the SDP lock set.
///
/// `header_ops` are applied first, then `txs`, matching the ledger's
/// header -> contents processing order.
pub struct WalletBlock {
    pub id: HeaderId,
    pub parent: HeaderId,
    pub epoch: Epoch,
    pub voucher_cm: VoucherCm,
    pub header_ops: Vec<HeaderOp>,
    pub txs: BlockTransactions<WalletTx>,
}

/// Wallet-relevant content of one transaction, in source order.
#[derive(Clone, Debug, Default)]
pub struct WalletTx {
    ops: UpperBoundedVec<WalletOp, MAX_OPS_PER_TX>,
}

/// A wallet-relevant effect produced by the block header processing.
#[derive(Clone, Debug)]
pub enum HeaderOp {
    /// An SDP-locked note has been unlocked.
    Unlock(NoteId),
    /// An SDP reward UTXO was minted.
    SdpReward(Utxo),
}

/// Build a [`HeaderOp`] for a wallet-relevant header event.
impl From<&HeaderEvent> for HeaderOp {
    fn from(event: &HeaderEvent) -> Self {
        match event {
            HeaderEvent::SdpNoteUnlocked { note_id, .. } => Self::Unlock(*note_id),
            HeaderEvent::SdpRewardDistributed { utxo, .. } => Self::SdpReward(*utxo),
        }
    }
}

/// A wallet-relevant effect extracted from a tx.
///
/// Some variants correspond directly to a Mantle op; others are derived by
/// pairing an op with the corresponding event it emits. Either way, each
/// variant carries exactly what `apply_block` needs to mirror the ledger's
/// effect.
#[derive(Clone, Debug)]
pub enum WalletOp {
    /// Spend `inputs`, create `outputs`.
    Transfer(TransferOp),
    /// Mark this note as locked.
    Lock(NoteId),
    /// Create the reward note.
    LeaderClaim(Utxo),
    /// Drop the deposited notes from the wallet and insert the channel notes
    /// they are re-created as. The re-created notes keep the same key, so they
    /// remain eligible for `PoL`, but are gated out of wallet-driven spending.
    ChannelDeposit(DepositOp),
    /// Drop the input channel notes from the wallet and insert the output
    /// channel notes owned by known keys.
    ChannelTransfer(ChannelTransferOp),
    /// Unmark the input channel notes, so they become spendable.
    ChannelWithdraw(ChannelWithdrawOp),
}

impl WalletBlock {
    #[must_use]
    pub fn from_block<Tx>(block: &Block<Tx>, epoch: Epoch, events: &Events) -> Self
    where
        Tx: MantleTxWithProofs + Clone,
    {
        // TODO: devise a better way to mirror ledger's execution always correctly: https://github.com/logos-blockchain/logos-blockchain/issues/2627
        let (header_events, tx_events) = group_events(events);
        Self {
            id: block.header().id(),
            parent: block.header().parent(),
            epoch,
            voucher_cm: *block.header().leader_proof().voucher_cm(),
            header_ops: header_events.iter().map(Into::into).collect(),
            txs: transform_txs(block.transactions(), tx_events),
        }
    }

    /// Note IDs this block spends or locks.
    #[must_use]
    pub fn spent_note_ids(&self) -> HashSet<NoteId> {
        self.txs
            .iter()
            .flat_map(|tx| tx.ops.iter())
            .flat_map(|op| match op {
                WalletOp::Transfer(transfer) => transfer.inputs.iter().copied().collect::<Vec<_>>(),
                WalletOp::ChannelDeposit(op) => op.inputs.iter().copied().collect::<Vec<_>>(),
                WalletOp::ChannelTransfer(op) => op.inputs.iter().copied().collect::<Vec<_>>(),
                WalletOp::Lock(note_id) => vec![*note_id],
                WalletOp::ChannelWithdraw(_) | WalletOp::LeaderClaim(_) => Vec::new(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletState {
    pub utxos: rpds::HashTrieMapSync<NoteId, Utxo>,
    pub pk_index: rpds::HashTrieMapSync<ZkPublicKey, rpds::HashTrieSetSync<NoteId>>,
    pub locked_notes: rpds::HashTrieSetSync<NoteId>,
    /// Notes known by the wallet but currently owned by a channel. They stay
    /// in `utxos` and are still eligible for `PoL`, but must be gated out of
    /// wallet-driven spending.
    #[serde(default)]
    pub channel_notes: rpds::HashTrieSetSync<NoteId>,
    pub epoch: Epoch,
    /// MMR of all voucher commitments included in the chain
    pub vouchers: MerkleMountainRange<VoucherCm, ZkHasher>,
    /// All **tracked** voucher merkle paths up to the current block
    pub voucher_paths: VoucherPaths,
    /// A snapshot of **tracked** voucher merkle paths,
    /// updated at the first block of each epoch.
    pub voucher_paths_snapshot: VoucherPaths,
}

pub type VoucherPaths = rpds::HashTrieMapSync<VoucherCm, MerklePath>;

impl WalletState {
    pub fn from_ledger<KeyId>(
        known_keys: &HashMap<ZkPublicKey, KeyId>,
        ledger: &LedgerState,
    ) -> Self {
        let mut utxos = rpds::HashTrieMapSync::new_sync();
        let mut pk_index = rpds::HashTrieMapSync::new_sync();
        let mut locked_notes = rpds::HashTrieSetSync::new_sync();
        let mut channel_notes = rpds::HashTrieSetSync::new_sync();

        let all_locked_notes = ledger.mantle_ledger().sdp_ledger().locked_notes();
        let channels = ledger.mantle_ledger().channels();
        for (_, (utxo, _)) in ledger.latest_utxos().utxos().iter() {
            if known_keys.contains_key(&utxo.note.pk) {
                let note_id = utxo.id();

                utxos = utxos.insert(note_id, *utxo);

                let note_set = pk_index
                    .get(&utxo.note.pk)
                    .cloned()
                    .unwrap_or_else(rpds::HashTrieSetSync::new_sync)
                    .insert(note_id);
                pk_index = pk_index.insert(utxo.note.pk, note_set);

                if all_locked_notes.contains(&note_id) {
                    locked_notes = locked_notes.insert(note_id);
                }

                // A channel note keeps the depositor's key but is owned by the channel:
                // keep it in `utxos` so it stays eligible for PoL,
                // and mark it so wallet-driven spending gates it out.
                if channels.is_channel_note(&note_id) {
                    channel_notes = channel_notes.insert(note_id);
                }
            }
        }

        Self {
            utxos,
            pk_index,
            locked_notes,
            channel_notes,
            epoch: ledger.epoch_state().epoch,
            vouchers: ledger.mantle_ledger().vouchers().clone(),
            voucher_paths: rpds::HashTrieMapSync::new_sync(),
            voucher_paths_snapshot: rpds::HashTrieMapSync::new_sync(),
        }
    }

    pub fn utxos_owned_by_pks(
        &self,
        pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
    ) -> Vec<Utxo> {
        pks.into_iter()
            .filter_map(|pk| self.pk_index.get(pk.borrow()))
            .flatten()
            .map(|id| self.utxos[id])
            .collect()
    }

    /// Funds the transaction so its excess balance is exactly
    /// `priority_fee` above the mandatory fee — the excess is the
    /// transaction's execution tip. `0` funds to the exact minimum.
    pub fn fund_tx<G: GasConstants>(
        &self,
        tx_builder: &MantleTxBuilder,
        change_pk: ZkPublicKey,
        pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
        context: &MantleTxContext,
        excluded_notes: &HashSet<NoteId>,
        priority_fee: Value,
    ) -> Result<MantleTxBuilder, WalletError> {
        // Get all UTXOs owned by the provided PKs, excluding the following notes:
        // - Notes that are being consumed/locked by the tx
        // - Notes that are already locked in Ledger
        // - Notes currently owned by a channel (deposited)
        // - Notes excluded by the caller (e.g. reserved for in-flight txs)
        let excluded_notes = tx_builder
            .consumed_or_locked_notes()
            .chain(self.locked_notes.iter().copied())
            .chain(self.channel_notes.iter().copied())
            .chain(excluded_notes.iter().copied())
            .collect::<HashSet<_>>();
        let mut utxos = self
            .utxos_owned_by_pks(pks)
            .into_iter()
            .filter(|utxo| !excluded_notes.contains(&utxo.id()))
            .collect::<Vec<_>>();

        // Consume large valued notes first to ensure we converge.
        utxos.sort_by_key(|utxo| -i128::from(utxo.note.value));

        // The funding target: the tx's excess balance over the mandatory fee
        // must end up exactly at `priority_fee` (the execution tip).
        let delta_target = i128::from(priority_fee);

        // The transaction may already be funded before we add any of the
        // wallet's UTXOs, for example when it pays no fee or the caller already
        // supplied inputs. In that case we must not pull in an extra input (and
        // the change note it would require).
        match tx_builder.funding_delta::<G>(context)?.cmp(&delta_target) {
            Ordering::Equal => return Ok(tx_builder.clone()),
            Ordering::Greater => {
                if let Some(tx_with_change) =
                    tx_builder
                        .clone()
                        .return_change::<G>(context, change_pk, priority_fee)?
                {
                    return Ok(tx_with_change);
                }
                // The change note costs more than the surplus covers, so fall
                // through and pull in more UTXO's below.
            }
            Ordering::Less => {}
        }

        for i in 0..utxos.len() {
            let funded_tx_builder = tx_builder
                .clone()
                .extend_ledger_inputs(utxos[..=i].iter().copied())?;

            let funding_delta = funded_tx_builder.funding_delta::<G>(context)?;

            match funding_delta.cmp(&delta_target) {
                Ordering::Less => {
                    // Insufficient funds, need more UTXO's.
                }
                Ordering::Equal => {
                    // We can exactly pay the tx cost plus the tip, no change
                    // note needed.
                    return Ok(funded_tx_builder);
                }
                Ordering::Greater => {
                    // We have enough balance, but we need to introduce a change note.
                    // The change note will slightly increase the storage cost of the tx so there is
                    // a chance that we will not be able to fund the tx with the change note.
                    if let Some(tx_with_change) =
                        funded_tx_builder.return_change::<G>(context, change_pk, priority_fee)?
                    {
                        // We were able to fund the tx with change note added.
                        return Ok(tx_with_change);
                    }
                    // Otherwise, need more UTXO's.
                }
            }
        }

        Err(WalletError::InsufficientFunds {
            available: utxos.iter().map(|u| u.note.value).sum::<u64>(),
        })
    }

    #[must_use]
    pub fn balance(&self, pk: ZkPublicKey) -> Option<WalletBalance> {
        let mut balance = WalletBalance {
            balance: 0,
            notes: HashMap::new(),
        };

        self.pk_index.get(&pk)?.iter().for_each(|id| {
            // Channel notes should be counted in the wallet's total balance
            if self.channel_notes.contains(id) {
                return;
            }
            let value = self.utxos[id].note.value;
            balance.balance += value;
            balance.notes.insert(*id, value);
        });

        Some(balance)
    }

    pub fn apply_block<KeyId, VoucherId>(
        &self,
        known_keys: &HashMap<ZkPublicKey, KeyId>,
        known_vouchers: &Vouchers<VoucherId>,
        block: &WalletBlock,
    ) -> Result<Self, WalletError> {
        let mut utxos = self.utxos.clone();
        let mut pk_index = self.pk_index.clone();
        let mut locked_notes = self.locked_notes.clone();
        let mut channel_notes = self.channel_notes.clone();

        // Header-level effects come first, matching the ledger's
        // header -> contents processing order.
        for op in &block.header_ops {
            match op {
                HeaderOp::Unlock(note_id) => {
                    // When unlocking notes, we don't check if the notes are owned
                    // by the wallet because they will be ignored automatically.
                    locked_notes.remove_mut(note_id);
                }
                HeaderOp::SdpReward(utxo) => {
                    insert_utxo_if_owned(*utxo, known_keys, &mut utxos, &mut pk_index);
                }
            }
        }

        // Apply each tx & op in source order, as the same as how ledger processes them.
        for tx in &block.txs {
            for op in &tx.ops {
                match op {
                    WalletOp::Transfer(transfer) => {
                        // When removing spent notes, we don't check if the notes are owned
                        // by the wallet because they will be ignored automatically.
                        for input_id in transfer.inputs.iter() {
                            remove_spent_utxo(input_id, &mut utxos, &mut pk_index);
                        }
                        for utxo in transfer.utxos() {
                            insert_utxo_if_owned(utxo, known_keys, &mut utxos, &mut pk_index);
                        }
                    }
                    WalletOp::ChannelDeposit(op) => {
                        // The deposit consumes its inputs and re-creates them as channel notes
                        // under a new NoteId, so drop the inputs and insert the re-created notes.
                        // They keep the same key, so they're still eligible for PoL.
                        for (output_index, input) in op.inputs.iter().enumerate() {
                            let note = utxos.get(input).map(|utxo| utxo.note);
                            remove_spent_utxo(input, &mut utxos, &mut pk_index);

                            let Some(note) = note else { continue };
                            let utxo = Utxo::new(op.op_id(), output_index, note);
                            if insert_utxo_if_owned(utxo, known_keys, &mut utxos, &mut pk_index) {
                                channel_notes.insert_mut(utxo.id());
                            }
                        }
                    }
                    WalletOp::ChannelTransfer(op) => {
                        for input_id in op.inputs.iter() {
                            remove_spent_utxo(input_id, &mut utxos, &mut pk_index);
                            channel_notes.remove_mut(input_id);
                        }
                        for utxo in op.utxos() {
                            if insert_utxo_if_owned(utxo, known_keys, &mut utxos, &mut pk_index) {
                                channel_notes.insert_mut(utxo.id());
                            }
                        }
                    }
                    WalletOp::ChannelWithdraw(op) => {
                        // Drop the channel-note marker so they're spendable again.
                        // They still stay in `utxos`.
                        for input_id in op.inputs.iter() {
                            channel_notes.remove_mut(input_id);
                        }
                    }
                    WalletOp::Lock(note_id) => {
                        if utxos.contains_key(note_id) {
                            locked_notes.insert_mut(*note_id);
                        }
                    }
                    WalletOp::LeaderClaim(utxo) => {
                        insert_utxo_if_owned(*utxo, known_keys, &mut utxos, &mut pk_index);
                    }
                }
            }
        }

        let (vouchers, voucher_paths, voucher_paths_snapshot) =
            self.apply_voucher(known_vouchers, block)?;

        Ok(Self {
            utxos,
            pk_index,
            locked_notes,
            channel_notes,
            epoch: block.epoch,
            vouchers,
            voucher_paths,
            voucher_paths_snapshot,
        })
    }

    /// Apply the voucher commitment from the block to the wallet state.
    ///
    /// Returns:
    /// - Updated MMR including the new voucher commitment
    /// - Updated tracked voucher paths, including the new voucher if it is
    ///   owned by us
    /// - Snapshot of trakced voucher paths (updated only if epoch is advancing)
    fn apply_voucher<VoucherId>(
        &self,
        known_vouchers: &Vouchers<VoucherId>,
        block: &WalletBlock,
    ) -> Result<
        (
            MerkleMountainRange<VoucherCm, ZkHasher>,
            VoucherPaths,
            VoucherPaths,
        ),
        WalletError,
    > {
        // Snapshot voucher paths if epoch is advancing
        let snapshot = if block.epoch > self.epoch {
            self.voucher_paths.clone()
        } else {
            self.voucher_paths_snapshot.clone()
        };

        // Filter out vouchers that have been claimed/finalized.
        // `known_vouchers` always reflects the latest set of vouchers to be tracked.
        let (cms, mut paths): (Vec<VoucherCm>, Vec<MerklePath>) = self
            .voucher_paths
            .iter()
            .filter(|(cm, _)| known_vouchers.get(cm).is_some())
            .map(|(cm, path)| (*cm, path.clone()))
            .unzip();

        // Push the new voucher to the MMR and update all tracked paths
        let (vouchers, new_path) = self
            .vouchers
            .push_with_paths(block.voucher_cm, &mut paths)
            .map_err(|error| match error {
                lb_mmr::PushWithPathsError::MmrFull(_) => WalletError::VoucherMmrFull,
                lb_mmr::PushWithPathsError::InvalidTrackedPath(error) => error.into(),
            })?;

        // Rebuild the tracked voucher paths map with updated paths.
        let mut voucher_paths = rpds::HashTrieMapSync::new_sync();
        for (cm, path) in cms.into_iter().zip(paths) {
            voucher_paths = voucher_paths.insert(cm, path);
        }

        // Track the new voucher's path if it is owned by us
        if known_vouchers.get(&block.voucher_cm).is_some() {
            voucher_paths = voucher_paths.insert(block.voucher_cm, new_path);
        }

        Ok((vouchers, voucher_paths, snapshot))
    }
}

/// Insert `utxo` into `utxos`/`pk_index` if the wallet owns its key.
/// Returns whether the insertion happened.
fn insert_utxo_if_owned<KeyId>(
    utxo: Utxo,
    known_keys: &HashMap<ZkPublicKey, KeyId>,
    utxos: &mut rpds::HashTrieMapSync<NoteId, Utxo>,
    pk_index: &mut rpds::HashTrieMapSync<ZkPublicKey, rpds::HashTrieSetSync<NoteId>>,
) -> bool {
    if !known_keys.contains_key(&utxo.note.pk) {
        return false;
    }

    let note_id = utxo.id();
    utxos.insert_mut(note_id, utxo);

    let note_set = pk_index
        .get(&utxo.note.pk)
        .cloned()
        .unwrap_or_else(rpds::HashTrieSetSync::new_sync)
        .insert(note_id);
    pk_index.insert_mut(utxo.note.pk, note_set);
    true
}

fn transform_txs<Tx>(
    txs: &BlockTransactions<Tx>,
    mut events_by_tx: HashMap<TxHash, HashMap<Hash, TxEventPayload>>,
) -> BlockTransactions<WalletTx>
where
    Tx: MantleTxWithProofs,
{
    txs.map_ref(move |tx| {
        let mut events_by_op = events_by_tx.remove(&tx.hash()).unwrap_or_default();

        let ops = tx.mantle_tx().ops().filter_map_ref(|op| {
            let event = op_id(op).and_then(|id| events_by_op.remove(&id));
            transform_op(op, event)
        });

        WalletTx { ops }
    })
}

/// Group a block's events:
/// - `Event::Header`: effects produced by the block header
/// - `Event::Tx`: effects produced by txs, indexed by `(tx_hash, op_id)`
fn group_events(
    events: &Events,
) -> (
    Vec<HeaderEvent>,
    HashMap<TxHash, HashMap<Hash, TxEventPayload>>,
) {
    let mut header_events = Vec::new();
    let mut tx_events: HashMap<TxHash, HashMap<Hash, TxEventPayload>> = HashMap::new();
    for event in events.iter() {
        match event {
            Event::Tx(TxEvent {
                tx_hash,
                op_id,
                payload,
            }) => {
                tx_events
                    .entry(*tx_hash)
                    .or_default()
                    .insert(*op_id, payload.clone());
            }
            Event::Header(event) => header_events.push(event.clone()),
        }
    }
    (header_events, tx_events)
}

// TODO: move this to Op after implementing OpId for all operations
fn op_id(op: &Op) -> Option<Hash> {
    match op {
        Op::LeaderClaim(o) => Some(o.op_id()),
        Op::ChannelWithdraw(o) => Some(o.op_id()),
        Op::ChannelDeposit(o) => Some(o.op_id()),
        Op::Transfer(o) => Some(o.op_id()),
        _ => None,
    }
}

/// Build a [`WalletOp`] for an op together with the event (if any) it
/// emitted in this block. Returns `None` for ops the wallet doesn't track.
fn transform_op(op: &Op, event: Option<TxEventPayload>) -> Option<WalletOp> {
    match op {
        Op::Transfer(transfer) => Some(WalletOp::Transfer(transfer.clone())),
        Op::ChannelDeposit(deposit) => Some(WalletOp::ChannelDeposit(deposit.clone())),
        Op::ChannelTransfer(op) => Some(WalletOp::ChannelTransfer(op.clone())),
        Op::ChannelWithdraw(op) => Some(WalletOp::ChannelWithdraw(op.clone())),
        Op::SDPDeclare(declaration) => Some(WalletOp::Lock(declaration.locked_note_id)),
        Op::LeaderClaim(_) => match event.expect("event for LeaderClaim op must exist") {
            TxEventPayload::LeaderRewardClaimed { utxo, .. } => Some(WalletOp::LeaderClaim(utxo)),
            TxEventPayload::Deposit { .. } => {
                panic!("event for LeaderClaim op must be LeaderRewardClaimed")
            }
        },
        // `Op::SDPWithdraw` is ignored here — the note will be unlocked
        // after the delay and the corresponding event will be handled by
        // [`HeaderOp::from`].
        Op::ChannelInscribe(_) | Op::ChannelConfig(_) | Op::SDPWithdraw(_) | Op::SDPActive(_) => {
            None
        }
    }
}

fn remove_spent_utxo(
    spent_id: &NoteId,
    utxos: &mut rpds::HashTrieMapSync<NoteId, Utxo>,
    pk_index: &mut rpds::HashTrieMapSync<ZkPublicKey, rpds::HashTrieSetSync<NoteId>>,
) {
    let Some(utxo) = utxos.get(spent_id) else {
        return;
    };

    let pk = utxo.note.pk;
    utxos.remove_mut(spent_id);

    let Some(note_set) = pk_index.get(&pk) else {
        return;
    };

    let updated_set = note_set.remove(spent_id);
    if updated_set.is_empty() {
        pk_index.remove_mut(&pk);
    } else {
        pk_index.insert_mut(pk, updated_set);
    }
}

#[derive(Clone)]
pub struct Wallet<KeyId, VoucherId> {
    known_keys: HashMap<ZkPublicKey, KeyId>,
    known_vouchers: Vouchers<VoucherId>,
    wallet_states: BTreeMap<HeaderId, WalletState>,
}

impl<KeyId, VoucherId> Wallet<KeyId, VoucherId>
where
    VoucherId: Debug,
{
    /// Initialize [`Wallet`] from a given [`LedgerState`] at LIB.
    ///
    /// It initializes empty Merkle paths for all known vouchers,
    /// which will be updated as new blocks are applied.
    pub fn from_lib_ledger_state(
        known_keys: impl IntoIterator<Item = (ZkPublicKey, KeyId)>,
        known_vouchers: Vouchers<VoucherId>,
        lib: HeaderId,
        ledger: &LedgerState,
    ) -> Self {
        let known_keys = known_keys.into_iter().collect();
        let wallet_state = WalletState::from_ledger(&known_keys, ledger);

        info!(
            target: LOG_TARGET,
            ?lib,
            n_known_keys = known_keys.len(),
            n_known_vouchers = known_vouchers.count(),
            n_all_vouchers = wallet_state.vouchers.len(),
            "initializing wallet with LIB ledger state"
        );

        Self {
            known_keys,
            known_vouchers,
            wallet_states: [(lib, wallet_state)].into(),
        }
    }

    /// Initialize [`Wallet`] from a given [`WalletState`] at LIB
    /// (e.g., restored from persisted state).
    ///
    /// Tracking of Merkle paths  for known vouchers starts from the paths
    /// stored in the [`WalletState`]. Persisted Merkle paths are globally
    /// bounded during deserialization. Compatibility with the production
    /// MMR height is validated when active paths are updated, and `PoC`
    /// compatibility is validated during witness construction.
    pub fn from_lib_wallet_state(
        known_keys: impl IntoIterator<Item = (ZkPublicKey, KeyId)>,
        known_vouchers: Vouchers<VoucherId>,
        lib: HeaderId,
        wallet_state: WalletState,
    ) -> Self {
        let known_keys = known_keys.into_iter().collect::<HashMap<_, _>>();

        info!(
            target: LOG_TARGET,
            ?lib,
            n_known_keys = known_keys.len(),
            n_known_vouchers = known_vouchers.count(),
            n_all_vouchers = wallet_state.vouchers.len(),
            n_voucher_paths = wallet_state.voucher_paths.size(),
            n_snapshotted_voucher_paths = wallet_state.voucher_paths_snapshot.size(),
            "initializing wallet with LIB wallet state"
        );

        Self {
            known_keys,
            known_vouchers,
            wallet_states: [(lib, wallet_state)].into(),
        }
    }

    #[must_use]
    pub const fn known_keys(&self) -> &HashMap<ZkPublicKey, KeyId> {
        &self.known_keys
    }

    pub fn add_known_voucher(&mut self, cm: VoucherCm, nf: VoucherNullifier, id: VoucherId) {
        self.known_vouchers.insert(cm, nf, id);
    }

    #[must_use]
    pub fn get_voucher_by_nullifier(&self, nf: &VoucherNullifier) -> Option<&VoucherId> {
        self.known_vouchers.get_by_nullifier(nf)
    }

    pub fn voucher_commitments_and_nullifiers(&self) -> impl Iterator<Item = Voucher> + '_ {
        self.known_vouchers.commitments_and_nullifiers()
    }

    #[must_use]
    pub const fn vouchers(&self) -> &Vouchers<VoucherId> {
        &self.known_vouchers
    }

    #[must_use]
    pub fn has_processed_block(&self, block_id: HeaderId) -> bool {
        self.wallet_states.contains_key(&block_id)
    }

    pub fn apply_block(&mut self, block: &WalletBlock) -> Result<(), WalletError> {
        if self.wallet_states.contains_key(&block.id) {
            // Already processed this block
            return Ok(());
        }

        let block_wallet_state = self.wallet_state_at(block.parent)?.apply_block(
            &self.known_keys,
            &self.known_vouchers,
            block,
        )?;
        self.wallet_states.insert(block.id, block_wallet_state);
        Ok(())
    }

    /// Get the snapshotted Merkle path for a voucher.
    pub fn voucher_path_snapshot(
        &self,
        tip: HeaderId,
        cm: &VoucherCm,
    ) -> Result<Option<MerklePath>, WalletError> {
        Ok(self
            .wallet_state_at(tip)?
            .voucher_paths_snapshot
            .get(cm)
            .cloned())
    }

    /// Get the Merkle path for a voucher that is not yet snapshotted
    /// (only for testing)
    #[cfg(test)]
    fn voucher_path(
        &self,
        tip: HeaderId,
        cm: &VoucherCm,
    ) -> Result<Option<MerklePath>, WalletError> {
        Ok(self.wallet_state_at(tip)?.voucher_paths.get(cm).cloned())
    }

    pub fn balance(
        &self,
        tip: HeaderId,
        pk: ZkPublicKey,
    ) -> Result<Option<WalletBalance>, WalletError> {
        Ok(self.wallet_state_at(tip)?.balance(pk))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "thin passthrough to `WalletState::fund_tx` plus the tip"
    )]
    pub fn fund_tx<G: GasConstants>(
        &self,
        tip: HeaderId,
        tx_builder: &MantleTxBuilder,
        change_pk: ZkPublicKey,
        funding_pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
        context: &MantleTxContext,
        excluded_notes: &HashSet<NoteId>,
        priority_fee: Value,
    ) -> Result<MantleTxBuilder, WalletError> {
        self.wallet_state_at(tip)?.fund_tx::<G>(
            tx_builder,
            change_pk,
            funding_pks,
            context,
            excluded_notes,
            priority_fee,
        )
    }

    pub fn wallet_state_at(&self, tip: HeaderId) -> Result<WalletState, WalletError> {
        self.wallet_states
            .get(&tip)
            .cloned()
            .ok_or(WalletError::UnknownBlock(tip))
    }

    /// Prune wallet states for blocks that have been pruned from the chain.
    ///
    /// This removes wallet states for blocks that are no longer part of the
    /// chain after LIB advancement. Both stale blocks (from abandoned
    /// forks) and immutable blocks (before the new LIB) are removed.
    pub fn prune_states(&mut self, pruned_blocks: impl IntoIterator<Item = HeaderId>) {
        let mut removed_count = 0;

        for block_id in pruned_blocks {
            if self.wallet_states.remove(&block_id).is_some() {
                removed_count += 1;
            }
        }

        if removed_count > 0 {
            tracing::trace!(
                target: LOG_TARGET,
                removed_states = removed_count,
                remaining_states = self.wallet_states.len(),
                "Pruned wallet states for pruned blocks"
            );
        }
    }

    pub fn prune_vouchers(
        &mut self,
        immutable_transactions: impl IntoIterator<Item = VoucherNullifier>,
    ) {
        for voucher_nullifier in immutable_transactions {
            if let Some(id) = self.known_vouchers.remove_by_nullifier(&voucher_nullifier) {
                tracing::trace!(target: LOG_TARGET, "Pruned voucher {:?} from wallet", id);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletBalance {
    pub balance: Value,
    pub notes: HashMap<NoteId, Value>,
}

#[cfg(test)]
mod tests {
    use std::{
        iter::empty,
        num::{NonZero, NonZeroU64},
        sync::Arc,
    };

    use lb_core::{
        crypto::ZkDigest as _,
        mantle::{
            Note, OpProof, RawMantleTx, SignedMantleTx,
            channel::Channels,
            gas::MainnetGasConstants as Gas,
            ledger::{Inputs, Outputs},
            ops::channel::{
                ChannelId, MsgId,
                deposit::Metadata,
                inscribe::{Inscription, InscriptionOp},
            },
            transactions::{GasPrices, MantleTxGasContext, Ops, OpsProofs, states::Unverified},
        },
        proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate, LeaderPublic},
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::{EpochConfig, Slot};
    use lb_groth16::{CompressedGroth16Proof, Field as _, Fr};
    use lb_key_management_system_keys::keys::{
        Ed25519Key, Ed25519Signature, UnsecuredZkKey, ZkSignature,
    };
    use lb_ledger::mantle::sdp::{ServiceRewardsParameters, rewards};
    use lb_pol::LotteryConstants;
    use lb_utils::math::{NonNegativeF64, NonNegativeRatio};
    use lb_utxotree::UtxoTree;
    use num_bigint::BigUint;
    use rpds::HashTrieSetSync;

    use super::*;

    fn pk(v: u64) -> ZkPublicKey {
        ZkPublicKey::from(BigUint::from(v))
    }

    fn tx_hash(v: u8) -> Hash {
        [v; 32]
    }

    fn voucher(key_id: u64, idx: u64) -> (VoucherCm, VoucherNullifier) {
        let secret =
            ZkHasher::digest(&[BigUint::from(key_id).into(), BigUint::from(idx).into()]).into();
        (
            VoucherCm::from_secret(secret),
            VoucherNullifier::from_secret(secret),
        )
    }

    type TestVoucherId = (u64, u64);

    #[test]
    fn test_initialization() {
        let alice = pk(1);
        let bob = pk(2);
        let (voucher_master_key, voucher_index) = (100, 0);
        let (voucher_cm, voucher_nf) = voucher(voucher_master_key, voucher_index);

        let genesis = HeaderId::from([0; 32]);

        let ledger = LedgerState::from_utxos(
            [
                Utxo::new(tx_hash(0), 0, Note::new(100, alice)),
                Utxo::new(tx_hash(0), 1, Note::new(20, bob)),
                Utxo::new(tx_hash(0), 2, Note::new(4, alice)),
            ],
            &ledger_config(),
        );

        let wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            empty::<(ZkPublicKey, u64)>(),
            Vouchers::default(),
            genesis,
            &ledger,
        );
        assert_eq!(wallet.balance(genesis, alice).unwrap(), None);
        assert_eq!(wallet.balance(genesis, bob).unwrap(), None);
        assert!(wallet.vouchers().get(&voucher_cm).is_none());

        let wallet = Wallet::from_lib_ledger_state(
            [(alice, 1)],
            Vouchers::new([(voucher_cm, voucher_nf, (voucher_master_key, voucher_index))]),
            genesis,
            &ledger,
        );
        assert_eq!(
            wallet.balance(genesis, alice).unwrap().unwrap().balance,
            104
        );
        assert_eq!(wallet.balance(genesis, bob).unwrap(), None);
        assert_eq!(
            wallet.vouchers().get(&voucher_cm),
            Some(&(voucher_master_key, voucher_index))
        );

        let wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(bob, 2)],
            Vouchers::default(),
            genesis,
            &ledger,
        );
        assert_eq!(wallet.balance(genesis, alice).unwrap(), None);
        assert_eq!(wallet.balance(genesis, bob).unwrap().unwrap().balance, 20);

        let wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(alice, 1), (bob, 2)],
            Vouchers::default(),
            genesis,
            &ledger,
        );
        assert_eq!(
            wallet.balance(genesis, alice).unwrap().unwrap().balance,
            104
        );
        assert_eq!(wallet.balance(genesis, bob).unwrap().unwrap().balance, 20);
    }

    #[test]
    fn test_recovered_malformed_voucher_path_fails_cleanly_on_append() {
        let genesis = HeaderId::from([0; 32]);
        let (voucher_cm, voucher_nf) = voucher(1, 0);
        let mut state = WalletState::from_ledger(
            &HashMap::<ZkPublicKey, u64>::new(),
            &LedgerState::from_utxos([], &ledger_config()),
        );
        state.vouchers = MerkleMountainRange::new().push(voucher_cm).unwrap();
        state.voucher_paths = rpds::HashTrieMapSync::new_sync()
            .insert(voucher_cm, MerklePath::try_new(0, vec![]).unwrap());
        state.voucher_paths_snapshot = state.voucher_paths.clone();
        let state: WalletState =
            bincode::deserialize(&bincode::serialize(&state).unwrap()).unwrap();

        let mut wallet = Wallet::<u64, TestVoucherId>::from_lib_wallet_state(
            [],
            Vouchers::new([(voucher_cm, voucher_nf, (1, 0))]),
            genesis,
            state,
        );
        assert_eq!(
            wallet
                .voucher_path_snapshot(genesis, &voucher_cm)
                .unwrap()
                .unwrap()
                .siblings()
                .len(),
            0
        );
        let next_block = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            epoch: 0.into(),
            voucher_cm: voucher(2, 0).0,
            header_ops: vec![],
            txs: BlockTransactions::empty(),
        };

        assert!(matches!(
            wallet.apply_block(&next_block),
            Err(WalletError::InvalidVoucherPath(
                lb_mmr::MerklePathError::InvalidSiblingCount {
                    expected: 32,
                    actual: 0,
                }
            ))
        ));
        assert!(!wallet.has_processed_block(next_block.id));
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "Test function.")]
    fn test_sync() {
        let alice = pk(1);
        let bob = pk(2);

        let genesis = HeaderId::from([0; 32]);

        let genesis_ledger = LedgerState::from_utxos([], &ledger_config());

        let (v1_cm, v1_nf) = voucher(1, 0);
        let (v2_cm, v2_nf) = voucher(1, 1);
        let (v3_cm, _v3_nf) = voucher(2, 0);

        let mut wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(alice, 1), (bob, 2)],
            Vouchers::new([(v1_cm, v1_nf, (1, 0)), (v2_cm, v2_nf, (1, 1))]),
            genesis,
            &genesis_ledger,
        );

        // Block 1 (epoch 1)
        // - alice is minted 104 NMO in two notes (100 NMO and 4 NMO)
        // - voucher v1 is ours -> should be tracked
        let transfer1 = TransferOp {
            inputs: Inputs::empty(),
            outputs: Outputs::new([Note::new(100, alice), Note::new(4, alice)]),
        };
        // immediately lock the 2nd note from `transfer1`
        let locked_note = transfer1.outputs.utxo_by_index(1, &transfer1).unwrap().id();

        let block_1 = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            epoch: 1.into(),
            voucher_cm: v1_cm,
            // Unknown unlocked note that will be ignored.
            header_ops: vec![HeaderOp::Unlock(NoteId::from(Fr::ONE))],
            txs: [WalletTx {
                ops: [
                    WalletOp::Transfer(transfer1.clone()),
                    WalletOp::Lock(locked_note),
                ]
                .into(),
            }]
            .into(),
        };

        wallet.apply_block(&block_1).unwrap();
        assert_locked_notes(&wallet, block_1.id, [locked_note]);
        // v1 is tracked but not yet claimable (no epoch transition yet)
        assert_tracked_but_not_snapshotted_voucher(&wallet, block_1.id, &v1_cm);

        // Block 2 (epoch 2) -- epoch transition snapshots v1's path as claimable
        //  - alice spends 100 NMO utxo, sending 20 NMO to bob and 80 to herself
        // - voucher v2 is ours -> should be tracked
        let alice_100_nmo_utxo = transfer1.outputs.utxo_by_index(0, &transfer1).unwrap();
        let transfer2 = TransferOp {
            inputs: Inputs::new([alice_100_nmo_utxo.id()]),
            outputs: Outputs::new([Note::new(20, bob), Note::new(80, alice)]),
        };

        let block_2 = WalletBlock {
            id: HeaderId::from([2; 32]),
            parent: block_1.id,
            epoch: 2.into(),
            voucher_cm: v2_cm,
            // Unlock the previously locked note
            header_ops: vec![HeaderOp::Unlock(locked_note)],
            txs: [WalletTx {
                ops: [
                    WalletOp::Transfer(transfer2.clone()),
                    // Unknown locked note that will be ignored
                    WalletOp::Lock(NoteId::from(Fr::ONE)),
                ]
                .into(),
            }]
            .into(),
        };
        wallet.apply_block(&block_2).unwrap();
        assert_locked_notes(&wallet, block_2.id, []);
        // v1 is now claimable after epoch transition
        assert_snapshotted_voucher(&wallet, block_2.id, &v1_cm);
        // v2 is ours, but not yet snapshotted
        assert_tracked_but_not_snapshotted_voucher(&wallet, block_2.id, &v2_cm);

        // Query the balance of for each pk at different points in the blockchain
        assert_eq!(wallet.balance(genesis, alice).unwrap(), None);
        assert_eq!(wallet.balance(genesis, bob).unwrap(), None);

        assert_eq!(
            wallet.balance(block_1.id, alice).unwrap().unwrap().balance,
            104
        );
        assert_eq!(wallet.balance(block_1.id, bob).unwrap(), None);

        assert_eq!(
            wallet.balance(block_2.id, alice).unwrap().unwrap().balance,
            84
        );
        assert_eq!(
            wallet.balance(block_2.id, bob).unwrap().unwrap().balance,
            20
        );

        // Block 3 (still, epoch 2)
        // - alice deposits the 80 NMO note into a channel. The note stays in the wallet
        //   (still eligible for PoL) but is marked as a channel note.
        // - voucher v3 is not ours -> should not be tracked
        let alice_80_nmo_utxo = transfer2.outputs.utxo_by_index(1, &transfer2).unwrap();

        let deposit = DepositOp {
            channel_id: ChannelId::from([0u8; 32]),
            inputs: [alice_80_nmo_utxo.id()].into(),
            metadata: Metadata::empty(),
        };
        let deposited = Utxo::new(deposit.op_id(), 0, alice_80_nmo_utxo.note);

        let block_3 = WalletBlock {
            id: HeaderId::from([3; 32]),
            parent: block_2.id,
            epoch: 2.into(),
            voucher_cm: v3_cm,
            header_ops: vec![],
            txs: [WalletTx {
                ops: [
                    WalletOp::ChannelDeposit(deposit),
                    WalletOp::LeaderClaim(Utxo::new(tx_hash(9), 0, Note::new(38, alice))),
                ]
                .into(),
            }]
            .into(),
        };
        wallet.apply_block(&block_3).unwrap();

        // The deposited note shouldn't be counted in the wallet's total balance
        assert_eq!(
            wallet.balance(block_3.id, alice).unwrap().unwrap().balance,
            42
        );
        assert_eq!(
            wallet.balance(block_3.id, bob).unwrap().unwrap().balance,
            20
        );
        // The deposit consumes the 80 NMO note and re-creates it as a channel
        // note under a new NoteId, still owned by Alice so it stays eligible
        // for PoL.
        let state = wallet.wallet_state_at(block_3.id).unwrap();
        assert!(!state.utxos.contains_key(&alice_80_nmo_utxo.id()));
        assert!(state.utxos.contains_key(&deposited.id()));
        assert!(state.channel_notes.contains(&deposited.id()));

        // v1 is still claimable
        assert_snapshotted_voucher(&wallet, block_3.id, &v1_cm);
        // v2 is still not snapshotted
        assert_tracked_but_not_snapshotted_voucher(&wallet, block_3.id, &v2_cm);
        // v3 is not ours, so not tracked at all
        assert_not_tracked_voucher(&wallet, block_3.id, &v3_cm);
    }

    /// Two transfers within a single transaction where the second consumes
    /// the first's change. After applying the block, the intermediate note
    /// must not appear in the wallet's UTXO set — only the final change
    /// note from the second transfer remains.
    #[test]
    fn test_apply_block_dependent_ops_within_tx() {
        let alice = pk(1);
        let bob = pk(2);
        let genesis = HeaderId::from([0; 32]);
        let genesis_ledger = LedgerState::from_utxos([], &ledger_config());
        let (v_cm, _v_nf) = voucher(1, 0);

        let mut wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(alice, 1), (bob, 2)],
            Vouchers::default(),
            genesis,
            &genesis_ledger,
        );

        // Op A: mint 100 NMO to alice.
        let transfer_a = TransferOp {
            inputs: Inputs::empty(),
            outputs: Outputs::new([Note::new(100, alice)]),
        };
        let intermediate = transfer_a
            .outputs
            .utxo_by_index(0, &transfer_a)
            .unwrap()
            .id();

        // Op B: spend the 100 NMO note created by op A; send 30 to bob,
        // 70 change back to alice.
        let transfer_b = TransferOp {
            inputs: Inputs::new([intermediate]),
            outputs: Outputs::new([Note::new(30, bob), Note::new(70, alice)]),
        };

        let block = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            epoch: 1.into(),
            voucher_cm: v_cm,
            header_ops: vec![],
            txs: [WalletTx {
                ops: [
                    WalletOp::Transfer(transfer_a),
                    WalletOp::Transfer(transfer_b),
                ]
                .into(),
            }]
            .into(),
        };
        wallet.apply_block(&block).unwrap();

        let alice_balance = wallet.balance(block.id, alice).unwrap().unwrap();
        assert_eq!(alice_balance.balance, 70);
        assert!(!alice_balance.notes.contains_key(&intermediate));

        let bob_balance = wallet.balance(block.id, bob).unwrap().unwrap();
        assert_eq!(bob_balance.balance, 30);
    }

    /// Two transactions in the same block where the second consumes a
    /// note the first produced. The wallet must end up matching the ledger:
    /// only the final change is tracked.
    #[test]
    fn test_apply_block_dependent_ops_across_txs() {
        let alice = pk(1);
        let bob = pk(2);
        let genesis = HeaderId::from([0; 32]);
        let genesis_ledger = LedgerState::from_utxos([], &ledger_config());
        let (v_cm, _v_nf) = voucher(1, 0);

        let mut wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(alice, 1), (bob, 2)],
            Vouchers::default(),
            genesis,
            &genesis_ledger,
        );

        let transfer_a = TransferOp {
            inputs: Inputs::empty(),
            outputs: Outputs::new([Note::new(100, alice)]),
        };
        let intermediate = transfer_a
            .outputs
            .utxo_by_index(0, &transfer_a)
            .unwrap()
            .id();
        let transfer_b = TransferOp {
            inputs: Inputs::new([intermediate]),
            outputs: Outputs::new([Note::new(30, bob), Note::new(70, alice)]),
        };

        let block = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            epoch: 1.into(),
            voucher_cm: v_cm,
            header_ops: vec![],
            txs: [
                WalletTx {
                    ops: [WalletOp::Transfer(transfer_a)].into(),
                },
                WalletTx {
                    ops: [WalletOp::Transfer(transfer_b)].into(),
                },
            ]
            .into(),
        };
        wallet.apply_block(&block).unwrap();

        let alice_balance = wallet.balance(block.id, alice).unwrap().unwrap();
        assert_eq!(alice_balance.balance, 70);
        assert!(!alice_balance.notes.contains_key(&intermediate));

        let bob_balance = wallet.balance(block.id, bob).unwrap().unwrap();
        assert_eq!(bob_balance.balance, 30);
    }

    // TODO: support channel note tracking in wallet
    // #[test]
    // fn test_channel_withdraw_outputs_update_balance() {
    //     let alice = pk(1);
    //     let bob = pk(2);
    //     let genesis = HeaderId::from([0; 32]);
    //     let ledger = LedgerState::from_utxos([], &ledger_config());
    //     let (voucher_cm, _voucher_nf) = voucher(1, 0);
    //     let alice_withdraw_utxo = Utxo::new(tx_hash(1), 0, Note::new(42, alice));
    //     let bob_withdraw_utxo = Utxo::new(tx_hash(1), 1, Note::new(7, bob));
    //
    //     let mut wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
    //         [(alice, 1)],
    //         Vouchers::default(),
    //         genesis,
    //         &ledger,
    //     );
    //
    //     let block = WalletBlock {
    //         id: HeaderId::from([1; 32]),
    //         parent: genesis,
    //         epoch: 1.into(),
    //         voucher_cm,
    //         header_ops: vec![],
    //         txs: vec![WalletTx {
    //             ops: vec![WalletOp::ChannelWithdraw(vec![
    //                 alice_withdraw_utxo,
    //                 bob_withdraw_utxo,
    //             ])],
    //         }],
    //     };
    //
    //     wallet.apply_block(&block).unwrap();
    //
    //     assert_eq!(
    //         wallet.balance(block.id, alice).unwrap().unwrap().balance,
    //         42
    //     );
    //     assert_eq!(wallet.balance(block.id, bob).unwrap(), None);
    // }

    #[test]
    fn test_sdp_reward_credits_owned_provider() {
        let alice = pk(1);
        let bob = pk(2);
        let genesis = HeaderId::from([0; 32]);
        let ledger = LedgerState::from_utxos([], &ledger_config());
        let (voucher_cm, _voucher_nf) = voucher(1, 0);

        // A wallet that only knows alice's key.
        let mut wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(alice, 1)],
            Vouchers::default(),
            genesis,
            &ledger,
        );

        // Two reward UTXOs — one to alice (owned), one to bob (unknown).
        let alice_reward = Utxo::new(tx_hash(1), 0, Note::new(100, alice));
        let bob_reward = Utxo::new(tx_hash(1), 1, Note::new(50, bob));

        let block = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            epoch: 1.into(),
            voucher_cm,
            header_ops: vec![
                HeaderOp::SdpReward(alice_reward),
                HeaderOp::SdpReward(bob_reward),
            ],
            txs: BlockTransactions::empty(),
        };

        wallet.apply_block(&block).unwrap();

        assert_eq!(
            wallet.balance(block.id, alice).unwrap().unwrap().balance,
            100,
            "alice's reward UTXO must be credited to her balance",
        );
        assert_eq!(
            wallet.balance(block.id, bob).unwrap(),
            None,
            "bob's key is unknown to this wallet; his reward must be ignored",
        );
    }

    #[test]
    fn test_fund_tx_with_change() {
        let alice = pk(1);
        let utxo1 = Utxo::new(tx_hash(0), 0, Note::new(5000, alice));
        let utxo2 = Utxo::new(tx_hash(0), 1, Note::new(5000, alice));
        let ledger_state = LedgerState::from_utxos([utxo1, utxo2], &ledger_config());

        let mut wallet_state =
            WalletState::from_ledger(&HashMap::from_iter([(alice, 1)]), &ledger_state);
        // Lock `utxo1` deliberately to ensure that `fund_tx` excludes locked notes
        wallet_state.locked_notes = wallet_state.locked_notes.insert(utxo1.id());

        let context = MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                &Channels::default(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: 0,
        };
        let tx_builder = MantleTxBuilder::new();

        // Fund the transaction
        let funded_tx_builder = wallet_state
            .fund_tx::<Gas>(&tx_builder, alice, [alice], &context, &HashSet::new(), 0)
            .unwrap();

        assert_eq!(
            794,
            funded_tx_builder
                .minimum_gas_cost::<Gas>(&context)
                .unwrap()
                .into_inner()
        );
        assert_eq!(794, funded_tx_builder.net_balance());
        assert_eq!(0, funded_tx_builder.funding_delta::<Gas>(&context).unwrap());

        let funded_tx = funded_tx_builder.build().unwrap();

        if let Op::Transfer(transfer_op) = &funded_tx.ops()[funded_tx.ops().len() - 1] {
            // ensure alices utxo was used to pay the fee
            assert_eq!(transfer_op.inputs, Inputs::new([utxo2.id()]));
            // ensure change was returned to alice
            assert_eq!(
                transfer_op.outputs,
                Outputs::new([Note {
                    value: 4206,
                    pk: alice,
                }])
            );
        } else {
            panic!("last op must be a transfer")
        }
    }

    #[test]
    fn test_fund_tx_with_priority_fee() {
        let alice = pk(1);
        let utxo = Utxo::new(tx_hash(0), 0, Note::new(5000, alice));
        let ledger_state = LedgerState::from_utxos([utxo], &ledger_config());
        let wallet_state =
            WalletState::from_ledger(&HashMap::from_iter([(alice, 1)]), &ledger_state);

        let context = MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                &Channels::default(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: 0,
        };
        let tx_builder = MantleTxBuilder::new();
        let priority_fee = 200;

        let funded_tx_builder = wallet_state
            .fund_tx::<Gas>(
                &tx_builder,
                alice,
                [alice],
                &context,
                &HashSet::new(),
                priority_fee,
            )
            .unwrap();

        // The tip is left as excess balance above the mandatory fee.
        let gas_cost = funded_tx_builder
            .minimum_gas_cost::<Gas>(&context)
            .unwrap()
            .into_inner();
        assert_eq!(
            i128::from(gas_cost + priority_fee),
            funded_tx_builder.net_balance()
        );
        assert_eq!(
            i128::from(priority_fee),
            funded_tx_builder.funding_delta::<Gas>(&context).unwrap()
        );

        // The change output shrank by exactly the tip.
        let funded_tx = funded_tx_builder.build().unwrap();
        if let Op::Transfer(transfer_op) = &funded_tx.ops()[funded_tx.ops().len() - 1] {
            assert_eq!(
                transfer_op.outputs,
                Outputs::new([Note {
                    value: 5000 - gas_cost - priority_fee,
                    pk: alice,
                }])
            );
        } else {
            panic!("last op must be a transfer")
        }
    }

    #[test]
    fn test_fund_tx_no_fee_adds_no_input() {
        let alice = pk(1);
        // The wallet has a spendable note available...
        let utxo = Utxo::new(tx_hash(0), 0, Note::new(5000, alice));
        let ledger_state = LedgerState::from_utxos([utxo], &ledger_config());
        let wallet_state =
            WalletState::from_ledger(&HashMap::from_iter([(alice, 1)]), &ledger_state);

        // ...but with zero gas prices a transfer-less transaction owes no fee,
        // so it needs no funding at all.
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                &Channels::default(),
                GasPrices::new(0, 0),
            ),
            leader_reward_amount: 0,
        };
        let mut tx_builder = MantleTxBuilder::new();

        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        tx_builder = tx_builder
            .push_op(Op::ChannelInscribe(InscriptionOp {
                channel_id: ChannelId::from([0xAA; 32]),
                inscription: [0xAB; 8].into(),
                parent: MsgId::from([0xBB; 32]),
                signer: signing_key.public_key(),
            }))
            .unwrap();

        assert_eq!(0, tx_builder.funding_delta::<Gas>(&context).unwrap());

        let funded_tx_builder = wallet_state
            .fund_tx::<Gas>(&tx_builder, alice, [alice], &context, &HashSet::new(), 0)
            .unwrap();

        // No input was pulled in (an added input would push the net balance to
        // 5000) and therefore no change note was added.
        assert_eq!(0, funded_tx_builder.net_balance());

        // The empty transfer is dropped, so the built tx carries no transfer op.
        let funded_tx = funded_tx_builder.build().unwrap();
        assert!(
            !funded_tx
                .ops()
                .iter()
                .any(|op| matches!(op, Op::Transfer(_))),
            "a fee-less transaction must not contain a transfer op"
        );
    }

    #[test]
    fn test_fund_tx_insufficient_funds() {
        let alice = pk(1);
        let ledger_state = LedgerState::from_utxos(
            [
                Utxo::new(tx_hash(0), 0, Note::new(100, alice)),
                Utxo::new(tx_hash(0), 1, Note::new(100, alice)),
                Utxo::new(tx_hash(0), 2, Note::new(100, alice)),
                Utxo::new(tx_hash(0), 3, Note::new(100, alice)),
            ],
            &ledger_config(),
        );

        let context = MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                ledger_state.mantle_ledger().channels(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: ledger_state.mantle_ledger().leader_reward_amount(),
        };

        let wallet_state =
            WalletState::from_ledger(&HashMap::from_iter([(alice, 1)]), &ledger_state);
        let mut tx_builder = MantleTxBuilder::new();

        // Add a costly inscription
        let signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let inscription = Op::ChannelInscribe(InscriptionOp {
            channel_id: ChannelId::from([0xAA; 32]),
            inscription: [0xAB; 1000].into(),
            parent: MsgId::from([0xBB; 32]),
            signer: signing_key.public_key(),
        });

        tx_builder = tx_builder.push_op(inscription).unwrap();

        // Fund the transaction
        let fund_attempt =
            wallet_state.fund_tx::<Gas>(&tx_builder, alice, [alice], &context, &HashSet::new(), 0);

        assert_eq!(
            fund_attempt.unwrap_err(),
            WalletError::InsufficientFunds { available: 400 }
        );
    }

    #[test]
    fn test_fund_tx_zero_funds() {
        let alice = pk(1);
        let ledger_state = LedgerState::from_utxos([], &ledger_config());

        let wallet_state =
            WalletState::from_ledger(&HashMap::from_iter([(alice, 1)]), &ledger_state);

        // Use non-zero gas prices so the transaction actually owes a fee and
        // therefore needs funding.
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                &Channels::default(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: 0,
        };
        let tx_builder = MantleTxBuilder::new();

        // Fund the transaction
        let fund_attempt =
            wallet_state.fund_tx::<Gas>(&tx_builder, alice, [alice], &context, &HashSet::new(), 0);

        assert_eq!(
            fund_attempt.unwrap_err(),
            WalletError::InsufficientFunds { available: 0 }
        );
    }

    #[test]
    fn test_fund_tx_all_locked_notes() {
        let alice = pk(1);
        let utxo = Utxo::new(tx_hash(0), 0, Note::new(5000, alice));
        let ledger_state = LedgerState::from_utxos([utxo], &ledger_config());

        let mut wallet_state =
            WalletState::from_ledger(&HashMap::from_iter([(alice, 1)]), &ledger_state);
        // Lock `utxo` deliberately to ensure that `fund_tx` excludes locked notes
        wallet_state.locked_notes = wallet_state.locked_notes.insert(utxo.id());

        // Use non-zero gas prices so the transaction actually owes a fee and
        // therefore needs funding.
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                &Channels::default(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: 0,
        };
        let tx_builder = MantleTxBuilder::new();

        // Fund the transaction
        let fund_attempt =
            wallet_state.fund_tx::<Gas>(&tx_builder, alice, [alice], &context, &HashSet::new(), 0);

        assert_eq!(
            fund_attempt.unwrap_err(),
            WalletError::InsufficientFunds { available: 0 }
        );
    }

    #[test]
    fn test_fund_tx_respects_pk_list() {
        let alice = pk(1);
        let bob = pk(2);
        let ledger_state = LedgerState::from_utxos(
            [Utxo::new(tx_hash(0), 0, Note::new(1_000_000, bob))],
            &ledger_config(),
        );

        let wallet_state =
            WalletState::from_ledger(&HashMap::from_iter([(alice, 1), (bob, 2)]), &ledger_state);

        // Use non-zero gas prices so the transaction actually owes a fee and
        // therefore needs funding.
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                &Channels::default(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: 0,
        };
        let tx_builder = MantleTxBuilder::new();

        // Attempt to fund the transaction with Alice's notes.
        let fund_attempt =
            wallet_state.fund_tx::<Gas>(&tx_builder, alice, [alice], &context, &HashSet::new(), 0);

        assert_eq!(
            fund_attempt.unwrap_err(),
            WalletError::InsufficientFunds { available: 0 }
        );

        // Fund the transaction with Bob's notes.
        wallet_state
            .fund_tx::<Gas>(&tx_builder, bob, [bob], &context, &HashSet::new(), 0)
            .unwrap(); // succesfully funded;
    }

    #[test]
    fn test_fund_tx_unfundable_region() {
        let alice = pk(1);

        let context = MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                &Channels::default(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: 0,
        };
        let tx_builder = MantleTxBuilder::new();

        // Determine gas cost without change note
        assert_eq!(
            754,
            tx_builder
                .clone()
                .add_ledger_input(Utxo::new(tx_hash(0), 0, Note::new(0, pk(0))))
                .unwrap()
                .minimum_gas_cost::<Gas>(&context)
                .unwrap()
                .into_inner()
        );

        // We can fund the tx if the note value is exactly the gas cost without change
        // note
        let wallet_state = WalletState::from_ledger(
            &HashMap::from_iter([(alice, 1)]),
            &LedgerState::from_utxos(
                [Utxo::new(tx_hash(0), 0, Note::new(754, alice))],
                &ledger_config(),
            ),
        );

        let funded_tx_wo_change = wallet_state
            .fund_tx::<Gas>(&tx_builder, alice, [alice], &context, &HashSet::new(), 0)
            .unwrap()
            .build()
            .unwrap(); // successfully funded the tx

        // verify that no change output was used.
        if let Op::Transfer(transfer_op) =
            &funded_tx_wo_change.ops()[funded_tx_wo_change.ops().len() - 1]
        {
            assert_eq!(transfer_op.outputs, Outputs::empty());
        } else {
            panic!("last op must be a transfer")
        }

        // Determine gas cost with change note
        assert_eq!(
            794,
            tx_builder
                .clone()
                .add_ledger_input(Utxo::new(tx_hash(0), 0, Note::new(0, pk(0))))
                .unwrap()
                .with_dummy_change_note()
                .unwrap()
                .minimum_gas_cost::<Gas>(&context)
                .unwrap()
                .into_inner()
        );

        for value in 755..=794 {
            // this region of note values will fail to fund the tx.
            // We can fund the tx if the note value is exactly the gas cost without change
            // note
            let wallet_state = WalletState::from_ledger(
                &HashMap::from_iter([(alice, 1)]),
                &LedgerState::from_utxos(
                    [Utxo::new(tx_hash(0), 0, Note::new(value, alice))],
                    &ledger_config(),
                ),
            );

            let fund_attempt = wallet_state.fund_tx::<Gas>(
                &tx_builder,
                alice,
                [alice],
                &context,
                &HashSet::new(),
                0,
            );

            assert_eq!(
                fund_attempt.unwrap_err(),
                WalletError::InsufficientFunds { available: value }
            );
        }

        // We can fund the tx if the note value exceeds gas cost with change note
        let wallet_state = WalletState::from_ledger(
            &HashMap::from_iter([(alice, 1)]),
            &LedgerState::from_utxos(
                [Utxo::new(tx_hash(0), 0, Note::new(795, alice))],
                &ledger_config(),
            ),
        );

        let funded_tx_wo_change = wallet_state
            .fund_tx::<Gas>(&tx_builder, alice, [alice], &context, &HashSet::new(), 0)
            .unwrap()
            .build()
            .unwrap(); // successfully funded the tx

        // verify that indeed a change output was used.
        if let Op::Transfer(transfer_op) =
            &funded_tx_wo_change.ops()[funded_tx_wo_change.ops().len() - 1]
        {
            assert_eq!(transfer_op.outputs, Outputs::new([Note::new(1, alice)]));
        } else {
            panic!("the last operation must be a transfer")
        }
    }

    #[must_use]
    fn ledger_config() -> lb_ledger::Config {
        let epoch_config = EpochConfig {
            epoch_stake_distribution_stabilization: NonZero::new(1).unwrap(),
            epoch_period_nonce_buffer: NonZero::new(1).unwrap(),
            epoch_period_nonce_stabilization: NonZero::new(1).unwrap(),
        };
        let consensus_config = lb_cryptarchia_engine::Config::new(
            NonZero::new(1).unwrap(),
            NonNegativeRatio::new(1, 10.try_into().unwrap()),
            1f64.try_into().expect("1 > 0"),
        );
        let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

        lb_ledger::Config {
            epoch_config,
            consensus_config,
            sdp_config: lb_ledger::mantle::sdp::Config {
                service_params: Arc::new(
                    [(
                        ServiceType::BlendNetwork,
                        ServiceParameters {
                            inactivity_period: 20.try_into().unwrap(),
                            epoch: 0.into(),
                        },
                    )]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: rewards::blend::RewardsParameters {
                        rounds_per_epoch: epoch_length.try_into().unwrap(),
                        message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
                        num_blend_layers: NonZeroU64::new(3).unwrap(),
                        minimum_network_size: NonZeroU64::new(1).unwrap(),
                        data_replication_factor: 0,
                        activity_threshold_sensitivity: 1,
                    },
                },
                min_stake: MinStake {
                    threshold: 1,
                    timestamp: 0,
                },
            },
            faucet_pk: None,
        }
    }

    fn assert_locked_notes<KeyId>(
        wallet: &Wallet<KeyId, TestVoucherId>,
        tip: HeaderId,
        notes: impl IntoIterator<Item = NoteId>,
    ) {
        let wallet_state = wallet.wallet_state_at(tip).unwrap();
        assert_eq!(wallet_state.locked_notes, HashTrieSetSync::from_iter(notes));
    }

    fn assert_snapshotted_voucher<KeyId>(
        wallet: &Wallet<KeyId, TestVoucherId>,
        tip: HeaderId,
        cm: &VoucherCm,
    ) {
        assert!(wallet.voucher_path(tip, cm).unwrap().is_some());
        assert!(wallet.voucher_path_snapshot(tip, cm).unwrap().is_some());
    }

    fn assert_tracked_but_not_snapshotted_voucher<KeyId>(
        wallet: &Wallet<KeyId, TestVoucherId>,
        tip: HeaderId,
        cm: &VoucherCm,
    ) {
        assert!(wallet.voucher_path(tip, cm).unwrap().is_some());
        assert!(wallet.voucher_path_snapshot(tip, cm).unwrap().is_none());
    }

    fn assert_not_tracked_voucher<KeyId>(
        wallet: &Wallet<KeyId, TestVoucherId>,
        tip: HeaderId,
        cm: &VoucherCm,
    ) {
        assert!(wallet.voucher_path(tip, cm).unwrap().is_none());
        assert!(wallet.voucher_path_snapshot(tip, cm).unwrap().is_none());
    }

    /// A `ChannelDeposit` must keep the deposited note in the wallet so the
    /// depositor retains `PoL` eligibility on their bridged funds, while
    /// marking it as a channel note so `fund_tx` gates it
    /// out.
    #[test]
    fn test_channel_deposit_keeps_note() {
        let alice = pk(1);
        let genesis = HeaderId::from([0; 32]);

        // Seed the wallet with a 100 NMO note owned by alice.
        let alice_utxo = Utxo::new(tx_hash(0), 0, Note::new(100, alice));
        let genesis_ledger = LedgerState::from_utxos([alice_utxo], &ledger_config());
        let (v_cm, _) = voucher(1, 0);
        let mut wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(alice, 1)],
            Vouchers::default(),
            genesis,
            &genesis_ledger,
        );

        // The note starts spendable.
        let state = wallet.wallet_state_at(genesis).unwrap();
        assert!(state.utxos.contains_key(&alice_utxo.id()));
        assert!(!state.channel_notes.contains(&alice_utxo.id()));
        assert_eq!(state.balance(alice).unwrap().balance, 100);

        // Deposit the note into a channel.
        let deposit = DepositOp {
            channel_id: ChannelId::from([0u8; 32]),
            inputs: [alice_utxo.id()].into(),
            metadata: Metadata::empty(),
        };
        let deposited = Utxo::new(deposit.op_id(), 0, alice_utxo.note);
        let block = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            epoch: 1.into(),
            voucher_cm: v_cm,
            header_ops: vec![],
            txs: [WalletTx {
                ops: [WalletOp::ChannelDeposit(deposit)].into(),
            }]
            .into(),
        };
        wallet.apply_block(&block).unwrap();

        // The re-created note is still in the wallet (still eligible for PoL)
        // but marked as a channel note, so it drops out of the balance.
        let state = wallet.wallet_state_at(block.id).unwrap();
        assert!(!state.utxos.contains_key(&alice_utxo.id()));
        assert!(state.utxos.contains_key(&deposited.id()));
        assert!(state.channel_notes.contains(&deposited.id()));
        assert_eq!(state.balance(alice).unwrap().balance, 0);

        // `fund_tx` must also exclude the channel note: with no other
        // UTXOs available, funding a fee-paying tx must fail.
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                &Channels::default(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: 0,
        };
        let err = state
            .fund_tx::<Gas>(
                &MantleTxBuilder::new(),
                alice,
                [alice],
                &context,
                &HashSet::new(),
                0,
            )
            .unwrap_err();
        assert_eq!(err, WalletError::InsufficientFunds { available: 0 });
    }

    /// A `ChannelTransfer` must drop the consumed channel note from the
    /// previous delegate and hand the newly created channel note to the
    /// new delegate (if their key is known to the wallet), so `PoS`
    /// participation follows the delegation.
    #[test]
    fn test_channel_transfer_moves_notes() {
        let pk1 = pk(1);
        let pk2 = pk(2);
        let channel_id = ChannelId::from([1; 32]);
        let genesis = HeaderId::from([0; 32]);

        // Seed the wallet with 100 NMO note with `pk1` and mark it as a
        // channel note via a prior `ChannelDeposit`.
        let pk1_utxo = Utxo::new(tx_hash(0), 0, Note::new(100, pk1));
        let genesis_ledger = LedgerState::from_utxos([pk1_utxo], &ledger_config());
        let (v_cm_1, _) = voucher(1, 0);
        let (v_cm_2, _) = voucher(1, 1);
        let mut wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(pk1, 1), (pk2, 2)],
            Vouchers::default(),
            genesis,
            &genesis_ledger,
        );
        let deposit = DepositOp {
            channel_id,
            inputs: [pk1_utxo.id()].into(),
            metadata: Metadata::empty(),
        };
        let pk1_channel_note = Utxo::new(deposit.op_id(), 0, pk1_utxo.note);
        let deposit_block = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            epoch: 1.into(),
            voucher_cm: v_cm_1,
            header_ops: vec![],
            txs: [WalletTx {
                ops: [WalletOp::ChannelDeposit(deposit)].into(),
            }]
            .into(),
        };
        wallet.apply_block(&deposit_block).unwrap();

        // Transfer the channel note to `pk2`.
        let transfer_op = ChannelTransferOp {
            channel_id,
            inputs: Inputs::new([pk1_channel_note.id()]),
            outputs: Outputs::new([Note::new(100, pk2)]),
        };
        let pk2_output_id = transfer_op
            .outputs
            .utxo_by_index(0, &transfer_op)
            .unwrap()
            .id();
        let transfer_block = WalletBlock {
            id: HeaderId::from([2; 32]),
            parent: deposit_block.id,
            epoch: 1.into(),
            voucher_cm: v_cm_2,
            header_ops: vec![],
            txs: [WalletTx {
                ops: [WalletOp::ChannelTransfer(transfer_op)].into(),
            }]
            .into(),
        };
        wallet.apply_block(&transfer_block).unwrap();

        let state = wallet.wallet_state_at(transfer_block.id).unwrap();
        // The `pk1` channel note has been gone from the wallet.
        assert!(!state.utxos.contains_key(&pk1_channel_note.id()));
        assert!(!state.channel_notes.contains(&pk1_channel_note.id()));
        assert!(
            state
                .pk_index
                .get(&pk1)
                .is_none_or(|set| !set.contains(&pk1_channel_note.id()))
        );
        // The `pk2` note has been added to the wallet, as a channel note.
        assert!(state.utxos.contains_key(&pk2_output_id));
        assert!(state.channel_notes.contains(&pk2_output_id));
        assert!(
            state
                .pk_index
                .get(&pk2)
                .is_some_and(|set| set.contains(&pk2_output_id))
        );
        // Balances stay at zero for both: the note is still channel-owned.
        assert_eq!(state.balance(pk1).map_or(0, |b| b.balance), 0);
        assert_eq!(state.balance(pk2).unwrap().balance, 0);
    }

    /// A `ChannelTransfer` output whose new `ZkPublicKey` is unknown to
    /// the wallet must not be inserted — the wallet stops tracking the
    /// note entirely.
    #[test]
    fn test_channel_transfer_ignores_unknown_outputs() {
        let alice = pk(1);
        let stranger = pk(99);
        let channel_id = ChannelId::from([1; 32]);
        let genesis = HeaderId::from([0; 32]);

        let alice_utxo = Utxo::new(tx_hash(0), 0, Note::new(100, alice));
        let genesis_ledger = LedgerState::from_utxos([alice_utxo], &ledger_config());
        let (v_cm_1, _) = voucher(1, 0);
        let (v_cm_2, _) = voucher(1, 1);
        let mut wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(alice, 1)],
            Vouchers::default(),
            genesis,
            &genesis_ledger,
        );
        let deposit = DepositOp {
            channel_id,
            inputs: [alice_utxo.id()].into(),
            metadata: Metadata::empty(),
        };
        let alice_channel_note = Utxo::new(deposit.op_id(), 0, alice_utxo.note);
        let deposit_block = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            epoch: 1.into(),
            voucher_cm: v_cm_1,
            header_ops: vec![],
            txs: [WalletTx {
                ops: [WalletOp::ChannelDeposit(deposit)].into(),
            }]
            .into(),
        };
        wallet.apply_block(&deposit_block).unwrap();

        // Transfer Alice -> an unknown-to-wallet pk.
        let transfer_op = ChannelTransferOp {
            channel_id,
            inputs: Inputs::new([alice_channel_note.id()]),
            outputs: Outputs::new([Note::new(100, stranger)]),
        };
        let stranger_output_id = transfer_op
            .outputs
            .utxo_by_index(0, &transfer_op)
            .unwrap()
            .id();
        let transfer_block = WalletBlock {
            id: HeaderId::from([2; 32]),
            parent: deposit_block.id,
            epoch: 1.into(),
            voucher_cm: v_cm_2,
            header_ops: vec![],
            txs: [WalletTx {
                ops: [WalletOp::ChannelTransfer(transfer_op)].into(),
            }]
            .into(),
        };
        wallet.apply_block(&transfer_block).unwrap();

        let state = wallet.wallet_state_at(transfer_block.id).unwrap();
        // Alice's channel note is gone.
        assert!(!state.utxos.contains_key(&alice_channel_note.id()));
        assert!(!state.channel_notes.contains(&alice_channel_note.id()));
        // Stranger's new note is not tracked.
        assert!(!state.utxos.contains_key(&stranger_output_id));
        assert!(!state.channel_notes.contains(&stranger_output_id));
    }

    /// A `ChannelWithdraw` unmarks channel notes so the wallet can spend
    /// them again.
    #[test]
    fn test_channel_withdraw_releases_note() {
        let alice = pk(1);
        let channel_id = ChannelId::from([1; 32]);
        let genesis = HeaderId::from([0; 32]);

        // Seed Alice with a 100 NMO note and deposit it into a channel.
        let alice_utxo = Utxo::new(tx_hash(0), 0, Note::new(100, alice));
        let genesis_ledger = LedgerState::from_utxos([alice_utxo], &ledger_config());
        let (v_cm_1, _) = voucher(1, 0);
        let (v_cm_2, _) = voucher(1, 1);
        let mut wallet = Wallet::<_, TestVoucherId>::from_lib_ledger_state(
            [(alice, 1)],
            Vouchers::default(),
            genesis,
            &genesis_ledger,
        );
        let deposit = DepositOp {
            channel_id,
            inputs: [alice_utxo.id()].into(),
            metadata: Metadata::empty(),
        };
        let alice_channel_note = Utxo::new(deposit.op_id(), 0, alice_utxo.note);
        let deposit_block = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            epoch: 1.into(),
            voucher_cm: v_cm_1,
            header_ops: vec![],
            txs: [WalletTx {
                ops: [WalletOp::ChannelDeposit(deposit)].into(),
            }]
            .into(),
        };
        wallet.apply_block(&deposit_block).unwrap();

        // Sanity: after deposit the note is channel-marked, so balance is 0.
        let state = wallet.wallet_state_at(deposit_block.id).unwrap();
        assert!(state.channel_notes.contains(&alice_channel_note.id()));
        assert_eq!(state.balance(alice).unwrap().balance, 0);

        // Withdraw releases the channel note back to spendable. It keeps the
        // NoteId it got when the deposit re-created it.
        let withdraw_op = ChannelWithdrawOp {
            channel_id,
            inputs: Inputs::new([alice_channel_note.id()]),
        };
        let withdraw_block = WalletBlock {
            id: HeaderId::from([2; 32]),
            parent: deposit_block.id,
            epoch: 1.into(),
            voucher_cm: v_cm_2,
            header_ops: vec![],
            txs: [WalletTx {
                ops: [WalletOp::ChannelWithdraw(withdraw_op)].into(),
            }]
            .into(),
        };
        wallet.apply_block(&withdraw_block).unwrap();

        let state = wallet.wallet_state_at(withdraw_block.id).unwrap();
        assert!(state.utxos.contains_key(&alice_channel_note.id()));
        assert!(!state.channel_notes.contains(&alice_channel_note.id()));
        assert!(
            state
                .pk_index
                .get(&alice)
                .is_some_and(|set| set.contains(&alice_channel_note.id()))
        );
        assert_eq!(state.balance(alice).unwrap().balance, 100);

        // `fund_tx` can now use the released note.
        let context = MantleTxContext {
            gas_context: MantleTxGasContext::from_channels(
                &Channels::default(),
                GasPrices::new(1, 1),
            ),
            leader_reward_amount: 0,
        };
        let err = state
            .fund_tx::<Gas>(
                &MantleTxBuilder::new(),
                alice,
                [alice],
                &context,
                &HashSet::new(),
                0,
            )
            .unwrap_err();
        // The error detail says that the withdrawn note is now spendable.
        assert_eq!(err, WalletError::InsufficientFunds { available: 100 });
    }

    #[test]
    fn wallet_block_transformation_preserves_bounds_and_source_order() {
        let first_input = NoteId::from(Fr::from(1));
        let second_input = NoteId::from(Fr::from(2));

        let transfer = Op::Transfer(TransferOp::new(Inputs::empty(), Outputs::new([])));

        let deposit = Op::ChannelDeposit(DepositOp {
            channel_id: ChannelId::from([0; 32]),
            inputs: Inputs::new([first_input, second_input]),
            metadata: Metadata::default(),
        });

        let ignored_inscription = Op::ChannelInscribe(InscriptionOp {
            channel_id: ChannelId::from([0; 32]),
            inscription: Inscription::default(),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[0; 32]).public_key(),
        });

        let source_transactions: BlockTransactions<SignedMantleTx<Unverified>> = [
            signed_test_tx(vec![transfer, deposit]),
            signed_test_tx(vec![ignored_inscription]),
        ]
        .into();

        let source_block = Block::create(
            HeaderId::from([0; 32]),
            Slot::from(1),
            test_leader_proof(),
            source_transactions,
            &Ed25519Key::from_bytes(&[0; 32]),
        )
        .expect("test block should be valid");

        let wallet_block =
            WalletBlock::from_block(&source_block, Epoch::new(0), &Events::default());

        assert_eq!(wallet_block.txs.len(), source_block.transactions().len());

        // Transaction order is preserved.
        assert_eq!(wallet_block.txs.len(), 2);

        // The first transaction keeps both wallet-relevant operations in order.
        let first_wallet_tx = &wallet_block.txs[0];
        assert_eq!(first_wallet_tx.ops.len(), 2);
        assert!(matches!(first_wallet_tx.ops[0], WalletOp::Transfer(_)));

        let WalletOp::ChannelDeposit(op) = &first_wallet_tx.ops[1] else {
            panic!("expected channel deposit as the second wallet operation");
        };

        assert_eq!(
            op.inputs.clone().into_inner().as_slice(),
            &[first_input, second_input]
        );

        // A transaction containing only ignored operations is retained, but its
        // wallet operation list is empty.
        let second_wallet_tx = &wallet_block.txs[1];
        assert!(second_wallet_tx.ops.is_empty());
    }

    fn signed_test_tx(ops: Vec<Op>) -> SignedMantleTx<Unverified> {
        let proofs = OpsProofs::try_from_iter(ops.iter().map(|op| match op {
            Op::ChannelInscribe(_) => OpProof::Ed25519Sig(Ed25519Signature::zero()),
            _ => OpProof::ZkSig(ZkSignature::new(CompressedGroth16Proof::from_bytes(
                &[0; 128],
            ))),
        }))
        .expect("test proofs should fit");

        SignedMantleTx::new(
            RawMantleTx(Ops::try_from(ops).expect("test operations should fit")),
            proofs,
        )
    }

    fn test_leader_proof() -> Groth16LeaderProof {
        let leader_sk = UnsecuredZkKey::zero();
        let utxo = Utxo::new(tx_hash(0), 0, Note::new(1000, leader_sk.to_public_key()));
        let utxo_tree = UtxoTree::<_, _, ZkHasher>::new().insert(utxo.id(), utxo).0;
        let utxo_merkle_path = utxo_tree.path(&utxo.id()).unwrap();
        let (lottery_0, lottery_1) =
            LotteryConstants::new(NonNegativeRatio::new(1, 10.try_into().unwrap()))
                .compute_lottery_values(1000);
        let public_inputs = (0..1000)
            .map(|nonce| {
                LeaderPublic::new(
                    utxo_tree.root(),
                    utxo_tree.root(),
                    Fr::from(nonce),
                    0,
                    lottery_0,
                    lottery_1,
                )
            })
            .find(|inputs| {
                inputs.check_winning(utxo.note.value, *utxo.id().as_fr(), *leader_sk.as_fr())
            })
            .unwrap();
        let signing_key = Ed25519Key::from_bytes(&[0; 32]);
        Groth16LeaderProof::prove(
            LeaderPrivate::new(
                public_inputs,
                utxo,
                &utxo_merkle_path,
                &utxo_merkle_path,
                *leader_sk.as_fr(),
                &signing_key.public_key(),
            ),
            VoucherCm::default(),
        )
        .unwrap()
    }
}
